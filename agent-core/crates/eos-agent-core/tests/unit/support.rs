//! Runtime-local Layer-A fixtures: the [`AgentCoreRuntime`] builder over a temp `SQLite`
//! db and the fake provisioner.
//!
//! These stay crate-local (not in `eos-testkit`) because they reference
//! `eos-agent-core` types — the dev-dependency two-instance rule bars an external
//! `build_test_state`/`FakeProvisioner` from this crate's own in-crate tests
//! (`TESTING_SPEC` §14.2). Included as a submodule of `runtime` via
//! `#[path]`, so it can name `super::{AgentCoreRuntime, ProviderStreamSourceFactory}`; it
//! drives the production `sandbox_gateway(...)` builder seam with a
//! [`FakeGateway`] wrapping a fake transport and provisioner. The
//! cross-crate-safe doubles (`ScriptedSource`, `FakeTransport`, factories,
//! `tool_use_turn`, `agent_def`) come from `eos-testkit`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use async_trait::async_trait;
use eos_sandbox_port::{
    RequestProvisioner, RequestSandboxBinding, SandboxGateway, SandboxProvisionError,
    SandboxTransport,
};
use eos_testkit::FakeTransport;
use eos_types::{AgentDefinition, AgentRegistry, RequestId};

use super::{AgentCoreRuntime, ProviderStreamSourceFactory};

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
    ) -> Result<RequestSandboxBinding, SandboxProvisionError> {
        let resolved = sandbox_id
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.id);
        Ok(RequestSandboxBinding {
            sandbox_id: resolved
                .parse()
                .map_err(|_| SandboxProvisionError::new("empty sandbox id"))?,
            request_id: request_id.clone(),
        })
    }
}

/// A [`SandboxGateway`] that hands back the injected transport and provisioner —
/// the test analogue of the backend `SandboxManager`, used to drive the single
/// `sandbox_gateway(...)` builder seam.
pub(crate) struct FakeGateway {
    transport: Arc<dyn SandboxTransport>,
    provisioner: Arc<dyn RequestProvisioner>,
}

impl FakeGateway {
    pub(crate) fn new(
        transport: Arc<dyn SandboxTransport>,
        provisioner: Arc<dyn RequestProvisioner>,
    ) -> Self {
        Self {
            transport,
            provisioner,
        }
    }
}

impl std::fmt::Debug for FakeGateway {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeGateway").finish_non_exhaustive()
    }
}

impl SandboxGateway for FakeGateway {
    fn transport(&self) -> Arc<dyn SandboxTransport> {
        self.transport.clone()
    }

    fn provisioner(&self) -> Arc<dyn RequestProvisioner> {
        self.provisioner.clone()
    }
}

/// Build fully-wired test [`AgentCoreRuntime`] over a temp `SQLite` db, the fake
/// provisioner, the workspace `FakeTransport`, the given agent registry, and an
/// optional provider-stream factory. Returns the state and the owning temp dir
/// (keep it alive for the test's duration).
pub(crate) async fn build_test_state(
    factory: Option<ProviderStreamSourceFactory>,
    agents: Vec<AgentDefinition>,
) -> (AgentCoreRuntime, tempfile::TempDir) {
    build_test_state_inner(factory, agents, false).await
}

pub(crate) async fn build_test_state_with_message_records(
    factory: Option<ProviderStreamSourceFactory>,
    agents: Vec<AgentDefinition>,
) -> (AgentCoreRuntime, tempfile::TempDir) {
    build_test_state_inner(factory, agents, true).await
}

async fn build_test_state_inner(
    factory: Option<ProviderStreamSourceFactory>,
    agents: Vec<AgentDefinition>,
    message_records: bool,
) -> (AgentCoreRuntime, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let url = format!("sqlite://{}", dir.path().join("test.db").display());
    let registry: AgentRegistry = agents.into_iter().collect();
    let mut builder = AgentCoreRuntime::builder()
        .database_url(url)
        .tools_root(eos_testkit::test_tools_root())
        .sandbox_gateway(Arc::new(FakeGateway::new(
            Arc::new(FakeTransport),
            Arc::new(FakeProvisioner::default()),
        )))
        .agent_registry(Arc::new(registry));
    if let Some(factory) = factory {
        builder = builder.provider_stream_source_factory(factory);
    }
    if message_records {
        builder = builder.message_records_root(dir.path().join("message-records"));
    }
    let state = builder.build().await.expect("build app state");
    (state, dir)
}
