//! `eos-plugin-catalog` — the static plugin catalog.
//!
//! This crate turns the on-disk plugin catalog into validated, in-memory
//! metadata the runtime can bind into real tools. It parses each plugin's
//! `plugin.md` frontmatter into a [`PluginManifest`], discovers all manifests
//! under one configured catalog root as an immutable [`PluginCatalog`], supplies
//! the catalog-native model-facing [`PluginToolSpec`] sources (today the 10 LSP
//! specs).
//!
//! It deliberately does **not** import or execute plugin tool modules (no
//! `importlib`/`BaseTool` binding — GC-plugin-catalog-01), own
//! `eos_llm_client::ToolSpec`/`ToolExecutor`/`ToolRegistry` (those are bound in
//! `eos-runtime` — GC-plugin-catalog-04), run `setup`/`runtime` scripts or hold
//! any Pyright/LSP session (GC-plugin-catalog-05), or traverse outside the
//! configured catalog root. See
//! `docs/plans/backend_agent_core_rust_migration/impl-eos-plugin-catalog.md`.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod discovery;
mod error;
mod frontmatter;
mod manifest;
mod names;
mod tool_specs;

pub use discovery::PluginCatalog;
pub use error::PluginCatalogError;
pub use manifest::{PluginKind, PluginManifest, ToolEntry};
pub use names::{PluginName, PluginResolvedPath, PluginToolName};
pub use tool_specs::{plugin_tool_specs, PluginToolSpec};
