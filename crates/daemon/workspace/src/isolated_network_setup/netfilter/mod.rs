use crate::isolated_workspace::caps::Rfc1918Egress;
use crate::isolated_workspace::error::IsolatedError;

use super::{NFT_FILTER_TABLE, NFT_NAT_TABLE, RFC1918_NETS};

mod exprs;
mod wire;

use exprs::{
    nft_bridge_peer_isolation_rule_exprs, nft_imds_drop_rule_exprs, nft_masquerade_rule_exprs,
    nft_peer_isolation_rule_exprs, nft_rfc1918_drop_rule_exprs, NFTA_CHAIN_HOOK, NFTA_CHAIN_NAME,
    NFTA_CHAIN_TABLE, NFTA_CHAIN_TYPE, NFTA_HOOK_HOOKNUM, NFTA_HOOK_PRIORITY, NFTA_LIST_ELEM,
    NFTA_RULE_CHAIN, NFTA_RULE_EXPRESSIONS, NFTA_RULE_TABLE, NFTA_TABLE_FLAGS, NFTA_TABLE_NAME,
};
use wire::{
    append_be_i32_attr, append_be_u32_attr, append_cstr_attr, append_nested_attr,
    libc_c_int_to_u16, libc_c_int_to_u32, libc_c_int_to_u8, nft_create_flags, nft_rule_flags,
    send_nft_command,
};

const NFT_BRIDGE_FILTER_TABLE: &str = "eos_iws_bridge_filter";

pub(super) fn install_static_rules(
    rfc1918_egress: Rfc1918Egress,
    bridge_index: u32,
) -> Result<(), IsolatedError> {
    let inet_family = libc_c_int_to_u8(libc::NFPROTO_INET, "NFPROTO_INET")?;
    add_nft_table(inet_family, NFT_NAT_TABLE)?;
    add_nft_base_chain(
        inet_family,
        NFT_NAT_TABLE,
        "postrouting",
        "nat",
        libc_c_int_to_u32(libc::NF_INET_POST_ROUTING, "NF_INET_POST_ROUTING")?,
        100,
    )?;
    add_nft_rule(
        inet_family,
        NFT_NAT_TABLE,
        "postrouting",
        nft_masquerade_rule_exprs(bridge_index)?,
    )?;

    add_nft_table(inet_family, NFT_FILTER_TABLE)?;
    add_nft_base_chain(
        inet_family,
        NFT_FILTER_TABLE,
        "forward",
        "filter",
        libc_c_int_to_u32(libc::NF_INET_FORWARD, "NF_INET_FORWARD")?,
        0,
    )?;
    add_nft_rule(
        inet_family,
        NFT_FILTER_TABLE,
        "forward",
        nft_imds_drop_rule_exprs()?,
    )?;
    add_nft_rule(
        inet_family,
        NFT_FILTER_TABLE,
        "forward",
        nft_peer_isolation_rule_exprs()?,
    )?;
    install_bridge_peer_isolation_rule()?;
    if rfc1918_egress == Rfc1918Egress::Deny {
        for cidr in RFC1918_NETS {
            add_nft_rule(
                inet_family,
                NFT_FILTER_TABLE,
                "forward",
                nft_rfc1918_drop_rule_exprs(cidr)?,
            )?;
        }
    }
    Ok(())
}

fn install_bridge_peer_isolation_rule() -> Result<(), IsolatedError> {
    let family = libc_c_int_to_u8(libc::NFPROTO_BRIDGE, "NFPROTO_BRIDGE")?;
    add_nft_table(family, NFT_BRIDGE_FILTER_TABLE)?;
    add_nft_base_chain(
        family,
        NFT_BRIDGE_FILTER_TABLE,
        "forward",
        "filter",
        libc_c_int_to_u32(libc::NF_BR_FORWARD, "NF_BR_FORWARD")?,
        0,
    )?;
    add_nft_rule(
        family,
        NFT_BRIDGE_FILTER_TABLE,
        "forward",
        nft_bridge_peer_isolation_rule_exprs()?,
    )
}

fn add_nft_table(family: u8, name: &str) -> Result<(), IsolatedError> {
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

fn add_nft_base_chain(
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

fn add_nft_rule(
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
