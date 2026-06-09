//! Runtime service construction.
//!
//! Every store and seam is constructed exactly once here (GC-eos-agent-core-02):
//! there are no module-level mutable singletons.

use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};

use anyhow::{Context, Result};
use async_trait::async_trait;
use eos_db::{Database, DatabaseConfig, DatabaseUrl};
use eos_engine::records::AgentRunRecordWriter;
use eos_llm_client::{
    Auth, ConfiguredLlmClient, LlmClient, LlmRequest, LlmRequestDefaults, LlmStream, ProviderError,
    ProviderKind, ProvidersConfig, SecretConfigValue,
};
use eos_sandbox_port::{
    DaemonOp, RequestProvisioner, RequestSandboxBinding, SandboxGateway, SandboxPortError,
    SandboxProvisionError, SandboxTransport,
};
use eos_tool::{
    build_registry_schema, CallerScope, SkillRegistry, ToolConfigSet, ToolKey, ToolRegistry,
};
use eos_types::{AgentRegistry, AgentRegistryBuilder, ModelRegistrationConfig, ModelsConfig};
use eos_types::{JsonObject, RequestId, SandboxId};
use eos_workflow::WorkflowConfig;

use super::audit::{AuditSink, BufferedAuditShutdown, BufferedJsonlSink, NoopAuditSink};
use super::config::{self, RuntimeConfig};
use super::plugins::register_plugin_tools;
use super::{
    AgentCoreRegistryService, AgentCoreRuntime, AuditRuntime, DbStoreService, EngineService,
    ProviderStreamSourceFactory, RecordWriterRuntime, SandboxService,
};
use crate::agents::load_agents_tree;

/// Placeholder client used when no provider is selected and no
/// `provider_stream_source_factory` is set. Streaming always errors; production
/// wires a real provider from `providers.active`, and tests set
/// `provider_stream_source_factory`.
#[derive(Debug, Default)]
struct UnconfiguredLlmClient;

#[async_trait]
impl LlmClient for UnconfiguredLlmClient {
    async fn stream_message(&self, _request: LlmRequest) -> Result<LlmStream, ProviderError> {
        Err(ProviderError::transport(
            "no llm provider configured (set providers.active in local.yml or inject a provider_stream_source_factory)",
        ))
    }
}

/// Placeholder sandbox transport used when none is injected: every RPC errors.
/// The Docker/daemon transport now lives in `eos-sandbox-host` and is injected
/// by the backend composition root (Phase 2 `SandboxGateway`); tests inject a
/// fake transport. Mirrors [`UnconfiguredLlmClient`].
#[derive(Debug, Default)]
struct UnconfiguredSandboxTransport;

#[async_trait]
impl SandboxTransport for UnconfiguredSandboxTransport {
    async fn call(
        &self,
        _sandbox_id: &SandboxId,
        _op: DaemonOp,
        _payload: JsonObject,
        _timeout_s: u32,
    ) -> Result<JsonObject, SandboxPortError> {
        Err(SandboxPortError::transport(
            None,
            "no sandbox transport configured (backend must inject a sandbox gateway)",
        ))
    }
}

/// Placeholder request provisioner used when none is injected: `prepare_for_run`
/// errors. The host-backed provisioner is injected by the backend composition
/// root; tests inject a fake.
#[derive(Debug, Default)]
struct UnconfiguredProvisioner;

#[async_trait]
impl RequestProvisioner for UnconfiguredProvisioner {
    async fn prepare_for_run(
        &self,
        _request_id: &RequestId,
        _sandbox_id: Option<&str>,
    ) -> Result<RequestSandboxBinding, SandboxProvisionError> {
        Err(SandboxProvisionError::new(
            "no sandbox provisioner configured (backend must inject a sandbox gateway)",
        ))
    }
}

/// `#[must_use]` builder for [`AgentCoreRuntime`]. Every field is an optional override:
/// `None` selects the production default. Tests inject in-memory stores, a mock
/// `provider_stream_source_factory`, a fake sandbox gateway, and explicit
/// registries.
#[must_use = "AgentCoreRuntimeBuilder does nothing until build() is called"]
#[derive(Default)]
pub struct AgentCoreRuntimeBuilder {
    database_url: Option<String>,
    llm_client: Option<Arc<dyn LlmClient>>,
    providers_config: Option<ProvidersConfig>,
    workflow_config: Option<WorkflowConfig>,
    runtime_config: Option<RuntimeConfig>,
    provider_stream_source_factory: Option<ProviderStreamSourceFactory>,
    audit: Option<Arc<dyn AuditSink>>,
    audit_path: Option<PathBuf>,
    record_root: Option<PathBuf>,
    agent_registry: Option<Arc<AgentRegistry>>,
    agents_dir: Option<PathBuf>,
    tool_config: Option<Arc<ToolConfigSet>>,
    tools_root: Option<PathBuf>,
    skill_registry: Option<Arc<SkillRegistry>>,
    skill_root: Option<PathBuf>,
    gateway: Option<Arc<dyn SandboxGateway>>,
}

impl std::fmt::Debug for AgentCoreRuntimeBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentCoreRuntimeBuilder")
            .finish_non_exhaustive()
    }
}

impl AgentCoreRuntimeBuilder {
    /// Override the database URL (test seam; a network URL makes `build()` fail
    /// fast).
    pub fn database_url(mut self, url: impl Into<String>) -> Self {
        self.database_url = Some(url.into());
        self
    }

    /// Inject an LLM client (an unconfigured placeholder by default).
    pub fn llm_client(mut self, client: Arc<dyn LlmClient>) -> Self {
        self.llm_client = Some(client);
        self
    }

    /// Inject provider config used to construct the default LLM client.
    pub fn providers_config(mut self, config: ProvidersConfig) -> Self {
        self.providers_config = Some(config);
        self
    }

    /// Inject workflow config used to wire attempt and planner-depth runtime
    /// tunables.
    pub fn workflow_config(mut self, config: WorkflowConfig) -> Self {
        self.workflow_config = Some(config);
        self
    }

    /// Inject runtime config used to wire runtime-local background tunables.
    pub fn runtime_config(mut self, config: RuntimeConfig) -> Self {
        self.runtime_config = Some(config);
        self
    }

    /// Inject the per-agent provider-stream factory (mock harness).
    pub fn provider_stream_source_factory(mut self, factory: ProviderStreamSourceFactory) -> Self {
        self.provider_stream_source_factory = Some(factory);
        self
    }

    /// Inject an audit sink (a no-op sink by default unless `audit_path` is set).
    pub fn audit(mut self, audit: Arc<dyn AuditSink>) -> Self {
        self.audit = Some(audit);
        self
    }

    /// Write audit events to a buffered JSONL file at `path`.
    pub fn audit_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.audit_path = Some(path.into());
        self
    }

    /// Write and serve agent-node `messages.jsonl` / `events.jsonl` under this
    /// record root.
    pub fn record_root(mut self, path: impl Into<PathBuf>) -> Self {
        self.record_root = Some(path.into());
        self
    }

    /// Inject a prebuilt agent registry (else load from `agents_dir`, else empty).
    pub fn agent_registry(mut self, registry: Arc<AgentRegistry>) -> Self {
        self.agent_registry = Some(registry);
        self
    }

    /// Load agent profiles from this directory tree.
    pub fn agents_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.agents_dir = Some(dir.into());
        self
    }

    /// Inject a prebuilt tool config (else load from `tools_root`).
    pub fn tool_config(mut self, config: Arc<ToolConfigSet>) -> Self {
        self.tool_config = Some(config);
        self
    }

    /// Load the externalized tool config from this `.eos-agents/tools` root.
    pub fn tools_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.tools_root = Some(root.into());
        self
    }

    /// Inject a prebuilt skill registry (else load from `skill_root`, else empty).
    pub fn skill_registry(mut self, registry: Arc<SkillRegistry>) -> Self {
        self.skill_registry = Some(registry);
        self
    }

    /// Load skills from this root directory.
    pub fn skill_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.skill_root = Some(root.into());
        self
    }

    /// Inject the production sandbox gateway: one handle that hands back the
    /// transport (per-tool daemon RPC) and provisioner (request binding) sharing
    /// the backend's registry/lifecycle. This is the production-visible seam the
    /// backend composition root wires its `SandboxManager` into; without it the
    /// runtime falls back to placeholders that error on first sandbox use. Tests
    /// inject a fake gateway wrapping a fake transport and provisioner.
    pub fn sandbox_gateway(mut self, gateway: Arc<dyn SandboxGateway>) -> Self {
        self.gateway = Some(gateway);
        self
    }

    /// Construct the runtime graph: build the `SQLite` pool (fail fast on a network
    /// URL), construct every store and seam, apply the model registry config,
    /// and validate agent profile tool names against the registry.
    ///
    /// # Errors
    /// Returns an error if the DB URL is non-local, the pool/migrations fail, a
    /// configured agent/skill/plugin root cannot be loaded, or an agent names an
    /// unknown tool.
    pub async fn build(self) -> Result<AgentCoreRuntime> {
        // Database: a network URL fails fast at parse (GC: SQLite-only).
        // `DatabaseConfig` is `#[non_exhaustive]`, so override the url by mutation
        // rather than struct-update syntax.
        let mut db_config = DatabaseConfig::default();
        if let Some(url) = self.database_url {
            db_config.url =
                DatabaseUrl::parse(url).context("database url is not a local sqlite url")?;
        }
        let database = Database::open(&db_config)
            .await
            .context("opening the sqlite database")?;

        let config_doc = config::load().context("loading runtime config")?;
        let workflow_config = match self.workflow_config {
            Some(config) => config,
            None => config_doc
                .section::<WorkflowConfig>("workflow")
                .context("loading workflow config")?,
        };
        workflow_config
            .validate()
            .context("validating workflow config")?;

        let runtime_config = match self.runtime_config {
            Some(config) => config,
            None => config_doc
                .section::<RuntimeConfig>("runtime")
                .context("loading runtime config")?,
        };
        runtime_config
            .validate()
            .context("validating runtime config")?;

        let providers_config = match self.providers_config {
            Some(config) => config,
            None => config_doc
                .section::<ProvidersConfig>("providers")
                .context("loading providers config")?,
        };
        providers_config
            .validate()
            .context("validating providers config")?;
        if let Some(provider_models) = providers_config.active_models() {
            sync_model_registry(&database, provider_models).await;
        }

        let llm_client: Arc<dyn LlmClient> = match self.llm_client {
            Some(client) => client,
            None => default_llm_client(&providers_config).context("configuring llm provider")?,
        };

        // Audit: explicit sink wins; else a buffered JSONL sink when a path is
        // configured; else a no-op sink.
        let (audit, audit_shutdown): (Arc<dyn AuditSink>, Option<BufferedAuditShutdown>) =
            match (self.audit, &self.audit_path) {
                (Some(sink), _) => (sink, None),
                (None, Some(path)) => {
                    let (sink, shutdown) = BufferedJsonlSink::new(path.clone(), 1024)
                        .context("opening the audit jsonl sink")?;
                    (Arc::new(sink), Some(shutdown))
                }
                (None, None) => (Arc::new(NoopAuditSink), None),
            };

        let agent_registry = match self.agent_registry {
            Some(registry) => registry,
            None => Arc::new(build_agent_registry(self.agents_dir.as_deref())?),
        };

        let skill_registry = match self.skill_registry {
            Some(registry) => registry,
            None => Arc::new(build_skill_registry(self.skill_root.as_deref())?),
        };

        let tool_config = match self.tool_config {
            Some(config) => Arc::new(
                (*config)
                    .clone()
                    .with_workflow_max_depth(workflow_config.max_depth),
            ),
            None => Arc::new(
                build_tool_config(self.tools_root.as_deref())?
                    .with_workflow_max_depth(workflow_config.max_depth),
            ),
        };

        // Sandbox access is one injected gateway (the backend `SandboxManager`)
        // that hands back the transport + provisioner sharing its registry; the
        // Docker/daemon host implementation lives in `eos-sandbox-host`. Without
        // injection the placeholders error at first use; tests inject a fake
        // gateway.
        let (transport, provisioner): (Arc<dyn SandboxTransport>, Arc<dyn RequestProvisioner>) =
            match self.gateway {
                Some(gateway) => (gateway.transport(), gateway.provisioner()),
                None => (
                    Arc::new(UnconfiguredSandboxTransport),
                    Arc::new(UnconfiguredProvisioner),
                ),
            };
        let mut tool_registry = build_registry_schema(&tool_config, &CallerScope::default());
        register_plugin_tools(&mut tool_registry, &transport);

        // Cross-registry validation: unknown agent tool names fail fast.
        validate_agent_tools(&agent_registry, &tool_registry)?;

        Ok(AgentCoreRuntime {
            db: DbStoreService {
                task_store: database.tasks(),
                request_store: database.requests(),
                workflow_store: database.workflows(),
                iteration_store: database.iterations(),
                attempt_store: database.attempts(),
                agent_run_store: database.agent_runs(),
                task_agent_run_store: database.task_agent_runs(),
            },
            agent_core: AgentCoreRegistryService {
                agent_registry,
                skill_registry,
                tool_config,
            },
            engine: EngineService {
                llm_client,
                provider_stream_source_factory: self.provider_stream_source_factory,
                runtime_config,
            },
            sandbox: SandboxService {
                transport,
                provisioner,
            },
            audit: AuditRuntime {
                sink: audit,
                shutdown: Arc::new(StdMutex::new(audit_shutdown)),
            },
            records: RecordWriterRuntime {
                run_record_writer: self.record_root.map(AgentRunRecordWriter::new),
            },
            agent_state: super::RuntimeAgentStateService::default(),
            cancel_registry: super::RequestCancelRegistry::new(),
        })
    }
}

async fn sync_model_registry(database: &Database, provider_models: &ModelsConfig) {
    match database
        .model_registry()
        .sync_from_config(provider_models)
        .await
    {
        Ok(count) => tracing::info!(models = count, "applied model registry config"),
        Err(err) => {
            tracing::warn!(error = %err, "model registry config skipped (non-fatal)");
        }
    }
}

/// Build the LLM client from the loaded `providers` config.
fn default_llm_client(providers: &ProvidersConfig) -> Result<Arc<dyn LlmClient>, ProviderError> {
    use eos_llm_client::{
        AnthropicApiClient, ClaudeCodingPlanClient, CodexCodingPlanClient, OpenAiApiClient,
    };

    let retry = Arc::new(providers.retry.clone());
    match providers.active {
        ProviderKind::Unconfigured => Ok(Arc::new(UnconfiguredLlmClient)),
        ProviderKind::OpenAiApi => {
            let key = provider_secret(
                "providers.openai_api.api_key",
                providers.openai_api.api_key.as_ref(),
            )?;
            configured_provider_client(
                providers,
                Arc::new(OpenAiApiClient::new(
                    &providers.openai_api.base_url,
                    Auth::bearer(key),
                    retry,
                )?),
            )
        }
        ProviderKind::AnthropicApi => {
            let key = provider_secret(
                "providers.anthropic_api.api_key",
                providers.anthropic_api.api_key.as_ref(),
            )?;
            configured_provider_client(
                providers,
                Arc::new(AnthropicApiClient::new(
                    &providers.anthropic_api.base_url,
                    Auth::api_key(key),
                    retry,
                )?),
            )
        }
        ProviderKind::CodexCodingPlan => {
            let token = provider_secret(
                "providers.codex_coding_plan.access_token",
                providers.codex_coding_plan.access_token.as_ref(),
            )?;
            let auth = Auth::codex_access_token_from_jwt(token)?;
            configured_provider_client(
                providers,
                Arc::new(CodexCodingPlanClient::new(
                    &providers.codex_coding_plan.base_url,
                    auth,
                    retry,
                )?),
            )
        }
        ProviderKind::ClaudeCodingPlan => {
            let token = provider_secret(
                "providers.claude_coding_plan.access_token",
                providers.claude_coding_plan.access_token.as_ref(),
            )?;
            configured_provider_client(
                providers,
                Arc::new(ClaudeCodingPlanClient::new(
                    &providers.claude_coding_plan.base_url,
                    token,
                    retry,
                )?),
            )
        }
        _ => Err(ProviderError::request("unsupported providers.active value")),
    }
}

fn configured_provider_client(
    providers: &ProvidersConfig,
    client: Arc<dyn LlmClient>,
) -> Result<Arc<dyn LlmClient>, ProviderError> {
    let model = providers.active_model_registration().ok_or_else(|| {
        ProviderError::request("active provider is missing providers.<active>.models.active")
    })?;
    Ok(Arc::new(ConfiguredLlmClient::new(
        client,
        defaults_from_model(&model),
    )))
}

fn defaults_from_model(model: &ModelRegistrationConfig) -> LlmRequestDefaults {
    LlmRequestDefaults::from_model_kwargs(model.key(), &model.kwargs)
}

fn provider_secret<'a>(
    field: &str,
    value: Option<&'a SecretConfigValue>,
) -> Result<&'a str, ProviderError> {
    match value {
        Some(value) if !value.is_empty() => Ok(value.expose_secret()),
        _ => Err(ProviderError::request(format!("{field} is required"))),
    }
}

fn build_agent_registry(dir: Option<&std::path::Path>) -> Result<AgentRegistry> {
    let Some(dir) = dir else {
        return Ok(AgentRegistryBuilder::new().build());
    };
    if !dir.is_dir() {
        return Ok(AgentRegistryBuilder::new().build());
    }
    let defs = load_agents_tree(dir).context("loading agent profiles")?;
    let mut builder = AgentRegistryBuilder::new();
    for def in defs {
        builder.add(def);
    }
    Ok(builder.build())
}

fn build_skill_registry(root: Option<&std::path::Path>) -> Result<SkillRegistry> {
    match root {
        Some(root) => SkillRegistry::load_from_dir(root).context("loading skills"),
        None => Ok(SkillRegistry::new()),
    }
}

/// Load the externalized tool config. Unlike skills/plugins, the tool config is
/// **mandatory** (the registry needs all tools), so a missing root is an error:
/// inject via [`AgentCoreRuntimeBuilder::tool_config`] or point at a `.eos-agents/tools`
/// tree via [`AgentCoreRuntimeBuilder::tools_root`].
fn build_tool_config(root: Option<&std::path::Path>) -> Result<ToolConfigSet> {
    let root = root.context(
        "tool config root not set: call AgentCoreRuntimeBuilder::tools_root or ::tool_config",
    )?;
    ToolConfigSet::load_from_dir(root).context("loading tool config")
}

/// Validate that every `allowed_tools`/`terminals` entry on every agent profile
/// is a known, registered tool (AC-eos-agent-core-09).
fn validate_agent_tools(agents: &AgentRegistry, registry: &ToolRegistry) -> Result<()> {
    for def in agents.list() {
        for tool in def.allowed_tools.iter().chain(def.terminals.iter()) {
            let known = ToolKey::from_wire(tool).is_some_and(|name| registry.get(name).is_some());
            if !known {
                anyhow::bail!(
                    "agent profile {:?} names unknown tool {:?}",
                    def.name.as_str(),
                    tool
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Build a `ProvidersConfig` (it is `#[non_exhaustive]`, so no struct literal)
    // by deserializing. `active` is serialized from the enum so the test never
    // hardcodes the snake_case wire string.
    fn providers_with(active: ProviderKind, extra: serde_json::Value) -> ProvidersConfig {
        let mut obj = serde_json::Map::new();
        obj.insert(
            "active".to_owned(),
            serde_json::to_value(active).expect("active wire value"),
        );
        if let serde_json::Value::Object(extra) = extra {
            obj.extend(extra);
        }
        serde_json::from_value(serde_json::Value::Object(obj)).expect("providers config")
    }

    // The config layer validates `providers`; this is the runtime's *consumption*
    // of it to build a client — only reached in production, since every runtime
    // test injects a fake LLM client. The provider-secret and active-model gates
    // were untested.
    //
    // `Arc<dyn LlmClient>` is not `Debug`, so `expect_err` is unavailable; match.
    fn client_error(providers: &ProvidersConfig) -> ProviderError {
        match default_llm_client(providers) {
            Err(err) => err,
            Ok(_) => panic!("expected default_llm_client to error"),
        }
    }

    #[test]
    fn default_llm_client_requires_active_provider_secret() {
        let providers = providers_with(ProviderKind::OpenAiApi, serde_json::json!({}));
        let err = client_error(&providers);
        assert!(err.to_string().contains("is required"), "{err}");
    }

    #[test]
    fn default_llm_client_requires_active_model() {
        let providers = providers_with(
            ProviderKind::OpenAiApi,
            serde_json::json!({ "openai_api": { "api_key": "sk-test" } }),
        );
        let err = client_error(&providers);
        assert!(err.to_string().contains("models.active"), "{err}");
    }

    #[test]
    fn default_llm_client_unconfigured_provider_builds_placeholder() {
        let providers = ProvidersConfig::default();
        assert!(matches!(providers.active, ProviderKind::Unconfigured));
        assert!(default_llm_client(&providers).is_ok());
    }
}
