//! Runtime-local Layer-A fixtures: the [`AppState`] builder over a temp `SQLite`
//! db and the fake provisioner.
//!
//! These stay crate-local (not in `eos-testkit`) because they reference
//! `eos-runtime` types — the dev-dependency two-instance rule bars an external
//! `build_test_state`/`FakeProvisioner` from this crate's own in-crate tests
//! (`TESTING_SPEC` §14.2). Included as a submodule of `app_state` via `#[path]`, so
//! it reaches the `pub(crate)` `RequestProvisioner` seam and `#[cfg(test)]`
//! `.provisioner(...)` setter through `super::`. The cross-crate-safe doubles
//! (`ScriptedSource`, `FakeTransport`, factories, `tool_use_turn`, `agent_def`)
//! come from `eos-testkit`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use async_trait::async_trait;
use eos_agent_def::{AgentDefinition, AgentRegistry};
use eos_sandbox_host::RequestSandboxBinding;
use eos_testkit::FakeTransport;
use eos_types::RequestId;

use super::{AppState, EventSourceFactory, RequestProvisioner};

/// A provisioner that binds a fixed sandbox id (`sb-test`) without touching
/// Docker.
#[derive(Debug)]
pub(crate) struct FakeProvisioner {
    id: String,
}

impl Default for FakeProvisioner {
    fn default() -> Self {
        Self {
            id: "sb-test".to_owned(),
        }
    }
}

#[async_trait]
impl RequestProvisioner for FakeProvisioner {
    async fn prepare_for_run(
        &self,
        request_id: &RequestId,
        sandbox_id: Option<&str>,
    ) -> anyhow::Result<RequestSandboxBinding> {
        let resolved = sandbox_id
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.id);
        Ok(RequestSandboxBinding {
            sandbox_id: resolved.parse()?,
            request_id: request_id.clone(),
        })
    }
}

/// Build a fully-wired test [`AppState`] over a temp `SQLite` db, the fake
/// provisioner, the workspace `FakeTransport`, the given agent registry, and an
/// optional event-source factory. Returns the state and the owning temp dir
/// (keep it alive for the test's duration).
pub(crate) async fn build_test_state(
    factory: Option<EventSourceFactory>,
    agents: Vec<AgentDefinition>,
) -> (AppState, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let url = format!("sqlite://{}", dir.path().join("test.db").display());
    let registry: AgentRegistry = agents.into_iter().collect();
    let mut builder = AppState::builder()
        .database_url(url)
        .cwd(dir.path().display().to_string())
        .tools_root(eos_testkit::test_tools_root())
        .provisioner(Arc::new(FakeProvisioner::default()))
        .transport(Arc::new(FakeTransport))
        .agent_registry(Arc::new(registry));
    if let Some(factory) = factory {
        builder = builder.event_source_factory(factory);
    }
    let state = builder.build().await.expect("build app state");
    (state, dir)
}
