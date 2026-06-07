//! Plugin PPC contracts — manifest/service metadata plus a bidirectional,
//! message-id'd plugin-process channel.
//!
//! # Invariant this crate owns
//!
//! The Rust path loaded a plugin handler with `dynamic loading.import_module(...)`
//! in-process per call. The Rust path replaces that with daemon-owned service
//! processes connected over a bidirectional PPC channel on an `AF_UNIX` socket.
//! This crate deliberately owns only the pure contract pieces: plugin manifests,
//! service keys/status, refresh messages, public op names, and PPC frames. The
//! live process registry, per-op overlay, and OCC callback handling stay in
//! `eos-daemon`.
//!
//! Isolated mode BLOCKS all plugin ops ([`PluginError::ForbiddenInIsolatedWorkspace`]).
//!
//! # MF-1 — ONE single writer per `layer_stack_root` (STATE LOUDLY)
//!
//! The self-managed OCC commit callback MUST route through the SAME
//! per-`layer_stack_root` single `occ-commit-queue` writer + storage lease as the
//! primary `WRITE_ALLOWED` path — NEVER a second writer instance. This crate
//! cannot publish and does not define a publish port; `eos-daemon` owns the
//! callback handler and routes it through its existing per-root OCC writer.
//!
//! # Build-time guarantee — NOT `eos-occ`
//!
//! This crate does NOT depend on `eos-occ`, `eos-overlay`, `eos-layerstack`,
//! `nix`, or `tokio`. Snapshot, overlay, publish, process, and
//! namespace behavior are daemon-side responsibilities.
//!
//! # Runtime payload (NOT a Cargo dependency)
//!
//! Plugin payload runtimes are package content provisioned into daemon-managed
//! service processes. They are runtime artifacts, not core dependencies of this
//! crate.
#![forbid(unsafe_code)]

pub mod error;
pub mod manifest;
pub mod ppc;
pub mod refresh;
pub mod registry;
pub mod service;
pub mod service_registry;

pub use error::{PluginError, Result};
pub use manifest::{
    PluginDependencyScope, PluginManifest, PluginOperationManifest, PluginPackageManifest,
    PluginServiceManifest, PluginSetupManifest, PACKAGE_SHA256_MARKER, SETUP_SHA256_MARKER,
};
pub use ppc::{PpcDirection, PpcEnvelope};
pub use refresh::{RefreshAck, RefreshRequest};
pub use registry::public_op_name;
pub use service::{PluginServiceKey, PluginServiceKeyParts, RefreshStrategy, ServiceMode};
pub use service_registry::{PluginServiceState, PluginServiceStatus};
