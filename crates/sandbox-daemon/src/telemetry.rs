//! Daemon-owned tracing subscriber setup.

use std::time::Duration;

use opentelemetry::trace::TracerProvider as _;
use opentelemetry::KeyValue;
use opentelemetry_otlp::{Protocol, WithExportConfig};
use opentelemetry_sdk::trace::{BatchConfig, BatchConfigBuilder, BatchSpanProcessor};
use opentelemetry_sdk::{trace::SdkTracerProvider, Resource};
use sandbox_config::configs::validate::{
    require_non_empty, require_u64_at_least, require_usize_at_least, ConfigFieldError,
};
use sandbox_config::{ConfigDocument, ConfigError};
use serde::Deserialize;
use thiserror::Error;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::Layer;

const OTLP_SHUTDOWN_TIMEOUT_MS: u64 = 5_000;
const TELEMETRY_SHUTDOWN_ERROR_MAX_CHARS: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelemetryConfig {
    pub enabled: bool,
    pub service_name: String,
    pub level: String,
    #[serde(default)]
    pub sink: Option<TelemetrySink>,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            service_name: "sandbox-daemon".to_owned(),
            level: "info".to_owned(),
            sink: None,
        }
    }
}

impl TelemetryConfig {
    /// Validate semantic constraints that YAML deserialization cannot express.
    ///
    /// # Errors
    /// Returns an error when the telemetry config is internally inconsistent.
    pub fn validate(&self) -> Result<(), ConfigFieldError> {
        require_non_empty(&self.service_name, "daemon.telemetry.service_name")?;
        validate_telemetry_level(&self.level)?;
        if let Some(sink) = self.sink.as_ref() {
            validate_telemetry_sink(sink)?;
        }
        if self.enabled && self.sink.is_none() {
            return Err(ConfigFieldError::new(
                "daemon.telemetry.sink",
                "enabled telemetry requires exactly one sink",
            ));
        }
        Ok(())
    }

    /// Validate serve-mode constraints for daemon-owned telemetry sinks.
    ///
    /// # Errors
    /// Returns an error when the configured sink cannot run in `mode`.
    pub fn validate_for_serve_mode(&self, mode: DaemonServeMode) -> Result<(), ConfigFieldError> {
        self.validate()?;
        if !self.enabled {
            return Ok(());
        }
        if matches!(
            (mode, &self.sink),
            (
                DaemonServeMode::Spawn,
                Some(TelemetrySink::LocalJson { .. })
            )
        ) {
            return Err(ConfigFieldError::new(
                "daemon.telemetry.sink",
                "local_json stdout/stderr telemetry requires foreground serve mode",
            ));
        }
        Ok(())
    }

    /// Validate serve-mode plus runtime identity requirements.
    ///
    /// # Errors
    /// Returns an error when the configured sink cannot run in `mode`, or when
    /// OTLP mode lacks the dynamic sandbox identity required for resource
    /// attributes.
    pub fn validate_for_daemon_startup(
        &self,
        mode: DaemonServeMode,
        sandbox_id: Option<&str>,
    ) -> Result<(), ConfigFieldError> {
        self.validate_for_serve_mode(mode)?;
        if self.enabled && self.uses_otlp() && !has_dynamic_sandbox_id(sandbox_id) {
            return Err(ConfigFieldError::new(
                "daemon.telemetry.sink",
                "otlp telemetry requires dynamic sandbox_id",
            ));
        }
        Ok(())
    }

    fn uses_otlp(&self) -> bool {
        matches!(self.sink, Some(TelemetrySink::Otlp { .. }))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TelemetrySink {
    LocalJson {
        stream: TelemetryOutputStream,
    },
    Otlp {
        endpoint: String,
        protocol: OtlpProtocol,
        timeout_ms: u64,
        queue_size: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TelemetryOutputStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OtlpProtocol {
    Http,
    Grpc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonServeMode {
    Foreground,
    Spawn,
}

/// Deserialize daemon telemetry from the shared config document.
///
/// # Errors
/// Returns an error when the `daemon` section or nested `telemetry` section
/// cannot be deserialized.
pub fn from_config_document(doc: &ConfigDocument) -> Result<TelemetryConfig, ConfigError> {
    doc.section::<DaemonTelemetrySection>("daemon")
        .map(|section| section.telemetry)
}

/// Install the process-global telemetry subscriber when daemon telemetry is on.
///
/// # Errors
/// Returns an error when the configured level is invalid or a subscriber has
/// already been installed.
pub fn install(
    config: &TelemetryConfig,
    sandbox_id: Option<&str>,
) -> Result<TelemetryGuard, TelemetryInstallError> {
    if !config.enabled {
        return Ok(TelemetryGuard::disabled());
    }
    let level = level_filter(&config.level)?;
    match config.sink.as_ref() {
        Some(TelemetrySink::LocalJson {
            stream: TelemetryOutputStream::Stdout,
        }) => {
            init_json_subscriber(level, std::io::stdout)?;
            Ok(TelemetryGuard::disabled())
        }
        Some(TelemetrySink::LocalJson {
            stream: TelemetryOutputStream::Stderr,
        }) => {
            init_json_subscriber(level, std::io::stderr)?;
            Ok(TelemetryGuard::disabled())
        }
        Some(TelemetrySink::Otlp {
            endpoint,
            protocol,
            timeout_ms,
            queue_size,
        }) => init_otlp_subscriber(
            level,
            &config.service_name,
            endpoint,
            *protocol,
            *timeout_ms,
            *queue_size,
            sandbox_id,
        ),
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

fn init_otlp_subscriber(
    level: LevelFilter,
    service_name: &str,
    endpoint: &str,
    protocol: OtlpProtocol,
    timeout_ms: u64,
    queue_size: usize,
    sandbox_id: Option<&str>,
) -> Result<TelemetryGuard, TelemetryInstallError> {
    if protocol != OtlpProtocol::Http {
        return Err(TelemetryInstallError::UnsupportedOtlpProtocol);
    }
    let sandbox_id = sandbox_id
        .filter(|value| !value.trim().is_empty())
        .ok_or(TelemetryInstallError::MissingSandboxId)?;
    let timeout = Duration::from_millis(timeout_ms);
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(endpoint.to_owned())
        .with_timeout(timeout)
        .with_protocol(Protocol::HttpBinary)
        .build()
        .map_err(|err| TelemetryInstallError::ExporterBuild(err.to_string()))?;
    let processor = BatchSpanProcessor::builder(exporter)
        .with_batch_config(otlp_batch_config(queue_size, timeout))
        .build();
    let provider = SdkTracerProvider::builder()
        .with_resource(otlp_resource(service_name, sandbox_id))
        .with_span_processor(processor)
        .build();
    let tracer = provider.tracer(service_name.to_owned());
    let otel_layer = tracing_opentelemetry::layer()
        .with_tracer(tracer)
        .with_filter(level);
    tracing_subscriber::registry()
        .with(otel_layer)
        .try_init()
        .map_err(|_| TelemetryInstallError::SubscriberAlreadyInstalled)?;
    Ok(TelemetryGuard::new(provider))
}

pub(crate) fn otlp_batch_config(queue_size: usize, scheduled_delay: Duration) -> BatchConfig {
    let limits = otlp_batch_limits(queue_size, scheduled_delay);
    BatchConfigBuilder::default()
        .with_max_queue_size(limits.max_queue_size)
        .with_max_export_batch_size(limits.max_export_batch_size)
        .with_scheduled_delay(limits.scheduled_delay)
        .build()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OtlpBatchLimits {
    pub(crate) max_queue_size: usize,
    pub(crate) max_export_batch_size: usize,
    pub(crate) scheduled_delay: Duration,
}

pub(crate) fn otlp_batch_limits(queue_size: usize, scheduled_delay: Duration) -> OtlpBatchLimits {
    OtlpBatchLimits {
        max_queue_size: queue_size,
        max_export_batch_size: queue_size.clamp(1, 512),
        scheduled_delay,
    }
}

pub(crate) fn otlp_resource(service_name: &str, sandbox_id: &str) -> Resource {
    Resource::builder()
        .with_service_name(service_name.to_owned())
        .with_attributes([
            KeyValue::new("service.instance.id", sandbox_id.to_owned()),
            KeyValue::new("sandbox.id", sandbox_id.to_owned()),
        ])
        .build()
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

fn validate_telemetry_sink(sink: &TelemetrySink) -> Result<(), ConfigFieldError> {
    match sink {
        TelemetrySink::LocalJson { .. } => Ok(()),
        TelemetrySink::Otlp {
            endpoint,
            protocol,
            timeout_ms,
            queue_size,
        } => {
            require_non_empty(endpoint, "daemon.telemetry.sink.endpoint")?;
            require_u64_at_least(*timeout_ms, 1, "daemon.telemetry.sink.timeout_ms")?;
            require_usize_at_least(*queue_size, 1, "daemon.telemetry.sink.queue_size")?;
            if *protocol != OtlpProtocol::Http {
                return Err(ConfigFieldError::new(
                    "daemon.telemetry.sink.protocol",
                    "only http OTLP protocol is supported by this build",
                ));
            }
            Ok(())
        }
    }
}

fn validate_telemetry_level(level: &str) -> Result<(), ConfigFieldError> {
    match level {
        "trace" | "debug" | "info" | "warn" | "error" => Ok(()),
        _ => Err(ConfigFieldError::new(
            "daemon.telemetry.level",
            "must be one of trace, debug, info, warn, error",
        )),
    }
}

fn has_dynamic_sandbox_id(value: Option<&str>) -> bool {
    value.is_some_and(|value| !value.trim().is_empty())
}

pub struct TelemetryGuard {
    provider: Option<SdkTracerProvider>,
    shutdown_timeout: Duration,
}

impl TelemetryGuard {
    fn disabled() -> Self {
        Self {
            provider: None,
            shutdown_timeout: Duration::from_millis(OTLP_SHUTDOWN_TIMEOUT_MS),
        }
    }

    fn new(provider: SdkTracerProvider) -> Self {
        Self {
            provider: Some(provider),
            shutdown_timeout: Duration::from_millis(OTLP_SHUTDOWN_TIMEOUT_MS),
        }
    }

    /// Flush and shut down the daemon-owned telemetry provider.
    ///
    /// # Errors
    /// Returns a bounded shutdown error reported by the OpenTelemetry SDK.
    pub fn shutdown(&mut self) -> Result<(), TelemetryShutdownError> {
        let Some(provider) = self.provider.take() else {
            return Ok(());
        };
        provider
            .shutdown_with_timeout(self.shutdown_timeout)
            .map_err(|err| TelemetryShutdownError::Provider(bounded_shutdown_error(err)))
    }

    #[cfg(test)]
    #[allow(dead_code, reason = "used by path-included daemon integration tests")]
    pub(crate) fn from_provider_for_test(provider: SdkTracerProvider, timeout: Duration) -> Self {
        Self {
            provider: Some(provider),
            shutdown_timeout: timeout,
        }
    }
}

fn bounded_shutdown_error(error: impl std::fmt::Display) -> String {
    let message = error.to_string();
    let mut chars = message.chars();
    let mut bounded = chars
        .by_ref()
        .take(TELEMETRY_SHUTDOWN_ERROR_MAX_CHARS)
        .collect::<String>();
    if chars.next().is_some() {
        bounded.push_str("...");
    }
    bounded
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

#[derive(Debug, Error)]
pub enum TelemetryInstallError {
    #[error("telemetry level is invalid")]
    InvalidLevel,
    #[error("enabled telemetry requires a sink")]
    MissingSink,
    #[error("otlp telemetry requires dynamic sandbox_id")]
    MissingSandboxId,
    #[error("configured OTLP protocol is not supported by this daemon build")]
    UnsupportedOtlpProtocol,
    #[error("failed to build OTLP exporter: {0}")]
    ExporterBuild(String),
    #[error("tracing subscriber is already installed")]
    SubscriberAlreadyInstalled,
}

#[derive(Debug, Error)]
pub enum TelemetryShutdownError {
    #[error("telemetry provider shutdown failed: {0}")]
    Provider(String),
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

#[derive(Deserialize)]
struct DaemonTelemetrySection {
    #[serde(default)]
    telemetry: TelemetryConfig,
}
