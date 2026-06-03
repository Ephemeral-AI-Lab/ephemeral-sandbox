//! The composition-root dependency graph ([`AppState`]) and its
//! [`AppStateBuilder`].
//!
//! Every store and seam is constructed exactly once here (GC-eos-runtime-02):
//! there are no module-level mutable singletons. `AppState` is a cheap-to-clone
//! handle (`Arc` fields) shared into each spawned agent and delegated workflow.

use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};

use anyhow::{Context, Result};
use async_trait::async_trait;
use eos_agent_def::{load_agents_tree, AgentDefinition, AgentRegistry, AgentRegistryBuilder};
use eos_audit::{AuditSink, BufferedAuditShutdown, BufferedJsonlSink, NoopAuditSink};
use eos_config::{CentralConfig, DatabaseUrl};
use eos_db::Database;
use eos_engine::{AdvisorService, EventSource, NotificationService, StreamEvent};
use eos_llm_client::{Auth, LlmClient, LlmRequest, LlmStream, ProviderError};
use eos_plugin_catalog::PluginCatalog;
use eos_sandbox_api::SandboxTransport;
use eos_sandbox_host::{
    DaemonClient, ProviderRegistry, RequestSandboxBinding, RequestSandboxProvisioner,
    SandboxLifecycle, DEFAULT_LAYER_STACK_ROOT,
};
use eos_skills::{load_skill_registry, SkillRegistry};
use eos_state::{
    AgentRunStore, AttemptStore, IterationStore, ModelStore, RequestStore, TaskStore, WorkflowStore,
};
use eos_tools::{
    build_default_registry, AdvisorPort, CallerScope, NotificationSink, ToolName, ToolRegistry,
};
use eos_types::{Clock, RequestId, SystemClock};
use tokio_util::sync::CancellationToken;

/// Per-agent event-source factory seam (replaces `RuntimeConfig.event_source_factory`).
///
/// `None` on [`AppState`] means the live provider stream is used; the mock
/// harness sets it so each spawned agent runs the real loop against a scripted
/// source. Kept as a synchronous composition-root closure (the Python factory is
/// synchronous and returns the trait object directly), not promoted to a named
/// trait — there is no future to erase.
pub type EventSourceFactory = Arc<dyn Fn(&AgentDefinition) -> Arc<dyn EventSource> + Send + Sync>;

/// Per-run stream-event callback (replaces the Python `AgentStreamEmitter`).
pub type EventCallback = Arc<dyn Fn(&StreamEvent) + Send + Sync>;

/// Request-scoped sandbox provisioning seam.
///
/// `eos-sandbox-host` owns the production [`RequestSandboxProvisioner`] over the
/// sealed `ProviderAdapter`/`SandboxLifecycle` seam (a parallel agent moved the
/// work there). Because that adapter is sealed, `eos-runtime` cannot build a mock
/// of it, so this narrow runtime seam exists purely for testability: production
/// wraps the host provisioner; tests inject a fake. Mirrors the Python
/// `RequestSandboxProvisioner` create/start injection.
#[async_trait]
pub trait RequestProvisioner: Send + Sync + std::fmt::Debug {
    /// Resolve the sandbox binding for one request (start an explicit id, or
    /// create a fresh `request-<hex8>` sandbox labelled `origin=workflow`).
    async fn prepare_for_run(
        &self,
        request_id: &RequestId,
        sandbox_id: Option<&str>,
    ) -> Result<RequestSandboxBinding>;
}

/// Production provisioner: wraps the `eos-sandbox-host` provisioner over the real
/// container lifecycle.
#[derive(Debug)]
struct HostProvisioner {
    inner: Arc<RequestSandboxProvisioner>,
}

#[async_trait]
impl RequestProvisioner for HostProvisioner {
    async fn prepare_for_run(
        &self,
        request_id: &RequestId,
        sandbox_id: Option<&str>,
    ) -> Result<RequestSandboxBinding> {
        self.inner
            .prepare_for_run(request_id, sandbox_id)
            .await
            .context("sandbox provisioning failed")
    }
}

/// Placeholder client used when no provider credentials are configured and no
/// `event_source_factory` is set. Streaming always errors; production wires a
/// real provider from env, and tests set `event_source_factory`.
#[derive(Debug, Default)]
struct UnconfiguredLlmClient;

#[async_trait]
impl LlmClient for UnconfiguredLlmClient {
    async fn stream_message(&self, _request: LlmRequest) -> Result<LlmStream, ProviderError> {
        Err(ProviderError::transport(
            "no llm provider configured (set an api key or inject an event_source_factory)",
        ))
    }
}

/// The composition-root dependency graph. Cloning is cheap (every field is an
/// `Arc` or `Clone`-internal handle).
#[derive(Clone)]
#[non_exhaustive]
pub struct AppState {
    pub(crate) config: Arc<CentralConfig>,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) cwd: String,
    pub(crate) repo_root: String,
    pub(crate) task_store: Arc<dyn TaskStore>,
    pub(crate) request_store: Arc<dyn RequestStore>,
    pub(crate) workflow_store: Arc<dyn WorkflowStore>,
    pub(crate) iteration_store: Arc<dyn IterationStore>,
    pub(crate) attempt_store: Arc<dyn AttemptStore>,
    pub(crate) agent_run_store: Arc<dyn AgentRunStore>,
    pub(crate) model_store: Arc<dyn ModelStore>,
    pub(crate) llm_client: Arc<dyn LlmClient>,
    pub(crate) event_source_factory: Option<EventSourceFactory>,
    pub(crate) audit: Arc<dyn AuditSink>,
    pub(crate) audit_shutdown: Arc<StdMutex<Option<BufferedAuditShutdown>>>,
    pub(crate) tool_registry: Arc<ToolRegistry>,
    pub(crate) agent_registry: Arc<AgentRegistry>,
    pub(crate) skill_registry: Arc<SkillRegistry>,
    pub(crate) plugin_catalog: Arc<PluginCatalog>,
    pub(crate) provider_registry: Arc<ProviderRegistry>,
    pub(crate) transport: Arc<dyn SandboxTransport>,
    pub(crate) provisioner: Arc<dyn RequestProvisioner>,
    pub(crate) advisor: Arc<dyn AdvisorPort>,
    pub(crate) notifications: Arc<dyn NotificationSink>,
    pub(crate) shutdown: CancellationToken,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("cwd", &self.cwd)
            .field("repo_root", &self.repo_root)
            .field("tools", &self.tool_registry.len())
            .field("agents", &self.agent_registry.list().count())
            .finish_non_exhaustive()
    }
}

impl AppState {
    /// Start building an `AppState`.
    pub fn builder() -> AppStateBuilder {
        AppStateBuilder::default()
    }

    /// The graceful-shutdown / parent-exit cancellation token.
    #[must_use]
    pub fn shutdown_token(&self) -> CancellationToken {
        self.shutdown.clone()
    }

    /// The shared central configuration.
    #[must_use]
    pub fn config(&self) -> &CentralConfig {
        &self.config
    }

    /// The injected clock (system clock in production, a test clock in tests).
    #[must_use]
    pub fn clock(&self) -> Arc<dyn Clock> {
        self.clock.clone()
    }

    /// The discovered plugin catalog (read-only after build).
    #[must_use]
    pub fn plugin_catalog(&self) -> Arc<PluginCatalog> {
        self.plugin_catalog.clone()
    }

    /// The sandbox provider registry (holds the per-process provider adapters).
    #[must_use]
    pub fn provider_registry(&self) -> Arc<ProviderRegistry> {
        self.provider_registry.clone()
    }

    /// Flush and join the buffered audit writer thread, if any (graceful
    /// shutdown). Idempotent: a second call is a no-op.
    pub fn flush_audit(&self) {
        if let Ok(mut guard) = self.audit_shutdown.lock() {
            if let Some(shutdown) = guard.take() {
                shutdown.shutdown();
            }
        }
    }
}

/// `#[must_use]` builder for [`AppState`]. Every field is an optional override:
/// `None` selects the production default. Tests inject in-memory stores, a mock
/// `event_source_factory`, a fake provisioner, and explicit registries.
#[must_use = "AppStateBuilder does nothing until build() is called"]
#[derive(Default)]
pub struct AppStateBuilder {
    config: Option<CentralConfig>,
    database_url: Option<String>,
    clock: Option<Arc<dyn Clock>>,
    cwd: Option<String>,
    llm_client: Option<Arc<dyn LlmClient>>,
    event_source_factory: Option<EventSourceFactory>,
    audit: Option<Arc<dyn AuditSink>>,
    audit_path: Option<PathBuf>,
    agent_registry: Option<Arc<AgentRegistry>>,
    agents_dir: Option<PathBuf>,
    skill_registry: Option<Arc<SkillRegistry>>,
    skill_root: Option<PathBuf>,
    plugin_catalog: Option<Arc<PluginCatalog>>,
    plugin_root: Option<PathBuf>,
    model_registry_path: Option<PathBuf>,
    provisioner: Option<Arc<dyn RequestProvisioner>>,
    transport: Option<Arc<dyn SandboxTransport>>,
    compatibility_mode: bool,
}

impl std::fmt::Debug for AppStateBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppStateBuilder")
            .field("has_config", &self.config.is_some())
            .field("compatibility_mode", &self.compatibility_mode)
            .finish_non_exhaustive()
    }
}

impl AppStateBuilder {
    /// Use an explicit central config (else [`CentralConfig::default`]).
    pub fn config(mut self, config: CentralConfig) -> Self {
        self.config = Some(config);
        self
    }

    /// Override the database URL (test seam; a network URL makes `build()` fail
    /// fast).
    pub fn database_url(mut self, url: impl Into<String>) -> Self {
        self.database_url = Some(url.into());
        self
    }

    /// Inject a clock (system clock by default).
    pub fn clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = Some(clock);
        self
    }

    /// Set the working directory (process cwd by default).
    pub fn cwd(mut self, cwd: impl Into<String>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// Inject an LLM client (an unconfigured placeholder by default).
    pub fn llm_client(mut self, client: Arc<dyn LlmClient>) -> Self {
        self.llm_client = Some(client);
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

    /// Inject a prebuilt plugin catalog (else discover under `plugin_root`, else empty).
    pub fn plugin_catalog(mut self, catalog: Arc<PluginCatalog>) -> Self {
        self.plugin_catalog = Some(catalog);
        self
    }

    /// Discover plugins under this catalog root.
    pub fn plugin_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.plugin_root = Some(root.into());
        self
    }

    /// Seed the model registry from this JSON file (missing file is non-fatal).
    pub fn model_registry_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.model_registry_path = Some(path.into());
        self
    }

    /// Inject a request provisioner (a host-backed provisioner by default).
    pub fn provisioner(mut self, provisioner: Arc<dyn RequestProvisioner>) -> Self {
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
    /// URL), construct every store and seam, optionally seed the model registry,
    /// and validate agent profile tool names against the registry.
    ///
    /// # Errors
    /// Returns an error if the DB URL is non-local, the pool/migrations fail, a
    /// configured agent/skill/plugin root cannot be loaded, or (without
    /// compatibility mode) an agent names an unknown tool.
    pub async fn build(self) -> Result<AppState> {
        let config = self.config.unwrap_or_default();

        // Database: a network URL fails fast at parse (GC: SQLite-only).
        // `DatabaseConfig` is `#[non_exhaustive]`, so override the url by mutation
        // rather than struct-update syntax.
        let mut db_config = config.database.clone();
        if let Some(url) = self.database_url {
            db_config.url =
                DatabaseUrl::parse(url).context("database url is not a local sqlite url")?;
        }
        let database = Database::open(&db_config)
            .await
            .context("opening the sqlite database")?;

        let cwd = self
            .cwd
            .or_else(|| {
                std::env::current_dir()
                    .ok()
                    .map(|p| p.display().to_string())
            })
            .unwrap_or_default();
        let repo_root = cwd.clone();

        // Optional model-registry seed (GC-eos-runtime-04: missing JSON is
        // non-fatal — seed_from_json returns Ok(0) for a missing file).
        let model_path = self.model_registry_path.clone().or_else(|| {
            let candidate = PathBuf::from(&repo_root)
                .join("models")
                .join("registry.json");
            candidate.is_file().then_some(candidate)
        });
        if let Some(path) = &model_path {
            match database
                .model_registry()
                .seed_from_json(&path.display().to_string())
                .await
            {
                Ok(count) => tracing::info!(models = count, "seeded model registry"),
                Err(err) => {
                    tracing::warn!(error = %err, "model registry seed skipped (non-fatal)");
                }
            }
        }

        let clock: Arc<dyn Clock> = self.clock.unwrap_or_else(|| Arc::new(SystemClock));

        let llm_client: Arc<dyn LlmClient> = self
            .llm_client
            .unwrap_or_else(|| default_llm_client(&config));

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

        let plugin_catalog = match self.plugin_catalog {
            Some(catalog) => catalog,
            None => Arc::new(build_plugin_catalog(self.plugin_root.as_deref())?),
        };

        let tool_registry = Arc::new(build_default_registry(&CallerScope::default()));

        // Cross-registry validation: unknown agent tool names fail fast unless
        // compatibility mode is enabled (anchor §10 / AC-eos-runtime-09).
        if !self.compatibility_mode {
            validate_agent_tools(&agent_registry, &tool_registry)?;
        }

        let provider_registry = Arc::new(ProviderRegistry::new());
        let transport: Arc<dyn SandboxTransport> = self
            .transport
            .unwrap_or_else(|| Arc::new(DaemonClient::new(provider_registry.clone())));

        let provisioner: Arc<dyn RequestProvisioner> = self.provisioner.unwrap_or_else(|| {
            let lifecycle = SandboxLifecycle::new(
                Arc::new(DaemonClient::new(provider_registry.clone())),
                PathBuf::from(DEFAULT_LAYER_STACK_ROOT),
            );
            Arc::new(HostProvisioner {
                inner: Arc::new(RequestSandboxProvisioner::new(Arc::new(lifecycle))),
            })
        });

        Ok(AppState {
            config: Arc::new(config),
            clock,
            cwd,
            repo_root,
            task_store: database.tasks(),
            request_store: database.requests(),
            workflow_store: database.workflows(),
            iteration_store: database.iterations(),
            attempt_store: database.attempts(),
            agent_run_store: database.agent_runs(),
            model_store: database.models(),
            llm_client,
            event_source_factory: self.event_source_factory,
            audit,
            audit_shutdown: Arc::new(StdMutex::new(audit_shutdown)),
            tool_registry,
            agent_registry,
            skill_registry,
            plugin_catalog,
            provider_registry,
            transport,
            provisioner,
            advisor: Arc::new(AdvisorService),
            notifications: Arc::new(NotificationService::new()),
            shutdown: CancellationToken::new(),
        })
    }
}

/// Build the LLM client from env credentials, falling back to an unconfigured
/// placeholder (Phase-6 tests inject an `event_source_factory`; real provider
/// selection is a cutover concern).
fn default_llm_client(config: &CentralConfig) -> Arc<dyn LlmClient> {
    use eos_llm_client::{AnthropicClient, OpenAiClient};
    let retry = Arc::new(config.providers.retry.clone());
    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        if let Ok(client) =
            AnthropicClient::new("https://api.anthropic.com", Auth::api_key(key), retry)
        {
            return Arc::new(client);
        }
    } else if let Ok(key) = std::env::var("OPENAI_API_KEY") {
        if let Ok(client) = OpenAiClient::new("https://api.openai.com", Auth::bearer(key), retry) {
            return Arc::new(client);
        }
    }
    Arc::new(UnconfiguredLlmClient)
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
        Some(root) => load_skill_registry(root).context("loading skills"),
        None => Ok(SkillRegistry::new()),
    }
}

fn build_plugin_catalog(root: Option<&std::path::Path>) -> Result<PluginCatalog> {
    // A missing root yields an empty catalog (the loader's empty-vs-RootNotDir
    // split), so the `None` default points at a path that does not exist.
    let root = root.unwrap_or_else(|| std::path::Path::new("/nonexistent-eos-plugin-root"));
    PluginCatalog::discover_under(root).context("discovering plugins")
}

/// Validate that every `allowed_tools`/`terminals` entry on every agent profile
/// is a known, registered tool (AC-eos-runtime-09).
fn validate_agent_tools(agents: &AgentRegistry, registry: &ToolRegistry) -> Result<()> {
    for def in agents.list() {
        for tool in def.allowed_tools.iter().chain(def.terminals.iter()) {
            let known = ToolName::from_wire(tool).is_some_and(|name| registry.get(name).is_some());
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
pub(crate) mod test_seams {
    //! Shared test fakes for the runtime ACs (`#[cfg(test)]` only).

    use std::num::NonZeroU32;
    use std::sync::Arc;

    use async_trait::async_trait;
    use eos_agent_def::{AgentDefinition, AgentName, AgentRole, AgentType};
    use eos_engine::{EngineError, EngineStream, EventSource, StreamEvent};
    use eos_llm_client::LlmRequest;
    use eos_sandbox_api::{DaemonOp, SandboxApiError, SandboxTransport};
    use eos_sandbox_host::RequestSandboxBinding;
    use eos_types::{JsonObject, RequestId, SandboxId};

    use super::{EventSourceFactory, RequestProvisioner};

    /// A sandbox transport that returns an empty payload for every op (so
    /// `command_session_count` resolves to 0) — keeps the no-inflight hook happy
    /// without a live daemon.
    #[derive(Debug, Default)]
    pub(crate) struct FakeTransport;

    #[async_trait]
    impl SandboxTransport for FakeTransport {
        async fn call(
            &self,
            _sandbox_id: &SandboxId,
            _op: DaemonOp,
            _payload: JsonObject,
            _timeout_s: u32,
        ) -> Result<JsonObject, SandboxApiError> {
            Ok(JsonObject::new())
        }
    }

    /// A scripted event source: each `stream()` call replays the next queued
    /// turn. When `block_when_empty` is set, an exhausted source blocks forever
    /// instead of returning an empty turn (keeps the agent "running").
    #[derive(Debug)]
    pub(crate) struct ScriptedSource {
        turns: tokio::sync::Mutex<Vec<Vec<StreamEvent>>>,
        block_when_empty: bool,
    }

    impl ScriptedSource {
        pub(crate) fn new(turns: Vec<Vec<StreamEvent>>) -> Self {
            Self {
                turns: tokio::sync::Mutex::new(turns),
                block_when_empty: false,
            }
        }

        pub(crate) fn new_blocking(turns: Vec<Vec<StreamEvent>>) -> Self {
            Self {
                turns: tokio::sync::Mutex::new(turns),
                block_when_empty: true,
            }
        }
    }

    #[async_trait]
    impl EventSource for ScriptedSource {
        async fn stream(&self, _request: &LlmRequest) -> Result<EngineStream, EngineError> {
            let mut turns = self.turns.lock().await;
            if turns.is_empty() {
                if self.block_when_empty {
                    drop(turns);
                    std::future::pending::<()>().await;
                    unreachable!("pending future never resolves");
                }
                return Ok(Box::pin(futures::stream::iter(Vec::new())));
            }
            let events = turns.remove(0);
            Ok(Box::pin(futures::stream::iter(events.into_iter().map(Ok))))
        }
    }

    /// An event source whose `stream()` never resolves; used to hold a root agent
    /// open so a test can abort the spawned task (join-error path, AC-03b).
    #[derive(Debug)]
    pub(crate) struct BlockingSource;

    #[async_trait]
    impl EventSource for BlockingSource {
        async fn stream(&self, _request: &LlmRequest) -> Result<EngineStream, EngineError> {
            std::future::pending::<()>().await;
            unreachable!("pending future never resolves")
        }
    }

    /// A provisioner that binds a fixed sandbox id without touching Docker.
    #[derive(Debug)]
    pub(crate) struct FakeProvisioner {
        pub(crate) id: String,
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

    /// Build a minimal agent definition for tests.
    pub(crate) fn agent_def(
        name: &str,
        role: AgentRole,
        allowed: &[&str],
        terminals: &[&str],
    ) -> AgentDefinition {
        AgentDefinition {
            name: AgentName::new(name).expect("name"),
            description: name.to_owned(),
            system_prompt: Some("test profile".to_owned()),
            model: Some("test-model".to_owned()),
            tool_call_limit: NonZeroU32::new(8).expect("nonzero"),
            role,
            agent_type: AgentType::Agent,
            allowed_tools: allowed.iter().map(|s| (*s).to_owned()).collect(),
            terminals: terminals.iter().map(|s| (*s).to_owned()).collect(),
            notification_triggers: Vec::new(),
            skill: None,
            context_recipe: None,
        }
    }

    /// An event-source factory that always returns the given scripted turns.
    pub(crate) fn factory_from(turns: Vec<Vec<StreamEvent>>) -> EventSourceFactory {
        Arc::new(move |_def: &AgentDefinition| {
            Arc::new(ScriptedSource::new(turns.clone())) as Arc<dyn EventSource>
        })
    }

    /// A factory where the `root` agent plays `root_turns` then blocks (stays
    /// running), and every other agent gets an empty source (errors on first
    /// turn). Used by the delegation test (AC-05).
    pub(crate) fn factory_root_blocks_after(
        root_turns: Vec<Vec<StreamEvent>>,
    ) -> EventSourceFactory {
        Arc::new(move |def: &AgentDefinition| {
            if def.name.as_str() == "root" {
                Arc::new(ScriptedSource::new_blocking(root_turns.clone())) as Arc<dyn EventSource>
            } else {
                Arc::new(ScriptedSource::new(Vec::new())) as Arc<dyn EventSource>
            }
        })
    }

    /// One model turn that calls `tool_name` with `input`.
    pub(crate) fn tool_use_turn(
        tool_use_id: &str,
        tool_name: &str,
        input: serde_json::Value,
    ) -> Vec<StreamEvent> {
        use eos_engine::AssistantMessageComplete;
        use eos_llm_client::{ContentBlock, Message, MessageRole, UsageSnapshot};

        let input = match input {
            serde_json::Value::Object(map) => map,
            _ => eos_types::JsonObject::new(),
        };
        vec![StreamEvent::AssistantMessageComplete {
            agent_name: String::new(),
            agent_run_id: None,
            payload: Box::new(AssistantMessageComplete {
                message: Message {
                    role: MessageRole::Assistant,
                    content: vec![ContentBlock::ToolUse {
                        tool_use_id: tool_use_id.parse().expect("tool use id"),
                        name: tool_name.to_owned(),
                        input,
                    }],
                },
                usage: UsageSnapshot::default(),
                stop_reason: None,
            }),
        }]
    }

    /// Build a fully-wired test `AppState` over a temp `SQLite` db, a fake
    /// provisioner, the given agent registry, and an optional event-source
    /// factory. Returns the state and the owning temp dir (keep it alive).
    pub(crate) async fn build_test_state(
        factory: Option<EventSourceFactory>,
        agents: Vec<AgentDefinition>,
    ) -> (super::AppState, tempfile::TempDir) {
        use eos_agent_def::AgentRegistry;

        let dir = tempfile::tempdir().expect("tempdir");
        let url = format!("sqlite://{}", dir.path().join("test.db").display());
        let registry: AgentRegistry = agents.into_iter().collect();
        let mut builder = super::AppState::builder()
            .database_url(url)
            .cwd(dir.path().display().to_string())
            .provisioner(Arc::new(FakeProvisioner {
                id: "sb-test".to_owned(),
            }))
            .transport(Arc::new(FakeTransport))
            .agent_registry(Arc::new(registry));
        if let Some(factory) = factory {
            builder = builder.event_source_factory(factory);
        }
        let state = builder.build().await.expect("build app state");
        (state, dir)
    }
}
