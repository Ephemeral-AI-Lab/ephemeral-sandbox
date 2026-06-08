//! Shared agent-core test doubles (the `EventSource` / Layer-A layer).
//!
//! The single home for the doubles per-crate mock tests substitute at the LLM
//! and daemon-RPC edges: the scripted [`EventSource`](eos_engine::EventSource)
//! ([`ScriptedSource`] — the only definition in the workspace, `TESTING_SPEC` AC3),
//! the fake [`SandboxTransport`](eos_sandbox_port::SandboxTransport)
//! ([`FakeTransport`]), agent-definition builders, the `run_until` stream
//! stepper, and the [`ExecutionMetadata`](eos_tools::ExecutionMetadata) fixture
//! ([`metadata`]). Consumed as a `[dev-dependencies]` crate, so its `src/` *is*
//! test infrastructure and no production crate carries test-support code in its
//! own `src/` (`TESTING_SPEC` I2).
//!
//! Scope note (`TESTING_SPEC` §14.2 / §15): this crate deliberately does **not**
//! hold `build_test_state`/`FakeProvisioner` (single-consumer `eos-runtime`
//! types) or the Layer-B workflow runner/store doubles (single-consumer
//! `eos-workflow` types). A dev-dependency double is only consumable by crate
//! `X`'s in-crate tests when none of its types are owned by `X` (the dev-dep
//! two-instance rule), so those stay local to their owning crate's `tests/`.
//! Everything here is built from types at or below `eos-engine`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod agents;
mod engine;
mod llm;
mod meta;
mod sandbox;

pub use agents::{agent_def, test_tools_root};
pub use engine::run_until;
pub use llm::{
    factory_by_agent, factory_from, factory_root_blocks_after, request_route_key_for, text_turn,
    tool_use_turn, ScriptedByAgentSource, ScriptedSource,
};
pub use meta::metadata;
pub use sandbox::FakeTransport;
