//! CAS byte-identity and shared in-box models: the renamed rump of the old
//! `eos-protocol` crate.
//!
//! Invariant: this crate owns the two correctness-bearing content hashes
//! (`manifest_root_hash`, `layer_digest`) and the manifest/layer types, pinned
//! by the 18 golden cases in `contract/fixtures/cas/cases.json` (AV-1c
//! byte-identity bar) per `docs/contract/02-cas-byte-identity.md`. It also owns
//! the daemon↔ns-runner wire DTOs ([`runner`]), shared by the tokio daemon and
//! the single-threaded `eos-ns-child` so neither depends on the other. It
//! depends on nothing internal, and host-side crates never depend on it.
#![forbid(unsafe_code)]

// Lib tests receive dev-dependencies used by fixture integration tests. Keep
// `unused_crate_dependencies` usable under `--all-targets` without an allow.
#[cfg(test)]
use base64 as _;
#[cfg(test)]
use proptest as _;

pub mod cas;
pub mod models;
pub mod runner;

pub use cas::{
    aggregate_layer_changes, layer_digest, manifest_root_hash, CasError, LayerChange, LayerPath,
    LayerRef, Manifest, MANIFEST_SCHEMA_VERSION,
};
pub use models::Intent;
pub use runner::{Fd, NsFds, RunMode, RunRequest, RunResult, RunnerVerb, ToolCall, WorkspaceRoot};
