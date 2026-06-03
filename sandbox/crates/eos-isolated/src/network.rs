//! Shared bridge + per-workspace veth + nftables wiring for isolated workspaces.
//!
//! Daemon-scope state: one bridge `eos-shared0` with gateway `10.244.0.1/24`, a
//! MASQUERADE rule on outbound from `10.244.0.0/24`, an IMDS drop rule, and an
//! opt-in RFC1918-deny rule. Per-workspace state: one veth pair and one `/32`
//! from `10.244.0.2 - 10.244.0.254`.
//!
//! `// PORT backend/src/sandbox/isolated_workspace/network.py:27-34 — net constants`
//!
//! # IPv6 hardening — shell-free port
//!
//! The Python holder shells out (`sysctl`, `ip -6 route flush`, `ip link set lo
//! up`) to purge IPv6 default routes and disable router-advertisement
//! acceptance so the v4-only MASQUERADE rule stays the sole egress. The Rust
//! port replaces those binaries with `rtnetlink` (`RTM_DELROUTE` for the IPv6
//! default route, `RTM_NEWLINK` to bring `lo` up) and direct
//! `/proc/sys/net/ipv6/conf/<iface>/accept_ra` writes — NO `ip`/`sysctl`
//! binaries. This work executes inside the namespace via `eos-ns-holder`
//! (see `host_runtime`), not in this daemon-scope module.
//! `// PORT backend/src/sandbox/isolated_workspace/scripts/ns_holder.py:29-49 — IPv6 hardening`

#[cfg(target_os = "linux")]
use std::future::Future;
use std::net::Ipv4Addr;
#[cfg(target_os = "linux")]
use std::thread;

use crate::caps::{Rfc1918Egress, HANDLE_PREFIX};
use crate::error::IsolatedError;
#[cfg(target_os = "linux")]
use futures_util::stream::TryStreamExt;
#[cfg(target_os = "linux")]
use netlink_sys::{Socket as NlSocket, SocketAddr as NlSocketAddr};
#[cfg(target_os = "linux")]
use rtnetlink::{new_connection, Handle, LinkBridge, LinkBridgePort, LinkUnspec, LinkVeth};

/// Shared bridge interface name. `// PORT backend/src/sandbox/isolated_workspace/network.py:27`
pub const BRIDGE_NAME: &str = "eos-shared0";
/// Shared bridge CIDR. `// PORT backend/src/sandbox/isolated_workspace/network.py:28`
pub const BRIDGE_CIDR: &str = "10.244.0.0/24";
/// Bridge gateway address. `// PORT backend/src/sandbox/isolated_workspace/network.py:29`
pub const GATEWAY: &str = "10.244.0.1";
/// nftables NAT table name. `// PORT backend/src/sandbox/isolated_workspace/network.py:30`
pub const NFT_NAT_TABLE: &str = "eos_iws_nat";
/// nftables filter table name. `// PORT backend/src/sandbox/isolated_workspace/network.py:31`
pub const NFT_FILTER_TABLE: &str = "eos_iws_filter";
#[cfg(target_os = "linux")]
const NFT_BRIDGE_FILTER_TABLE: &str = "eos_iws_bridge_filter";
/// Cloud IMDS address dropped on the forward chain. `// PORT backend/src/sandbox/isolated_workspace/network.py:32`
pub const IMDS_ADDR: &str = "169.254.169.254";
/// RFC1918 private networks (for the opt-in deny rule). `// PORT backend/src/sandbox/isolated_workspace/network.py:33`
pub const RFC1918_NETS: [&str; 3] = ["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16"];
/// Per-workspace veth name prefix — the SAME literal as [`HANDLE_PREFIX`].
/// `// PORT backend/src/sandbox/isolated_workspace/network.py:34`
pub const VETH_PREFIX: &str = HANDLE_PREFIX;

/// Bridge CIDR prefix length (matches `BRIDGE_CIDR`).
pub const BRIDGE_PREFIX_LEN: u8 = 24;
/// First allocatable host octet (skips `.0` network + `.1` gateway).
pub const POOL_FIRST_HOST: u8 = 2;
/// Last allocatable host octet (skips `.255` broadcast).
pub const POOL_LAST_HOST: u8 = 254;

/// One veth `/32` allocation for a workspace.
/// `// PORT backend/src/sandbox/isolated_workspace/network.py:41-44 — VethAllocation`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VethAllocation {
    /// Host-side veth name (attached to the bridge).
    pub host_name: String,
    /// Namespace-side veth name (moved into the holder netns).
    pub ns_name: String,
    /// Namespace-side IPv4 address allocated from the pool.
    pub ns_ip: Ipv4Addr,
}

/// Host/peer veth names for a workspace handle.
///
/// Linux `IFNAMSIZ` caps names at 15 chars: `eos-iws-` (8) + 6 handle chars +
/// suffix (1) = 15 exactly. Host ends in `h`, peer ends in `n`.
/// `// PORT backend/src/sandbox/isolated_workspace/network.py:231-235 — _veth_names`
#[must_use]
pub fn veth_names(workspace_handle_id: &str) -> (String, String) {
    // Python `_veth_names` takes the FIRST 6 chars (`workspace_handle_id[:6]`);
    // reproduce that exactly so the interface naming is parity-equal.
    let short: String = workspace_handle_id.chars().take(6).collect();
    (
        format!("{VETH_PREFIX}{short}h"),
        format!("{VETH_PREFIX}{short}n"),
    )
}

/// Pure IPv4 `/32` allocator over `10.244.0.2 - 10.244.0.254`.
///
/// Lowest-IP-first O(N) scan; N <= 253. No Linux deps.
/// `// PORT backend/src/sandbox/isolated_workspace/network.py:47-75 — BridgeAddressPool`
#[derive(Debug, Clone, Default)]
pub struct BridgeAddressPool {
    allocated: Vec<Ipv4Addr>,
}

impl BridgeAddressPool {
    /// Build an empty pool spanning the bridge CIDR's allocatable range.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark `ip` as in-use (used to rebuild pool state from `manager.json`).
    ///
    /// # Errors
    ///
    /// Returns [`IsolatedError::NetworkUnavailable`] when `ip` is outside
    /// [`BRIDGE_CIDR`].
    // PORT backend/src/sandbox/isolated_workspace/network.py:60-64 — BridgeAddressPool.reserve
    pub fn reserve(&mut self, ip: Ipv4Addr) -> Result<(), IsolatedError> {
        if !is_pool_ip(ip) {
            return Err(IsolatedError::NetworkUnavailable(format!(
                "isolated workspace IP {ip} is outside {BRIDGE_CIDR}"
            )));
        }
        if !self.allocated.contains(&ip) {
            self.allocated.push(ip);
            self.allocated.sort_unstable();
        }
        Ok(())
    }

    /// Allocate the lowest free `/32` in the pool.
    ///
    /// # Errors
    ///
    /// Returns [`IsolatedError::NetworkUnavailable`] when the address pool is
    /// exhausted.
    // PORT backend/src/sandbox/isolated_workspace/network.py:66-72 — BridgeAddressPool.allocate
    pub fn allocate(&mut self) -> Result<Ipv4Addr, IsolatedError> {
        for host in POOL_FIRST_HOST..=POOL_LAST_HOST {
            let ip = Ipv4Addr::new(10, 244, 0, host);
            if !self.allocated.contains(&ip) {
                self.allocated.push(ip);
                self.allocated.sort_unstable();
                return Ok(ip);
            }
        }
        Err(IsolatedError::NetworkUnavailable(
            "isolated_workspace_ip_pool_exhausted".to_owned(),
        ))
    }

    /// Release `ip` back into the pool.
    // PORT backend/src/sandbox/isolated_workspace/network.py:74-75 — BridgeAddressPool.free
    pub fn free(&mut self, ip: Ipv4Addr) {
        self.allocated.retain(|allocated| *allocated != ip);
    }
}

/// Owns the `eos-shared0` bridge + static nft rules + per-workspace veth wiring.
///
/// The Python implementation shells out to `ip`/`nft`; the Rust port replaces
/// the bridge/veth path with `rtnetlink` link/address operations and the static
/// NAT/filter path with `NETLINK_NETFILTER` messages — NO `ip`/`nft` binaries.
/// `// PORT backend/src/sandbox/isolated_workspace/network.py:78-228 — IsolatedNetwork`
#[derive(Debug)]
pub struct IsolatedNetwork {
    rfc1918_egress: Rfc1918Egress,
    pool: BridgeAddressPool,
    initialized: bool,
}

impl IsolatedNetwork {
    /// Construct an uninitialized network with the given egress policy.
    #[must_use]
    pub fn new(rfc1918_egress: Rfc1918Egress) -> Self {
        Self {
            rfc1918_egress,
            pool: BridgeAddressPool::new(),
            initialized: false,
        }
    }

    /// Whether [`initialize`](Self::initialize) has installed the bridge + rules.
    #[must_use]
    pub const fn initialized(&self) -> bool {
        self.initialized
    }

    /// Install the bridge + MASQUERADE + IMDS drop (+ optional RFC1918 deny).
    /// Idempotent for table/chain creation; rule insertion mirrors Python's
    /// sequential `nft add rule` calls.
    ///
    /// # Errors
    ///
    /// Returns [`IsolatedError::NetworkUnavailable`] when route or netfilter
    /// netlink setup fails.
    // PORT backend/src/sandbox/isolated_workspace/network.py:95-100 — IsolatedNetwork.initialize (require_tools/ensure_bridge/install_static_rules)
    pub fn initialize(&mut self) -> Result<(), IsolatedError> {
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

    /// Create a veth pair, attach the host end to the bridge with port
    /// isolation, and configure the namespace-side end (up, `/24` addr,
    /// default route via gateway).
    ///
    /// # Errors
    ///
    /// Returns [`IsolatedError::NetworkUnavailable`] when bridge initialization,
    /// IP allocation, holder PID conversion, or veth netlink wiring fails.
    // PORT backend/src/sandbox/isolated_workspace/network.py:102-146 — IsolatedNetwork.install_veth
    pub fn install_veth(
        &mut self,
        workspace_handle_id: &str,
        holder_pid: i32,
    ) -> Result<VethAllocation, IsolatedError> {
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
                    IsolatedError::NetworkUnavailable(format!(
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

    /// Tear down a veth pair and return its `/32` to the pool.
    // PORT backend/src/sandbox/isolated_workspace/network.py:148-150 — IsolatedNetwork.teardown_veth
    pub fn teardown_veth(&mut self, allocation: &VethAllocation) {
        self.teardown_host_veth(&allocation.host_name);
        self.pool.free(allocation.ns_ip);
    }

    /// Delete a host-side veth by name without touching the IP pool.
    ///
    /// Startup/test cleanup uses this for naming-convention orphan sweeps where
    /// no persisted `/32` allocation is available.
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

    /// Reserve a previously persisted namespace IP during startup reconciliation.
    ///
    /// Crash-recovered handles are reaped before new enters run, but their
    /// persisted IPs stay reserved for this daemon lifetime so a fresh handle
    /// cannot race a stale kernel resource that is still being torn down.
    ///
    /// # Errors
    ///
    /// Returns [`IsolatedError::NetworkUnavailable`] when `ip` is outside the
    /// isolated bridge pool.
    pub fn reserve_persisted_ip(&mut self, ip: Ipv4Addr) -> Result<(), IsolatedError> {
        self.pool.reserve(ip)
    }
}

fn test_harness_enabled() -> bool {
    std::env::var("EOS_ISOLATED_WORKSPACE_TEST_HARNESS")
        .is_ok_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        && std::env::var("EOS_ISOLATED_WORKSPACE_TEST_SCRATCH_ROOT")
            .is_ok_and(|value| !value.trim().is_empty())
}

#[cfg(target_os = "linux")]
const fn gateway_addr() -> Ipv4Addr {
    Ipv4Addr::new(10, 244, 0, 1)
}

#[cfg(target_os = "linux")]
fn run_netlink<T, F, Fut>(operation: F) -> Result<T, IsolatedError>
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

#[cfg(target_os = "linux")]
async fn ensure_bridge(handle: &Handle) -> Result<u32, IsolatedError> {
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

#[cfg(target_os = "linux")]
fn install_static_rules(
    rfc1918_egress: Rfc1918Egress,
    bridge_index: u32,
) -> Result<(), IsolatedError> {
    add_nft_table(NFT_NAT_TABLE)?;
    add_nft_base_chain(
        NFT_NAT_TABLE,
        "postrouting",
        "nat",
        libc_c_int_to_u32(libc::NF_INET_POST_ROUTING, "NF_INET_POST_ROUTING")?,
        100,
    )?;
    add_nft_rule(
        NFT_NAT_TABLE,
        "postrouting",
        nft_masquerade_rule_exprs(bridge_index)?,
    )?;

    add_nft_table(NFT_FILTER_TABLE)?;
    add_nft_base_chain(
        NFT_FILTER_TABLE,
        "forward",
        "filter",
        libc_c_int_to_u32(libc::NF_INET_FORWARD, "NF_INET_FORWARD")?,
        0,
    )?;
    add_nft_rule(NFT_FILTER_TABLE, "forward", nft_imds_drop_rule_exprs()?)?;
    add_nft_rule(
        NFT_FILTER_TABLE,
        "forward",
        nft_peer_isolation_rule_exprs()?,
    )?;
    install_bridge_peer_isolation_rule()?;
    if rfc1918_egress == Rfc1918Egress::Deny {
        for cidr in RFC1918_NETS {
            add_nft_rule(
                NFT_FILTER_TABLE,
                "forward",
                nft_rfc1918_drop_rule_exprs(cidr)?,
            )?;
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn install_bridge_peer_isolation_rule() -> Result<(), IsolatedError> {
    let family = libc_c_int_to_u8(libc::NFPROTO_BRIDGE, "NFPROTO_BRIDGE")?;
    add_nft_table_in_family(family, NFT_BRIDGE_FILTER_TABLE)?;
    add_nft_base_chain_in_family(
        family,
        NFT_BRIDGE_FILTER_TABLE,
        "forward",
        "filter",
        libc_c_int_to_u32(libc::NF_BR_FORWARD, "NF_BR_FORWARD")?,
        0,
    )?;
    add_nft_rule_in_family(
        family,
        NFT_BRIDGE_FILTER_TABLE,
        "forward",
        nft_bridge_peer_isolation_rule_exprs()?,
    )
}

#[cfg(target_os = "linux")]
fn add_nft_table(name: &str) -> Result<(), IsolatedError> {
    add_nft_table_in_family(libc_c_int_to_u8(libc::NFPROTO_INET, "NFPROTO_INET")?, name)
}

#[cfg(target_os = "linux")]
fn add_nft_table_in_family(family: u8, name: &str) -> Result<(), IsolatedError> {
    let mut attrs = Vec::new();
    append_cstr_attr(&mut attrs, NFTA_TABLE_NAME, name);
    append_be_u32_attr(&mut attrs, NFTA_TABLE_FLAGS, 0);
    send_nft_command(
        family,
        format!("create nft table {name}"),
        libc_c_int_to_u16(libc::NFT_MSG_NEWTABLE, "NFT_MSG_NEWTABLE")?,
        nft_create_flags(),
        &attrs,
        true,
    )
}

#[cfg(target_os = "linux")]
fn add_nft_base_chain(
    table: &str,
    name: &str,
    chain_type: &str,
    hook: u32,
    priority: i32,
) -> Result<(), IsolatedError> {
    add_nft_base_chain_in_family(
        libc_c_int_to_u8(libc::NFPROTO_INET, "NFPROTO_INET")?,
        table,
        name,
        chain_type,
        hook,
        priority,
    )
}

#[cfg(target_os = "linux")]
fn add_nft_base_chain_in_family(
    family: u8,
    table: &str,
    name: &str,
    chain_type: &str,
    hook: u32,
    priority: i32,
) -> Result<(), IsolatedError> {
    let mut hook_attrs = Vec::new();
    append_be_u32_attr(&mut hook_attrs, NFTA_HOOK_HOOKNUM, hook);
    append_be_i32_attr(&mut hook_attrs, NFTA_HOOK_PRIORITY, priority);

    let mut attrs = Vec::new();
    append_cstr_attr(&mut attrs, NFTA_CHAIN_TABLE, table);
    append_cstr_attr(&mut attrs, NFTA_CHAIN_NAME, name);
    append_cstr_attr(&mut attrs, NFTA_CHAIN_TYPE, chain_type);
    append_nested_attr(&mut attrs, NFTA_CHAIN_HOOK, &hook_attrs);
    send_nft_command(
        family,
        format!("create nft chain {table}/{name}"),
        libc_c_int_to_u16(libc::NFT_MSG_NEWCHAIN, "NFT_MSG_NEWCHAIN")?,
        nft_create_flags(),
        &attrs,
        true,
    )
}

#[cfg(target_os = "linux")]
fn add_nft_rule(table: &str, chain: &str, expressions: Vec<Vec<u8>>) -> Result<(), IsolatedError> {
    add_nft_rule_in_family(
        libc_c_int_to_u8(libc::NFPROTO_INET, "NFPROTO_INET")?,
        table,
        chain,
        expressions,
    )
}

#[cfg(target_os = "linux")]
fn add_nft_rule_in_family(
    family: u8,
    table: &str,
    chain: &str,
    expressions: Vec<Vec<u8>>,
) -> Result<(), IsolatedError> {
    let mut expression_list = Vec::new();
    for expression in expressions {
        append_nested_attr(&mut expression_list, NFTA_LIST_ELEM, &expression);
    }

    let mut attrs = Vec::new();
    append_cstr_attr(&mut attrs, NFTA_RULE_TABLE, table);
    append_cstr_attr(&mut attrs, NFTA_RULE_CHAIN, chain);
    append_nested_attr(&mut attrs, NFTA_RULE_EXPRESSIONS, &expression_list);
    send_nft_command(
        family,
        format!("add nft rule {table}/{chain}"),
        libc_c_int_to_u16(libc::NFT_MSG_NEWRULE, "NFT_MSG_NEWRULE")?,
        nft_rule_flags(),
        &attrs,
        true,
    )
}

#[cfg(target_os = "linux")]
fn nft_masquerade_rule_exprs(bridge_index: u32) -> Result<Vec<Vec<u8>>, IsolatedError> {
    let (bridge_net, bridge_prefix) = bridge_network();
    let mut expressions =
        nft_ipv4_network_match(IPV4_SADDR_OFFSET, bridge_net, bridge_prefix, NFT_CMP_EQ)?;
    expressions.push(nft_meta_expr(libc_c_int_to_u32(
        libc::NFT_META_OIF,
        "NFT_META_OIF",
    )?));
    expressions.push(nft_cmp_expr(
        NFT_CMP_NEQ,
        bridge_index.to_ne_bytes().as_slice(),
    ));
    expressions.push(nft_expr("masq", &[]));
    Ok(expressions)
}

#[cfg(target_os = "linux")]
fn nft_imds_drop_rule_exprs() -> Result<Vec<Vec<u8>>, IsolatedError> {
    let imds = parse_ipv4_addr(IMDS_ADDR)?;
    let mut expressions = nft_ipv4_guard_exprs()?;
    expressions.push(nft_payload_ipv4_expr(IPV4_DADDR_OFFSET)?);
    expressions.push(nft_cmp_expr(NFT_CMP_EQ, &imds.octets()));
    expressions.push(nft_drop_expr()?);
    Ok(expressions)
}

#[cfg(target_os = "linux")]
fn nft_peer_isolation_rule_exprs() -> Result<Vec<Vec<u8>>, IsolatedError> {
    let gateway = gateway_addr();
    let (bridge_net, bridge_prefix) = bridge_network();
    let mut expressions =
        nft_ipv4_network_match(IPV4_SADDR_OFFSET, bridge_net, bridge_prefix, NFT_CMP_EQ)?;
    expressions.extend(nft_ipv4_addr_match(
        IPV4_SADDR_OFFSET,
        gateway,
        NFT_CMP_NEQ,
    )?);
    expressions.extend(nft_ipv4_network_match(
        IPV4_DADDR_OFFSET,
        bridge_net,
        bridge_prefix,
        NFT_CMP_EQ,
    )?);
    expressions.extend(nft_ipv4_addr_match(
        IPV4_DADDR_OFFSET,
        gateway,
        NFT_CMP_NEQ,
    )?);
    expressions.push(nft_drop_expr()?);
    Ok(expressions)
}

#[cfg(target_os = "linux")]
fn nft_bridge_peer_isolation_rule_exprs() -> Result<Vec<Vec<u8>>, IsolatedError> {
    let gateway = gateway_addr();
    let (bridge_net, bridge_prefix) = bridge_network();
    let mut expressions =
        nft_bridge_ipv4_network_match(IPV4_SADDR_OFFSET, bridge_net, bridge_prefix, NFT_CMP_EQ)?;
    expressions.extend(nft_bridge_ipv4_addr_match(
        IPV4_SADDR_OFFSET,
        gateway,
        NFT_CMP_NEQ,
    )?);
    expressions.extend(nft_bridge_ipv4_network_match(
        IPV4_DADDR_OFFSET,
        bridge_net,
        bridge_prefix,
        NFT_CMP_EQ,
    )?);
    expressions.extend(nft_bridge_ipv4_addr_match(
        IPV4_DADDR_OFFSET,
        gateway,
        NFT_CMP_NEQ,
    )?);
    expressions.push(nft_drop_expr()?);
    Ok(expressions)
}

#[cfg(target_os = "linux")]
fn nft_rfc1918_drop_rule_exprs(cidr: &str) -> Result<Vec<Vec<u8>>, IsolatedError> {
    let (private_net, private_prefix) = parse_ipv4_cidr(cidr)?;
    let (bridge_net, bridge_prefix) = bridge_network();
    let mut expressions =
        nft_ipv4_network_match(IPV4_DADDR_OFFSET, private_net, private_prefix, NFT_CMP_EQ)?;
    expressions.extend(nft_ipv4_network_match(
        IPV4_DADDR_OFFSET,
        bridge_net,
        bridge_prefix,
        NFT_CMP_NEQ,
    )?);
    expressions.push(nft_drop_expr()?);
    Ok(expressions)
}

#[cfg(target_os = "linux")]
fn nft_ipv4_network_match(
    offset: u32,
    network: Ipv4Addr,
    prefix_len: u8,
    op: u32,
) -> Result<Vec<Vec<u8>>, IsolatedError> {
    let mut expressions = nft_ipv4_guard_exprs()?;
    expressions.push(nft_payload_ipv4_expr(offset)?);
    expressions.push(nft_bitwise_mask_expr(ipv4_mask(prefix_len)?));
    expressions.push(nft_cmp_expr(op, &network.octets()));
    Ok(expressions)
}

#[cfg(target_os = "linux")]
fn nft_bridge_ipv4_network_match(
    offset: u32,
    network: Ipv4Addr,
    prefix_len: u8,
    op: u32,
) -> Result<Vec<Vec<u8>>, IsolatedError> {
    let mut expressions = nft_bridge_ipv4_guard_exprs()?;
    expressions.push(nft_payload_ipv4_expr(offset)?);
    expressions.push(nft_bitwise_mask_expr(ipv4_mask(prefix_len)?));
    expressions.push(nft_cmp_expr(op, &network.octets()));
    Ok(expressions)
}

#[cfg(target_os = "linux")]
fn nft_ipv4_addr_match(
    offset: u32,
    address: Ipv4Addr,
    op: u32,
) -> Result<Vec<Vec<u8>>, IsolatedError> {
    let mut expressions = nft_ipv4_guard_exprs()?;
    expressions.push(nft_payload_ipv4_expr(offset)?);
    expressions.push(nft_cmp_expr(op, &address.octets()));
    Ok(expressions)
}

#[cfg(target_os = "linux")]
fn nft_bridge_ipv4_addr_match(
    offset: u32,
    address: Ipv4Addr,
    op: u32,
) -> Result<Vec<Vec<u8>>, IsolatedError> {
    let mut expressions = nft_bridge_ipv4_guard_exprs()?;
    expressions.push(nft_payload_ipv4_expr(offset)?);
    expressions.push(nft_cmp_expr(op, &address.octets()));
    Ok(expressions)
}

#[cfg(target_os = "linux")]
fn nft_bridge_ipv4_guard_exprs() -> Result<Vec<Vec<u8>>, IsolatedError> {
    let eth_p_ip = libc_c_int_to_u16(libc::ETH_P_IP, "ETH_P_IP")?;
    Ok(vec![
        nft_payload_expr(
            libc_c_int_to_u32(libc::NFT_PAYLOAD_LL_HEADER, "NFT_PAYLOAD_LL_HEADER")?,
            ETHER_TYPE_OFFSET,
            ETHER_TYPE_LEN,
        )?,
        nft_cmp_expr(NFT_CMP_EQ, &eth_p_ip.to_be_bytes()),
    ])
}

#[cfg(target_os = "linux")]
fn nft_ipv4_guard_exprs() -> Result<Vec<Vec<u8>>, IsolatedError> {
    let mut data = Vec::new();
    append_be_u32_attr(&mut data, NFTA_META_DREG, NFT_REG_1);
    append_be_u32_attr(
        &mut data,
        NFTA_META_KEY,
        libc_c_int_to_u32(libc::NFT_META_NFPROTO, "NFT_META_NFPROTO")?,
    );
    Ok(vec![
        nft_expr("meta", &data),
        nft_cmp_expr(
            NFT_CMP_EQ,
            &[libc_c_int_to_u8(libc::NFPROTO_IPV4, "NFPROTO_IPV4")?],
        ),
    ])
}

#[cfg(target_os = "linux")]
fn nft_payload_ipv4_expr(offset: u32) -> Result<Vec<u8>, IsolatedError> {
    nft_payload_expr(
        libc_c_int_to_u32(
            libc::NFT_PAYLOAD_NETWORK_HEADER,
            "NFT_PAYLOAD_NETWORK_HEADER",
        )?,
        offset,
        IPV4_ADDR_LEN,
    )
}

#[cfg(target_os = "linux")]
fn nft_payload_expr(base: u32, offset: u32, len: u32) -> Result<Vec<u8>, IsolatedError> {
    let mut data = Vec::new();
    append_be_u32_attr(&mut data, NFTA_PAYLOAD_DREG, NFT_REG_1);
    append_be_u32_attr(&mut data, NFTA_PAYLOAD_BASE, base);
    append_be_u32_attr(&mut data, NFTA_PAYLOAD_OFFSET, offset);
    append_be_u32_attr(&mut data, NFTA_PAYLOAD_LEN, len);
    Ok(nft_expr("payload", &data))
}

#[cfg(target_os = "linux")]
fn nft_meta_expr(key: u32) -> Vec<u8> {
    let mut data = Vec::new();
    append_be_u32_attr(&mut data, NFTA_META_DREG, NFT_REG_1);
    append_be_u32_attr(&mut data, NFTA_META_KEY, key);
    nft_expr("meta", &data)
}

#[cfg(target_os = "linux")]
fn nft_bitwise_mask_expr(mask: [u8; 4]) -> Vec<u8> {
    let mut data = Vec::new();
    append_be_u32_attr(&mut data, NFTA_BITWISE_SREG, NFT_REG_1);
    append_be_u32_attr(&mut data, NFTA_BITWISE_DREG, NFT_REG_1);
    append_be_u32_attr(&mut data, NFTA_BITWISE_LEN, IPV4_ADDR_LEN);
    append_data_value_attr(&mut data, NFTA_BITWISE_MASK, &mask);
    append_data_value_attr(&mut data, NFTA_BITWISE_XOR, &[0, 0, 0, 0]);
    nft_expr("bitwise", &data)
}

#[cfg(target_os = "linux")]
fn nft_cmp_expr(op: u32, value: &[u8]) -> Vec<u8> {
    let mut data = Vec::new();
    append_be_u32_attr(&mut data, NFTA_CMP_SREG, NFT_REG_1);
    append_be_u32_attr(&mut data, NFTA_CMP_OP, op);
    append_data_value_attr(&mut data, NFTA_CMP_DATA, value);
    nft_expr("cmp", &data)
}

#[cfg(target_os = "linux")]
fn nft_drop_expr() -> Result<Vec<u8>, IsolatedError> {
    let mut verdict = Vec::new();
    append_be_u32_attr(
        &mut verdict,
        NFTA_VERDICT_CODE,
        libc_c_int_to_u32(libc::NF_DROP, "NF_DROP")?,
    );

    let mut data_value = Vec::new();
    append_nested_attr(&mut data_value, NFTA_DATA_VERDICT, &verdict);

    let mut data = Vec::new();
    append_be_u32_attr(&mut data, NFTA_IMMEDIATE_DREG, NFT_REG_VERDICT);
    append_nested_attr(&mut data, NFTA_IMMEDIATE_DATA, &data_value);
    Ok(nft_expr("immediate", &data))
}

#[cfg(target_os = "linux")]
fn nft_expr(name: &str, data: &[u8]) -> Vec<u8> {
    let mut expression = Vec::new();
    append_cstr_attr(&mut expression, NFTA_EXPR_NAME, name);
    if !data.is_empty() {
        append_nested_attr(&mut expression, NFTA_EXPR_DATA, data);
    }
    expression
}

#[cfg(target_os = "linux")]
fn send_nft_command(
    family: u8,
    step: impl Into<String>,
    message_type: u16,
    flags: u16,
    attrs: &[u8],
    ignore_exists: bool,
) -> Result<(), IsolatedError> {
    let step = step.into();
    let batch_start_seq = 1;
    let operation_seq = 2;
    let batch_end_seq = 3;
    let mut message = nft_batch_message(
        libc_c_int_to_u16(libc::NFNL_MSG_BATCH_BEGIN, "NFNL_MSG_BATCH_BEGIN")?,
        batch_start_seq,
    )?;
    message.extend(nft_message(
        message_type,
        flags,
        operation_seq,
        family,
        attrs,
    )?);
    message.extend(nft_batch_message(
        libc_c_int_to_u16(libc::NFNL_MSG_BATCH_END, "NFNL_MSG_BATCH_END")?,
        batch_end_seq,
    )?);
    let mut socket = NlSocket::new(libc_c_int_to_isize(
        libc::NETLINK_NETFILTER,
        "NETLINK_NETFILTER",
    )?)
    .map_err(|err| network_error_at(step.as_str(), err))?;
    socket
        .bind_auto()
        .map_err(|err| network_error_at(step.as_str(), err))?;
    socket
        .connect(&NlSocketAddr::new(0, 0))
        .map_err(|err| network_error_at(step.as_str(), err))?;
    socket
        .send(&message, 0)
        .map_err(|err| network_error_at(step.as_str(), err))?;
    recv_nft_ack(
        &socket,
        operation_seq,
        batch_start_seq,
        batch_end_seq,
        ignore_exists,
    )
    .map_err(|err| network_error_with_context(&step, err))
}

#[cfg(target_os = "linux")]
fn recv_nft_ack(
    socket: &NlSocket,
    operation_seq: u32,
    batch_start_seq: u32,
    batch_end_seq: u32,
    ignore_exists: bool,
) -> Result<(), IsolatedError> {
    let mut buffer = vec![0_u8; 8192];
    loop {
        let received = socket
            .recv(&mut &mut buffer[..], 0)
            .map_err(network_error)?;
        let mut offset = 0;
        while offset + NLMSG_HEADER_LEN <= received {
            let Some(message_len) = read_u32_ne(&buffer[offset..]) else {
                return Err(IsolatedError::NetworkUnavailable(
                    "short nftables netlink header".to_owned(),
                ));
            };
            let message_len = usize::try_from(message_len).map_err(|_| {
                IsolatedError::NetworkUnavailable(
                    "nftables netlink message length does not fit usize".to_owned(),
                )
            })?;
            if message_len < NLMSG_HEADER_LEN || offset + message_len > received {
                return Err(IsolatedError::NetworkUnavailable(
                    "invalid nftables netlink message length".to_owned(),
                ));
            }
            let Some(message_type) = read_u16_ne(&buffer[offset + 4..]) else {
                return Err(IsolatedError::NetworkUnavailable(
                    "short nftables netlink message type".to_owned(),
                ));
            };
            let Some(message_seq) = read_u32_ne(&buffer[offset + 8..]) else {
                return Err(IsolatedError::NetworkUnavailable(
                    "short nftables netlink sequence".to_owned(),
                ));
            };
            if message_type == NLMSG_ERROR {
                let errno =
                    parse_nft_ack_errno(&buffer[offset + NLMSG_HEADER_LEN..offset + message_len])?;
                if message_seq == operation_seq {
                    return handle_nft_ack_errno(errno, ignore_exists);
                }
                if (batch_start_seq..=batch_end_seq).contains(&message_seq) && errno != 0 {
                    return handle_nft_ack_errno(errno, ignore_exists);
                }
            }
            offset += align4(message_len);
        }
    }
}

#[cfg(target_os = "linux")]
fn parse_nft_ack_errno(payload: &[u8]) -> Result<i32, IsolatedError> {
    let Some(errno) = read_i32_ne(payload) else {
        return Err(IsolatedError::NetworkUnavailable(
            "short nftables netlink ack".to_owned(),
        ));
    };
    Ok(errno)
}

#[cfg(target_os = "linux")]
fn handle_nft_ack_errno(errno: i32, ignore_exists: bool) -> Result<(), IsolatedError> {
    if errno == 0 || (ignore_exists && errno == -libc::EEXIST) {
        return Ok(());
    }
    let code = -errno;
    let message = if code > 0 {
        std::io::Error::from_raw_os_error(code).to_string()
    } else {
        format!("unexpected errno {errno}")
    };
    Err(IsolatedError::NetworkUnavailable(format!(
        "nftables netlink error: {message}"
    )))
}

#[cfg(target_os = "linux")]
fn nft_message(
    message_type: u16,
    flags: u16,
    seq: u32,
    family: u8,
    attrs: &[u8],
) -> Result<Vec<u8>, IsolatedError> {
    nfnetlink_message(nft_msg_type(message_type)?, flags, seq, family, 0, attrs)
}

#[cfg(target_os = "linux")]
fn nft_batch_message(message_type: u16, seq: u32) -> Result<Vec<u8>, IsolatedError> {
    nfnetlink_message(
        message_type,
        NLM_F_REQUEST,
        seq,
        libc_c_int_to_u8(libc::AF_UNSPEC, "AF_UNSPEC")?,
        libc_c_int_to_u16(libc::NFNL_SUBSYS_NFTABLES, "NFNL_SUBSYS_NFTABLES")?,
        &[],
    )
}

#[cfg(target_os = "linux")]
fn nfnetlink_message(
    message_type: u16,
    flags: u16,
    seq: u32,
    family: u8,
    res_id: u16,
    attrs: &[u8],
) -> Result<Vec<u8>, IsolatedError> {
    let total_len = NLMSG_HEADER_LEN + NFGENMSG_LEN + attrs.len();
    let total_len_wire = u32::try_from(total_len).map_err(|_| {
        IsolatedError::NetworkUnavailable("nftables netlink message too large".to_owned())
    })?;
    let mut message = Vec::with_capacity(total_len);
    message.extend_from_slice(&total_len_wire.to_ne_bytes());
    message.extend_from_slice(&message_type.to_ne_bytes());
    message.extend_from_slice(&flags.to_ne_bytes());
    message.extend_from_slice(&seq.to_ne_bytes());
    message.extend_from_slice(&0_u32.to_ne_bytes());
    message.push(family);
    message.push(NFNETLINK_V0);
    message.extend_from_slice(&res_id.to_be_bytes());
    message.extend_from_slice(attrs);
    Ok(message)
}

#[cfg(target_os = "linux")]
fn append_cstr_attr(buffer: &mut Vec<u8>, kind: u16, value: &str) {
    let mut bytes = value.as_bytes().to_vec();
    bytes.push(0);
    append_attr(buffer, kind, &bytes);
}

#[cfg(target_os = "linux")]
fn append_be_u32_attr(buffer: &mut Vec<u8>, kind: u16, value: u32) {
    append_attr(buffer, kind, &value.to_be_bytes());
}

#[cfg(target_os = "linux")]
fn append_be_i32_attr(buffer: &mut Vec<u8>, kind: u16, value: i32) {
    append_attr(buffer, kind, &value.to_be_bytes());
}

#[cfg(target_os = "linux")]
fn append_data_value_attr(buffer: &mut Vec<u8>, kind: u16, value: &[u8]) {
    let mut nested = Vec::new();
    append_attr(&mut nested, NFTA_DATA_VALUE, value);
    append_nested_attr(buffer, kind, &nested);
}

#[cfg(target_os = "linux")]
fn append_nested_attr(buffer: &mut Vec<u8>, kind: u16, value: &[u8]) {
    append_attr(buffer, kind | NFA_F_NESTED, value);
}

#[cfg(target_os = "linux")]
fn append_attr(buffer: &mut Vec<u8>, kind: u16, value: &[u8]) {
    let length = NLA_HEADER_LEN + value.len();
    buffer.extend_from_slice(&usize_to_u16_saturating(length).to_ne_bytes());
    buffer.extend_from_slice(&kind.to_ne_bytes());
    buffer.extend_from_slice(value);
    buffer.resize(buffer.len() + align4(length) - length, 0);
}

#[cfg(target_os = "linux")]
fn parse_ipv4_cidr(cidr: &str) -> Result<(Ipv4Addr, u8), IsolatedError> {
    let Some((addr, prefix_len)) = cidr.split_once('/') else {
        return Err(IsolatedError::NetworkUnavailable(format!(
            "invalid IPv4 CIDR {cidr}"
        )));
    };
    let addr = parse_ipv4_addr(addr)?;
    let prefix_len = prefix_len.parse::<u8>().map_err(|err| {
        IsolatedError::NetworkUnavailable(format!("invalid IPv4 CIDR prefix {cidr}: {err}"))
    })?;
    if prefix_len > 32 {
        return Err(IsolatedError::NetworkUnavailable(format!(
            "invalid IPv4 CIDR prefix {cidr}"
        )));
    }
    Ok((addr, prefix_len))
}

#[cfg(target_os = "linux")]
fn parse_ipv4_addr(addr: &str) -> Result<Ipv4Addr, IsolatedError> {
    addr.parse::<Ipv4Addr>().map_err(|err| {
        IsolatedError::NetworkUnavailable(format!("invalid IPv4 address {addr}: {err}"))
    })
}

#[cfg(target_os = "linux")]
fn ipv4_mask(prefix_len: u8) -> Result<[u8; 4], IsolatedError> {
    if prefix_len > 32 {
        return Err(IsolatedError::NetworkUnavailable(format!(
            "invalid IPv4 prefix length {prefix_len}"
        )));
    }
    let mask = if prefix_len == 0 {
        0
    } else {
        u32::MAX << (32 - prefix_len)
    };
    Ok(mask.to_be_bytes())
}

#[cfg(target_os = "linux")]
const fn bridge_network() -> (Ipv4Addr, u8) {
    (Ipv4Addr::new(10, 244, 0, 0), BRIDGE_PREFIX_LEN)
}

#[cfg(target_os = "linux")]
fn nft_msg_type(message_type: u16) -> Result<u16, IsolatedError> {
    Ok(
        (libc_c_int_to_u16(libc::NFNL_SUBSYS_NFTABLES, "NFNL_SUBSYS_NFTABLES")? << 8)
            | message_type,
    )
}

#[cfg(target_os = "linux")]
const fn nft_create_flags() -> u16 {
    NLM_F_REQUEST | NLM_F_ACK | NLM_F_EXCL | NLM_F_CREATE
}

#[cfg(target_os = "linux")]
const fn nft_rule_flags() -> u16 {
    NLM_F_REQUEST | NLM_F_ACK | NLM_F_CREATE | NLM_F_APPEND
}

#[cfg(target_os = "linux")]
fn libc_c_int_to_u8(value: libc::c_int, name: &str) -> Result<u8, IsolatedError> {
    u8::try_from(value).map_err(|_| {
        IsolatedError::NetworkUnavailable(format!("invalid libc {name} value {value}"))
    })
}

#[cfg(target_os = "linux")]
fn libc_c_int_to_u16(value: libc::c_int, name: &str) -> Result<u16, IsolatedError> {
    u16::try_from(value).map_err(|_| {
        IsolatedError::NetworkUnavailable(format!("invalid libc {name} value {value}"))
    })
}

#[cfg(target_os = "linux")]
fn libc_c_int_to_u32(value: libc::c_int, name: &str) -> Result<u32, IsolatedError> {
    u32::try_from(value).map_err(|_| {
        IsolatedError::NetworkUnavailable(format!("invalid libc {name} value {value}"))
    })
}

#[cfg(target_os = "linux")]
fn libc_c_int_to_isize(value: libc::c_int, name: &str) -> Result<isize, IsolatedError> {
    isize::try_from(value).map_err(|_| {
        IsolatedError::NetworkUnavailable(format!("invalid libc {name} value {value}"))
    })
}

#[cfg(target_os = "linux")]
fn usize_to_u16_saturating(value: usize) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}

#[cfg(target_os = "linux")]
fn read_u16_ne(bytes: &[u8]) -> Option<u16> {
    let bytes = bytes.get(..2)?;
    Some(u16::from_ne_bytes([bytes[0], bytes[1]]))
}

#[cfg(target_os = "linux")]
fn read_u32_ne(bytes: &[u8]) -> Option<u32> {
    let bytes = bytes.get(..4)?;
    Some(u32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

#[cfg(target_os = "linux")]
fn read_i32_ne(bytes: &[u8]) -> Option<i32> {
    let bytes = bytes.get(..4)?;
    Some(i32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

#[cfg(target_os = "linux")]
const fn align4(length: usize) -> usize {
    (length + 3) & !3
}

#[cfg(target_os = "linux")]
const NFNETLINK_V0: u8 = 0;
#[cfg(target_os = "linux")]
const NLMSG_HEADER_LEN: usize = 16;
#[cfg(target_os = "linux")]
const NFGENMSG_LEN: usize = 4;
#[cfg(target_os = "linux")]
const NLA_HEADER_LEN: usize = 4;
#[cfg(target_os = "linux")]
const NLMSG_ERROR: u16 = 0x2;
#[cfg(target_os = "linux")]
const NLM_F_REQUEST: u16 = 0x01;
#[cfg(target_os = "linux")]
const NLM_F_ACK: u16 = 0x04;
#[cfg(target_os = "linux")]
const NLM_F_EXCL: u16 = 0x0200;
#[cfg(target_os = "linux")]
const NLM_F_CREATE: u16 = 0x0400;
#[cfg(target_os = "linux")]
const NLM_F_APPEND: u16 = 0x0800;
#[cfg(target_os = "linux")]
const NFA_F_NESTED: u16 = 0x8000;
#[cfg(target_os = "linux")]
const NFT_REG_VERDICT: u32 = 0;
#[cfg(target_os = "linux")]
const NFT_REG_1: u32 = 1;
#[cfg(target_os = "linux")]
const NFT_CMP_EQ: u32 = 0;
#[cfg(target_os = "linux")]
const NFT_CMP_NEQ: u32 = 1;
#[cfg(target_os = "linux")]
const IPV4_SADDR_OFFSET: u32 = 12;
#[cfg(target_os = "linux")]
const IPV4_DADDR_OFFSET: u32 = 16;
#[cfg(target_os = "linux")]
const IPV4_ADDR_LEN: u32 = 4;
#[cfg(target_os = "linux")]
const ETHER_TYPE_OFFSET: u32 = 12;
#[cfg(target_os = "linux")]
const ETHER_TYPE_LEN: u32 = 2;

#[cfg(target_os = "linux")]
const NFTA_TABLE_NAME: u16 = 1;
#[cfg(target_os = "linux")]
const NFTA_TABLE_FLAGS: u16 = 2;
#[cfg(target_os = "linux")]
const NFTA_CHAIN_TABLE: u16 = 1;
#[cfg(target_os = "linux")]
const NFTA_CHAIN_NAME: u16 = 3;
#[cfg(target_os = "linux")]
const NFTA_CHAIN_HOOK: u16 = 4;
#[cfg(target_os = "linux")]
const NFTA_CHAIN_TYPE: u16 = 7;
#[cfg(target_os = "linux")]
const NFTA_HOOK_HOOKNUM: u16 = 1;
#[cfg(target_os = "linux")]
const NFTA_HOOK_PRIORITY: u16 = 2;
#[cfg(target_os = "linux")]
const NFTA_RULE_TABLE: u16 = 1;
#[cfg(target_os = "linux")]
const NFTA_RULE_CHAIN: u16 = 2;
#[cfg(target_os = "linux")]
const NFTA_RULE_EXPRESSIONS: u16 = 4;
#[cfg(target_os = "linux")]
const NFTA_LIST_ELEM: u16 = 1;
#[cfg(target_os = "linux")]
const NFTA_EXPR_NAME: u16 = 1;
#[cfg(target_os = "linux")]
const NFTA_EXPR_DATA: u16 = 2;
#[cfg(target_os = "linux")]
const NFTA_PAYLOAD_DREG: u16 = 1;
#[cfg(target_os = "linux")]
const NFTA_PAYLOAD_BASE: u16 = 2;
#[cfg(target_os = "linux")]
const NFTA_PAYLOAD_OFFSET: u16 = 3;
#[cfg(target_os = "linux")]
const NFTA_PAYLOAD_LEN: u16 = 4;
#[cfg(target_os = "linux")]
const NFTA_META_DREG: u16 = 1;
#[cfg(target_os = "linux")]
const NFTA_META_KEY: u16 = 2;
#[cfg(target_os = "linux")]
const NFTA_CMP_SREG: u16 = 1;
#[cfg(target_os = "linux")]
const NFTA_CMP_OP: u16 = 2;
#[cfg(target_os = "linux")]
const NFTA_CMP_DATA: u16 = 3;
#[cfg(target_os = "linux")]
const NFTA_DATA_VALUE: u16 = 1;
#[cfg(target_os = "linux")]
const NFTA_DATA_VERDICT: u16 = 2;
#[cfg(target_os = "linux")]
const NFTA_IMMEDIATE_DREG: u16 = 1;
#[cfg(target_os = "linux")]
const NFTA_IMMEDIATE_DATA: u16 = 2;
#[cfg(target_os = "linux")]
const NFTA_VERDICT_CODE: u16 = 1;
#[cfg(target_os = "linux")]
const NFTA_BITWISE_SREG: u16 = 1;
#[cfg(target_os = "linux")]
const NFTA_BITWISE_DREG: u16 = 2;
#[cfg(target_os = "linux")]
const NFTA_BITWISE_LEN: u16 = 3;
#[cfg(target_os = "linux")]
const NFTA_BITWISE_MASK: u16 = 4;
#[cfg(target_os = "linux")]
const NFTA_BITWISE_XOR: u16 = 5;

#[cfg(target_os = "linux")]
async fn install_veth_pair(
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

#[cfg(target_os = "linux")]
async fn require_link_index(handle: &Handle, name: &str) -> Result<u32, IsolatedError> {
    link_index(handle, name)
        .await?
        .ok_or_else(|| IsolatedError::NetworkUnavailable(format!("link {name} not found")))
}

#[cfg(target_os = "linux")]
async fn link_index(handle: &Handle, name: &str) -> Result<Option<u32>, IsolatedError> {
    let mut links = handle.link().get().match_name(name.to_owned()).execute();
    match links.try_next().await {
        Ok(link) => Ok(link.map(|link| link.header.index)),
        Err(error) if is_error_text(&error, &["not found", "no such", "-19"]) => Ok(None),
        Err(error) => Err(network_error_at(format!("query link {name}"), error)),
    }
}

#[cfg(target_os = "linux")]
fn ignore_exists(
    step: impl Into<String>,
    result: Result<(), rtnetlink::Error>,
) -> Result<(), IsolatedError> {
    let step = step.into();
    match result {
        Ok(()) => Ok(()),
        Err(error) if is_error_text(&error, &["exists", "-17"]) => Ok(()),
        Err(error) => Err(network_error_at(step, error)),
    }
}

#[cfg(target_os = "linux")]
fn ignore_not_found(
    step: impl Into<String>,
    result: Result<(), rtnetlink::Error>,
) -> Result<(), IsolatedError> {
    let step = step.into();
    match result {
        Ok(()) => Ok(()),
        Err(error) if is_error_text(&error, &["not found", "no such", "-19"]) => Ok(()),
        Err(error) => Err(network_error_at(step, error)),
    }
}

#[cfg(target_os = "linux")]
fn ignore_unsupported(
    step: impl Into<String>,
    result: Result<(), rtnetlink::Error>,
) -> Result<(), IsolatedError> {
    let step = step.into();
    match result {
        Ok(()) => Ok(()),
        Err(error) if is_error_text(&error, &["operation not supported", "not supported"]) => {
            Ok(())
        }
        // Bridge-port hardening is best-effort. Some kernels report ENODEV
        // briefly after enslaving the veth before the bridge-port view exists.
        Err(error) if is_error_text(&error, &["no such device", "-19"]) => Ok(()),
        Err(error) => Err(network_error_at(step, error)),
    }
}

#[cfg(target_os = "linux")]
fn is_error_text(error: &rtnetlink::Error, needles: &[&str]) -> bool {
    let text = error.to_string().to_ascii_lowercase();
    needles.iter().any(|needle| text.contains(needle))
}

#[cfg(target_os = "linux")]
fn network_error(error: impl std::fmt::Display) -> IsolatedError {
    IsolatedError::NetworkUnavailable(error.to_string())
}

#[cfg(target_os = "linux")]
fn network_error_at(step: impl Into<String>, error: impl std::fmt::Display) -> IsolatedError {
    IsolatedError::NetworkUnavailable(format!("{}: {error}", step.into()))
}

#[cfg(target_os = "linux")]
fn network_error_with_context(step: &str, error: IsolatedError) -> IsolatedError {
    match error {
        IsolatedError::NetworkUnavailable(message) => {
            IsolatedError::NetworkUnavailable(format!("{step}: {message}"))
        }
        other => other,
    }
}

fn is_pool_ip(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    octets[0] == 10
        && octets[1] == 244
        && octets[2] == 0
        && (POOL_FIRST_HOST..=POOL_LAST_HOST).contains(&octets[3])
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn peer_isolation_rule_builds_drop_verdict() {
        let expressions = nft_peer_isolation_rule_exprs().expect("peer isolation rule");

        assert!(expressions.len() > 8);
        let verdict = expressions.last().expect("drop verdict expression");
        assert!(String::from_utf8_lossy(verdict).contains("immediate"));
    }
}
