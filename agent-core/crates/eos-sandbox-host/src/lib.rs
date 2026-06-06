//! eos-sandbox-host — the host side of the sandbox.
//!
//! Uses Docker as the only Rust production sandbox provider, owns the per-process
//! [`ProviderRegistry`] as explicit application state, runs container lifecycle
//! with post-lifecycle setup, transports JSON envelopes to the resident
//! in-sandbox daemon with spawn/connect recovery and typed error decoding, and
//! uploads + verifies the pinned `eosd` bootstrap artifact.
//!
//! AC-eos-sandbox-host-10 — the [`ProviderAdapter`] trait is **sealed**: its
//! `Sealed` supertrait lives in a `pub(crate)` module, so a downstream crate
//! cannot name it and therefore cannot implement the trait. This is enforced by
//! the type system; the doctest below proves the path is unreachable (it fails
//! to compile), which is exactly what would change if the seal were weakened:
//!
//! ```compile_fail
//! // error[E0603]: module `provider` is private — `Sealed` is unreachable, so
//! // no out-of-crate type can implement `ProviderAdapter`.
//! use eos_sandbox_host::provider::sealed::Sealed;
//! struct Foreign;
//! impl Sealed for Foreign {}
//! ```
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod bootstrap_artifact;
mod daemon_client;
mod docker;
mod error;
mod lifecycle;
mod plugin_package;
mod provider;
mod provisioning;
mod registry;
mod sandbox_upload;

#[cfg(test)]
#[path = "../tests/support/mod.rs"]
mod support;

pub use bootstrap_artifact::{EOSD_VERSION, PROTOCOL_VERSION};
pub use daemon_client::{
    with_daemon_protocol_version, DaemonClient, DAEMON_PROTOCOL_VERSION, DEFAULT_LAYER_STACK_ROOT,
};
pub use docker::DockerProviderAdapter;
pub use error::SandboxHostError;
pub use lifecycle::SandboxLifecycle;
pub use provider::{
    ContextPreparer, CreateSandboxSpec, DaemonTcpEndpoint, DockerContextPreparer, ExecOpts, Labels,
    PreviewUrl, ProviderAdapter, ProviderHealth, ProviderKind, RawExecResult, SandboxInfo,
    SnapshotInfo,
};
pub use provisioning::{RequestProvisioner, RequestSandboxBinding, RequestSandboxProvisioner};
pub use registry::{resolve_provider_kind, ProviderRegistry};
