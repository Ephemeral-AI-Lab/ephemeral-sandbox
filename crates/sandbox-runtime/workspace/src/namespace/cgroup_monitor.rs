use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::model::{WorkspaceHandle, WorkspaceSessionId};
use crate::overlay::tree::TreeResourceStats;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CgroupMonitorConfig {
    pub enabled: bool,
    pub sample_interval_ms: u64,
    pub retained_samples_per_target: usize,
    pub include_pids: bool,
    pub include_pressure: bool,
    pub include_disk: bool,
}

impl Default for CgroupMonitorConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            sample_interval_ms: 1000,
            retained_samples_per_target: 100,
            include_pids: true,
            include_pressure: true,
            include_disk: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CgroupMonitorTargetKind {
    Session,
    Command,
}

impl CgroupMonitorTargetKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Command => "command",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CgroupSampleKind {
    Periodic,
    CommandFinal,
    SessionFinal,
    Cleanup,
}

impl CgroupSampleKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Periodic => "periodic",
            Self::CommandFinal => "command_final",
            Self::SessionFinal => "session_final",
            Self::Cleanup => "cleanup",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CgroupMonitorTarget {
    pub kind: CgroupMonitorTargetKind,
    pub cgroup_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CgroupMonitorSnapshot {
    pub target: CgroupMonitorTarget,
    pub monitor: CgroupMonitorState,
    pub latest: Option<CgroupMonitorSample>,
    pub cleanup: CgroupCleanupState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CgroupMonitorState {
    pub enabled: bool,
    pub sample_interval_ms: u64,
    pub retained_samples: usize,
    pub last_sampled_at_unix_ms: Option<u64>,
    pub read_error_count: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CgroupCleanupState {
    pub final_sample_recorded: bool,
    pub cgroup_exists_after_destroy: Option<bool>,
    pub last_cleanup_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CgroupMonitorSample {
    pub sample_kind: CgroupSampleKind,
    pub sampled_at_unix_ms: u64,
    pub interval_ms: u64,
    pub cpu: CgroupCpuSample,
    pub memory: CgroupMemorySample,
    pub io: CgroupIoSample,
    pub pids: CgroupPidSample,
    pub pressure: CgroupPressureSample,
    pub disk: CgroupDiskSample,
    pub state: CgroupRuntimeState,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct CgroupCpuSample {
    pub usage_usec: Option<u64>,
    pub user_usec: Option<u64>,
    pub system_usec: Option<u64>,
    pub delta_usage_usec: Option<u64>,
    pub percent_over_interval: Option<f64>,
    pub nr_periods: Option<u64>,
    pub nr_throttled: Option<u64>,
    pub throttled_usec: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CgroupMemorySample {
    pub current_bytes: Option<u64>,
    pub peak_bytes: Option<u64>,
    pub anon_bytes: Option<u64>,
    pub file_bytes: Option<u64>,
    pub kernel_bytes: Option<u64>,
    pub events: CgroupMemoryEvents,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CgroupMemoryEvents {
    pub low: Option<u64>,
    pub high: Option<u64>,
    pub max: Option<u64>,
    pub oom: Option<u64>,
    pub oom_kill: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CgroupIoSample {
    pub read_bytes: Option<u64>,
    pub write_bytes: Option<u64>,
    pub read_ops: Option<u64>,
    pub write_ops: Option<u64>,
    pub discard_bytes: Option<u64>,
    pub discard_ops: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CgroupPidSample {
    pub current: Option<u64>,
    pub peak: Option<u64>,
    pub sampled: Vec<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct CgroupPressureSample {
    pub cpu: PressureResourceSample,
    pub memory: PressureResourceSample,
    pub io: PressureResourceSample,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct PressureResourceSample {
    pub some_avg10: Option<f64>,
    pub some_avg60: Option<f64>,
    pub some_avg300: Option<f64>,
    pub some_total_usec: Option<u64>,
    pub full_avg10: Option<f64>,
    pub full_avg60: Option<f64>,
    pub full_avg300: Option<f64>,
    pub full_total_usec: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CgroupDiskSample {
    pub upperdir_bytes: u64,
    pub upperdir_files: u64,
    pub upperdir_dirs: u64,
    pub upperdir_symlinks: u64,
    pub upperdir_scan_truncated: bool,
    pub upperdir_read_error_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CgroupRuntimeState {
    pub cgroup_exists: bool,
    pub cgroup_populated: Option<bool>,
    pub frozen: Option<bool>,
    pub read_error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CgroupSampleRequest<'a> {
    pub cgroup_path: &'a Path,
    pub upperdir: Option<&'a Path>,
    pub sample_kind: CgroupSampleKind,
    pub interval_ms: u64,
    pub previous: Option<&'a CgroupMonitorSample>,
    pub config: &'a CgroupMonitorConfig,
}

#[derive(Debug)]
pub struct CgroupMonitorRegistry {
    config: CgroupMonitorConfig,
    records: Arc<Mutex<HashMap<CgroupTargetKey, RetainedTarget>>>,
    sampler_started: Mutex<bool>,
}

impl CgroupMonitorRegistry {
    #[must_use]
    pub fn new(config: CgroupMonitorConfig) -> Self {
        Self {
            config,
            records: Arc::new(Mutex::new(HashMap::new())),
            sampler_started: Mutex::new(false),
        }
    }

    #[must_use]
    pub fn config(&self) -> &CgroupMonitorConfig {
        &self.config
    }

    pub fn register_session_from_handle(&self, handle: &WorkspaceHandle) {
        let Ok(entry) = handle.entry() else {
            return;
        };
        let Some(cgroup_path) = entry.cgroup_path else {
            return;
        };
        if cgroup_path.as_os_str().is_empty() {
            return;
        }
        let key = CgroupTargetKey::session(handle.id.clone());
        self.register_target(
            key.clone(),
            CgroupMonitorTarget {
                kind: CgroupMonitorTargetKind::Session,
                cgroup_path,
            },
            entry.upperdir,
        );
        self.sample_target(&key, CgroupSampleKind::Periodic, false);
        self.start_sampler();
    }

    pub fn register_command(
        &self,
        workspace_session_id: WorkspaceSessionId,
        command_session_id: impl Into<String>,
        cgroup_path: PathBuf,
        upperdir: PathBuf,
    ) {
        if cgroup_path.as_os_str().is_empty() {
            return;
        }
        let key = CgroupTargetKey::command(workspace_session_id, command_session_id.into());
        self.register_target(
            key.clone(),
            CgroupMonitorTarget {
                kind: CgroupMonitorTargetKind::Command,
                cgroup_path,
            },
            upperdir,
        );
        self.sample_target(&key, CgroupSampleKind::Periodic, false);
        self.start_sampler();
    }

    pub fn contains_target(
        &self,
        workspace_session_id: &WorkspaceSessionId,
        command_session_id: Option<&str>,
    ) -> bool {
        let records = self.records.lock().unwrap_or_else(PoisonError::into_inner);
        let key = CgroupTargetKey::new(workspace_session_id.clone(), command_session_id);
        records.contains_key(&key)
    }

    pub fn inspect(
        &self,
        workspace_session_id: &WorkspaceSessionId,
        command_session_id: Option<&str>,
    ) -> Option<CgroupMonitorSnapshot> {
        let records = self.records.lock().unwrap_or_else(PoisonError::into_inner);
        let key = CgroupTargetKey::new(workspace_session_id.clone(), command_session_id);
        let record = records.get(&key)?;
        Some(record.snapshot(&self.config))
    }

    pub fn read_samples(
        &self,
        workspace_session_id: &WorkspaceSessionId,
        command_session_id: Option<&str>,
        limit: usize,
    ) -> Option<CgroupMonitorSampleWindow> {
        let records = self.records.lock().unwrap_or_else(PoisonError::into_inner);
        let key = CgroupTargetKey::new(workspace_session_id.clone(), command_session_id);
        let record = records.get(&key)?;
        let limit = limit.min(record.samples.len());
        let samples = record
            .samples
            .iter()
            .skip(record.samples.len().saturating_sub(limit))
            .cloned()
            .collect();
        Some(CgroupMonitorSampleWindow {
            target: record.target.clone(),
            samples,
        })
    }

    pub fn record_session_final_from_handle(&self, handle: &WorkspaceHandle) {
        let sample = self.session_final_sample_from_handle(handle);
        self.record_session_final_sample(&handle.id, sample);
    }

    pub fn session_final_sample_from_handle(
        &self,
        handle: &WorkspaceHandle,
    ) -> Option<CgroupMonitorSample> {
        let key = CgroupTargetKey::session(handle.id.clone());
        self.build_sample_for_key(&key, CgroupSampleKind::SessionFinal)
    }

    pub fn record_session_final_sample(
        &self,
        workspace_session_id: &WorkspaceSessionId,
        sample: Option<CgroupMonitorSample>,
    ) {
        let Some(sample) = sample else {
            return;
        };
        let key = CgroupTargetKey::session(workspace_session_id.clone());
        let mut records = self.records.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(record) = records.get_mut(&key) else {
            return;
        };
        record.push_sample(sample, &self.config);
        record.cleanup.final_sample_recorded = true;
        emit_cgroup_final_summary(
            "session_final",
            record.target.kind,
            record.samples.back(),
            &record.cleanup,
        );
    }

    pub fn record_command_final(
        &self,
        workspace_session_id: &WorkspaceSessionId,
        command_session_id: &str,
        sample: Option<CgroupMonitorSample>,
        cleanup: Option<CgroupCleanupState>,
    ) {
        let key = CgroupTargetKey::command(workspace_session_id.clone(), command_session_id);
        let mut records = self.records.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(record) = records.get_mut(&key) else {
            return;
        };
        if self.config.enabled {
            if let Some(mut sample) = sample {
                enrich_final_cpu_sample(&mut sample, record.samples.back(), &self.config);
                record.push_sample(sample, &self.config);
                record.cleanup.final_sample_recorded = true;
            }
        }
        if let Some(cleanup) = cleanup {
            record.cleanup.cgroup_exists_after_destroy = cleanup.cgroup_exists_after_destroy;
            record.cleanup.last_cleanup_error = cleanup.last_cleanup_error;
        }
        emit_cgroup_final_summary(
            "command_final",
            record.target.kind,
            record.samples.back(),
            &record.cleanup,
        );
    }

    pub fn record_cleanup(
        &self,
        workspace_session_id: &WorkspaceSessionId,
        command_session_id: Option<&str>,
        cgroup_exists_after_destroy: Option<bool>,
        last_cleanup_error: Option<String>,
    ) {
        let key = CgroupTargetKey::new(workspace_session_id.clone(), command_session_id);
        let mut records = self.records.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(record) = records.get_mut(&key) else {
            return;
        };
        record.cleanup.cgroup_exists_after_destroy = cgroup_exists_after_destroy;
        record.cleanup.last_cleanup_error = last_cleanup_error;
        emit_cgroup_final_summary(
            "cleanup",
            record.target.kind,
            record.samples.back(),
            &record.cleanup,
        );
    }

    fn register_target(
        &self,
        key: CgroupTargetKey,
        target: CgroupMonitorTarget,
        upperdir: PathBuf,
    ) {
        let mut records = self.records.lock().unwrap_or_else(PoisonError::into_inner);
        records
            .entry(key)
            .and_modify(|record| {
                record.target = target.clone();
                record.upperdir = upperdir.clone();
            })
            .or_insert_with(|| RetainedTarget::new(target, upperdir));
    }

    fn start_sampler(&self) {
        if !self.config.enabled {
            return;
        }
        let mut sampler_started = self
            .sampler_started
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if *sampler_started {
            return;
        }
        let interval = Duration::from_millis(self.config.sample_interval_ms.max(1));
        let records = Arc::downgrade(&self.records);
        let config = self.config.clone();
        if thread::Builder::new()
            .name("eos-cgroup-monitor".to_owned())
            .spawn(move || sampler_loop(records, config, interval))
            .is_ok()
        {
            *sampler_started = true;
        }
    }

    fn sample_target(
        &self,
        key: &CgroupTargetKey,
        sample_kind: CgroupSampleKind,
        mark_final: bool,
    ) {
        let Some(sample) = self.build_sample_for_key(key, sample_kind) else {
            return;
        };
        let mut records = self.records.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(record) = records.get_mut(key) else {
            return;
        };
        record.push_sample(sample, &self.config);
        if mark_final {
            record.cleanup.final_sample_recorded = true;
        }
    }

    fn build_sample_for_key(
        &self,
        key: &CgroupTargetKey,
        sample_kind: CgroupSampleKind,
    ) -> Option<CgroupMonitorSample> {
        if !self.config.enabled {
            return None;
        }
        let plan = {
            let records = self.records.lock().unwrap_or_else(PoisonError::into_inner);
            let record = records.get(key)?;
            if sample_kind == CgroupSampleKind::Periodic && !record.accepts_periodic_sample() {
                return None;
            }
            record.sample_plan(key.clone(), sample_kind)
        };
        Some(build_sample_from_plan(&plan, &self.config))
    }
}

impl Default for CgroupMonitorRegistry {
    fn default() -> Self {
        Self::new(CgroupMonitorConfig::default())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CgroupMonitorSampleWindow {
    pub target: CgroupMonitorTarget,
    pub samples: Vec<CgroupMonitorSample>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CgroupTargetKey {
    workspace_session_id: WorkspaceSessionId,
    command_session_id: Option<String>,
}

impl CgroupTargetKey {
    fn new(workspace_session_id: WorkspaceSessionId, command_session_id: Option<&str>) -> Self {
        Self {
            workspace_session_id,
            command_session_id: command_session_id.map(str::to_owned),
        }
    }

    fn session(workspace_session_id: WorkspaceSessionId) -> Self {
        Self::new(workspace_session_id, None)
    }

    fn command(
        workspace_session_id: WorkspaceSessionId,
        command_session_id: impl Into<String>,
    ) -> Self {
        Self {
            workspace_session_id,
            command_session_id: Some(command_session_id.into()),
        }
    }
}

#[derive(Debug, Clone)]
struct CgroupSamplePlan {
    key: CgroupTargetKey,
    target: CgroupMonitorTarget,
    upperdir: PathBuf,
    previous: Option<CgroupMonitorSample>,
    sample_kind: CgroupSampleKind,
}

#[derive(Debug)]
struct RetainedTarget {
    target: CgroupMonitorTarget,
    upperdir: PathBuf,
    samples: VecDeque<CgroupMonitorSample>,
    read_error_count: u64,
    cleanup: CgroupCleanupState,
}

impl RetainedTarget {
    fn new(target: CgroupMonitorTarget, upperdir: PathBuf) -> Self {
        Self {
            target,
            upperdir,
            samples: VecDeque::new(),
            read_error_count: 0,
            cleanup: CgroupCleanupState::default(),
        }
    }

    fn sample_plan(&self, key: CgroupTargetKey, sample_kind: CgroupSampleKind) -> CgroupSamplePlan {
        CgroupSamplePlan {
            key,
            target: self.target.clone(),
            upperdir: self.upperdir.clone(),
            previous: self.samples.back().cloned(),
            sample_kind,
        }
    }

    fn accepts_periodic_sample(&self) -> bool {
        !self.cleanup.final_sample_recorded && self.cleanup.cgroup_exists_after_destroy.is_none()
    }

    fn push_sample(&mut self, sample: CgroupMonitorSample, config: &CgroupMonitorConfig) {
        if sample.state.read_error.is_some() {
            self.read_error_count = self.read_error_count.saturating_add(1);
            emit_cgroup_anomaly(&self.target, &sample, self.read_error_count);
        }
        self.samples.push_back(sample);
        while self.samples.len() > config.retained_samples_per_target {
            self.samples.pop_front();
        }
    }

    fn snapshot(&self, config: &CgroupMonitorConfig) -> CgroupMonitorSnapshot {
        CgroupMonitorSnapshot {
            target: self.target.clone(),
            monitor: CgroupMonitorState {
                enabled: config.enabled,
                sample_interval_ms: config.sample_interval_ms,
                retained_samples: self.samples.len(),
                last_sampled_at_unix_ms: self
                    .samples
                    .back()
                    .map(|sample| sample.sampled_at_unix_ms),
                read_error_count: self.read_error_count,
            },
            latest: self.samples.back().cloned(),
            cleanup: self.cleanup.clone(),
        }
    }
}

fn emit_cgroup_anomaly(
    target: &CgroupMonitorTarget,
    sample: &CgroupMonitorSample,
    read_error_count: u64,
) {
    tracing::warn!(
        name: "cgroup_monitor.anomaly",
        target_kind = target.kind.as_str(),
        sample_kind = sample.sample_kind.as_str(),
        anomaly_class = cgroup_anomaly_class(sample),
        cgroup_exists = sample.state.cgroup_exists,
        cgroup_populated = sample.state.cgroup_populated.unwrap_or(false),
        cgroup_populated_present = sample.state.cgroup_populated.is_some(),
        frozen = sample.state.frozen.unwrap_or(false),
        frozen_present = sample.state.frozen.is_some(),
        read_error_count = read_error_count,
    );
}

fn emit_cgroup_final_summary(
    boundary: &'static str,
    target_kind: CgroupMonitorTargetKind,
    sample: Option<&CgroupMonitorSample>,
    cleanup: &CgroupCleanupState,
) {
    tracing::info!(
        name: "cgroup_monitor.final_summary",
        boundary = boundary,
        target_kind = target_kind.as_str(),
        sample_available = sample.is_some(),
        sample_kind = sample
            .map(|sample| sample.sample_kind.as_str())
            .unwrap_or("none"),
        cgroup_exists = sample
            .map(|sample| sample.state.cgroup_exists)
            .unwrap_or(false),
        read_error = sample
            .map(|sample| sample.state.read_error.is_some())
            .unwrap_or(false),
        cpu_delta_available = sample
            .and_then(|sample| sample.cpu.delta_usage_usec)
            .is_some(),
        memory_current_available = sample
            .and_then(|sample| sample.memory.current_bytes)
            .is_some(),
        final_sample_recorded = cleanup.final_sample_recorded,
        cgroup_exists_after_destroy = cleanup.cgroup_exists_after_destroy.unwrap_or(false),
        cgroup_exists_after_destroy_present = cleanup.cgroup_exists_after_destroy.is_some(),
        cleanup_error = cleanup.last_cleanup_error.is_some(),
    );
}

fn cgroup_anomaly_class(sample: &CgroupMonitorSample) -> &'static str {
    let Some(error) = sample.state.read_error.as_deref() else {
        return "none";
    };
    if !sample.state.cgroup_exists {
        "cgroup_missing"
    } else if error.contains("malformed") {
        "malformed_cgroup_file"
    } else if error.contains("No such file") || error.contains("not found") {
        "missing_cgroup_file"
    } else {
        "read_error"
    }
}

fn sampler_loop(
    records: Weak<Mutex<HashMap<CgroupTargetKey, RetainedTarget>>>,
    config: CgroupMonitorConfig,
    interval: Duration,
) {
    loop {
        thread::sleep(interval);
        let Some(records) = records.upgrade() else {
            break;
        };
        sample_periodic_targets(&records, &config);
    }
}

fn sample_periodic_targets(
    records: &Arc<Mutex<HashMap<CgroupTargetKey, RetainedTarget>>>,
    config: &CgroupMonitorConfig,
) {
    if !config.enabled {
        return;
    }
    let plans = {
        let records = records.lock().unwrap_or_else(PoisonError::into_inner);
        records
            .iter()
            .filter(|(_, record)| record.accepts_periodic_sample())
            .map(|(key, record)| record.sample_plan(key.clone(), CgroupSampleKind::Periodic))
            .collect::<Vec<_>>()
    };
    for plan in plans {
        let sample = build_sample_from_plan(&plan, config);
        let mut records = records.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(record) = records.get_mut(&plan.key) else {
            continue;
        };
        if record.target == plan.target && record.accepts_periodic_sample() {
            record.push_sample(sample, config);
        }
    }
}

fn build_sample_from_plan(
    plan: &CgroupSamplePlan,
    config: &CgroupMonitorConfig,
) -> CgroupMonitorSample {
    build_cgroup_monitor_sample(CgroupSampleRequest {
        cgroup_path: &plan.target.cgroup_path,
        upperdir: Some(&plan.upperdir),
        sample_kind: plan.sample_kind,
        interval_ms: config.sample_interval_ms,
        previous: plan.previous.as_ref(),
        config,
    })
}

fn enrich_final_cpu_sample(
    sample: &mut CgroupMonitorSample,
    previous: Option<&CgroupMonitorSample>,
    config: &CgroupMonitorConfig,
) {
    if !matches!(
        sample.sample_kind,
        CgroupSampleKind::CommandFinal | CgroupSampleKind::SessionFinal
    ) {
        return;
    }
    let interval_ms = effective_interval_ms(
        previous,
        sample.sampled_at_unix_ms,
        config.sample_interval_ms,
    );
    let delta_usage_usec = sample.cpu.usage_usec.and_then(|usage| {
        previous
            .and_then(|previous| previous.cpu.usage_usec)
            .map(|previous_usage| usage.saturating_sub(previous_usage))
    });
    sample.interval_ms = interval_ms;
    sample.cpu.delta_usage_usec = delta_usage_usec;
    sample.cpu.percent_over_interval = delta_usage_usec.and_then(|delta| {
        let interval_usec = interval_ms.saturating_mul(1000);
        (interval_usec > 0).then(|| (delta as f64 / interval_usec as f64) * 100.0)
    });
}

#[must_use]
pub fn session_cgroup_path(root: &Path, workspace_session_id: &WorkspaceSessionId) -> PathBuf {
    root.join("eos")
        .join("sessions")
        .join(&workspace_session_id.0)
}

#[must_use]
pub fn command_cgroup_path(session_cgroup_path: &Path, command_session_id: &str) -> PathBuf {
    session_cgroup_path
        .join("commands")
        .join(command_session_id)
}

#[must_use]
pub fn build_cgroup_monitor_sample(request: CgroupSampleRequest<'_>) -> CgroupMonitorSample {
    let mut errors = Vec::new();
    let sampled_at_unix_ms = unix_time_ms();
    let interval_ms =
        effective_interval_ms(request.previous, sampled_at_unix_ms, request.interval_ms);
    let cgroup_exists = request.cgroup_path.is_dir();
    let cpu = read_cpu_sample(
        request.cgroup_path,
        request.previous,
        interval_ms,
        &mut errors,
    );
    let memory = read_memory_sample(request.cgroup_path, &mut errors);
    let io = read_io_sample(request.cgroup_path, &mut errors);
    let pids = if request.config.include_pids {
        read_pid_sample(request.cgroup_path, &mut errors)
    } else {
        CgroupPidSample::default()
    };
    let pressure = if request.config.include_pressure {
        read_pressure_sample(request.cgroup_path, &mut errors)
    } else {
        CgroupPressureSample::default()
    };
    let disk = if request.config.include_disk {
        request.upperdir.map(disk_sample).unwrap_or_default()
    } else {
        CgroupDiskSample::default()
    };
    let (cgroup_populated, frozen) = read_cgroup_events(request.cgroup_path, &mut errors);

    CgroupMonitorSample {
        sample_kind: request.sample_kind,
        sampled_at_unix_ms,
        interval_ms,
        cpu,
        memory,
        io,
        pids,
        pressure,
        disk,
        state: CgroupRuntimeState {
            cgroup_exists,
            cgroup_populated,
            frozen,
            read_error: (!errors.is_empty()).then(|| errors.join("; ")),
        },
    }
}

fn read_cpu_sample(
    cgroup_path: &Path,
    previous: Option<&CgroupMonitorSample>,
    interval_ms: u64,
    errors: &mut Vec<String>,
) -> CgroupCpuSample {
    let stats = read_key_value_file(&cgroup_path.join("cpu.stat"), errors);
    let usage_usec = stats.get("usage_usec").copied();
    let delta_usage_usec = usage_usec.and_then(|usage| {
        previous
            .and_then(|sample| sample.cpu.usage_usec)
            .map(|previous_usage| usage.saturating_sub(previous_usage))
    });
    let percent_over_interval = delta_usage_usec.and_then(|delta| {
        let interval_usec = interval_ms.saturating_mul(1000);
        (interval_usec > 0).then(|| (delta as f64 / interval_usec as f64) * 100.0)
    });
    CgroupCpuSample {
        usage_usec,
        user_usec: stats.get("user_usec").copied(),
        system_usec: stats.get("system_usec").copied(),
        delta_usage_usec,
        percent_over_interval,
        nr_periods: stats.get("nr_periods").copied(),
        nr_throttled: stats.get("nr_throttled").copied(),
        throttled_usec: stats.get("throttled_usec").copied(),
    }
}

fn read_memory_sample(cgroup_path: &Path, errors: &mut Vec<String>) -> CgroupMemorySample {
    let stats = read_key_value_file(&cgroup_path.join("memory.stat"), errors);
    let events = read_key_value_file(&cgroup_path.join("memory.events"), errors);
    CgroupMemorySample {
        current_bytes: read_u64_file(&cgroup_path.join("memory.current"), errors),
        peak_bytes: read_u64_file(&cgroup_path.join("memory.peak"), errors),
        anon_bytes: stats.get("anon").copied(),
        file_bytes: stats.get("file").copied(),
        kernel_bytes: stats.get("kernel").copied(),
        events: CgroupMemoryEvents {
            low: events.get("low").copied(),
            high: events.get("high").copied(),
            max: events.get("max").copied(),
            oom: events.get("oom").copied(),
            oom_kill: events.get("oom_kill").copied(),
        },
    }
}

fn read_io_sample(cgroup_path: &Path, errors: &mut Vec<String>) -> CgroupIoSample {
    let Some(content) = read_optional_string(&cgroup_path.join("io.stat"), errors) else {
        return CgroupIoSample::default();
    };
    let mut sample = CgroupIoSample::default();
    let mut saw_value = false;
    for line in content.lines().filter(|line| !line.trim().is_empty()) {
        for token in line.split_whitespace().skip(1) {
            let Some((key, value)) = token.split_once('=') else {
                errors.push(format!("io.stat malformed token: {token}"));
                continue;
            };
            let Ok(parsed) = value.parse::<u64>() else {
                errors.push(format!("io.stat malformed value for {key}: {value}"));
                continue;
            };
            saw_value = true;
            match key {
                "rbytes" => sample.read_bytes = add_option(sample.read_bytes, parsed),
                "wbytes" => sample.write_bytes = add_option(sample.write_bytes, parsed),
                "rios" => sample.read_ops = add_option(sample.read_ops, parsed),
                "wios" => sample.write_ops = add_option(sample.write_ops, parsed),
                "dbytes" => sample.discard_bytes = add_option(sample.discard_bytes, parsed),
                "dios" => sample.discard_ops = add_option(sample.discard_ops, parsed),
                _ => {}
            }
        }
    }
    if !saw_value && !content.trim().is_empty() {
        errors.push("io.stat did not contain parseable counters".to_owned());
    }
    sample
}

fn read_pid_sample(cgroup_path: &Path, errors: &mut Vec<String>) -> CgroupPidSample {
    let sampled = read_optional_string(&cgroup_path.join("cgroup.procs"), errors)
        .map(|content| {
            let mut sampled = Vec::new();
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                match trimmed.parse::<u32>() {
                    Ok(pid) => sampled.push(pid),
                    Err(_) => errors.push(format!("cgroup.procs malformed pid: {trimmed}")),
                }
            }
            sampled
        })
        .unwrap_or_default();
    CgroupPidSample {
        current: read_u64_file(&cgroup_path.join("pids.current"), errors),
        peak: read_u64_file(&cgroup_path.join("pids.peak"), errors),
        sampled,
    }
}

fn read_pressure_sample(cgroup_path: &Path, errors: &mut Vec<String>) -> CgroupPressureSample {
    CgroupPressureSample {
        cpu: read_pressure_file(&cgroup_path.join("cpu.pressure"), errors),
        memory: read_pressure_file(&cgroup_path.join("memory.pressure"), errors),
        io: read_pressure_file(&cgroup_path.join("io.pressure"), errors),
    }
}

fn read_pressure_file(path: &Path, errors: &mut Vec<String>) -> PressureResourceSample {
    let Some(content) = read_optional_string(path, errors) else {
        return PressureResourceSample::default();
    };
    let mut sample = PressureResourceSample::default();
    for line in content.lines().filter(|line| !line.trim().is_empty()) {
        let mut tokens = line.split_whitespace();
        let Some(kind) = tokens.next() else {
            continue;
        };
        for token in tokens {
            let Some((key, value)) = token.split_once('=') else {
                errors.push(format!("{} malformed token: {token}", display_name(path)));
                continue;
            };
            match (kind, key) {
                ("some", "avg10") => sample.some_avg10 = parse_f64(value, path, key, errors),
                ("some", "avg60") => sample.some_avg60 = parse_f64(value, path, key, errors),
                ("some", "avg300") => sample.some_avg300 = parse_f64(value, path, key, errors),
                ("some", "total") => sample.some_total_usec = parse_u64(value, path, key, errors),
                ("full", "avg10") => sample.full_avg10 = parse_f64(value, path, key, errors),
                ("full", "avg60") => sample.full_avg60 = parse_f64(value, path, key, errors),
                ("full", "avg300") => sample.full_avg300 = parse_f64(value, path, key, errors),
                ("full", "total") => sample.full_total_usec = parse_u64(value, path, key, errors),
                _ => {}
            }
        }
    }
    sample
}

fn read_cgroup_events(
    cgroup_path: &Path,
    errors: &mut Vec<String>,
) -> (Option<bool>, Option<bool>) {
    let events = read_key_value_file(&cgroup_path.join("cgroup.events"), errors);
    (
        events.get("populated").map(|value| *value != 0),
        events.get("frozen").map(|value| *value != 0),
    )
}

fn read_key_value_file(path: &Path, errors: &mut Vec<String>) -> HashMap<String, u64> {
    let Some(content) = read_optional_string(path, errors) else {
        return HashMap::new();
    };
    let mut values = HashMap::new();
    for line in content.lines().filter(|line| !line.trim().is_empty()) {
        let mut parts = line.split_whitespace();
        let Some(key) = parts.next() else {
            continue;
        };
        let Some(value) = parts.next() else {
            errors.push(format!("{} malformed line: {line}", display_name(path)));
            continue;
        };
        match value.parse::<u64>() {
            Ok(value) => {
                values.insert(key.to_owned(), value);
            }
            Err(_) => errors.push(format!(
                "{} malformed value for {key}: {value}",
                display_name(path)
            )),
        }
    }
    values
}

fn read_u64_file(path: &Path, errors: &mut Vec<String>) -> Option<u64> {
    let content = read_optional_string(path, errors)?;
    let trimmed = content.trim();
    match trimmed.parse::<u64>() {
        Ok(value) => Some(value),
        Err(_) => {
            errors.push(format!(
                "{} malformed integer: {trimmed}",
                display_name(path)
            ));
            None
        }
    }
}

fn read_optional_string(path: &Path, errors: &mut Vec<String>) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(content) => Some(content),
        Err(error) => {
            errors.push(format!("{}: {error}", display_name(path)));
            None
        }
    }
}

fn parse_f64(value: &str, path: &Path, key: &str, errors: &mut Vec<String>) -> Option<f64> {
    match value.parse::<f64>() {
        Ok(value) if value.is_finite() => Some(value),
        _ => {
            errors.push(format!(
                "{} malformed finite value for {key}: {value}",
                display_name(path)
            ));
            None
        }
    }
}

fn parse_u64(value: &str, path: &Path, key: &str, errors: &mut Vec<String>) -> Option<u64> {
    match value.parse::<u64>() {
        Ok(value) => Some(value),
        Err(_) => {
            errors.push(format!(
                "{} malformed integer for {key}: {value}",
                display_name(path)
            ));
            None
        }
    }
}

fn disk_sample(path: &Path) -> CgroupDiskSample {
    let stats = TreeResourceStats::collect(path);
    CgroupDiskSample {
        upperdir_bytes: stats.bytes,
        upperdir_files: stats.files,
        upperdir_dirs: stats.dirs,
        upperdir_symlinks: stats.symlinks,
        upperdir_scan_truncated: stats.truncated,
        upperdir_read_error_count: stats.read_error_count,
    }
}

fn add_option(current: Option<u64>, value: u64) -> Option<u64> {
    Some(current.unwrap_or(0).saturating_add(value))
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .unwrap_or_else(|| path.display().to_string())
}

fn effective_interval_ms(
    previous: Option<&CgroupMonitorSample>,
    sampled_at_unix_ms: u64,
    fallback_interval_ms: u64,
) -> u64 {
    previous
        .and_then(|sample| {
            sampled_at_unix_ms
                .checked_sub(sample.sampled_at_unix_ms)
                .filter(|elapsed| *elapsed > 0)
        })
        .unwrap_or(fallback_interval_ms)
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}
