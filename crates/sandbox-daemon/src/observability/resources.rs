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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::AtomicUsize;
    use std::sync::{Condvar, Mutex};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    fn cgroup_fixture(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sandbox-daemon-resource-{label}-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).expect("create cgroup fixture");
        fs::write(dir.join("cpu.stat"), "usage_usec 10\n").expect("write cpu.stat");
        fs::write(dir.join("memory.current"), "20\n").expect("write memory.current");
        fs::write(dir.join("memory.max"), "max\n").expect("write memory.max");
        fs::write(dir.join("io.stat"), "8:0 rbytes=30 wbytes=40\n").expect("write io.stat");
        fs::write(dir.join("pids.current"), "5\n").expect("write pids.current");
        dir
    }

    fn test_sampler(
        cgroup_dir: PathBuf,
        resource_path: PathBuf,
        interval: Duration,
    ) -> ResourceSampler {
        ResourceSampler {
            enabled: true,
            sample_interval: interval,
            cgroup_dir: Ok(cgroup_dir),
            sink: Arc::new(Sink::with_budget(resource_path, MAX_LINE_BYTES, 128 * 1024)),
            collection_failures: AtomicU64::new(0),
        }
    }

    #[test]
    fn daemon_leaf_resolves_to_aggregate_parent_once() {
        assert_eq!(
            resolve_cgroup_dir("0::/ephemeral/sbox/_daemon\n", Path::new("/cgroup")),
            Ok(PathBuf::from("/cgroup/ephemeral/sbox"))
        );
    }

    #[test]
    fn ordinary_and_root_cgroups_resolve_without_rewriting() {
        assert_eq!(
            resolve_cgroup_dir("0::/ephemeral/sbox\n", Path::new("/cgroup")),
            Ok(PathBuf::from("/cgroup/ephemeral/sbox"))
        );
        assert_eq!(
            resolve_cgroup_dir("0::/\n", Path::new("/cgroup")),
            Ok(PathBuf::from("/cgroup"))
        );
    }

    #[test]
    fn malformed_or_escaping_cgroup_entries_are_rejected() {
        assert!(resolve_cgroup_dir("1:name=/legacy\n", Path::new("/cgroup")).is_err());
        assert!(resolve_cgroup_dir("0::/../escape\n", Path::new("/cgroup")).is_err());
    }

    #[test]
    fn storage_failure_is_counted_without_escaping_the_sampler() {
        let cgroup_dir = cgroup_fixture("storage-failure-cgroup");
        let blocked_parent = cgroup_dir.parent().expect("fixture parent").join(format!(
            "{}-blocked",
            cgroup_dir
                .file_name()
                .expect("fixture directory name")
                .to_string_lossy()
        ));
        fs::write(&blocked_parent, "not a directory").expect("create blocked parent file");
        let sampler = test_sampler(
            cgroup_dir.clone(),
            blocked_parent.join("resources.ndjson"),
            Duration::from_millis(1),
        );

        sampler.sample_once();

        assert_eq!(sampler.collection_failures(), 0);
        assert_eq!(sampler.sink_stats().dropped_storage, 1);
        let _ = fs::remove_file(blocked_parent);
        let _ = fs::remove_dir_all(cgroup_dir);
    }

    #[tokio::test]
    async fn shutdown_joins_sampler_and_prevents_later_appends() {
        let cgroup_dir = cgroup_fixture("shutdown-cgroup");
        let resource_path = cgroup_dir.parent().expect("fixture parent").join(format!(
            "{}-resources.ndjson",
            cgroup_dir
                .file_name()
                .expect("fixture directory name")
                .to_string_lossy()
        ));
        let sampler = Arc::new(test_sampler(
            cgroup_dir.clone(),
            resource_path.clone(),
            Duration::from_millis(1),
        ));
        let tasks = TaskTracker::new();
        let shutdown = CancellationToken::new();
        sampler.start(&tasks, shutdown.clone());

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let line_count = fs::read_to_string(&resource_path)
                    .map(|contents| contents.lines().count())
                    .unwrap_or(0);
                if line_count >= 2 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("sampler writes before timeout");

        shutdown.cancel();
        tasks.close();
        tasks.wait().await;
        let bytes_after_join = fs::read(&resource_path).expect("resource bytes after join");
        tokio::time::sleep(Duration::from_millis(5)).await;
        assert_eq!(
            fs::read(&resource_path).expect("resource bytes after quiet window"),
            bytes_after_join,
            "joined sampler must not append after shutdown"
        );

        let _ = fs::remove_file(&resource_path);
        let _ = fs::remove_file(format!("{}.lock", resource_path.display()));
        let _ = fs::remove_dir_all(cgroup_dir);
    }

    #[test]
    fn sampler_owns_one_blocking_worker_and_pressure_workers_retire() {
        let live_threads = Arc::new(AtomicUsize::new(0));
        let started_threads = Arc::clone(&live_threads);
        let stopped_threads = Arc::clone(&live_threads);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .max_blocking_threads(4)
            .thread_keep_alive(Duration::from_millis(10))
            .on_thread_start(move || {
                started_threads.fetch_add(1, Ordering::SeqCst);
            })
            .on_thread_stop(move || {
                stopped_threads.fetch_sub(1, Ordering::SeqCst);
            })
            .enable_all()
            .build()
            .expect("build sampler regression runtime");

        runtime.block_on(async {
            let cgroup_dir = cgroup_fixture("blocking-worker-cgroup");
            let resource_path = cgroup_dir.parent().expect("fixture parent").join(format!(
                "{}-resources.ndjson",
                cgroup_dir
                    .file_name()
                    .expect("fixture directory name")
                    .to_string_lossy()
            ));
            let sampler = Arc::new(test_sampler(
                cgroup_dir.clone(),
                resource_path.clone(),
                Duration::from_millis(1),
            ));
            let tasks = TaskTracker::new();
            let shutdown = CancellationToken::new();
            sampler.start(&tasks, shutdown.clone());

            tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    if fs::metadata(&resource_path).is_ok() {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("sampler starts before timeout");

            tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    if live_threads.load(Ordering::SeqCst) == 2 {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("runtime has one core worker and one sampler worker");

            let release = Arc::new((Mutex::new(false), Condvar::new()));
            let started_pressure = Arc::new(AtomicUsize::new(0));
            let pressure = (0..4)
                .map(|_| {
                    let release = Arc::clone(&release);
                    let started_pressure = Arc::clone(&started_pressure);
                    tokio::task::spawn_blocking(move || {
                        started_pressure.fetch_add(1, Ordering::SeqCst);
                        let (released, wake) = &*release;
                        let mut released = released.lock().expect("lock pressure release");
                        while !*released {
                            released = wake.wait(released).expect("wait for pressure release");
                        }
                    })
                })
                .collect::<Vec<_>>();
            tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    if started_pressure.load(Ordering::SeqCst) == 3 {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("pressure activates the three non-sampler blocking slots");
            tokio::time::sleep(Duration::from_millis(20)).await;
            assert_eq!(
                started_pressure.load(Ordering::SeqCst),
                3,
                "the owned sampler worker must not accept unrelated blocking work"
            );

            {
                let (released, wake) = &*release;
                *released.lock().expect("lock pressure release") = true;
                wake.notify_all();
            }
            for task in pressure {
                task.await.expect("pressure worker joins");
            }

            tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    if live_threads.load(Ordering::SeqCst) == 2 {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .expect("pressure workers retire after keepalive");

            shutdown.cancel();
            tasks.close();
            tasks.wait().await;
            tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    if live_threads.load(Ordering::SeqCst) == 1 {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .expect("sampler worker retires after shutdown");

            let _ = fs::remove_file(&resource_path);
            let _ = fs::remove_file(format!("{}.lock", resource_path.display()));
            let _ = fs::remove_dir_all(cgroup_dir);
        });
    }
}
