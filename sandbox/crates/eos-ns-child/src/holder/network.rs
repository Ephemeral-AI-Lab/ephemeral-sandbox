#[cfg(target_os = "linux")]
use std::ffi::CString;
use std::fs;
use std::net::Ipv4Addr;
use std::path::Path;

use super::{FALLBACK_IPV6_CONF_INTERFACES, IPV6_CONF_ROOT};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NetworkConfig {
    pub(crate) iface: String,
    pub(crate) ns_ip: Ipv4Addr,
    pub(crate) prefix_len: u8,
    pub(crate) gateway: Ipv4Addr,
}

pub(crate) fn parse_network_config(buf: &[u8]) -> Option<NetworkConfig> {
    let line = std::str::from_utf8(buf).ok()?.trim();
    let mut parts = line.split_whitespace();
    if parts.next()? != "net-ready" {
        return None;
    }
    let iface = parts.next()?.to_owned();
    let ns_ip = parts.next()?.parse().ok()?;
    let prefix_len = parts.next()?.parse().ok()?;
    let gateway = parts.next()?.parse().ok()?;
    Some(NetworkConfig {
        iface,
        ns_ip,
        prefix_len,
        gateway,
    })
}

/// Disable IPv6 router-advertisement acceptance on every interface, shell-free.
///
/// Replaces `sysctl -w net.ipv6.conf.{iface}.accept_ra=0` with a write of `"0"`
/// to `/proc/sys/net/ipv6/conf/{iface}/accept_ra`, iterating
/// [`IPV6_CONF_ROOT`] (falling back to [`FALLBACK_IPV6_CONF_INTERFACES`]).
/// Best-effort per iface.
pub(crate) fn disable_ipv6_ra() {
    let mut interfaces = Vec::new();
    if let Ok(entries) = fs::read_dir(IPV6_CONF_ROOT) {
        interfaces.extend(
            entries
                .flatten()
                .filter_map(|entry| entry.file_name().into_string().ok()),
        );
    }
    if interfaces.is_empty() {
        interfaces.extend(
            FALLBACK_IPV6_CONF_INTERFACES
                .iter()
                .copied()
                .map(str::to_owned),
        );
    }
    for iface in interfaces {
        let _ = fs::write(
            Path::new(IPV6_CONF_ROOT).join(iface).join("accept_ra"),
            b"0",
        );
    }
}

/// Bring loopback up through rtnetlink, shell-free.
///
/// Replaces `ip link set lo up` with `RTM_NEWLINK` so holder readiness does not
/// depend on `ip(8)` being present inside the image. Best-effort.
#[cfg(target_os = "linux")]
pub(crate) fn bring_loopback_up() {
    let Ok(lo) = CString::new("lo") else {
        return;
    };
    // SAFETY: `lo` is a valid NUL-terminated C string and `if_nametoindex`
    // does not retain the pointer after returning.
    let index = unsafe { libc::if_nametoindex(lo.as_ptr()) };
    if index == 0 {
        return;
    }
    let Some(ifi_family) = libc_c_int_to_u8(libc::AF_UNSPEC) else {
        return;
    };
    let Ok(ifi_index) = i32::try_from(index) else {
        return;
    };
    let Some(iff_up) = libc_c_int_to_u32(libc::IFF_UP) else {
        return;
    };
    let msg = IfInfoMsg {
        ifi_family,
        ifi_pad: 0,
        ifi_type: 0,
        ifi_index,
        ifi_flags: iff_up,
        ifi_change: iff_up,
    };
    let Some(flags) = netlink_request_flags() else {
        return;
    };
    let _ = send_netlink_message(libc::RTM_NEWLINK, flags, &msg);
}

#[cfg(not(target_os = "linux"))]
pub(crate) const fn bring_loopback_up() {}

/// Configure the namespace-side veth after the daemon moved it into this netns.
///
/// The daemon owns veth creation and host-side bridge attachment. The holder is
/// already in the target netns, so it configures the peer's link state, address,
/// and default route without `nsenter(1)` or `ip(8)`. Best-effort.
#[cfg(target_os = "linux")]
pub(crate) fn configure_namespace_veth(config: &NetworkConfig) {
    let index = link_index(&config.iface);
    if index == 0 {
        return;
    }
    set_link_up(index);
    add_ipv4_address(index, config.ns_ip, config.prefix_len);
    add_ipv4_default_route(index, config.gateway);
}

#[cfg(not(target_os = "linux"))]
pub(crate) const fn configure_namespace_veth(_config: &NetworkConfig) {}

#[cfg(target_os = "linux")]
fn link_index(name: &str) -> libc::c_uint {
    let Ok(name) = CString::new(name) else {
        return 0;
    };
    // SAFETY: `name` is a valid NUL-terminated C string and `if_nametoindex`
    // does not retain the pointer after returning.
    unsafe { libc::if_nametoindex(name.as_ptr()) }
}

#[cfg(target_os = "linux")]
fn set_link_up(index: libc::c_uint) {
    let Some(ifi_family) = libc_c_int_to_u8(libc::AF_UNSPEC) else {
        return;
    };
    let Ok(ifi_index) = i32::try_from(index) else {
        return;
    };
    let Some(iff_up) = libc_c_int_to_u32(libc::IFF_UP) else {
        return;
    };
    let msg = IfInfoMsg {
        ifi_family,
        ifi_pad: 0,
        ifi_type: 0,
        ifi_index,
        ifi_flags: iff_up,
        ifi_change: iff_up,
    };
    let Some(flags) = netlink_request_flags() else {
        return;
    };
    let _ = send_netlink_message(libc::RTM_NEWLINK, flags, &msg);
}

#[cfg(target_os = "linux")]
fn add_ipv4_address(index: libc::c_uint, ip: Ipv4Addr, prefix_len: u8) {
    let Some(ifa_family) = libc_c_int_to_u8(libc::AF_INET) else {
        return;
    };
    let msg = IfAddrMsg {
        ifa_family,
        ifa_prefixlen: prefix_len,
        ifa_flags: 0,
        ifa_scope: 0,
        ifa_index: index,
    };
    let attrs = [
        NetlinkAttr::new(IFA_ADDRESS, ip.octets().to_vec()),
        NetlinkAttr::new(IFA_LOCAL, ip.octets().to_vec()),
    ];
    let Some(flags) = netlink_create_flags() else {
        return;
    };
    let _ = send_netlink_message_with_attrs(libc::RTM_NEWADDR, flags, &msg, &attrs);
}

#[cfg(target_os = "linux")]
fn add_ipv4_default_route(index: libc::c_uint, gateway: Ipv4Addr) {
    let Some(rtm_family) = libc_c_int_to_u8(libc::AF_INET) else {
        return;
    };
    let route = RouteMsg {
        rtm_family,
        rtm_dst_len: 0,
        rtm_src_len: 0,
        rtm_tos: 0,
        rtm_table: libc::RT_TABLE_MAIN,
        rtm_protocol: libc::RTPROT_STATIC,
        rtm_scope: libc::RT_SCOPE_UNIVERSE,
        rtm_type: libc::RTN_UNICAST,
        rtm_flags: 0,
    };
    let attrs = [
        NetlinkAttr::new(RTA_GATEWAY, gateway.octets().to_vec()),
        NetlinkAttr::new(RTA_OIF, index.to_ne_bytes().to_vec()),
    ];
    let Some(flags) = netlink_create_flags() else {
        return;
    };
    let _ = send_netlink_message_with_attrs(libc::RTM_NEWROUTE, flags, &route, &attrs);
}

/// Flush the IPv6 default route via rtnetlink, shell-free.
///
/// Replaces `ip -6 route flush default` with a netlink `RTM_DELROUTE` (or
/// dump+delete) so no bridge-side RA can repopulate a v6 default route and
/// bypass the v4-only MASQUERADE filter. Best-effort.
#[cfg(target_os = "linux")]
pub(crate) fn flush_ipv6_default_route() {
    let Some(rtm_family) = libc_c_int_to_u8(libc::AF_INET6) else {
        return;
    };
    let route = RouteMsg {
        rtm_family,
        rtm_dst_len: 0,
        rtm_src_len: 0,
        rtm_tos: 0,
        rtm_table: libc::RT_TABLE_MAIN,
        rtm_protocol: libc::RTPROT_UNSPEC,
        rtm_scope: libc::RT_SCOPE_UNIVERSE,
        rtm_type: libc::RTN_UNICAST,
        rtm_flags: 0,
    };
    let Some(flags) = netlink_request_flags() else {
        return;
    };
    let _ = send_netlink_message(libc::RTM_DELROUTE, flags, &route);
}

#[cfg(not(target_os = "linux"))]
pub(crate) const fn flush_ipv6_default_route() {}

#[cfg(target_os = "linux")]
fn netlink_request_flags() -> Option<u16> {
    libc_c_int_to_u16(libc::NLM_F_REQUEST)
}

#[cfg(target_os = "linux")]
fn netlink_create_flags() -> Option<u16> {
    libc_c_int_to_u16(libc::NLM_F_REQUEST | libc::NLM_F_CREATE | libc::NLM_F_EXCL)
}

#[cfg(target_os = "linux")]
fn send_netlink_message<T>(
    message_type: u16,
    flags: u16,
    payload: &T,
) -> Result<(), std::io::Error> {
    send_netlink_message_with_attrs(message_type, flags, payload, &[])
}

#[cfg(target_os = "linux")]
fn send_netlink_message_with_attrs<T>(
    message_type: u16,
    flags: u16,
    payload: &T,
    attrs: &[NetlinkAttr],
) -> Result<(), std::io::Error> {
    let length = std::mem::size_of::<libc::nlmsghdr>() + std::mem::size_of::<T>();
    let attrs_len: usize = attrs
        .iter()
        .map(|attr| align4(RTATTR_HEADER_LEN + attr.value.len()))
        .sum();
    let nlmsg_len = u32::try_from(length + attrs_len).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "netlink message too large",
        )
    })?;
    let mut message = Vec::with_capacity(length + attrs_len);
    let header = libc::nlmsghdr {
        nlmsg_len,
        nlmsg_type: message_type,
        nlmsg_flags: flags,
        nlmsg_seq: 1,
        nlmsg_pid: 0,
    };
    append_struct_bytes(&mut message, &header);
    append_struct_bytes(&mut message, payload);
    for attr in attrs {
        append_attr(&mut message, attr);
    }
    let nl_family = libc_c_int_to_sock_family(libc::AF_NETLINK).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid netlink socket family",
        )
    })?;
    let addr = NetlinkSocketAddress {
        nl_family,
        nl_pad: 0,
        nl_pid: 0,
        nl_groups: 0,
    };
    // SAFETY: `socket` is called with constant arguments and returns an owned fd
    // on success, closed below before returning.
    let fd = unsafe {
        libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_RAW | libc::SOCK_CLOEXEC,
            libc::NETLINK_ROUTE,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `message` and `addr` are valid for the duration of this call; the
    // kernel copies the bytes before returning. The fd is a netlink socket just
    // opened by this function.
    let rc = unsafe {
        libc::sendto(
            fd,
            message.as_ptr().cast(),
            message.len(),
            0,
            std::ptr::from_ref(&addr).cast(),
            libc_socklen(std::mem::size_of::<NetlinkSocketAddress>()).ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "netlink socket address too large",
                )
            })?,
        )
    };
    let result = if rc < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    };
    // SAFETY: `fd` is owned by this function after a successful `socket` call.
    let _ = unsafe { libc::close(fd) };
    result
}

#[cfg(target_os = "linux")]
fn append_struct_bytes<T>(buffer: &mut Vec<u8>, value: &T) {
    // SAFETY: every caller passes a fully-initialized, padding-free `#[repr(C)]`
    // netlink struct, so all `size_of::<T>()` bytes are initialized and reading
    // them as `u8` is sound. The bytes are copied into `buffer` before `value`
    // is dropped. Callers MUST NOT pass a type with compiler-inserted padding.
    let bytes = unsafe {
        std::slice::from_raw_parts(
            std::ptr::from_ref(value).cast::<u8>(),
            std::mem::size_of::<T>(),
        )
    };
    buffer.extend_from_slice(bytes);
}

#[cfg(target_os = "linux")]
fn append_attr(buffer: &mut Vec<u8>, attr: &NetlinkAttr) {
    let length = RTATTR_HEADER_LEN + attr.value.len();
    buffer.extend_from_slice(&usize_to_u16_saturating(length).to_ne_bytes());
    buffer.extend_from_slice(&attr.kind.to_ne_bytes());
    buffer.extend_from_slice(&attr.value);
    let padded = align4(length);
    buffer.resize(buffer.len() + padded - length, 0);
}

#[cfg(target_os = "linux")]
fn libc_c_int_to_u8(value: libc::c_int) -> Option<u8> {
    u8::try_from(value).ok()
}

#[cfg(target_os = "linux")]
fn libc_c_int_to_u16(value: libc::c_int) -> Option<u16> {
    u16::try_from(value).ok()
}

#[cfg(target_os = "linux")]
fn libc_c_int_to_u32(value: libc::c_int) -> Option<u32> {
    u32::try_from(value).ok()
}

#[cfg(target_os = "linux")]
fn libc_c_int_to_sock_family(value: libc::c_int) -> Option<libc::sa_family_t> {
    libc::sa_family_t::try_from(value).ok()
}

#[cfg(target_os = "linux")]
fn libc_socklen(value: usize) -> Option<libc::socklen_t> {
    libc::socklen_t::try_from(value).ok()
}

#[cfg(target_os = "linux")]
fn usize_to_u16_saturating(value: usize) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}

#[cfg(target_os = "linux")]
const fn align4(length: usize) -> usize {
    (length + 3) & !3
}

#[cfg(target_os = "linux")]
const RTATTR_HEADER_LEN: usize = 4;
#[cfg(target_os = "linux")]
const IFA_ADDRESS: u16 = 1;
#[cfg(target_os = "linux")]
const IFA_LOCAL: u16 = 2;
#[cfg(target_os = "linux")]
const RTA_OIF: u16 = 4;
#[cfg(target_os = "linux")]
const RTA_GATEWAY: u16 = 5;

#[cfg(target_os = "linux")]
struct NetlinkAttr {
    kind: u16,
    value: Vec<u8>,
}

#[cfg(target_os = "linux")]
impl NetlinkAttr {
    const fn new(kind: u16, value: Vec<u8>) -> Self {
        Self { kind, value }
    }
}

#[cfg(target_os = "linux")]
#[expect(
    clippy::struct_field_names,
    reason = "repr(C) layout mirrors the Linux ifinfomsg field names"
)]
#[repr(C)]
struct IfInfoMsg {
    ifi_family: u8,
    ifi_pad: u8,
    ifi_type: u16,
    ifi_index: i32,
    ifi_flags: u32,
    ifi_change: u32,
}

#[cfg(target_os = "linux")]
#[expect(
    clippy::struct_field_names,
    reason = "repr(C) layout mirrors the Linux ifaddrmsg field names"
)]
#[repr(C)]
struct IfAddrMsg {
    ifa_family: u8,
    ifa_prefixlen: u8,
    ifa_flags: u8,
    ifa_scope: u8,
    ifa_index: u32,
}

#[cfg(target_os = "linux")]
#[expect(
    clippy::struct_field_names,
    reason = "repr(C) layout mirrors the Linux rtmsg field names"
)]
#[repr(C)]
struct RouteMsg {
    rtm_family: u8,
    rtm_dst_len: u8,
    rtm_src_len: u8,
    rtm_tos: u8,
    rtm_table: u8,
    rtm_protocol: u8,
    rtm_scope: u8,
    rtm_type: u8,
    rtm_flags: u32,
}

#[cfg(target_os = "linux")]
#[expect(
    clippy::struct_field_names,
    reason = "repr(C) layout mirrors the Linux sockaddr_nl field names"
)]
#[repr(C)]
struct NetlinkSocketAddress {
    nl_family: libc::sa_family_t,
    nl_pad: u16,
    nl_pid: u32,
    nl_groups: u32,
}

#[cfg(test)]
mod tests {
    use super::parse_network_config;

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

    #[test]
    fn parse_net_ready_with_optional_veth_config() -> TestResult {
        let config = parse_network_config(b"net-ready eos-iws-abcden 10.244.0.2 24 10.244.0.1\n")
            .ok_or_else(|| std::io::Error::other("network config should parse"))?;

        assert_eq!(config.iface, "eos-iws-abcden");
        assert_eq!(config.ns_ip.to_string(), "10.244.0.2");
        assert_eq!(config.prefix_len, 24);
        assert_eq!(config.gateway.to_string(), "10.244.0.1");
        Ok(())
    }
}
