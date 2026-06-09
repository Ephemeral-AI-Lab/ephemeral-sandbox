//! `eos-runtime` — the composition root of agent-core.
//!
//! This crate owns the typed dependency graph ([`RuntimeServices`]) that constructs
//! every concrete store (from `eos-db`) and every concrete seam implementation
//! ([`LlmClient`](eos_llm_client::LlmClient), provider registry, audit sink,
//! clock, registries), then injects those concretes into the trait seams the
//! engine and workflow crates depend on (DIP). For a top-level request it mints
//! the root [`Task`](eos_types::Task) — its id derived from the caller-injected
//! `request_id` — and task-triggers the root agent through
//! [`AgentRunApi`](eos_agent_run::AgentRunApi), returning the root's outcome
//! ([`run_request`] → [`RequestOutcome`]); there is no root workflow and no
//! request handle. It provisions one sandbox binding per request and wires the
//! per-request delegated-workflow runtime
//! ([`AttemptDeps`](eos_workflow::AttemptDeps) + the `AgentRunner` adapter + the
//! downstream-state ports).
//!
//! What this crate must **not** do: define any domain/store/seam trait (those
//! are owned upstream), implement query-loop / tool-dispatch / workflow
//! scheduling logic, introduce a global agent orchestrator, or mutate the parent
//! Task at workflow close. It is the only crate that may use `anyhow` and the
//! only crate that constructs/owns the async runtime.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod agent_runner;
mod agents;
mod cancel;
mod entry;
pub mod observability;
mod plugins;
mod request_input;
mod runtime_services;

#[cfg(test)]
#[path = "../tests/unit/mod.rs"]
mod tests;

pub use cancel::{cancel_agent_core_user_request, CancelReport};
pub use entry::{run_request, RequestOutcome};
pub use request_input::RequestRunInput;
pub use runtime_services::{
    EventCallback, EventSourceFactory, RuntimeServices, RuntimeServicesBuilder, StateReader,
};

// Re-export the sandbox binding value object owned by the sandbox port; this
// crate references it in its public run input/outcome surface.
pub use eos_sandbox_port::RequestSandboxBinding;
