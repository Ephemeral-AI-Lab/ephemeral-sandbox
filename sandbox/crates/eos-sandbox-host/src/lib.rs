//! Host-side sandbox engine: owns and reaches sandbox containers.
//!
//! # Dependency law (SPEC §2)
//!
//! `eos-api → eos-sandbox-host → (std + boring externals only)`. This crate
//! must NEVER depend on a workspace-internal crate: no compiled code is shared
//! across the host/box boundary. The wire vocabulary it speaks ([`protocol`])
//! is a deliberate host-side copy of the daemon protocol; drift is caught by the
//! conformance tests against `contract/fixtures/`, not by a shared crate.
//!
//! # What lives here
//!
//! - [`protocol`] — host-side daemon envelope/client contract.
//! - [`host`] — fleet registry, endpoint cache, forwarding, and recovery.
//! - [`runtime`] — Docker-backed daemon container lifecycle.
//!
//! This crate must never parse op semantics beyond catalog metadata.
#![forbid(unsafe_code)]

mod host;
pub mod protocol;
mod runtime;

pub use host::{ForwardError, HostConfig, SandboxHost, SandboxStatus};
pub use protocol::MAX_REQUEST_BYTES;

/// Explicit support surface for the live E2E harness.
pub mod e2e_support {
    pub use crate::protocol::{error_kind, is_success, ClientError, ProtocolClient};
    pub use crate::runtime::{
        container_label, docker_available, remove_labeled_containers, running_container_ids,
        ContainerLifetime, ContainerSpec, DaemonContainer, DaemonSpec,
    };
}
