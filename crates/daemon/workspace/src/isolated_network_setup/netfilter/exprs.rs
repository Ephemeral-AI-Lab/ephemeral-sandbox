use std::net::Ipv4Addr;

use crate::isolated_network_setup::{BRIDGE_PREFIX_LEN, IMDS_ADDR};
use crate::isolated_workspace::error::IsolatedError;

use super::wire::{
    append_be_u32_attr, append_cstr_attr, append_data_value_attr, append_nested_attr, ipv4_mask,
    libc_c_int_to_u16, libc_c_int_to_u32, libc_c_int_to_u8, parse_ipv4_addr, parse_ipv4_cidr,
};

pub(super) fn nft_masquerade_rule_exprs(bridge_index: u32) -> Result<Vec<Vec<u8>>, IsolatedError> {
    let (bridge_net, bridge_prefix) = bridge_network();
    let mut expressions = nft_ipv4_network_match(
        IpHeader::Inet,
        IPV4_SADDR_OFFSET,
        bridge_net,
        bridge_prefix,
        NFT_CMP_EQ,
    )?;
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

pub(super) fn nft_imds_drop_rule_exprs() -> Result<Vec<Vec<u8>>, IsolatedError> {
    let imds = parse_ipv4_addr(IMDS_ADDR)?;
    let mut expressions = nft_ipv4_guard_exprs(IpHeader::Inet)?;
    expressions.push(nft_payload_ipv4_expr(IPV4_DADDR_OFFSET)?);
    expressions.push(nft_cmp_expr(NFT_CMP_EQ, &imds.octets()));
    expressions.push(nft_drop_expr()?);
    Ok(expressions)
}

pub(super) fn nft_peer_isolation_rule_exprs() -> Result<Vec<Vec<u8>>, IsolatedError> {
    let gateway = gateway_addr();
    let (bridge_net, bridge_prefix) = bridge_network();
    let mut expressions = nft_ipv4_network_match(
        IpHeader::Inet,
        IPV4_SADDR_OFFSET,
        bridge_net,
        bridge_prefix,
        NFT_CMP_EQ,
    )?;
    expressions.extend(nft_ipv4_addr_match(
        IpHeader::Inet,
        IPV4_SADDR_OFFSET,
        gateway,
        NFT_CMP_NEQ,
    )?);
    expressions.extend(nft_ipv4_network_match(
        IpHeader::Inet,
        IPV4_DADDR_OFFSET,
        bridge_net,
        bridge_prefix,
        NFT_CMP_EQ,
    )?);
    expressions.extend(nft_ipv4_addr_match(
        IpHeader::Inet,
        IPV4_DADDR_OFFSET,
        gateway,
        NFT_CMP_NEQ,
    )?);
    expressions.push(nft_drop_expr()?);
    Ok(expressions)
}

pub(super) fn nft_bridge_peer_isolation_rule_exprs() -> Result<Vec<Vec<u8>>, IsolatedError> {
    let gateway = gateway_addr();
    let (bridge_net, bridge_prefix) = bridge_network();
    let mut expressions = nft_ipv4_network_match(
        IpHeader::Bridge,
        IPV4_SADDR_OFFSET,
        bridge_net,
        bridge_prefix,
        NFT_CMP_EQ,
    )?;
    expressions.extend(nft_ipv4_addr_match(
        IpHeader::Bridge,
        IPV4_SADDR_OFFSET,
        gateway,
        NFT_CMP_NEQ,
    )?);
    expressions.extend(nft_ipv4_network_match(
        IpHeader::Bridge,
        IPV4_DADDR_OFFSET,
        bridge_net,
        bridge_prefix,
        NFT_CMP_EQ,
    )?);
    expressions.extend(nft_ipv4_addr_match(
        IpHeader::Bridge,
        IPV4_DADDR_OFFSET,
        gateway,
        NFT_CMP_NEQ,
    )?);
    expressions.push(nft_drop_expr()?);
    Ok(expressions)
}

pub(super) fn nft_rfc1918_drop_rule_exprs(cidr: &str) -> Result<Vec<Vec<u8>>, IsolatedError> {
    let (private_net, private_prefix) = parse_ipv4_cidr(cidr)?;
    let (bridge_net, bridge_prefix) = bridge_network();
    let mut expressions = nft_ipv4_network_match(
        IpHeader::Inet,
        IPV4_DADDR_OFFSET,
        private_net,
        private_prefix,
        NFT_CMP_EQ,
    )?;
    expressions.extend(nft_ipv4_network_match(
        IpHeader::Inet,
        IPV4_DADDR_OFFSET,
        bridge_net,
        bridge_prefix,
        NFT_CMP_NEQ,
    )?);
    expressions.push(nft_drop_expr()?);
    Ok(expressions)
}

fn nft_ipv4_network_match(
    header: IpHeader,
    offset: u32,
    network: Ipv4Addr,
    prefix_len: u8,
    op: u32,
) -> Result<Vec<Vec<u8>>, IsolatedError> {
    let mut expressions = nft_ipv4_guard_exprs(header)?;
    expressions.push(nft_payload_ipv4_expr(offset)?);
    expressions.push(nft_bitwise_mask_expr(ipv4_mask(prefix_len)?));
    expressions.push(nft_cmp_expr(op, &network.octets()));
    Ok(expressions)
}

fn nft_ipv4_addr_match(
    header: IpHeader,
    offset: u32,
    address: Ipv4Addr,
    op: u32,
) -> Result<Vec<Vec<u8>>, IsolatedError> {
    let mut expressions = nft_ipv4_guard_exprs(header)?;
    expressions.push(nft_payload_ipv4_expr(offset)?);
    expressions.push(nft_cmp_expr(op, &address.octets()));
    Ok(expressions)
}

fn nft_ipv4_guard_exprs(header: IpHeader) -> Result<Vec<Vec<u8>>, IsolatedError> {
    match header {
        IpHeader::Bridge => {
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
        IpHeader::Inet => {
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
    }
}

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

fn nft_payload_expr(base: u32, offset: u32, len: u32) -> Result<Vec<u8>, IsolatedError> {
    let mut data = Vec::new();
    append_be_u32_attr(&mut data, NFTA_PAYLOAD_DREG, NFT_REG_1);
    append_be_u32_attr(&mut data, NFTA_PAYLOAD_BASE, base);
    append_be_u32_attr(&mut data, NFTA_PAYLOAD_OFFSET, offset);
    append_be_u32_attr(&mut data, NFTA_PAYLOAD_LEN, len);
    Ok(nft_expr("payload", &data))
}

fn nft_meta_expr(key: u32) -> Vec<u8> {
    let mut data = Vec::new();
    append_be_u32_attr(&mut data, NFTA_META_DREG, NFT_REG_1);
    append_be_u32_attr(&mut data, NFTA_META_KEY, key);
    nft_expr("meta", &data)
}

fn nft_bitwise_mask_expr(mask: [u8; 4]) -> Vec<u8> {
    let mut data = Vec::new();
    append_be_u32_attr(&mut data, NFTA_BITWISE_SREG, NFT_REG_1);
    append_be_u32_attr(&mut data, NFTA_BITWISE_DREG, NFT_REG_1);
    append_be_u32_attr(&mut data, NFTA_BITWISE_LEN, IPV4_ADDR_LEN);
    append_data_value_attr(&mut data, NFTA_BITWISE_MASK, &mask);
    append_data_value_attr(&mut data, NFTA_BITWISE_XOR, &[0, 0, 0, 0]);
    nft_expr("bitwise", &data)
}

fn nft_cmp_expr(op: u32, value: &[u8]) -> Vec<u8> {
    let mut data = Vec::new();
    append_be_u32_attr(&mut data, NFTA_CMP_SREG, NFT_REG_1);
    append_be_u32_attr(&mut data, NFTA_CMP_OP, op);
    append_data_value_attr(&mut data, NFTA_CMP_DATA, value);
    nft_expr("cmp", &data)
}

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

fn nft_expr(name: &str, data: &[u8]) -> Vec<u8> {
    let mut expression = Vec::new();
    append_cstr_attr(&mut expression, NFTA_EXPR_NAME, name);
    if !data.is_empty() {
        append_nested_attr(&mut expression, NFTA_EXPR_DATA, data);
    }
    expression
}

const fn gateway_addr() -> Ipv4Addr {
    Ipv4Addr::new(10, 244, 0, 1)
}

const fn bridge_network() -> (Ipv4Addr, u8) {
    (Ipv4Addr::new(10, 244, 0, 0), BRIDGE_PREFIX_LEN)
}

#[derive(Clone, Copy)]
enum IpHeader {
    Bridge,
    Inet,
}

const NFT_REG_VERDICT: u32 = 0;
const NFT_REG_1: u32 = 1;
const NFT_CMP_EQ: u32 = 0;
const NFT_CMP_NEQ: u32 = 1;
const IPV4_SADDR_OFFSET: u32 = 12;
const IPV4_DADDR_OFFSET: u32 = 16;
const IPV4_ADDR_LEN: u32 = 4;
const ETHER_TYPE_OFFSET: u32 = 12;
const ETHER_TYPE_LEN: u32 = 2;

pub(super) const NFTA_TABLE_NAME: u16 = 1;
pub(super) const NFTA_TABLE_FLAGS: u16 = 2;
pub(super) const NFTA_CHAIN_TABLE: u16 = 1;
pub(super) const NFTA_CHAIN_NAME: u16 = 3;
pub(super) const NFTA_CHAIN_HOOK: u16 = 4;
pub(super) const NFTA_CHAIN_TYPE: u16 = 7;
pub(super) const NFTA_HOOK_HOOKNUM: u16 = 1;
pub(super) const NFTA_HOOK_PRIORITY: u16 = 2;
pub(super) const NFTA_RULE_TABLE: u16 = 1;
pub(super) const NFTA_RULE_CHAIN: u16 = 2;
pub(super) const NFTA_RULE_EXPRESSIONS: u16 = 4;
pub(super) const NFTA_LIST_ELEM: u16 = 1;
const NFTA_EXPR_NAME: u16 = 1;
const NFTA_EXPR_DATA: u16 = 2;
const NFTA_PAYLOAD_DREG: u16 = 1;
const NFTA_PAYLOAD_BASE: u16 = 2;
const NFTA_PAYLOAD_OFFSET: u16 = 3;
const NFTA_PAYLOAD_LEN: u16 = 4;
const NFTA_META_DREG: u16 = 1;
const NFTA_META_KEY: u16 = 2;
const NFTA_CMP_SREG: u16 = 1;
const NFTA_CMP_OP: u16 = 2;
const NFTA_CMP_DATA: u16 = 3;
pub(super) const NFTA_DATA_VALUE: u16 = 1;
const NFTA_DATA_VERDICT: u16 = 2;
const NFTA_IMMEDIATE_DREG: u16 = 1;
const NFTA_IMMEDIATE_DATA: u16 = 2;
const NFTA_VERDICT_CODE: u16 = 1;
const NFTA_BITWISE_SREG: u16 = 1;
const NFTA_BITWISE_DREG: u16 = 2;
const NFTA_BITWISE_LEN: u16 = 3;
const NFTA_BITWISE_MASK: u16 = 4;
const NFTA_BITWISE_XOR: u16 = 5;

#[cfg(test)]
#[path = "../../../tests/unit/network/netfilter/exprs.rs"]
mod tests;
