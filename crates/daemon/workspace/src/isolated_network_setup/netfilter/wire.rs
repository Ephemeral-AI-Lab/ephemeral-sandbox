use netlink_sys::{Socket as NlSocket, SocketAddr as NlSocketAddr};

use std::net::Ipv4Addr;

use crate::isolated_network_setup::network_error_at;
use crate::isolated_workspace::error::IsolatedError;

use super::exprs::NFTA_DATA_VALUE;

pub(super) fn send_nft_command(
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

fn parse_nft_ack_errno(payload: &[u8]) -> Result<i32, IsolatedError> {
    let Some(errno) = read_i32_ne(payload) else {
        return Err(IsolatedError::NetworkUnavailable(
            "short nftables netlink ack".to_owned(),
        ));
    };
    Ok(errno)
}

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

fn nft_message(
    message_type: u16,
    flags: u16,
    seq: u32,
    family: u8,
    attrs: &[u8],
) -> Result<Vec<u8>, IsolatedError> {
    nfnetlink_message(nft_msg_type(message_type)?, flags, seq, family, 0, attrs)
}

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

pub(super) fn append_cstr_attr(buffer: &mut Vec<u8>, kind: u16, value: &str) {
    let mut bytes = value.as_bytes().to_vec();
    bytes.push(0);
    append_attr(buffer, kind, &bytes);
}

pub(super) fn append_be_u32_attr(buffer: &mut Vec<u8>, kind: u16, value: u32) {
    append_attr(buffer, kind, &value.to_be_bytes());
}

pub(super) fn append_be_i32_attr(buffer: &mut Vec<u8>, kind: u16, value: i32) {
    append_attr(buffer, kind, &value.to_be_bytes());
}

pub(super) fn append_data_value_attr(buffer: &mut Vec<u8>, kind: u16, value: &[u8]) {
    let mut nested = Vec::new();
    append_attr(&mut nested, NFTA_DATA_VALUE, value);
    append_nested_attr(buffer, kind, &nested);
}

pub(super) fn append_nested_attr(buffer: &mut Vec<u8>, kind: u16, value: &[u8]) {
    append_attr(buffer, kind | NFA_F_NESTED, value);
}

fn append_attr(buffer: &mut Vec<u8>, kind: u16, value: &[u8]) {
    let length = NLA_HEADER_LEN + value.len();
    buffer.extend_from_slice(&usize_to_u16_saturating(length).to_ne_bytes());
    buffer.extend_from_slice(&kind.to_ne_bytes());
    buffer.extend_from_slice(value);
    buffer.resize(buffer.len() + align4(length) - length, 0);
}

pub(super) fn parse_ipv4_cidr(cidr: &str) -> Result<(Ipv4Addr, u8), IsolatedError> {
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

pub(super) fn parse_ipv4_addr(addr: &str) -> Result<Ipv4Addr, IsolatedError> {
    addr.parse::<Ipv4Addr>().map_err(|err| {
        IsolatedError::NetworkUnavailable(format!("invalid IPv4 address {addr}: {err}"))
    })
}

pub(super) fn ipv4_mask(prefix_len: u8) -> Result<[u8; 4], IsolatedError> {
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

fn nft_msg_type(message_type: u16) -> Result<u16, IsolatedError> {
    Ok(
        (libc_c_int_to_u16(libc::NFNL_SUBSYS_NFTABLES, "NFNL_SUBSYS_NFTABLES")? << 8)
            | message_type,
    )
}

pub(super) const fn nft_create_flags() -> u16 {
    NLM_F_REQUEST | NLM_F_ACK | NLM_F_EXCL | NLM_F_CREATE
}

pub(super) const fn nft_rule_flags() -> u16 {
    NLM_F_REQUEST | NLM_F_ACK | NLM_F_CREATE | NLM_F_APPEND
}

pub(super) fn libc_c_int_to_u8(value: libc::c_int, name: &str) -> Result<u8, IsolatedError> {
    u8::try_from(value).map_err(|_| {
        IsolatedError::NetworkUnavailable(format!("invalid libc {name} value {value}"))
    })
}

pub(super) fn libc_c_int_to_u16(value: libc::c_int, name: &str) -> Result<u16, IsolatedError> {
    u16::try_from(value).map_err(|_| {
        IsolatedError::NetworkUnavailable(format!("invalid libc {name} value {value}"))
    })
}

pub(super) fn libc_c_int_to_u32(value: libc::c_int, name: &str) -> Result<u32, IsolatedError> {
    u32::try_from(value).map_err(|_| {
        IsolatedError::NetworkUnavailable(format!("invalid libc {name} value {value}"))
    })
}

fn libc_c_int_to_isize(value: libc::c_int, name: &str) -> Result<isize, IsolatedError> {
    isize::try_from(value).map_err(|_| {
        IsolatedError::NetworkUnavailable(format!("invalid libc {name} value {value}"))
    })
}

fn usize_to_u16_saturating(value: usize) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}

fn read_u16_ne(bytes: &[u8]) -> Option<u16> {
    let bytes = bytes.get(..2)?;
    Some(u16::from_ne_bytes([bytes[0], bytes[1]]))
}

fn read_u32_ne(bytes: &[u8]) -> Option<u32> {
    let bytes = bytes.get(..4)?;
    Some(u32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn read_i32_ne(bytes: &[u8]) -> Option<i32> {
    let bytes = bytes.get(..4)?;
    Some(i32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

const fn align4(length: usize) -> usize {
    (length + 3) & !3
}

fn network_error(error: impl std::fmt::Display) -> IsolatedError {
    IsolatedError::NetworkUnavailable(error.to_string())
}

fn network_error_with_context(step: &str, error: IsolatedError) -> IsolatedError {
    match error {
        IsolatedError::NetworkUnavailable(message) => {
            IsolatedError::NetworkUnavailable(format!("{step}: {message}"))
        }
        other => other,
    }
}

const NFNETLINK_V0: u8 = 0;
const NLMSG_HEADER_LEN: usize = 16;
const NFGENMSG_LEN: usize = 4;
const NLA_HEADER_LEN: usize = 4;
const NLMSG_ERROR: u16 = 0x2;
const NLM_F_REQUEST: u16 = 0x01;
const NLM_F_ACK: u16 = 0x04;
const NLM_F_EXCL: u16 = 0x0200;
const NLM_F_CREATE: u16 = 0x0400;
const NLM_F_APPEND: u16 = 0x0800;
const NFA_F_NESTED: u16 = 0x8000;
