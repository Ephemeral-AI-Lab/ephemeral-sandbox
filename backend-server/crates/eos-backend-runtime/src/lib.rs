//! `eos-backend-runtime` — backend-owned sandbox lifecycle and event streaming.
//!
//! [`SandboxManager`] (Phase 4) is the in-memory owner of sandbox setup, binding,
//! refcounting, delete policy, and teardown; it composes the Docker/daemon host
//! (`eos-sandbox-host`) behind one shared registry and implements the
//! [`SandboxGateway`](eos_sandbox_port::SandboxGateway) port so agent-core
//! request services can be wired against it without importing the host.
//!
//! The backend runtime owns:
//!
//! - [`EventBus`] persists engine milestones and serves replay-safe live streams.
//! - [`resolve_api_status`] joins backend and agent-core status into the API
//!   vocabulary.
#![warn(missing_docs)]

mod event_bus;
mod sandbox_manager;
mod status;

pub use event_bus::{EventBus, EventSubscription};
pub use sandbox_manager::{DeleteRejection, SandboxManager, SandboxManagerError};
pub use status::resolve_api_status;

// Shared test helpers. Body lives under the crate `tests/` tree (spec §Backend
// Test Layout); declared here so event-bus tests can reuse temp-store helpers.
#[cfg(test)]
#[path = "../tests/support/mod.rs"]
mod test_support;
