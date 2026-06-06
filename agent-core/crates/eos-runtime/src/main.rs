//! eos-runtime binary entry point.
//!
//! Thin by design (`proj-lib-main-split`): it initializes tracing, constructs the
//! single multi-thread Tokio runtime, builds the [`RuntimeServices`] graph, and — when a
//! prompt argument is given — starts one request and awaits it. All logic lives
//! in the library.
#![forbid(unsafe_code)]

use eos_config::WorkflowConfig;
use eos_runtime::observability::{init_tracing, LogFormat};
use eos_runtime::{run_request, RequestRunInput, RuntimeServices};

fn main() -> anyhow::Result<()> {
    init_tracing(LogFormat::Text).map_err(|err| anyhow::anyhow!(err.to_string()))?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    runtime.block_on(async {
        let services = build_runtime_services().await?;
        tracing::info!("eos-runtime services constructed");

        if let Some(prompt) = std::env::args().nth(1) {
            let request_id = eos_types::RequestId::new_v4();
            let workspace_root = std::env::current_dir()?.display().to_string();
            let workflow_config = load_workflow_config()?;
            let input =
                RequestRunInput::new(request_id.clone(), prompt, workspace_root, workflow_config);
            let outcome = run_request(&services, input, None).await?;
            tracing::info!(
                request_id = %request_id,
                status = ?outcome.status,
                "request finished"
            );
        }
        services.flush_audit();
        anyhow::Ok(())
    })
}

/// Repo-relative agent-profile tree used as the default registry source for the
/// shipped binary. The canonical bundle lives at `.eos-agents/` (profiles under
/// `profile/`, their coupled skills under `skills/`).
const DEFAULT_AGENTS_DIR: &str = ".eos-agents/profile";

/// Repo-relative externalized tool-config tree (`.eos-agents/tools/*.md`), the
/// default source for the shipped binary. `EOS_TOOLS_DIR` overrides it.
const DEFAULT_TOOLS_DIR: &str = ".eos-agents/tools";

/// Build runtime services, seeding the agent registry so `root` resolves
/// (`request_completion` NF1 — the binary otherwise ships with an empty registry
/// and fails every request at root resolution).
///
/// `EOS_AGENTS_DIR` overrides the source and is validated normally. Otherwise we
/// fall back to the repo-relative bundled tree when present (it only resolves
/// when run from the repo root; a missing dir yields an empty registry, the
/// prior no-op behavior). Bundled profiles are validated normally; dynamic
/// plugin tools such as `lsp.*` are registered through the Rust plugin facade.
async fn build_runtime_services() -> anyhow::Result<RuntimeServices> {
    let mut builder = RuntimeServices::builder();
    if let Ok(dir) = std::env::var("EOS_AGENTS_DIR") {
        builder = builder.agents_dir(dir);
    } else if std::path::Path::new(DEFAULT_AGENTS_DIR).is_dir() {
        builder = builder.agents_dir(DEFAULT_AGENTS_DIR);
    }
    // The tool config is mandatory (the registry needs every tool). `EOS_TOOLS_DIR`
    // overrides; otherwise use the repo-relative bundled tree (resolves when run
    // from the repo root). A missing/invalid tree fails the build loudly.
    let tools_dir = std::env::var("EOS_TOOLS_DIR").unwrap_or_else(|_| DEFAULT_TOOLS_DIR.to_owned());
    builder = builder.tools_root(tools_dir);
    builder.build().await
}

fn load_workflow_config() -> anyhow::Result<WorkflowConfig> {
    let config = eos_config::load()?;
    let workflow = config.section::<WorkflowConfig>("workflow")?;
    workflow.validate()?;
    Ok(workflow)
}
