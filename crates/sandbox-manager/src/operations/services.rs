use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::export_apply::ExportApplyCaps;
use crate::{
    ResourceRingStore, ResourceSample, SandboxDaemonClient, SandboxDaemonInstaller, SandboxRuntime,
    SandboxStore, WorkspaceRootPolicy,
};

pub(crate) const MAX_RESOURCE_HISTORY_MS: i64 = 600_000;
const RESOURCE_SAMPLE_INTERVAL: Duration = Duration::from_secs(2);

/// `manager.observability_snapshot` fan-out limits; the gateway overwrites
/// the default with the configured values before serving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObservabilitySnapshotLimits {
    pub max_concurrent_requests: usize,
    pub timeout_ms: u64,
}

impl Default for ObservabilitySnapshotLimits {
    fn default() -> Self {
        Self {
            max_concurrent_requests: 8,
            timeout_ms: 1_500,
        }
    }
}

pub struct ManagerServices {
    pub store: Arc<SandboxStore>,
    pub runtime: Arc<dyn SandboxRuntime>,
    pub daemon_installer: Arc<dyn SandboxDaemonInstaller>,
    pub daemon_client: Arc<dyn SandboxDaemonClient>,
    /// `manager.export` apply caps; the gateway overwrites the default with
    /// the configured values before serving.
    pub export_caps: ExportApplyCaps,
    pub snapshot_limits: ObservabilitySnapshotLimits,
    pub workspace_roots: WorkspaceRootPolicy,
    pub(crate) resource_ring: Arc<ResourceRingStore>,
    resource_sampler: Mutex<Option<ResourceSamplerWorker>>,
}

impl ManagerServices {
    #[must_use]
    pub fn new(
        store: Arc<SandboxStore>,
        runtime: Arc<dyn SandboxRuntime>,
        daemon_installer: Arc<dyn SandboxDaemonInstaller>,
        daemon_client: Arc<dyn SandboxDaemonClient>,
    ) -> Self {
        let resource_ring = Arc::new(ResourceRingStore::for_store(&store));
        Self {
            store,
            runtime,
            daemon_installer,
            daemon_client,
            export_caps: ExportApplyCaps::default(),
            snapshot_limits: ObservabilitySnapshotLimits::default(),
            workspace_roots: WorkspaceRootPolicy::default(),
            resource_ring,
            resource_sampler: Mutex::new(None),
        }
    }

    pub fn start_resource_sampler(&self) {
        let mut worker = self
            .resource_sampler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if worker.is_some() {
            return;
        }
        let signal = Arc::new(ResourceSamplerSignal::default());
        let thread_signal = Arc::clone(&signal);
        let store = Arc::clone(&self.store);
        let runtime = Arc::clone(&self.runtime);
        let resource_ring = Arc::clone(&self.resource_ring);
        let handle = std::thread::Builder::new()
            .name("sandbox-resource-sampler".to_owned())
            .spawn(move || {
                while !thread_signal.cancelled.load(Ordering::Acquire) {
                    sample_ready_sandboxes(&store, &runtime, &resource_ring);
                    let guard = thread_signal
                        .wake_lock
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let _ = thread_signal
                        .wake
                        .wait_timeout(guard, RESOURCE_SAMPLE_INTERVAL);
                }
            });
        if let Ok(handle) = handle {
            *worker = Some(ResourceSamplerWorker {
                signal,
                handle: Some(handle),
            });
        }
    }

    #[must_use]
    pub fn sample_resources_once(&self) -> usize {
        sample_ready_sandboxes(&self.store, &self.runtime, &self.resource_ring)
    }

    #[must_use]
    pub fn resource_ring(&self) -> &ResourceRingStore {
        &self.resource_ring
    }
}

impl Drop for ManagerServices {
    fn drop(&mut self) {
        let worker = self
            .resource_sampler
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(mut worker) = worker.take() {
            worker.signal.cancelled.store(true, Ordering::Release);
            worker.signal.wake.notify_all();
            if let Some(handle) = worker.handle.take() {
                let _ = handle.join();
            }
        }
    }
}

#[derive(Default)]
struct ResourceSamplerSignal {
    cancelled: AtomicBool,
    wake_lock: Mutex<()>,
    wake: Condvar,
}

struct ResourceSamplerWorker {
    signal: Arc<ResourceSamplerSignal>,
    handle: Option<JoinHandle<()>>,
}

fn sample_ready_sandboxes(
    store: &SandboxStore,
    runtime: &Arc<dyn SandboxRuntime>,
    resource_ring: &ResourceRingStore,
) -> usize {
    let Ok(ids) = store.ready_ids() else {
        return 0;
    };
    runtime
        .read_sandbox_resource_metrics_batch(&ids)
        .into_iter()
        .filter_map(|(id, metrics)| {
            let metrics = metrics.ok()?;
            let sample = ResourceSample {
                sampled_at_unix_ms: now_unix_ms(),
                metrics,
            };
            resource_ring
                .append_if(&id, sample, || store.is_ready(&id).is_ok_and(|ready| ready))
                .ok()
                .filter(|appended| *appended)
        })
        .count()
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}
