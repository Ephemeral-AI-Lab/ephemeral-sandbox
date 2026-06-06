//! Runtime service construction.
//!
//! Every store and seam is constructed exactly once here (GC-eos-runtime-02):
//! there are no module-level mutable singletons.

use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};

use anyhow::{Context, Result};
use async_trait::async_trait;
use eos_agent_def::{load_agents_tree, AgentRegistry, AgentRegistryBuilder};
use eos_audit::{AuditSink, BufferedAuditShutdown, BufferedJsonlSink, NoopAuditSink};
use eos_config::{
    DatabaseConfig, DatabaseUrl, ModelsConfig, ProviderKind, ProvidersConfig, SecretConfigValue,
    WorkflowConfig,
};
use eos_db::Database;
use eos_llm_client::{Auth, LlmClient, LlmRequest, LlmStream, ProviderError};
use eos_sandbox_api::SandboxTransport;
use eos_sandbox_host::{
    resolve_provider_kind, DaemonClient, DockerProviderAdapter, ProviderRegistry,
    RequestProvisioner, RequestSandboxProvisioner, SandboxLifecycle,
};
use eos_skills::SkillRegistry;
use eos_tools::{
    build_default_registry, CallerScope, SandboxToolService, ToolConfigSet, ToolKey, ToolRegistry,
};

use super::{
    AgentCoreRegistryService, AuditService, DbStoreService, EngineService, EventSourceFactory,
    RuntimeServices, SandboxService,
};
use crate::plugin_tools::register_plugin_tools;

/// Placeholder client used when no provider is selected and no
/// `event_source_factory` is set. Streaming always errors; production wires a
/// real provider from `providers.active`, and tests set `event_source_factory`.
#[derive(Debug, Default)]
struct UnconfiguredLlmClient;

#[async_trait]
impl LlmClient for UnconfiguredLlmClient {
    async fn stream_message(&self, _request: LlmRequest) -> Result<LlmStream, ProviderError> {
        Err(ProviderError::transport(
            "no llm provider configured (set providers.active in local.yml or inject an event_source_factory)",
        ))
    }
}

/// `#[must_use]` builder for [`RuntimeServices`]. Every field is an optional override:
/// `None` selects the production default. Tests inject in-memory stores, a mock
/// `event_source_factory`, a fake provisioner, and explicit registries.
#[must_use = "RuntimeServicesBuilder does nothing until build() is called"]
#[derive(Default)]
pub struct RuntimeServicesBuilder {
    database_url: Option<String>,
    workspace_root: Option<String>,
    llm_client: Option<Arc<dyn LlmClient>>,
    models_config: Option<ModelsConfig>,
    providers_config: Option<ProvidersConfig>,
    workflow_config: Option<WorkflowConfig>,
    event_source_factory: Option<EventSourceFactory>,
    audit: Option<Arc<dyn AuditSink>>,
    audit_path: Option<PathBuf>,
    agent_registry: Option<Arc<AgentRegistry>>,
    agents_dir: Option<PathBuf>,
    tool_config: Option<Arc<ToolConfigSet>>,
    tools_root: Option<PathBuf>,
    skill_registry: Option<Arc<SkillRegistry>>,
    skill_root: Option<PathBuf>,
    provisioner: Option<Arc<dyn RequestProvisioner>>,
    transport: Option<Arc<dyn SandboxTransport>>,
    compatibility_mode: bool,
}

impl std::fmt::Debug for RuntimeServicesBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeServicesBuilder")
            .field("compatibility_mode", &self.compatibility_mode)
            .finish_non_exhaustive()
    }
}

impl RuntimeServicesBuilder {
    /// Override the database URL (test seam; a network URL makes `build()` fail
    /// fast).
    pub fn database_url(mut self, url: impl Into<String>) -> Self {
        self.database_url = Some(url.into());
        self
    }

    /// Set the workspace root used for build-time defaults.
    pub fn workspace_root(mut self, workspace_root: impl Into<String>) -> Self {
        self.workspace_root = Some(workspace_root.into());
        self
    }

    /// Inject an LLM client (an unconfigured placeholder by default).
    pub fn llm_client(mut self, client: Arc<dyn LlmClient>) -> Self {
        self.llm_client = Some(client);
        self
    }

    /// Inject model registry config.
    pub fn models_config(mut self, config: ModelsConfig) -> Self {
        self.models_config = Some(config);
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

    /// Inject the per-agent event-source factory (mock harness).
    pub fn event_source_factory(mut self, factory: EventSourceFactory) -> Self {
        self.event_source_factory = Some(factory);
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

    /// Inject a request provisioner (a host-backed provisioner by default).
    #[cfg(test)]
    pub(crate) fn provisioner(mut self, provisioner: Arc<dyn RequestProvisioner>) -> Self {
        self.provisioner = Some(provisioner);
        self
    }

    /// Inject the sandbox transport (a daemon client over the provider registry
    /// by default). Tests inject a fake transport to avoid a live daemon.
    pub fn transport(mut self, transport: Arc<dyn SandboxTransport>) -> Self {
        self.transport = Some(transport);
        self
    }

    /// Allow agent profiles to name tools absent from the registry (skip-unknown
    /// compatibility instead of failing startup).
    pub fn compatibility_mode(mut self, enabled: bool) -> Self {
        self.compatibility_mode = enabled;
        self
    }

    /// Construct the runtime graph: build the `SQLite` pool (fail fast on a network
    /// URL), construct every store and seam, apply the model registry config,
    /// and validate agent profile tool names against the registry.
    ///
    /// # Errors
    /// Returns an error if the DB URL is non-local, the pool/migrations fail, a
    /// configured agent/skill/plugin root cannot be loaded, or (without
    /// compatibility mode) an agent names an unknown tool.
    pub async fn build(self) -> Result<RuntimeServices> {
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

        let workspace_root = self
            .workspace_root
            .or_else(|| {
                std::env::current_dir()
                    .ok()
                    .map(|p| p.display().to_string())
            })
            .unwrap_or_default();

        let config_doc = eos_config::load().context("loading runtime config")?;
        let models_config = match self.models_config {
            Some(config) => config,
            None => config_doc
                .section::<ModelsConfig>("models")
                .context("loading models config")?,
        };
        models_config
            .validate()
            .context("validating models config")?;
        match database
            .model_registry()
            .sync_from_config(&models_config)
            .await
        {
            Ok(count) => tracing::info!(models = count, "applied model registry config"),
            Err(err) => {
                tracing::warn!(error = %err, "model registry config skipped (non-fatal)");
            }
        }

        let workflow_config = match self.workflow_config {
            Some(config) => config,
            None => config_doc
                .section::<WorkflowConfig>("workflow")
                .context("loading workflow config")?,
        };
        workflow_config
            .validate()
            .context("validating workflow config")?;

        let llm_client: Arc<dyn LlmClient> = match self.llm_client {
            Some(client) => client,
            None => {
                let providers_config = match self.providers_config {
                    Some(config) => config,
                    None => config_doc
                        .section::<ProvidersConfig>("providers")
                        .context("loading providers config")?,
                };
                providers_config
                    .validate()
                    .context("validating providers config")?;
                default_llm_client(&providers_config).context("configuring llm provider")?
            }
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

        let needs_host_provider = self.transport.is_none() || self.provisioner.is_none();
        let provider_registry = Arc::new(ProviderRegistry::new());
        if needs_host_provider {
            seed_default_sandbox_provider(&provider_registry)?;
        }
        let daemon_client = Arc::new(DaemonClient::new(provider_registry));
        let transport: Arc<dyn SandboxTransport> =
            self.transport.unwrap_or_else(|| daemon_client.clone());
        let mut tool_registry = build_default_registry(&tool_config, &CallerScope::default());
        register_plugin_tools(
            &mut tool_registry,
            &SandboxToolService::new(transport.clone()),
        );

        // Cross-registry validation: unknown agent tool names fail fast unless
        // compatibility mode is enabled (anchor §10 / AC-eos-runtime-09).
        if !self.compatibility_mode {
            validate_agent_tools(&agent_registry, &tool_registry)?;
        }

        let eosd_artifact_dir = default_eosd_artifact_dir(&workspace_root);

        let provisioner: Arc<dyn RequestProvisioner> = self.provisioner.unwrap_or_else(|| {
            let lifecycle = SandboxLifecycle::new(daemon_client.clone(), eosd_artifact_dir);
            Arc::new(RequestSandboxProvisioner::with_default_snapshot(
                Arc::new(lifecycle),
                // No host-side default snapshot: a fresh sandbox uses the
                // provider default unless a request supplies an explicit id.
                None,
            ))
        });

        Ok(RuntimeServices {
            db: DbStoreService {
                task_store: database.tasks(),
                request_store: database.requests(),
                workflow_store: database.workflows(),
                iteration_store: database.iterations(),
                attempt_store: database.attempts(),
                agent_run_store: database.agent_runs(),
                model_store: database.models(),
            },
            agent_core: AgentCoreRegistryService {
                agent_registry,
                skill_registry,
                tool_config,
            },
            engine: EngineService {
                llm_client,
                event_source_factory: self.event_source_factory,
            },
            sandbox: SandboxService {
                transport,
                provisioner,
            },
            audit: AuditService {
                sink: audit,
                shutdown: Arc::new(StdMutex::new(audit_shutdown)),
            },
        })
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
            Ok(Arc::new(OpenAiApiClient::new(
                &providers.openai_api.base_url,
                Auth::bearer(key),
                retry,
            )?))
        }
        ProviderKind::AnthropicApi => {
            let key = provider_secret(
                "providers.anthropic_api.api_key",
                providers.anthropic_api.api_key.as_ref(),
            )?;
            Ok(Arc::new(AnthropicApiClient::new(
                &providers.anthropic_api.base_url,
                Auth::api_key(key),
                retry,
            )?))
        }
        ProviderKind::CodexCodingPlan => {
            let token = provider_secret(
                "providers.codex_coding_plan.access_token",
                providers.codex_coding_plan.access_token.as_ref(),
            )?;
            let auth = Auth::codex_access_token_from_jwt(token)?;
            Ok(Arc::new(CodexCodingPlanClient::new(
                &providers.codex_coding_plan.base_url,
                auth,
                retry,
            )?))
        }
        ProviderKind::ClaudeCodingPlan => {
            let token = provider_secret(
                "providers.claude_coding_plan.access_token",
                providers.claude_coding_plan.access_token.as_ref(),
            )?;
            Ok(Arc::new(ClaudeCodingPlanClient::new(
                &providers.claude_coding_plan.base_url,
                token,
                retry,
            )?))
        }
        _ => Err(ProviderError::request("unsupported providers.active value")),
    }
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

fn seed_default_sandbox_provider(registry: &ProviderRegistry) -> Result<()> {
    let provider_kind = resolve_provider_kind();
    let docker = DockerProviderAdapter::connect().context("connecting docker sandbox provider")?;
    registry.set_default(Arc::new(docker));
    tracing::info!(
        sandbox_provider = provider_kind.as_str(),
        "sandbox provider configured"
    );
    Ok(())
}

fn default_eosd_artifact_dir(workspace_root: &str) -> PathBuf {
    PathBuf::from(workspace_root).join("sandbox").join("dist")
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
/// inject via [`RuntimeServicesBuilder::tool_config`] or point at a `.eos-agents/tools`
/// tree via [`RuntimeServicesBuilder::tools_root`].
fn build_tool_config(root: Option<&std::path::Path>) -> Result<ToolConfigSet> {
    let root = root.context(
        "tool config root not set: call RuntimeServicesBuilder::tools_root or ::tool_config",
    )?;
    ToolConfigSet::load_from_dir(root).context("loading tool config")
}

/// Validate that every `allowed_tools`/`terminals` entry on every agent profile
/// is a known, registered tool (AC-eos-runtime-09).
fn validate_agent_tools(agents: &AgentRegistry, registry: &ToolRegistry) -> Result<()> {
    for def in agents.list() {
        for tool in def.allowed_tools.iter().chain(def.terminals.iter()) {
            let known = ToolKey::from_wire(tool).is_some_and(|name| registry.get(name).is_some());
            if !known {
                anyhow::bail!(
                    "agent profile {:?} names unknown tool {:?}; enable compatibility mode to skip",
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
    // Pure-logic unit test for a module-private fn (no reusable doubles defined,
    // so I2-permitted inline). The shared Layer-A doubles moved to `eos-testkit`
    // (`TESTING_SPEC` §7); the behavior tests live in `tests/unit/mod.rs` and pull
    // those doubles from the `eos-testkit` dev-dep plus the local `support`
    // module.
    #[test]
    fn eosd_artifact_dir_is_repo_sandbox_dist() {
        assert_eq!(
            super::default_eosd_artifact_dir("/repo"),
            std::path::PathBuf::from("/repo/sandbox/dist")
        );
    }
}
