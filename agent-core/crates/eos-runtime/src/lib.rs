//! `eos-runtime` — the composition root of agent-core.
//!
//! This crate owns the typed dependency graph ([`AppState`]) that constructs
//! every concrete store (from `eos-db`) and every concrete seam implementation
//! ([`LlmClient`](eos_llm_client::LlmClient), provider registry, audit sink,
//! clock, registries), then injects those concretes into the trait seams the
//! engine and workflow crates depend on (DIP). It mints the root
//! [`Task`](eos_state::Task) for a top-level request and runs the root agent
//! **directly** through `eos-engine` — there is no root workflow. It provisions
//! one sandbox binding per request and wires the per-request delegated-workflow
//! runtime ([`AttemptDeps`](eos_workflow::AttemptDeps) + the `AgentRunner`
//! adapter + the downstream-state ports).
//!
//! What this crate must **not** do: define any domain/store/seam trait (those
//! are owned upstream), implement query-loop / tool-dispatch / workflow
//! scheduling logic, introduce a global agent orchestrator, or mutate the parent
//! Task at workflow close. It is the only crate that may use `anyhow` and the
//! only crate that constructs/owns the async runtime.
//!
//! See `docs/plans/backend_agent_core_rust_migration/impl-eos-runtime.md`.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod agent_loop;
mod agent_runner;
mod app_state;
mod entry;
pub mod observability;
mod root_agent;
mod tool_context;

#[cfg(test)]
mod tests;

pub use app_state::{
    AppState, AppStateBuilder, EventCallback, EventSourceFactory, RequestProvisioner,
};
pub use entry::{start_request, RequestEntryHandle};

// Re-export the sandbox binding value object owned upstream by `eos-sandbox-host`
// (a parallel agent moved provisioning there); this crate references it.
pub use eos_sandbox_host::RequestSandboxBinding;
