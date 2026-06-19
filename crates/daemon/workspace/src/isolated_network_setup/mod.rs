use std::collections::BTreeSet;
use std::net::Ipv4Addr;

use crate::namespace::test_harness_enabled;
use crate::profile::IsolatedNetworkError;
use crate::profile::{Rfc1918Egress, HANDLE_PREFIX};

mod dns;
#[cfg(target_os = "linux")]
mod netfilter;
#[cfg(target_os = "linux")]
mod rtnl;

#[cfg(target_os = "linux")]
use netfilter::install_static_rules;
#[cfg(target_os = "linux")]
use rtnl::{ensure_bridge, ignore_not_found, install_veth_pair, link_index, run_netlink};

#[cfg(target_os = "linux")]
pub const BRIDGE_NAME: &str = "eos-shared0";
pub(crate) const BRIDGE_CIDR: &str = "10.244.0.0/24";
#[cfg(target_os = "linux")]
pub const GATEWAY: &str = "10.244.0.1";
#[cfg(target_os = "linux")]
pub const NFT_NAT_TABLE: &str = "eos_iws_nat";
#[cfg(target_os = "linux")]
pub const NFT_FILTER_TABLE: &str = "eos_iws_filter";
#[cfg(target_os = "linux")]
pub const IMDS_ADDR: &str = "169.254.169.254";
#[cfg(target_os = "linux")]
pub const RFC1918_NETS: [&str; 3] = ["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16"];
pub(crate) const VETH_PREFIX: &str = HANDLE_PREFIX;

#[cfg(target_os = "linux")]
pub const BRIDGE_PREFIX_LEN: u8 = 24;
pub(crate) const POOL_FIRST_HOST: u8 = 2;
pub(crate) const POOL_LAST_HOST: u8 = 254;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VethAllocation {
    pub host_name: String,
    pub ns_name: String,
    pub ns_ip: Ipv4Addr,
}

#[must_use]
pub(crate) fn veth_names(workspace_handle_id: &str) -> (String, String) {
    let short: String = workspace_handle_id.chars().take(6).collect();
    (
        format!("{VETH_PREFIX}{short}h"),
        format!("{VETH_PREFIX}{short}n"),
    )
}

#[derive(Debug, Clone, Default)]
pub(crate) struct BridgeAddressPool {
    allocated: BTreeSet<Ipv4Addr>,
}

impl BridgeAddressPool {
    pub fn reserve(&mut self, ip: Ipv4Addr) -> Result<(), IsolatedNetworkError> {
        if !is_pool_ip(ip) {
            return Err(IsolatedNetworkError::NetworkUnavailable(format!(
                "isolated network IP {ip} is outside {BRIDGE_CIDR}"
            )));
        }
        self.allocated.insert(ip);
        Ok(())
    }

    pub fn allocate(&mut self) -> Result<Ipv4Addr, IsolatedNetworkError> {
        for host in POOL_FIRST_HOST..=POOL_LAST_HOST {
            let ip = Ipv4Addr::new(10, 244, 0, host);
            if self.allocated.insert(ip) {
                return Ok(ip);
            }
        }
        Err(IsolatedNetworkError::NetworkUnavailable(
            "isolated_network_ip_pool_exhausted".to_owned(),
        ))
    }

    pub fn free(&mut self, ip: Ipv4Addr) {
        self.allocated.remove(&ip);
    }
}

#[derive(Debug)]
pub(crate) struct IsolatedNetwork {
    rfc1918_egress: Rfc1918Egress,
    pool: BridgeAddressPool,
    initialized: bool,
}

impl IsolatedNetwork {
    #[must_use]
    pub fn new(rfc1918_egress: Rfc1918Egress) -> Self {
        Self {
            rfc1918_egress,
            pool: BridgeAddressPool::default(),
            initialized: false,
        }
    }

    pub fn initialize(&mut self) -> Result<(), IsolatedNetworkError> {
        if test_harness_enabled() {
            self.initialized = true;
            return Ok(());
        }
        let rfc1918_egress = self.rfc1918_egress;
        #[cfg(target_os = "linux")]
        {
            run_netlink(move |handle| async move {
                let bridge_index = ensure_bridge(&handle).await?;
                install_static_rules(rfc1918_egress, bridge_index)?;
                Ok(())
            })?;
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = rfc1918_egress;
        }
        self.initialized = true;
        Ok(())
    }

    pub fn install_veth(
        &mut self,
        workspace_handle_id: &str,
        holder_pid: i32,
    ) -> Result<VethAllocation, IsolatedNetworkError> {
        if !self.initialized {
            self.initialize()?;
        }
        let (host_name, ns_name) = veth_names(workspace_handle_id);
        let ns_ip = self.pool.allocate()?;
        if !test_harness_enabled() && holder_pid > 0 {
            #[cfg(target_os = "linux")]
            {
                let host = host_name.clone();
                let ns = ns_name.clone();
                let holder_pid = u32::try_from(holder_pid).map_err(|_| {
                    IsolatedNetworkError::NetworkUnavailable(format!(
                        "invalid isolated holder pid {holder_pid}"
                    ))
                })?;
                if let Err(error) = run_netlink(move |handle| async move {
                    install_veth_pair(&handle, &host, &ns, holder_pid).await
                }) {
                    self.pool.free(ns_ip);
                    return Err(error);
                }
            }
        }
        Ok(VethAllocation {
            host_name,
            ns_name,
            ns_ip,
        })
    }

    pub(crate) fn install_stub_veth(
        &mut self,
        workspace_handle_id: &str,
    ) -> Result<VethAllocation, IsolatedNetworkError> {
        self.initialized = true;
        let (host_name, ns_name) = veth_names(workspace_handle_id);
        let ns_ip = self.pool.allocate()?;
        Ok(VethAllocation {
            host_name,
            ns_name,
            ns_ip,
        })
    }

    pub fn teardown_veth(&mut self, allocation: &VethAllocation) {
        self.teardown_host_veth(&allocation.host_name);
        self.pool.free(allocation.ns_ip);
    }

    pub(crate) fn release_stub_veth(&mut self, allocation: &VethAllocation) {
        self.pool.free(allocation.ns_ip);
    }

    pub fn teardown_host_veth(&mut self, host_name: &str) {
        #[cfg(not(target_os = "linux"))]
        let _ = host_name;
        if !test_harness_enabled() {
            #[cfg(target_os = "linux")]
            {
                let host_name = host_name.to_owned();
                let _ = run_netlink(move |handle| async move {
                    if let Some(index) = link_index(&handle, &host_name).await? {
                        ignore_not_found(
                            "delete host veth",
                            handle.link().del(index).execute().await,
                        )?;
                    }
                    Ok(())
                });
            }
        }
    }

    pub fn reserve_persisted_ip(&mut self, ip: Ipv4Addr) -> Result<(), IsolatedNetworkError> {
        self.pool.reserve(ip)
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn network_error_at(
    step: impl Into<String>,
    error: impl std::fmt::Display,
) -> IsolatedNetworkError {
    IsolatedNetworkError::NetworkUnavailable(format!("{}: {error}", step.into()))
}

fn is_pool_ip(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    octets[0] == 10
        && octets[1] == 244
        && octets[2] == 0
        && (POOL_FIRST_HOST..=POOL_LAST_HOST).contains(&octets[3])
}
