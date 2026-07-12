use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, File, Metadata};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Deserializer, Serialize};
use tokio_util::sync::CancellationToken;

pub const MIN_RESOURCE_INTERVAL_MS: u64 = 20;
pub const MAX_RESOURCE_INTERVAL_MS: u64 = 1_000;
pub const DEFAULT_RESOURCE_INTERVAL_MS: u64 = 100;
pub const LIVE_RESOURCE_INTERVAL_MS: u64 = 500;

const MAX_PROBE_BYTES: u64 = 64 * 1024;
const MAX_FIXED_PROBE_BYTES: u64 = 16 * 1024;
const FIXED_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const FIXED_PROBE_POLL_INTERVAL: Duration = Duration::from_millis(5);
const MAX_VOLUME_ENTRIES: u64 = 1_000_000;
const MAX_VOLUME_WALK_TIME: Duration = Duration::from_secs(30);

const SOURCE_CGROUP: &str = "cgroup_v2";
const SOURCE_CGROUP_MEMORY_CURRENT: &str = "cgroup_v2.memory.current";
const SOURCE_CGROUP_MEMORY_PEAK: &str = "cgroup_v2.memory.peak";
const SOURCE_CGROUP_MEMORY_PEAK_SAMPLED: &str = "cgroup_v2.memory.current.sampled_peak";
const SOURCE_CGROUP_CPU: &str = "cgroup_v2.cpu.stat";
const SOURCE_CGROUP_IO: &str = "cgroup_v2.io.stat";
#[cfg(target_os = "linux")]
const SOURCE_PROCFS: &str = "linux.procfs";
const SOURCE_MACOS_PS: &str = "macos.ps";
const SOURCE_FILESYSTEM: &str = "filesystem.metadata";
const SOURCE_HOST_VOLUME: &str = "host_volume.df_posix";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricUnit {
    Bytes,
    BytesPerSecond,
    Nanoseconds,
    OperationsPerSecond,
    Count,
    Ratio,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricScope {
    Operation,
    HostVolume,
    Runner,
    Daemon,
    Sandbox,
    Workspace,
    Layerstack,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricKind {
    Gauge,
    MonotonicCounter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AggregationRule {
    Maximum,
    Minimum,
    Mean,
    Delta,
    Integral,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AvailabilityPolicy {
    ExplicitUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricDirection {
    LowerIsPreferred,
    HigherIsPreferred,
    DescriptiveOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MetricDefinition {
    pub id: &'static str,
    pub semantic_revision: u32,
    pub unit: MetricUnit,
    pub scope: MetricScope,
    pub kind: MetricKind,
    pub availability: AvailabilityPolicy,
    pub aggregation: AggregationRule,
    pub direction: MetricDirection,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "availability", rename_all = "snake_case", deny_unknown_fields)]
pub enum Availability<T> {
    Available { value: T },
    Unavailable { source: String, reason: String },
}

impl<T> Availability<T> {
    #[must_use]
    pub fn map<U>(self, map: impl FnOnce(T) -> U) -> Availability<U> {
        match self {
            Self::Available { value } => Availability::Available { value: map(value) },
            Self::Unavailable { source, reason } => Availability::Unavailable { source, reason },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceReading {
    pub schema_version: u32,
    pub metric_id: String,
    pub metric_semantic_revision: u32,
    pub unit: MetricUnit,
    pub scope: MetricScope,
    pub kind: MetricKind,
    pub aggregation: AggregationRule,
    pub source: String,
    pub monotonic_offset_ns: u64,
    #[serde(default, skip_serializing_if = "is_false")]
    pub sampled: bool,
    pub value: Availability<f64>,
}

const fn metric(
    id: &'static str,
    unit: MetricUnit,
    scope: MetricScope,
    kind: MetricKind,
    aggregation: AggregationRule,
    direction: MetricDirection,
) -> MetricDefinition {
    MetricDefinition {
        id,
        semantic_revision: 1,
        unit,
        scope,
        kind,
        availability: AvailabilityPolicy::ExplicitUnavailable,
        aggregation,
        direction,
    }
}

pub const RUNNER_RSS: MetricDefinition = metric(
    "runner_rss_bytes",
    MetricUnit::Bytes,
    MetricScope::Runner,
    MetricKind::Gauge,
    AggregationRule::Maximum,
    MetricDirection::DescriptiveOnly,
);
pub const DAEMON_RSS: MetricDefinition = metric(
    "daemon_rss_bytes",
    MetricUnit::Bytes,
    MetricScope::Daemon,
    MetricKind::Gauge,
    AggregationRule::Maximum,
    MetricDirection::LowerIsPreferred,
);
pub const DAEMON_CPU_TIME: MetricDefinition = metric(
    "daemon_cpu_time_ns",
    MetricUnit::Nanoseconds,
    MetricScope::Daemon,
    MetricKind::MonotonicCounter,
    AggregationRule::Delta,
    MetricDirection::LowerIsPreferred,
);
pub const SANDBOX_MEMORY_CURRENT: MetricDefinition = metric(
    "sandbox_memory_current_bytes",
    MetricUnit::Bytes,
    MetricScope::Sandbox,
    MetricKind::Gauge,
    AggregationRule::Maximum,
    MetricDirection::LowerIsPreferred,
);
pub const SANDBOX_MEMORY_PEAK: MetricDefinition = metric(
    "sandbox_memory_peak_bytes",
    MetricUnit::Bytes,
    MetricScope::Sandbox,
    MetricKind::Gauge,
    AggregationRule::Maximum,
    MetricDirection::LowerIsPreferred,
);
pub const SANDBOX_CPU_TIME: MetricDefinition = metric(
    "sandbox_cpu_time_ns",
    MetricUnit::Nanoseconds,
    MetricScope::Sandbox,
    MetricKind::MonotonicCounter,
    AggregationRule::Delta,
    MetricDirection::LowerIsPreferred,
);
pub const SANDBOX_BLOCK_READ: MetricDefinition = metric(
    "sandbox_block_read_bytes",
    MetricUnit::Bytes,
    MetricScope::Sandbox,
    MetricKind::MonotonicCounter,
    AggregationRule::Delta,
    MetricDirection::DescriptiveOnly,
);
pub const SANDBOX_BLOCK_WRITE: MetricDefinition = metric(
    "sandbox_block_write_bytes",
    MetricUnit::Bytes,
    MetricScope::Sandbox,
    MetricKind::MonotonicCounter,
    AggregationRule::Delta,
    MetricDirection::DescriptiveOnly,
);
pub const WORKSPACE_LOGICAL_BYTES: MetricDefinition = metric(
    "workspace_logical_bytes",
    MetricUnit::Bytes,
    MetricScope::Workspace,
    MetricKind::Gauge,
    AggregationRule::Maximum,
    MetricDirection::DescriptiveOnly,
);
pub const WORKSPACE_ALLOCATED_BYTES: MetricDefinition = metric(
    "workspace_allocated_bytes",
    MetricUnit::Bytes,
    MetricScope::Workspace,
    MetricKind::Gauge,
    AggregationRule::Maximum,
    MetricDirection::LowerIsPreferred,
);
pub const WORKSPACE_FILE_COUNT: MetricDefinition = metric(
    "workspace_file_count",
    MetricUnit::Count,
    MetricScope::Workspace,
    MetricKind::Gauge,
    AggregationRule::Maximum,
    MetricDirection::DescriptiveOnly,
);
pub const LAYERSTACK_BYTES: MetricDefinition = metric(
    "layerstack_bytes",
    MetricUnit::Bytes,
    MetricScope::Layerstack,
    MetricKind::Gauge,
    AggregationRule::Maximum,
    MetricDirection::LowerIsPreferred,
);
pub const UPPERDIR_BYTES: MetricDefinition = metric(
    "upperdir_bytes",
    MetricUnit::Bytes,
    MetricScope::Layerstack,
    MetricKind::Gauge,
    AggregationRule::Maximum,
    MetricDirection::LowerIsPreferred,
);
pub const HOST_FREE_BYTES: MetricDefinition = metric(
    "host_free_bytes",
    MetricUnit::Bytes,
    MetricScope::HostVolume,
    MetricKind::Gauge,
    AggregationRule::Minimum,
    MetricDirection::HigherIsPreferred,
);

pub const METRICS: &[MetricDefinition] = &[
    RUNNER_RSS,
    DAEMON_RSS,
    DAEMON_CPU_TIME,
    SANDBOX_MEMORY_CURRENT,
    SANDBOX_MEMORY_PEAK,
    SANDBOX_CPU_TIME,
    SANDBOX_BLOCK_READ,
    SANDBOX_BLOCK_WRITE,
    WORKSPACE_LOGICAL_BYTES,
    WORKSPACE_ALLOCATED_BYTES,
    WORKSPACE_FILE_COUNT,
    LAYERSTACK_BYTES,
    UPPERDIR_BYTES,
    HOST_FREE_BYTES,
];

const RUNNER_DEFINITIONS: &[MetricDefinition] = &[RUNNER_RSS];
const DAEMON_DEFINITIONS: &[MetricDefinition] = &[DAEMON_RSS, DAEMON_CPU_TIME];
const CGROUP_DEFINITIONS: &[MetricDefinition] = &[
    SANDBOX_MEMORY_CURRENT,
    SANDBOX_MEMORY_PEAK,
    SANDBOX_CPU_TIME,
    SANDBOX_BLOCK_READ,
    SANDBOX_BLOCK_WRITE,
];
const WORKSPACE_DEFINITIONS: &[MetricDefinition] = &[
    WORKSPACE_LOGICAL_BYTES,
    WORKSPACE_ALLOCATED_BYTES,
    WORKSPACE_FILE_COUNT,
];
const LAYERSTACK_DEFINITIONS: &[MetricDefinition] = &[LAYERSTACK_BYTES];
const UPPERDIR_DEFINITIONS: &[MetricDefinition] = &[UPPERDIR_BYTES];
const HOST_VOLUME_DEFINITIONS: &[MetricDefinition] = &[HOST_FREE_BYTES];

#[must_use]
pub fn metric_definition(id: &str) -> Option<&'static MetricDefinition> {
    METRICS.iter().find(|definition| definition.id == id)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MonotonicInstant(u64);

impl MonotonicInstant {
    #[must_use]
    pub const fn from_offset_ns(offset_ns: u64) -> Self {
        Self(offset_ns)
    }

    #[must_use]
    pub const fn offset_ns(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone)]
pub struct MonotonicClock {
    origin: Instant,
}

impl MonotonicClock {
    #[must_use]
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }

    #[must_use]
    pub fn now(&self) -> MonotonicInstant {
        let elapsed = self.origin.elapsed().as_nanos();
        MonotonicInstant(u64::try_from(elapsed).unwrap_or(u64::MAX))
    }
}

impl Default for MonotonicClock {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SamplingIntervalError {
    requested_ms: u64,
}

impl SamplingIntervalError {
    #[must_use]
    pub const fn requested_ms(self) -> u64 {
        self.requested_ms
    }
}

impl fmt::Display for SamplingIntervalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "resource sampling interval {} ms is outside {MIN_RESOURCE_INTERVAL_MS}..={MAX_RESOURCE_INTERVAL_MS} ms",
            self.requested_ms
        )
    }
}

impl std::error::Error for SamplingIntervalError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct SamplingInterval(u64);

impl SamplingInterval {
    pub const DEFAULT: Self = Self(DEFAULT_RESOURCE_INTERVAL_MS);
    pub const LIVE: Self = Self(LIVE_RESOURCE_INTERVAL_MS);

    pub const fn from_millis(milliseconds: u64) -> Result<Self, SamplingIntervalError> {
        if milliseconds < MIN_RESOURCE_INTERVAL_MS || milliseconds > MAX_RESOURCE_INTERVAL_MS {
            Err(SamplingIntervalError {
                requested_ms: milliseconds,
            })
        } else {
            Ok(Self(milliseconds))
        }
    }

    #[must_use]
    pub const fn as_millis(self) -> u64 {
        self.0
    }

    #[must_use]
    pub const fn as_duration(self) -> Duration {
        Duration::from_millis(self.0)
    }
}

impl<'de> Deserialize<'de> for SamplingInterval {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let milliseconds = u64::deserialize(deserializer)?;
        Self::from_millis(milliseconds).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SamplingWindowError;

impl fmt::Display for SamplingWindowError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("sampling window ends before it starts")
    }
}

impl std::error::Error for SamplingWindowError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SamplingWindow {
    start: MonotonicInstant,
    end: MonotonicInstant,
}

impl SamplingWindow {
    pub const fn new(
        start: MonotonicInstant,
        end: MonotonicInstant,
    ) -> Result<Self, SamplingWindowError> {
        if end.0 < start.0 {
            Err(SamplingWindowError)
        } else {
            Ok(Self { start, end })
        }
    }

    #[must_use]
    pub const fn duration_ns(self) -> u64 {
        self.end.0 - self.start.0
    }

    #[must_use]
    pub const fn start(self) -> MonotonicInstant {
        self.start
    }

    #[must_use]
    pub const fn end(self) -> MonotonicInstant {
        self.end
    }
}

impl<'de> Deserialize<'de> for SamplingWindow {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct SerializedWindow {
            start: MonotonicInstant,
            end: MonotonicInstant,
        }

        let window = SerializedWindow::deserialize(deserializer)?;
        Self::new(window.start, window.end).map_err(serde::de::Error::custom)
    }
}

pub trait MetricCollector: Send {
    fn definitions(&self) -> &'static [MetricDefinition];
    fn read(&mut self, at: MonotonicInstant) -> Vec<ResourceReading>;
}

/// A bounded, statically typed sampling task shared by the production
/// scheduler and lifecycle acceptance tests. The collector and monotonic
/// clock are monomorphized at the call site; this is not a runtime plugin or
/// service-locator seam.
pub struct MetricSamplingTask {
    stop: CancellationToken,
    task: tokio::task::JoinHandle<Vec<ResourceReading>>,
}

impl MetricSamplingTask {
    #[must_use]
    pub fn start<C, N>(interval: SamplingInterval, mut collector: C, now: N) -> Self
    where
        C: MetricCollector + 'static,
        N: Fn() -> MonotonicInstant + Send + 'static,
    {
        let stop = CancellationToken::new();
        let sampler_stop = stop.clone();
        let task = tokio::task::spawn_blocking(move || {
            let mut readings = Vec::new();
            loop {
                let mut sample = collector.read(now());
                for reading in &mut sample {
                    reading.sampled = true;
                }
                readings.extend(sample);
                if sampler_stop.is_cancelled() {
                    break;
                }
                thread::sleep(interval.as_duration());
            }
            readings
        });
        Self { stop, task }
    }

    pub async fn finish(self) -> Result<Vec<ResourceReading>, tokio::task::JoinError> {
        self.stop.cancel();
        self.task.await
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessScope {
    Runner,
    Daemon,
}

impl ProcessScope {
    const fn definitions(self) -> &'static [MetricDefinition] {
        match self {
            Self::Runner => RUNNER_DEFINITIONS,
            Self::Daemon => DAEMON_DEFINITIONS,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProcessCollector {
    scope: ProcessScope,
    pid: u32,
    expected_start_identity: Availability<String>,
}

impl ProcessCollector {
    #[must_use]
    pub fn attach(scope: ProcessScope, pid: u32) -> Self {
        let expected_start_identity = read_process_snapshot(pid).map(|snapshot| snapshot.identity);
        Self {
            scope,
            pid,
            expected_start_identity,
        }
    }

    #[must_use]
    pub const fn pid(&self) -> u32 {
        self.pid
    }

    #[must_use]
    pub const fn scope(&self) -> ProcessScope {
        self.scope
    }

    #[must_use]
    pub const fn expected_start_identity(&self) -> &Availability<String> {
        &self.expected_start_identity
    }
}

impl MetricCollector for ProcessCollector {
    fn definitions(&self) -> &'static [MetricDefinition] {
        self.scope.definitions()
    }

    fn read(&mut self, at: MonotonicInstant) -> Vec<ResourceReading> {
        let snapshot = match (
            &self.expected_start_identity,
            read_process_snapshot(self.pid),
        ) {
            (Availability::Unavailable { source, reason }, _) => {
                return unavailable_readings(
                    self.definitions(),
                    at,
                    source,
                    &format!("start_identity_unavailable:{reason}"),
                );
            }
            (_, Availability::Unavailable { source, reason }) => {
                return unavailable_readings(self.definitions(), at, &source, &reason);
            }
            (
                Availability::Available { value: expected },
                Availability::Available { value: snapshot },
            ) if expected == &snapshot.identity => snapshot,
            (Availability::Available { .. }, Availability::Available { .. }) => {
                return unavailable_readings(
                    self.definitions(),
                    at,
                    process_source(),
                    "pid_start_identity_changed",
                );
            }
        };

        match self.scope {
            ProcessScope::Runner => vec![reading(
                &RUNNER_RSS,
                at,
                process_source(),
                false,
                snapshot.rss_bytes,
            )],
            ProcessScope::Daemon => vec![
                reading(&DAEMON_RSS, at, process_source(), false, snapshot.rss_bytes),
                reading(
                    &DAEMON_CPU_TIME,
                    at,
                    process_source(),
                    false,
                    snapshot.cpu_time_ns,
                ),
            ],
        }
    }
}

#[derive(Debug, Clone)]
struct ProcessSnapshot {
    identity: String,
    rss_bytes: Availability<u64>,
    cpu_time_ns: Availability<u64>,
}

#[cfg(target_os = "linux")]
fn process_source() -> &'static str {
    SOURCE_PROCFS
}

#[cfg(target_os = "macos")]
fn process_source() -> &'static str {
    SOURCE_MACOS_PS
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_source() -> &'static str {
    "process_probe"
}

#[cfg(target_os = "linux")]
fn read_process_snapshot(pid: u32) -> Availability<ProcessSnapshot> {
    let root = PathBuf::from("/proc").join(pid.to_string());
    let identity = match read_bounded_text(&root.join("stat"), SOURCE_PROCFS).and_then(|text| {
        parse_proc_start_identity(&text).map_err(|reason| failure(SOURCE_PROCFS, reason))
    }) {
        Ok(identity) => identity,
        Err(error) => return error.unavailable(),
    };
    let rss_bytes = probe_text(&root.join("status"), SOURCE_PROCFS, parse_proc_rss_bytes);
    let cpu_time_ns = probe_text(
        &root.join("schedstat"),
        SOURCE_PROCFS,
        parse_schedstat_cpu_ns,
    );
    Availability::Available {
        value: ProcessSnapshot {
            identity,
            rss_bytes,
            cpu_time_ns,
        },
    }
}

#[cfg(target_os = "macos")]
fn read_process_snapshot(pid: u32) -> Availability<ProcessSnapshot> {
    let pid = pid.to_string();
    let args = [
        OsStr::new("-o"),
        OsStr::new("rss="),
        OsStr::new("-o"),
        OsStr::new("time="),
        OsStr::new("-o"),
        OsStr::new("lstart="),
        OsStr::new("-p"),
        OsStr::new(&pid),
    ];
    match run_fixed_probe(Path::new("/bin/ps"), &args).and_then(|output| {
        parse_ps_snapshot(&output).map_err(|reason| failure(SOURCE_MACOS_PS, reason))
    }) {
        Ok(snapshot) => Availability::Available { value: snapshot },
        Err(error) => error.unavailable(),
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn read_process_snapshot(_pid: u32) -> Availability<ProcessSnapshot> {
    unavailable("process_probe", "platform_unsupported")
}

#[derive(Debug, Clone)]
pub struct CgroupCollector {
    root: CanonicalRoot,
    sampled_current_peak: Option<u64>,
}

impl CgroupCollector {
    #[must_use]
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: CanonicalRoot::new(root.as_ref(), SOURCE_CGROUP),
            sampled_current_peak: None,
        }
    }
}

impl MetricCollector for CgroupCollector {
    fn definitions(&self) -> &'static [MetricDefinition] {
        CGROUP_DEFINITIONS
    }

    fn read(&mut self, at: MonotonicInstant) -> Vec<ResourceReading> {
        let root = match self.root.checked() {
            Ok(root) => root,
            Err(error) => {
                return unavailable_readings(self.definitions(), at, &error.source, &error.reason);
            }
        };

        let memory_current = probe_text(
            &root.join("memory.current"),
            SOURCE_CGROUP_MEMORY_CURRENT,
            parse_single_u64,
        );
        if let Availability::Available { value } = memory_current {
            self.sampled_current_peak = Some(
                self.sampled_current_peak
                    .map_or(value, |peak| peak.max(value)),
            );
        }
        let native_peak = probe_text(
            &root.join("memory.peak"),
            SOURCE_CGROUP_MEMORY_PEAK,
            parse_single_u64,
        );
        let (memory_peak, memory_peak_source, sampled_peak) = match native_peak {
            Availability::Available { value } => (
                Availability::Available { value },
                SOURCE_CGROUP_MEMORY_PEAK,
                false,
            ),
            unavailable_peak => match self.sampled_current_peak {
                Some(value) => (
                    Availability::Available { value },
                    SOURCE_CGROUP_MEMORY_PEAK_SAMPLED,
                    true,
                ),
                None => (unavailable_peak, SOURCE_CGROUP_MEMORY_PEAK, false),
            },
        };
        let cpu_time = probe_text(&root.join("cpu.stat"), SOURCE_CGROUP_CPU, parse_cpu_stat_ns);
        let io_text = read_bounded_text(&root.join("io.stat"), SOURCE_CGROUP_IO);
        let block_read = probe_loaded_text(&io_text, SOURCE_CGROUP_IO, |text| {
            parse_io_stat_bytes(text, "rbytes")
        });
        let block_write = probe_loaded_text(&io_text, SOURCE_CGROUP_IO, |text| {
            parse_io_stat_bytes(text, "wbytes")
        });

        vec![
            reading(
                &SANDBOX_MEMORY_CURRENT,
                at,
                SOURCE_CGROUP_MEMORY_CURRENT,
                false,
                memory_current,
            ),
            reading(
                &SANDBOX_MEMORY_PEAK,
                at,
                memory_peak_source,
                sampled_peak,
                memory_peak,
            ),
            reading(&SANDBOX_CPU_TIME, at, SOURCE_CGROUP_CPU, false, cpu_time),
            reading(&SANDBOX_BLOCK_READ, at, SOURCE_CGROUP_IO, false, block_read),
            reading(
                &SANDBOX_BLOCK_WRITE,
                at,
                SOURCE_CGROUP_IO,
                false,
                block_write,
            ),
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeScope {
    Workspace,
    Layerstack,
    Upperdir,
}

impl VolumeScope {
    const fn definitions(self) -> &'static [MetricDefinition] {
        match self {
            Self::Workspace => WORKSPACE_DEFINITIONS,
            Self::Layerstack => LAYERSTACK_DEFINITIONS,
            Self::Upperdir => UPPERDIR_DEFINITIONS,
        }
    }

    const fn source(self) -> &'static str {
        match self {
            Self::Workspace => "filesystem.workspace",
            Self::Layerstack => "filesystem.layerstack",
            Self::Upperdir => "filesystem.upperdir",
        }
    }
}

#[derive(Debug, Clone)]
pub struct VolumeCollector {
    scope: VolumeScope,
    root: CanonicalRoot,
}

impl VolumeCollector {
    #[must_use]
    pub fn new(scope: VolumeScope, root: impl AsRef<Path>) -> Self {
        Self {
            scope,
            root: CanonicalRoot::new(root.as_ref(), scope.source()),
        }
    }

    #[must_use]
    pub const fn scope(&self) -> VolumeScope {
        self.scope
    }
}

impl MetricCollector for VolumeCollector {
    fn definitions(&self) -> &'static [MetricDefinition] {
        self.scope.definitions()
    }

    fn read(&mut self, at: MonotonicInstant) -> Vec<ResourceReading> {
        let root = match self.root.checked() {
            Ok(root) => root,
            Err(error) => {
                return unavailable_readings(self.definitions(), at, &error.source, &error.reason);
            }
        };
        let usage = match walk_volume(root) {
            Ok(usage) => usage,
            Err(error) => {
                return unavailable_readings(self.definitions(), at, &error.source, &error.reason);
            }
        };

        match self.scope {
            VolumeScope::Workspace => vec![
                reading(
                    &WORKSPACE_LOGICAL_BYTES,
                    at,
                    SOURCE_FILESYSTEM,
                    false,
                    Availability::Available {
                        value: usage.logical_bytes,
                    },
                ),
                reading(
                    &WORKSPACE_ALLOCATED_BYTES,
                    at,
                    SOURCE_FILESYSTEM,
                    false,
                    usage.allocated_bytes,
                ),
                reading(
                    &WORKSPACE_FILE_COUNT,
                    at,
                    SOURCE_FILESYSTEM,
                    false,
                    Availability::Available {
                        value: usage.file_count,
                    },
                ),
            ],
            VolumeScope::Layerstack => vec![reading(
                &LAYERSTACK_BYTES,
                at,
                SOURCE_FILESYSTEM,
                false,
                usage.allocated_bytes,
            )],
            VolumeScope::Upperdir => vec![reading(
                &UPPERDIR_BYTES,
                at,
                SOURCE_FILESYSTEM,
                false,
                usage.allocated_bytes,
            )],
        }
    }
}

#[derive(Debug, Clone)]
pub struct HostVolumeCollector {
    root: CanonicalRoot,
}

impl HostVolumeCollector {
    #[must_use]
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: CanonicalRoot::new(root.as_ref(), SOURCE_HOST_VOLUME),
        }
    }
}

impl MetricCollector for HostVolumeCollector {
    fn definitions(&self) -> &'static [MetricDefinition] {
        HOST_VOLUME_DEFINITIONS
    }

    fn read(&mut self, at: MonotonicInstant) -> Vec<ResourceReading> {
        let value = match self.root.checked() {
            Ok(root) => read_host_free_bytes(root),
            Err(error) => error.unavailable(),
        };
        vec![reading(
            &HOST_FREE_BYTES,
            at,
            SOURCE_HOST_VOLUME,
            false,
            value,
        )]
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn read_host_free_bytes(root: &Path) -> Availability<u64> {
    let args = [OsStr::new("-Pk"), root.as_os_str()];
    match run_fixed_probe(Path::new("/bin/df"), &args).and_then(|output| {
        parse_df_available_bytes(&output).map_err(|reason| failure(SOURCE_HOST_VOLUME, reason))
    }) {
        Ok(value) => Availability::Available { value },
        Err(error) => error.unavailable(),
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn read_host_free_bytes(_root: &Path) -> Availability<u64> {
    unavailable(SOURCE_HOST_VOLUME, "platform_unsupported")
}

#[must_use]
pub fn counter_delta(
    before: &Availability<u64>,
    after: &Availability<u64>,
    source: &str,
) -> Availability<u64> {
    match (before, after) {
        (Availability::Available { value: before }, Availability::Available { value: after })
            if after >= before =>
        {
            Availability::Available {
                value: after - before,
            }
        }
        (Availability::Available { .. }, Availability::Available { .. }) => {
            unavailable(source, "counter_reset_or_regression")
        }
        (Availability::Unavailable { source, reason }, _) => Availability::Unavailable {
            source: source.clone(),
            reason: format!("baseline_unavailable:{reason}"),
        },
        (_, Availability::Unavailable { source, reason }) => Availability::Unavailable {
            source: source.clone(),
            reason: format!("final_unavailable:{reason}"),
        },
    }
}

fn reading(
    definition: &MetricDefinition,
    at: MonotonicInstant,
    source: &str,
    sampled: bool,
    value: Availability<u64>,
) -> ResourceReading {
    ResourceReading {
        schema_version: 1,
        metric_id: definition.id.to_owned(),
        metric_semantic_revision: definition.semantic_revision,
        unit: definition.unit,
        scope: definition.scope,
        kind: definition.kind,
        aggregation: definition.aggregation,
        source: source.to_owned(),
        monotonic_offset_ns: at.offset_ns(),
        sampled,
        value: value.map(|value| value as f64),
    }
}

fn unavailable_readings(
    definitions: &'static [MetricDefinition],
    at: MonotonicInstant,
    source: &str,
    reason: &str,
) -> Vec<ResourceReading> {
    definitions
        .iter()
        .map(|definition| reading(definition, at, source, false, unavailable(source, reason)))
        .collect()
}

const fn is_false(value: &bool) -> bool {
    !*value
}

fn unavailable<T>(source: &str, reason: &str) -> Availability<T> {
    debug_assert!(!source.is_empty());
    debug_assert!(!reason.is_empty());
    Availability::Unavailable {
        source: source.to_owned(),
        reason: reason.to_owned(),
    }
}

#[derive(Debug, Clone)]
struct ProbeFailure {
    source: String,
    reason: String,
}

impl ProbeFailure {
    fn unavailable<T>(self) -> Availability<T> {
        unavailable(&self.source, &self.reason)
    }
}

fn failure(source: &str, reason: impl Into<String>) -> ProbeFailure {
    ProbeFailure {
        source: source.to_owned(),
        reason: reason.into(),
    }
}

#[derive(Debug, Clone)]
struct CanonicalRoot {
    configured: PathBuf,
    canonical: Result<PathBuf, ProbeFailure>,
    source: &'static str,
}

impl CanonicalRoot {
    fn new(path: &Path, source: &'static str) -> Self {
        let configured = absolute_path(path).unwrap_or_else(|_| path.to_path_buf());
        let canonical = canonical_directory(&configured, source);
        Self {
            configured,
            canonical,
            source,
        }
    }

    fn checked(&self) -> Result<&Path, ProbeFailure> {
        let expected = self.canonical.as_ref().map_err(Clone::clone)?;
        let current = canonical_directory(&self.configured, self.source)?;
        if current != *expected {
            return Err(failure(self.source, "root_identity_changed"));
        }
        Ok(expected)
    }
}

fn absolute_path(path: &Path) -> io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        std::env::current_dir().map(|directory| directory.join(path))
    }
}

fn canonical_directory(path: &Path, source: &str) -> Result<PathBuf, ProbeFailure> {
    let canonical = fs::canonicalize(path)
        .map_err(|error| failure(source, format!("root_{}", io_error_reason(&error))))?;
    let metadata = fs::metadata(&canonical)
        .map_err(|error| failure(source, format!("root_{}", io_error_reason(&error))))?;
    if !metadata.is_dir() {
        return Err(failure(source, "root_not_directory"));
    }
    Ok(canonical)
}

fn read_bounded_text(path: &Path, source: &str) -> Result<String, ProbeFailure> {
    let file = File::open(path)
        .map_err(|error| failure(source, format!("probe_{}", io_error_reason(&error))))?;
    let mut bytes = Vec::new();
    file.take(MAX_PROBE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| failure(source, format!("probe_{}", io_error_reason(&error))))?;
    if bytes.len() as u64 > MAX_PROBE_BYTES {
        return Err(failure(source, "probe_output_limit_exceeded"));
    }
    String::from_utf8(bytes).map_err(|_| failure(source, "probe_output_not_utf8"))
}

fn probe_text(
    path: &Path,
    source: &str,
    parser: impl FnOnce(&str) -> Result<u64, &'static str>,
) -> Availability<u64> {
    match read_bounded_text(path, source)
        .and_then(|text| parser(&text).map_err(|reason| failure(source, reason)))
    {
        Ok(value) => Availability::Available { value },
        Err(error) => error.unavailable(),
    }
}

fn probe_loaded_text(
    text: &Result<String, ProbeFailure>,
    source: &str,
    parser: impl FnOnce(&str) -> Result<u64, &'static str>,
) -> Availability<u64> {
    match text {
        Ok(text) => match parser(text) {
            Ok(value) => Availability::Available { value },
            Err(reason) => unavailable(source, reason),
        },
        Err(error) => unavailable(&error.source, &error.reason),
    }
}

fn parse_single_u64(text: &str) -> Result<u64, &'static str> {
    let mut fields = text.split_whitespace();
    let value = fields.next().ok_or("counter_missing")?;
    if fields.next().is_some() {
        return Err("counter_malformed");
    }
    value.parse().map_err(|_| "counter_malformed")
}

fn parse_cpu_stat_ns(text: &str) -> Result<u64, &'static str> {
    if let Some(value) = parse_keyed_u64(text, "usage_nsec")? {
        return Ok(value);
    }
    let microseconds = parse_keyed_u64(text, "usage_usec")?.ok_or("counter_missing")?;
    microseconds.checked_mul(1_000).ok_or("counter_overflow")
}

fn parse_keyed_u64(text: &str, key: &str) -> Result<Option<u64>, &'static str> {
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        if fields.next() == Some(key) {
            let value = fields.next().ok_or("counter_malformed")?;
            if fields.next().is_some() {
                return Err("counter_malformed");
            }
            return value.parse().map(Some).map_err(|_| "counter_malformed");
        }
    }
    Ok(None)
}

fn parse_io_stat_bytes(text: &str, key: &str) -> Result<u64, &'static str> {
    let mut found = false;
    let mut total = 0_u64;
    for field in text.split_whitespace() {
        let Some((name, value)) = field.split_once('=') else {
            continue;
        };
        if name != key {
            continue;
        }
        found = true;
        let value = value.parse::<u64>().map_err(|_| "counter_malformed")?;
        total = total.checked_add(value).ok_or("counter_overflow")?;
    }
    if found {
        Ok(total)
    } else {
        Err("counter_missing")
    }
}

#[cfg(any(target_os = "linux", test))]
fn parse_proc_start_identity(text: &str) -> Result<String, &'static str> {
    let close = text.rfind(')').ok_or("process_identity_malformed")?;
    let tail = text.get(close + 1..).ok_or("process_identity_malformed")?;
    let start_ticks = tail
        .split_whitespace()
        .nth(19)
        .ok_or("process_identity_malformed")?;
    start_ticks
        .parse::<u64>()
        .map_err(|_| "process_identity_malformed")?;
    Ok(format!("linux_start_ticks:{start_ticks}"))
}

#[cfg(any(target_os = "linux", test))]
fn parse_proc_rss_bytes(text: &str) -> Result<u64, &'static str> {
    let line = text
        .lines()
        .find(|line| line.starts_with("VmRSS:"))
        .ok_or("rss_counter_missing")?;
    let mut fields = line.split_whitespace();
    if fields.next() != Some("VmRSS:") {
        return Err("rss_counter_malformed");
    }
    let kibibytes = fields
        .next()
        .ok_or("rss_counter_malformed")?
        .parse::<u64>()
        .map_err(|_| "rss_counter_malformed")?;
    if fields.next() != Some("kB") || fields.next().is_some() {
        return Err("rss_counter_malformed");
    }
    kibibytes.checked_mul(1_024).ok_or("counter_overflow")
}

#[cfg(any(target_os = "linux", test))]
fn parse_schedstat_cpu_ns(text: &str) -> Result<u64, &'static str> {
    text.split_whitespace()
        .next()
        .ok_or("cpu_counter_missing")?
        .parse()
        .map_err(|_| "cpu_counter_malformed")
}

#[cfg(any(target_os = "macos", test))]
fn parse_ps_snapshot(output: &[u8]) -> Result<ProcessSnapshot, &'static str> {
    let text = std::str::from_utf8(output).map_err(|_| "probe_output_not_utf8")?;
    let line = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .ok_or("process_not_found")?;
    let mut fields = line.split_whitespace();
    let rss_kib = fields
        .next()
        .ok_or("process_probe_malformed")?
        .parse::<u64>()
        .map_err(|_| "process_probe_malformed")?;
    let cpu = fields.next().ok_or("process_probe_malformed")?;
    let start_fields = fields.collect::<Vec<_>>();
    if start_fields.len() != 5 {
        return Err("process_probe_malformed");
    }
    let rss_bytes = rss_kib.checked_mul(1_024).ok_or("counter_overflow")?;
    let cpu_time_ns = parse_ps_cpu_time_ns(cpu)?;
    Ok(ProcessSnapshot {
        identity: format!("macos_lstart:{}", start_fields.join(" ")),
        rss_bytes: Availability::Available { value: rss_bytes },
        cpu_time_ns: Availability::Available { value: cpu_time_ns },
    })
}

#[cfg(any(target_os = "macos", test))]
fn parse_ps_cpu_time_ns(value: &str) -> Result<u64, &'static str> {
    let (prefix, seconds) = value.rsplit_once(':').ok_or("cpu_counter_malformed")?;
    let (whole_seconds, fraction_ns) = parse_decimal_seconds(seconds)?;
    let mut prefix_fields = prefix.rsplit(':');
    let minutes = parse_time_component(prefix_fields.next())?;
    let hours_with_days = prefix_fields.next();
    if prefix_fields.next().is_some() {
        return Err("cpu_counter_malformed");
    }
    let (days, hours) = match hours_with_days {
        None => (0, 0),
        Some(hours) => match hours.split_once('-') {
            Some((days, hours)) => (
                parse_time_component(Some(days))?,
                parse_time_component(Some(hours))?,
            ),
            None => (0, parse_time_component(Some(hours))?),
        },
    };
    let total_seconds = days
        .checked_mul(24)
        .and_then(|value| value.checked_add(hours))
        .and_then(|value| value.checked_mul(60))
        .and_then(|value| value.checked_add(minutes))
        .and_then(|value| value.checked_mul(60))
        .and_then(|value| value.checked_add(whole_seconds))
        .ok_or("counter_overflow")?;
    total_seconds
        .checked_mul(1_000_000_000)
        .and_then(|value| value.checked_add(fraction_ns))
        .ok_or("counter_overflow")
}

#[cfg(any(target_os = "macos", test))]
fn parse_time_component(value: Option<&str>) -> Result<u64, &'static str> {
    value
        .ok_or("cpu_counter_malformed")?
        .parse()
        .map_err(|_| "cpu_counter_malformed")
}

#[cfg(any(target_os = "macos", test))]
fn parse_decimal_seconds(value: &str) -> Result<(u64, u64), &'static str> {
    let (whole, fraction) = value.split_once('.').map_or((value, ""), |parts| parts);
    let whole = whole.parse().map_err(|_| "cpu_counter_malformed")?;
    if fraction.len() > 9 || !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("cpu_counter_malformed");
    }
    let mut nanos = if fraction.is_empty() {
        0
    } else {
        fraction.parse().map_err(|_| "cpu_counter_malformed")?
    };
    for _ in fraction.len()..9 {
        nanos *= 10;
    }
    Ok((whole, nanos))
}

pub(crate) fn parse_df_available_bytes(output: &[u8]) -> Result<u64, &'static str> {
    let text = std::str::from_utf8(output).map_err(|_| "probe_output_not_utf8")?;
    let line = text
        .lines()
        .skip(1)
        .find(|line| !line.trim().is_empty())
        .ok_or("host_free_counter_missing")?;
    let blocks = line
        .split_whitespace()
        .nth(3)
        .ok_or("host_free_counter_malformed")?
        .parse::<u64>()
        .map_err(|_| "host_free_counter_malformed")?;
    blocks.checked_mul(1_024).ok_or("counter_overflow")
}

#[derive(Debug)]
struct VolumeUsage {
    logical_bytes: u64,
    allocated_bytes: Availability<u64>,
    file_count: u64,
}

fn walk_volume(root: &Path) -> Result<VolumeUsage, ProbeFailure> {
    let started = Instant::now();
    let mut stack = vec![root.to_path_buf()];
    let mut visited = 0_u64;
    let mut logical_bytes = 0_u64;
    let mut allocated_total = 0_u64;
    let mut allocated_supported = true;
    let mut file_count = 0_u64;

    while let Some(path) = stack.pop() {
        if started.elapsed() > MAX_VOLUME_WALK_TIME {
            return Err(failure(SOURCE_FILESYSTEM, "volume_walk_timeout"));
        }
        visited = visited
            .checked_add(1)
            .ok_or_else(|| failure(SOURCE_FILESYSTEM, "entry_count_overflow"))?;
        if visited > MAX_VOLUME_ENTRIES {
            return Err(failure(SOURCE_FILESYSTEM, "volume_entry_limit_exceeded"));
        }
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            failure(
                SOURCE_FILESYSTEM,
                format!("volume_walk_{}", io_error_reason(&error)),
            )
        })?;
        match allocated_bytes(&metadata) {
            Some(bytes) if allocated_supported => {
                allocated_total = allocated_total
                    .checked_add(bytes)
                    .ok_or_else(|| failure(SOURCE_FILESYSTEM, "allocated_bytes_overflow"))?;
            }
            None => allocated_supported = false,
            Some(_) => {}
        }

        if metadata.is_file() {
            file_count = file_count
                .checked_add(1)
                .ok_or_else(|| failure(SOURCE_FILESYSTEM, "file_count_overflow"))?;
            logical_bytes = logical_bytes
                .checked_add(metadata.len())
                .ok_or_else(|| failure(SOURCE_FILESYSTEM, "logical_bytes_overflow"))?;
        } else if metadata.is_dir() {
            let canonical = fs::canonicalize(&path).map_err(|error| {
                failure(
                    SOURCE_FILESYSTEM,
                    format!("volume_walk_{}", io_error_reason(&error)),
                )
            })?;
            if !canonical.starts_with(root) {
                return Err(failure(SOURCE_FILESYSTEM, "volume_path_escape"));
            }
            let entries = fs::read_dir(&canonical).map_err(|error| {
                failure(
                    SOURCE_FILESYSTEM,
                    format!("volume_walk_{}", io_error_reason(&error)),
                )
            })?;
            for entry in entries {
                let entry = entry.map_err(|error| {
                    failure(
                        SOURCE_FILESYSTEM,
                        format!("volume_walk_{}", io_error_reason(&error)),
                    )
                })?;
                stack.push(entry.path());
            }
        }
    }

    let allocated_bytes = if allocated_supported {
        Availability::Available {
            value: allocated_total,
        }
    } else {
        unavailable(SOURCE_FILESYSTEM, "allocated_bytes_unsupported")
    };
    Ok(VolumeUsage {
        logical_bytes,
        allocated_bytes,
        file_count,
    })
}

#[cfg(unix)]
fn allocated_bytes(metadata: &Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;

    metadata.blocks().checked_mul(512)
}

#[cfg(not(unix))]
fn allocated_bytes(_metadata: &Metadata) -> Option<u64> {
    None
}

fn run_fixed_probe(program: &Path, args: &[&OsStr]) -> Result<Vec<u8>, ProbeFailure> {
    let mut child = Command::new(program)
        .args(args)
        .env_clear()
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            failure(
                program_source(program),
                format!("probe_spawn_{}", io_error_reason(&error)),
            )
        })?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| failure(program_source(program), "probe_stdout_unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| failure(program_source(program), "probe_stderr_unavailable"))?;
    let stdout_reader = thread::spawn(move || read_capped_bytes(stdout));
    let stderr_reader = thread::spawn(move || read_capped_bytes(stderr));
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < FIXED_PROBE_TIMEOUT => {
                thread::sleep(FIXED_PROBE_POLL_INTERVAL);
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(failure(program_source(program), "probe_timeout"));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(failure(
                    program_source(program),
                    format!("probe_wait_{}", io_error_reason(&error)),
                ));
            }
        }
    };
    let stdout = stdout_reader
        .join()
        .map_err(|_| failure(program_source(program), "probe_reader_panicked"))??;
    let _stderr = stderr_reader
        .join()
        .map_err(|_| failure(program_source(program), "probe_reader_panicked"))??;
    if !status.success() {
        return Err(failure(program_source(program), "probe_exit_failure"));
    }
    Ok(stdout)
}

fn read_capped_bytes(reader: impl Read) -> Result<Vec<u8>, ProbeFailure> {
    let mut output = Vec::new();
    reader
        .take(MAX_FIXED_PROBE_BYTES + 1)
        .read_to_end(&mut output)
        .map_err(|error| {
            failure(
                "fixed_platform_probe",
                format!("probe_read_{}", io_error_reason(&error)),
            )
        })?;
    if output.len() as u64 > MAX_FIXED_PROBE_BYTES {
        return Err(failure(
            "fixed_platform_probe",
            "probe_output_limit_exceeded",
        ));
    }
    Ok(output)
}

fn program_source(program: &Path) -> &'static str {
    match program.file_name().and_then(OsStr::to_str) {
        Some("ps") => SOURCE_MACOS_PS,
        Some("df") => SOURCE_HOST_VOLUME,
        _ => "fixed_platform_probe",
    }
}

fn io_error_reason(error: &io::Error) -> &'static str {
    match error.kind() {
        io::ErrorKind::NotFound => "not_found",
        io::ErrorKind::PermissionDenied => "permission_denied",
        io::ErrorKind::InvalidData => "invalid_data",
        io::ErrorKind::InvalidInput => "invalid_input",
        io::ErrorKind::TimedOut => "timed_out",
        io::ErrorKind::UnexpectedEof => "unexpected_eof",
        _ => "io_error",
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new(label: &str) -> Self {
            let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "sandbox-benchmark-resources-{label}-{}-{id}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create test directory");
            Self(path)
        }
    }

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn sampling_contract_uses_bounded_intervals_and_ordered_windows() {
        assert_eq!(SamplingInterval::DEFAULT.as_millis(), 100);
        assert_eq!(SamplingInterval::LIVE.as_millis(), 500);
        assert!(SamplingInterval::from_millis(19).is_err());
        assert!(SamplingInterval::from_millis(20).is_ok());
        assert!(SamplingInterval::from_millis(1_000).is_ok());
        assert!(SamplingInterval::from_millis(1_001).is_err());
        assert_eq!(
            serde_json::from_str::<SamplingInterval>("100")
                .expect("valid serialized interval")
                .as_millis(),
            100
        );
        assert!(serde_json::from_str::<SamplingInterval>("19").is_err());

        let start = MonotonicInstant::from_offset_ns(10);
        let end = MonotonicInstant::from_offset_ns(25);
        assert_eq!(
            SamplingWindow::new(start, end)
                .expect("ordered window")
                .duration_ns(),
            15
        );
        assert!(SamplingWindow::new(end, start).is_err());
        assert!(serde_json::from_str::<SamplingWindow>(r#"{"start":25,"end":10}"#).is_err());
    }

    #[test]
    fn canonical_metric_registry_has_exact_scopes_units_and_kinds() {
        assert_eq!(METRICS.len(), 14);
        let mut ids = METRICS
            .iter()
            .map(|definition| definition.id)
            .collect::<Vec<_>>();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), METRICS.len());

        let cpu = metric_definition("sandbox_cpu_time_ns").expect("sandbox CPU definition");
        assert_eq!(cpu.unit, MetricUnit::Nanoseconds);
        assert_eq!(cpu.scope, MetricScope::Sandbox);
        assert_eq!(cpu.kind, MetricKind::MonotonicCounter);
        assert_eq!(cpu.aggregation, AggregationRule::Delta);
        assert!(metric_definition("sandbox.cpu.time").is_none());
    }

    #[test]
    fn cgroup_collector_reads_counters_and_labels_peak_fallback() {
        let root = TempDirectory::new("cgroup");
        fs::write(root.0.join("memory.current"), "4096\n").expect("memory.current");
        fs::write(root.0.join("cpu.stat"), "usage_usec 23\nuser_usec 10\n").expect("cpu.stat");
        fs::write(
            root.0.join("io.stat"),
            "8:0 rbytes=7 wbytes=11 rios=1 wios=1\n8:1 rbytes=13 wbytes=17\n",
        )
        .expect("io.stat");

        let mut collector = CgroupCollector::new(&root.0);
        let readings = collector.read(MonotonicInstant::from_offset_ns(50));
        assert_eq!(readings.len(), CGROUP_DEFINITIONS.len());
        assert_reading(&readings, "sandbox_memory_current_bytes", 4_096.0, false);
        assert_reading(&readings, "sandbox_memory_peak_bytes", 4_096.0, true);
        assert_reading(&readings, "sandbox_cpu_time_ns", 23_000.0, false);
        assert_reading(&readings, "sandbox_block_read_bytes", 20.0, false);
        assert_reading(&readings, "sandbox_block_write_bytes", 28.0, false);

        fs::write(root.0.join("memory.current"), "8192\n").expect("update memory.current");
        let readings = collector.read(MonotonicInstant::from_offset_ns(100));
        assert_reading(&readings, "sandbox_memory_peak_bytes", 8_192.0, true);

        fs::write(root.0.join("memory.peak"), "16384\n").expect("native memory.peak");
        let readings = collector.read(MonotonicInstant::from_offset_ns(150));
        assert_reading(&readings, "sandbox_memory_peak_bytes", 16_384.0, false);
    }

    #[test]
    fn missing_cgroup_counters_are_explicitly_unavailable() {
        let root = TempDirectory::new("missing-cgroup");
        fs::write(root.0.join("memory.current"), "0\n").expect("memory.current");
        let mut collector = CgroupCollector::new(&root.0);
        let readings = collector.read(MonotonicInstant::from_offset_ns(0));
        let cpu = find(&readings, "sandbox_cpu_time_ns");
        assert!(matches!(
            &cpu.value,
            Availability::Unavailable { source, reason }
                if !source.is_empty() && reason.contains("not_found")
        ));
        assert_ne!(cpu.value, Availability::Available { value: 0.0 });
    }

    #[test]
    fn workspace_walk_counts_files_and_does_not_follow_symlinks() {
        let root = TempDirectory::new("workspace");
        let outside = TempDirectory::new("outside");
        fs::create_dir(root.0.join("nested")).expect("nested directory");
        fs::write(root.0.join("one"), b"123").expect("first file");
        fs::write(root.0.join("nested/two"), b"45678").expect("second file");
        fs::write(outside.0.join("secret"), vec![0_u8; 1_024]).expect("outside file");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside.0, root.0.join("outside-link"))
            .expect("outside symlink");

        let mut collector = VolumeCollector::new(VolumeScope::Workspace, &root.0);
        let readings = collector.read(MonotonicInstant::from_offset_ns(9));
        assert_reading(&readings, "workspace_logical_bytes", 8.0, false);
        assert_reading(&readings, "workspace_file_count", 2.0, false);
        let allocated = find(&readings, "workspace_allocated_bytes");
        #[cfg(unix)]
        assert!(matches!(allocated.value, Availability::Available { .. }));
        assert_eq!(allocated.monotonic_offset_ns, 9);
    }

    #[test]
    fn parsers_preserve_exact_units_and_reject_absent_counters() {
        assert_eq!(
            parse_cpu_stat_ns("usage_usec 42\n").expect("cpu usec"),
            42_000
        );
        assert_eq!(parse_cpu_stat_ns("usage_nsec 43\n").expect("cpu nsec"), 43);
        assert_eq!(
            parse_proc_rss_bytes("Name:\ttest\nVmRSS:\t12 kB\n").expect("rss"),
            12_288
        );
        assert_eq!(parse_schedstat_cpu_ns("987 1 2\n").expect("schedstat"), 987);
        assert_eq!(
            parse_io_stat_bytes("8:0 rbytes=0 wbytes=2\n", "rbytes").expect("present zero"),
            0
        );
        assert_eq!(
            parse_io_stat_bytes("8:0 wbytes=2\n", "rbytes"),
            Err("counter_missing")
        );
        assert_eq!(
            parse_df_available_bytes(
                b"Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/x 100 20 80 20% /\n"
            )
            .expect("df"),
            81_920
        );
    }

    #[test]
    fn process_parsers_capture_start_identity_rss_and_cpu() {
        let stat = format!("42 (name with ) paren) S {}", vec!["1"; 19].join(" "));
        assert_eq!(
            parse_proc_start_identity(&stat).expect("process identity"),
            "linux_start_ticks:1"
        );
        let snapshot =
            parse_ps_snapshot(b"  128   1:02.03 Sun Jul 12 03:38:25 2026\n").expect("ps snapshot");
        assert_eq!(snapshot.identity, "macos_lstart:Sun Jul 12 03:38:25 2026");
        assert_eq!(
            snapshot.rss_bytes,
            Availability::Available { value: 131_072 }
        );
        assert_eq!(
            snapshot.cpu_time_ns,
            Availability::Available {
                value: 62_030_000_000
            }
        );
        assert_eq!(
            parse_ps_cpu_time_ns("2-03:04:05.006").expect("day CPU time"),
            183_845_006_000_000
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn fixed_platform_collectors_read_current_process_and_host_volume() {
        let mut process = ProcessCollector::attach(ProcessScope::Runner, std::process::id());
        let process_readings = process.read(MonotonicInstant::from_offset_ns(1));
        assert!(matches!(
            find(&process_readings, "runner_rss_bytes").value,
            Availability::Available { value } if value > 0.0
        ));

        let root = TempDirectory::new("host-volume");
        let mut host = HostVolumeCollector::new(&root.0);
        let host_readings = host.read(MonotonicInstant::from_offset_ns(2));
        assert!(matches!(
            find(&host_readings, "host_free_bytes").value,
            Availability::Available { value } if value > 0.0
        ));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn process_collector_rejects_pid_identity_changes() {
        let mut collector = ProcessCollector {
            scope: ProcessScope::Daemon,
            pid: std::process::id(),
            expected_start_identity: Availability::Available {
                value: "not-this-process".to_owned(),
            },
        };
        for reading in collector.read(MonotonicInstant::from_offset_ns(3)) {
            assert!(matches!(
                reading.value,
                Availability::Unavailable { reason, .. }
                    if reason == "pid_start_identity_changed"
            ));
        }
    }

    #[test]
    fn counter_delta_never_wraps_or_turns_unavailable_into_zero() {
        assert_eq!(
            counter_delta(
                &Availability::Available { value: 10 },
                &Availability::Available { value: 17 },
                SOURCE_CGROUP_CPU,
            ),
            Availability::Available { value: 7 }
        );
        assert_eq!(
            counter_delta(
                &Availability::Available { value: 17 },
                &Availability::Available { value: 10 },
                SOURCE_CGROUP_CPU,
            ),
            unavailable(SOURCE_CGROUP_CPU, "counter_reset_or_regression")
        );
        assert_eq!(
            counter_delta(
                &unavailable("baseline", "permission_denied"),
                &Availability::Available { value: 10 },
                SOURCE_CGROUP_CPU,
            ),
            unavailable("baseline", "baseline_unavailable:permission_denied")
        );
    }

    fn find<'a>(readings: &'a [ResourceReading], id: &str) -> &'a ResourceReading {
        readings
            .iter()
            .find(|reading| reading.metric_id == id)
            .expect("metric reading")
    }

    fn assert_reading(readings: &[ResourceReading], id: &str, value: f64, sampled: bool) {
        let reading = find(readings, id);
        assert_eq!(reading.value, Availability::Available { value });
        assert_eq!(reading.sampled, sampled);
    }
}
