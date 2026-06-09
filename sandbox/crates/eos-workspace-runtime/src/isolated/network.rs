//! Shared bridge + per-workspace veth + nftables wiring for isolated workspaces.
//!
//! Daemon-scope state: one bridge `eos-shared0` with gateway `10.244.0.1/24`, a
//! MASQUERADE rule on outbound from `10.244.0.0/24`, an IMDS drop rule, and an
//! opt-in RFC1918-deny rule. Per-workspace state: one veth pair and one `/32`
//! from `10.244.0.2 - 10.244.0.254`.
//!
//!
//! # IPv6 hardening — shell-free port
//!
//! The Rust holder shells out (`sysctl`, `ip -6 route flush`, `ip link set lo
//! up`) to purge IPv6 default routes and disable router-advertisement
//! acceptance so the v4-only MASQUERADE rule stays the sole egress. The Rust
//! port replaces those binaries with `rtnetlink` (`RTM_DELROUTE` for the IPv6
//! default route, `RTM_NEWLINK` to bring `lo` up) and direct
//! `/proc/sys/net/ipv6/conf/<iface>/accept_ra` writes — NO `ip`/`sysctl`
//! binaries. This work executes inside the namespace via `eos-ns-holder`
//! (see `host_runtime`), not in this daemon-scope module.

use std::net::Ipv4Addr;

use crate::isolated::caps::{Rfc1918Egress, HANDLE_PREFIX};
use crate::isolated::error::IsolatedError;

#[cfg(target_os = "linux")]
mod netfilter;
#[cfg(target_os = "linux")]
mod rtnl;

#[cfg(target_os = "linux")]
use netfilter::install_static_rules;
#[cfg(target_os = "linux")]
use rtnl::{ensure_bridge, ignore_not_found, install_veth_pair, link_index, run_netlink};

/// Shared bridge interface name.
pub const BRIDGE_NAME: &str = "eos-shared0";
/// Shared bridge CIDR.
pub(crate) const BRIDGE_CIDR: &str = "10.244.0.0/24";
/// Bridge gateway address.
pub const GATEWAY: &str = "10.244.0.1";
/// nftables NAT table name.
pub const NFT_NAT_TABLE: &str = "eos_iws_nat";
/// nftables filter table name.
pub const NFT_FILTER_TABLE: &str = "eos_iws_filter";
/// Cloud IMDS address dropped on the forward chain.
pub const IMDS_ADDR: &str = "169.254.169.254";
/// RFC1918 private networks (for the opt-in deny rule).
pub const RFC1918_NETS: [&str; 3] = ["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16"];
/// Per-workspace veth name prefix — the SAME literal as [`HANDLE_PREFIX`].
pub(crate) const VETH_PREFIX: &str = HANDLE_PREFIX;

/// Bridge CIDR prefix length (matches `BRIDGE_CIDR`).
pub const BRIDGE_PREFIX_LEN: u8 = 24;
/// First allocatable host octet (skips `.0` network + `.1` gateway).
pub(crate) const POOL_FIRST_HOST: u8 = 2;
/// Last allocatable host octet (skips `.255` broadcast).
pub(crate) const POOL_LAST_HOST: u8 = 254;

/// One veth `/32` allocation for a workspace.
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
#[must_use]
pub(crate) fn veth_names(workspace_handle_id: &str) -> (String, String) {
    // Rust `_veth_names` takes the FIRST 6 chars (`workspace_handle_id[:6]`);
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
#[derive(Debug, Clone, Default)]
pub(crate) struct BridgeAddressPool {
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
    pub fn free(&mut self, ip: Ipv4Addr) {
        self.allocated.retain(|allocated| *allocated != ip);
    }
}

/// Owns the `eos-shared0` bridge + static nft rules + per-workspace veth wiring.
///
/// The Rust implementation shells out to `ip`/`nft`; the Rust port replaces
/// the bridge/veth path with `rtnetlink` link/address operations and the static
/// NAT/filter path with `NETLINK_NETFILTER` messages — NO `ip`/`nft` binaries.
#[derive(Debug)]
pub(crate) struct IsolatedNetwork {
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

    /// Install the bridge + MASQUERADE + IMDS drop (+ optional RFC1918 deny).
    /// Idempotent for table/chain creation; rule insertion mirrors Rust's
    /// sequential `nft add rule` calls.
    ///
    /// # Errors
    ///
    /// Returns [`IsolatedError::NetworkUnavailable`] when route or netfilter
    /// netlink setup fails.
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
}

/// Wrap a netlink/netfilter failure as [`IsolatedError::NetworkUnavailable`],
/// prefixing the failing step. Shared by the rtnetlink and nft wire paths.
#[cfg(target_os = "linux")]
pub(crate) fn network_error_at(
    step: impl Into<String>,
    error: impl std::fmt::Display,
) -> IsolatedError {
    IsolatedError::NetworkUnavailable(format!("{}: {error}", step.into()))
}

fn is_pool_ip(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    octets[0] == 10
        && octets[1] == 244
        && octets[2] == 0
        && (POOL_FIRST_HOST..=POOL_LAST_HOST).contains(&octets[3])
}
