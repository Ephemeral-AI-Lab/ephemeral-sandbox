//! Static first-party plugin op adapters.

pub(crate) mod admin;
pub(crate) mod pyright_lsp;

pub(crate) use admin::{op_health, op_list};
pub(crate) use pyright_lsp::{
    op_pyright_lsp_definition, op_pyright_lsp_diagnostics, op_pyright_lsp_query_symbols,
    op_pyright_lsp_references,
};
