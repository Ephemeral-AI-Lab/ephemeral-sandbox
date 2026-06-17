use std::{future::Future, net::Ipv4Addr, thread};

use futures_util::stream::TryStreamExt;
use rtnetlink::{new_connection, Handle, LinkBridge, LinkBridgePort, LinkUnspec, LinkVeth};

use crate::isolated_workspace::error::IsolatedError;

use super::{network_error_at, BRIDGE_NAME, BRIDGE_PREFIX_LEN};

pub(super) fn run_netlink<T, F, Fut>(operation: F) -> Result<T, IsolatedError>
where
    T: Send + 'static,
    F: FnOnce(Handle) -> Fut + Send + 'static,
    Fut: Future<Output = Result<T, IsolatedError>> + Send + 'static,
{
    thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
            .map_err(|err| network_error_at("build netlink runtime", err))?;
        runtime.block_on(async move {
            let (connection, handle, _) = new_connection()
                .map_err(|err| network_error_at("open route netlink socket", err))?;
            tokio::spawn(connection);
            operation(handle).await
        })
    })
    .join()
    .map_err(|_| IsolatedError::NetworkUnavailable("netlink thread panicked".to_owned()))?
}

pub(super) async fn ensure_bridge(handle: &Handle) -> Result<u32, IsolatedError> {
    if link_index(handle, BRIDGE_NAME).await?.is_none() {
        ignore_exists(
            "create shared bridge",
            handle
                .link()
                .add(LinkBridge::new(BRIDGE_NAME).up().build())
                .execute()
                .await,
        )?;
    }
    let bridge_index = require_link_index(handle, BRIDGE_NAME).await?;
    ignore_exists(
        "add shared bridge gateway",
        handle
            .address()
            .add(bridge_index, gateway_addr().into(), BRIDGE_PREFIX_LEN)
            .execute()
            .await,
    )?;
    ignore_exists(
        "bring shared bridge up",
        handle
            .link()
            .change(LinkUnspec::new_with_index(bridge_index).up().build())
            .execute()
            .await,
    )?;
    Ok(bridge_index)
}

pub(super) async fn install_veth_pair(
    handle: &Handle,
    host_name: &str,
    ns_name: &str,
    holder_pid: u32,
) -> Result<(), IsolatedError> {
    let bridge_index = require_link_index(handle, BRIDGE_NAME).await?;
    if link_index(handle, host_name).await?.is_none() {
        ignore_exists(
            "create veth pair",
            handle
                .link()
                .add(LinkVeth::new(host_name, ns_name).build())
                .execute()
                .await,
        )?;
    }
    if let Some(ns_index) = link_index(handle, ns_name).await? {
        ignore_exists(
            "move namespace veth into holder netns",
            handle
                .link()
                .change(
                    LinkUnspec::new_with_index(ns_index)
                        .setns_by_pid(holder_pid)
                        .build(),
                )
                .execute()
                .await,
        )?;
    }
    let host_index = require_link_index(handle, host_name).await?;
    ignore_exists(
        "attach host veth to bridge",
        handle
            .link()
            .change(
                LinkUnspec::new_with_index(host_index)
                    .controller(bridge_index)
                    .up()
                    .build(),
            )
            .execute()
            .await,
    )?;
    ignore_unsupported(
        "set bridge port isolation",
        handle
            .link()
            .set_port(
                LinkBridgePort::new(host_index)
                    .isolated(true)
                    .mcast_flood(false)
                    .build(),
            )
            .execute()
            .await,
    )?;
    Ok(())
}

async fn require_link_index(handle: &Handle, name: &str) -> Result<u32, IsolatedError> {
    link_index(handle, name)
        .await?
        .ok_or_else(|| IsolatedError::NetworkUnavailable(format!("link {name} not found")))
}

pub(super) async fn link_index(handle: &Handle, name: &str) -> Result<Option<u32>, IsolatedError> {
    let mut links = handle.link().get().match_name(name.to_owned()).execute();
    match links.try_next().await {
        Ok(link) => Ok(link.map(|link| link.header.index)),
        Err(error) if is_error_text(&error, &["not found", "no such", "-19"]) => Ok(None),
        Err(error) => Err(network_error_at(format!("query link {name}"), error)),
    }
}

fn ignore_exists(
    step: impl Into<String>,
    result: Result<(), rtnetlink::Error>,
) -> Result<(), IsolatedError> {
    ignore_matching(step, result, &["exists", "-17"])
}

pub(super) fn ignore_not_found(
    step: impl Into<String>,
    result: Result<(), rtnetlink::Error>,
) -> Result<(), IsolatedError> {
    ignore_matching(step, result, &["not found", "no such", "-19"])
}

fn ignore_unsupported(
    step: impl Into<String>,
    result: Result<(), rtnetlink::Error>,
) -> Result<(), IsolatedError> {
    ignore_matching(
        step,
        result,
        &[
            "operation not supported",
            "not supported",
            "no such device",
            "-19",
        ],
    )
}

fn ignore_matching(
    step: impl Into<String>,
    result: Result<(), rtnetlink::Error>,
    needles: &[&str],
) -> Result<(), IsolatedError> {
    let step = step.into();
    match result {
        Ok(()) => Ok(()),
        Err(error) if is_error_text(&error, needles) => Ok(()),
        Err(error) => Err(network_error_at(step, error)),
    }
}

fn is_error_text(error: &rtnetlink::Error, needles: &[&str]) -> bool {
    let text = error.to_string().to_ascii_lowercase();
    needles.iter().any(|needle| text.contains(needle))
}

const fn gateway_addr() -> Ipv4Addr {
    Ipv4Addr::new(10, 244, 0, 1)
}
