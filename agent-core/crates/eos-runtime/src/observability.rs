//! `tracing-subscriber` setup for the binary / sync-wrapper boundary.
//!
//! Library callers must not initialize a global subscriber; only the binary
//! (`main.rs`) or an explicit test wrapper calls [`init_tracing`]. The optional
//! `tokio-console` feature layers `console-subscriber` for stuck-task debugging.

use tracing_subscriber::prelude::*;
use tracing_subscriber::{fmt, EnvFilter};

/// Output format for the text/JSON subscriber.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogFormat {
    /// Human-readable text (default).
    #[default]
    Text,
    /// Newline-delimited JSON.
    Json,
}

/// Initialize the process-global tracing subscriber.
///
/// Reads `RUST_LOG` (falling back to `info`). Idempotent only in the sense that a
/// second call returns an error from the global default already being set; the
/// binary calls it exactly once. The `tokio-console` feature adds the
/// `console_subscriber` layer.
///
/// # Errors
/// Returns an error if a global subscriber was already installed.
pub fn init_tracing(format: LogFormat) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let registry = tracing_subscriber::registry().with(filter);

    #[cfg(feature = "tokio-console")]
    let registry = registry.with(console_subscriber::spawn());

    match format {
        LogFormat::Text => registry.with(fmt::layer()).try_init()?,
        LogFormat::Json => registry.with(fmt::layer().json()).try_init()?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_format_default_is_text() {
        assert_eq!(LogFormat::default(), LogFormat::Text);
    }
}
