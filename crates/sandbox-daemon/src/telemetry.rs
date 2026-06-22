//! Daemon-owned tracing subscriber setup.

use sandbox_runtime_config::configs::daemon::{
    TelemetryConfig, TelemetryOutputStream, TelemetrySink,
};
use thiserror::Error;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::util::SubscriberInitExt;

/// Install the process-global telemetry subscriber when daemon telemetry is on.
///
/// # Errors
/// Returns an error when the configured level is invalid or a subscriber has
/// already been installed.
pub fn install(config: &TelemetryConfig) -> Result<(), TelemetryInstallError> {
    if !config.enabled {
        return Ok(());
    }
    let level = level_filter(&config.level)?;
    match config.sink.as_ref() {
        Some(TelemetrySink::LocalJson {
            stream: TelemetryOutputStream::Stdout,
        }) => init_json_subscriber(level, std::io::stdout),
        Some(TelemetrySink::LocalJson {
            stream: TelemetryOutputStream::Stderr,
        }) => init_json_subscriber(level, std::io::stderr),
        None => Err(TelemetryInstallError::MissingSink),
    }
}

fn init_json_subscriber<W>(level: LevelFilter, writer: W) -> Result<(), TelemetryInstallError>
where
    W: for<'writer> MakeWriter<'writer> + Send + Sync + 'static,
{
    json_subscriber(level, writer)
        .try_init()
        .map_err(|_| TelemetryInstallError::SubscriberAlreadyInstalled)
}

fn json_subscriber<W>(
    level: LevelFilter,
    writer: W,
) -> impl tracing::Subscriber + Send + Sync + 'static
where
    W: for<'writer> MakeWriter<'writer> + Send + Sync + 'static,
{
    tracing_subscriber::fmt()
        .json()
        .with_writer(writer)
        .with_max_level(level)
        .with_current_span(true)
        .with_span_list(true)
        .with_span_events(FmtSpan::CLOSE)
        .finish()
}

fn level_filter(level: &str) -> Result<LevelFilter, TelemetryInstallError> {
    match level {
        "trace" => Ok(LevelFilter::TRACE),
        "debug" => Ok(LevelFilter::DEBUG),
        "info" => Ok(LevelFilter::INFO),
        "warn" => Ok(LevelFilter::WARN),
        "error" => Ok(LevelFilter::ERROR),
        _ => Err(TelemetryInstallError::InvalidLevel),
    }
}

#[derive(Debug, Error)]
pub enum TelemetryInstallError {
    #[error("telemetry level is invalid")]
    InvalidLevel,
    #[error("enabled telemetry requires a sink")]
    MissingSink,
    #[error("tracing subscriber is already installed")]
    SubscriberAlreadyInstalled,
}

#[cfg(test)]
#[allow(dead_code, reason = "used by path-included daemon integration tests")]
pub(crate) fn with_test_json_subscriber<W, T>(
    config: &TelemetryConfig,
    writer: W,
    run: impl FnOnce() -> T,
) -> Result<T, TelemetryInstallError>
where
    W: for<'writer> MakeWriter<'writer> + Send + Sync + 'static,
{
    if !config.enabled {
        return Ok(run());
    }
    let level = level_filter(&config.level)?;
    Ok(tracing::subscriber::with_default(
        json_subscriber(level, writer),
        run,
    ))
}
