use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sandbox_config::configs::observability::ResourceStatsConfig;
use sandbox_observability_telemetry::collect::cgroup::CgroupSample;
use sandbox_observability_telemetry::{
    Attrs, Record, Sample, Sink, SinkStats, COUNTERS_METRIC_KEY, MAX_LINE_BYTES,
};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

pub(super) struct ResourceSampler {
    enabled: bool,
    sample_interval: Duration,
    cgroup_dir: Result<PathBuf, String>,
    sink: Arc<Sink>,
    collection_failures: AtomicU64,
}

impl ResourceSampler {
    pub(super) fn new(config: ResourceStatsConfig, resource_path: PathBuf) -> Self {
        let cgroup_dir = std::fs::read_to_string("/proc/self/cgroup")
            .map_err(|error| format!("/proc/self/cgroup: {error}"))
            .and_then(|contents| resolve_cgroup_dir(&contents, Path::new("/sys/fs/cgroup")));
        Self {
            enabled: config.enabled,
            sample_interval: Duration::from_millis(config.sample_interval_ms),
            cgroup_dir,
            sink: Arc::new(Sink::with_budget(
                resource_path,
                MAX_LINE_BYTES,
                config.max_disk_bytes,
            )),
            collection_failures: AtomicU64::new(0),
        }
    }

    pub(super) fn start(self: &Arc<Self>, tasks: &TaskTracker, shutdown: CancellationToken) {
        if !self.enabled {
            return;
        }
        let (shutdown_tx, shutdown_rx) = mpsc::channel();
        tasks.spawn(async move {
            shutdown.cancelled().await;
            let _ = shutdown_tx.send(());
        });
        let sampler = Arc::clone(self);
        tasks.spawn_blocking(move || loop {
            sampler.sample_once();
            match shutdown_rx.recv_timeout(sampler.sample_interval) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        });
    }

    pub(super) fn sample_once(&self) {
        let cgroup = match &self.cgroup_dir {
            Ok(path) => CgroupSample::read(path),
            Err(_) => {
                self.collection_failures.fetch_add(1, Ordering::Relaxed);
                return;
            }
        };
        if !cgroup.cgroup_available {
            self.collection_failures.fetch_add(1, Ordering::Relaxed);
        }
        let mut metrics = Attrs::new();
        metrics.insert("metrics_source".to_owned(), json!("sandbox_cgroup"));
        insert_option(&mut metrics, "cgroup_path", cgroup.cgroup_path);
        metrics.insert(
            "cgroup_available".to_owned(),
            Value::Bool(cgroup.cgroup_available),
        );
        insert_option(&mut metrics, "cgroup_error", cgroup.cgroup_error);
        insert_option(&mut metrics, "cpu_usec", cgroup.cpu_usage_usec);
        insert_option(&mut metrics, "mem_cur", cgroup.memory_current_bytes);
        insert_option(&mut metrics, "mem_max", cgroup.memory_max_bytes);
        insert_option(
            &mut metrics,
            "mem_max_unlimited",
            cgroup.memory_max_unlimited,
        );
        insert_option(&mut metrics, "io_rbytes", cgroup.io_read_bytes);
        insert_option(&mut metrics, "io_wbytes", cgroup.io_write_bytes);
        insert_option(&mut metrics, "pids_cur", cgroup.pids_current);
        let counters = ["cpu_usec", "io_rbytes", "io_wbytes"]
            .into_iter()
            .filter(|key| metrics.contains_key(*key))
            .collect::<Vec<_>>();
        metrics.insert(COUNTERS_METRIC_KEY.to_owned(), json!(counters));
        let record = Record::Sample(Sample {
            ts: unix_now_ms(),
            scope: "sandbox".to_owned(),
            metrics,
        });
        let _ = self.sink.append_strict(&record);
    }

    pub(super) fn sink_stats(&self) -> SinkStats {
        self.sink.stats()
    }

    pub(super) fn collection_failures(&self) -> u64 {
        self.collection_failures.load(Ordering::Relaxed)
    }
}

fn insert_option<T: serde::Serialize>(metrics: &mut Attrs, key: &str, value: Option<T>) {
    if let Some(value) = value.and_then(|value| serde_json::to_value(value).ok()) {
        metrics.insert(key.to_owned(), value);
    }
}

pub(super) fn resolve_cgroup_dir(contents: &str, root: &Path) -> Result<PathBuf, String> {
    let hierarchy = contents
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .ok_or_else(|| "unified cgroup entry missing in /proc/self/cgroup".to_owned())?;
    let relative = Path::new(hierarchy.trim_start_matches('/'));
    if relative
        .components()
        .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
    {
        return Err("invalid unified cgroup path".to_owned());
    }
    let mut target = root.join(relative);
    if target.file_name().is_some_and(|name| name == "_daemon") {
        target.pop();
    }
    Ok(target)
}

fn unix_now_ms() -> i64 {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}
