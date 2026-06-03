//! eos-runtime binary entry point.
//!
//! Thin by design (`proj-lib-main-split`): it initializes tracing, constructs the
//! single multi-thread Tokio runtime, builds the [`AppState`] graph, and — when a
//! prompt argument is given — starts one request and awaits it. All logic lives
//! in the library.
#![forbid(unsafe_code)]

use eos_runtime::observability::{init_tracing, LogFormat};
use eos_runtime::{start_request, AppState};

fn main() -> anyhow::Result<()> {
    init_tracing(LogFormat::Text).map_err(|err| anyhow::anyhow!(err.to_string()))?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    runtime.block_on(async {
        let state = AppState::builder().build().await?;
        tracing::info!("eos-runtime app state constructed");

        if let Some(prompt) = std::env::args().nth(1) {
            let handle = start_request(&state, prompt, None, None).await?;
            tracing::info!(request_id = %handle.request_id, "request started");
            handle.join().await;
        }
        state.flush_audit();
        anyhow::Ok(())
    })
}
