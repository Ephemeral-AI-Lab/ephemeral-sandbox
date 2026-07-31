use crate::lock::{assert_writer_lock_allows, WriterLockForbiddenWork};

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

const MAX_STORAGE_OWNERS: usize = 64;
const MAX_SAME_KEY_WAITERS: usize = 16;
const MAX_MATERIALIZATION_TARGETS: usize = 4;
pub(crate) const MAX_METADATA_QUEUE_ITEMS: usize = 16;
#[cfg(target_os = "linux")]
pub(crate) const MAX_METADATA_QUEUE_BYTES: usize = 64 * 1024;
const MAX_BYTE_PERMITS: usize = 64 * 1024 * 1024;
const MAX_OPERATION_FDS: usize = 16;
const MAX_STORAGE_FDS: usize = 64;
const MAX_MATERIALIZATION_WORKSPACE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const WAIT_SLICE: Duration = Duration::from_millis(50);

#[derive(Debug)]
pub(crate) enum SupervisorError {
    Cancelled,
    Deadline,
    ResourceExhausted(&'static str),
    MaterializationStorePeak {
        requested_bytes: u64,
        aggregate_bytes: u64,
        available_bytes: u64,
        capacity_bytes: u64,
        workspace_limit_bytes: u64,
    },
    Io(String),
    Poisoned(&'static str),
    ShuttingDown,
}

impl fmt::Display for SupervisorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => write!(formatter, "storage admission cancelled"),
            Self::Deadline => write!(formatter, "storage admission deadline expired"),
            Self::ResourceExhausted(resource) => {
                write!(formatter, "storage resource exhausted: {resource}")
            }
            Self::MaterializationStorePeak {
                requested_bytes,
                aggregate_bytes,
                available_bytes,
                capacity_bytes,
                workspace_limit_bytes,
            } => write!(
                formatter,
                "storage resource exhausted: predicted materialization store peak \
                 (requested_bytes={requested_bytes}, aggregate_bytes={aggregate_bytes}, \
                 available_bytes={available_bytes}, capacity_bytes={capacity_bytes}, \
                 workspace_limit_bytes={workspace_limit_bytes})"
            ),
            Self::Io(message) => write!(formatter, "storage admission I/O: {message}"),
            Self::Poisoned(resource) => {
                write!(formatter, "storage supervisor poisoned: {resource}")
            }
            Self::ShuttingDown => write!(formatter, "storage supervisor is shutting down"),
        }
    }
}

impl std::error::Error for SupervisorError {}

#[derive(Debug, Default)]
struct FlightState {
    waiters: usize,
}

#[derive(Debug, Default)]
struct SupervisorState {
    flights: HashMap<String, FlightState>,
    active_owners: usize,
    active_waiters: usize,
    active_materialization_targets: usize,
    active_metadata_queues: usize,
    byte_permits_in_use: usize,
    fd_permits_in_use: usize,
    workspace_bytes_in_use: u64,
    shutting_down: bool,
}

pub(crate) struct StorageSupervisor {
    storage_root: PathBuf,
    workspace_byte_limit: u64,
    allocation_unit: u64,
    state: Mutex<SupervisorState>,
    changed: Condvar,
    worker_pool: Mutex<Option<Arc<rayon::ThreadPool>>>,
}

impl fmt::Debug for StorageSupervisor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StorageSupervisor")
            .field("storage_root", &self.storage_root)
            .field("workspace_byte_limit", &self.workspace_byte_limit)
            .field("allocation_unit", &self.allocation_unit)
            .finish_non_exhaustive()
    }
}

impl StorageSupervisor {
    fn new(storage_root: &Path) -> Result<Self, SupervisorError> {
        let filesystem = filesystem_space(storage_root)?;
        let workspace_byte_limit =
            MAX_MATERIALIZATION_WORKSPACE_BYTES.min(filesystem.capacity_bytes / 10);
        if workspace_byte_limit == 0 {
            return Err(SupervisorError::ResourceExhausted(
                "materialization workspace capacity",
            ));
        }
        Ok(Self {
            storage_root: storage_root.to_path_buf(),
            workspace_byte_limit,
            allocation_unit: filesystem.allocation_unit,
            state: Mutex::new(SupervisorState::default()),
            changed: Condvar::new(),
            // Warm admission and native-seeded materialization never submit a
            // hydration job. Keep their fast path thread-free; the bounded
            // worker pool is created only when reconstruction actually needs
            // it.
            worker_pool: Mutex::new(None),
        })
    }

    pub(crate) const fn workspace_profile(&self) -> MaterializationWorkspaceProfile {
        MaterializationWorkspaceProfile {
            allocation_unit: self.allocation_unit,
            byte_limit: self.workspace_byte_limit,
        }
    }

    pub(crate) fn admit_materialization(
        self: &Arc<Self>,
        key: String,
        deadline: Instant,
        cancellation: &AtomicBool,
    ) -> Result<MaterializationAdmission, SupervisorError> {
        assert_writer_lock_allows(WriterLockForbiddenWork::PermitOrFlightWait);
        let mut state = self
            .state
            .lock()
            .map_err(|_| SupervisorError::Poisoned("state"))?;
        check_wait(deadline, cancellation)?;
        if state.shutting_down {
            return Err(SupervisorError::ShuttingDown);
        }
        if let Some(flight) = state.flights.get_mut(&key) {
            if flight.waiters >= MAX_SAME_KEY_WAITERS {
                return Err(SupervisorError::ResourceExhausted("same-key waiters"));
            }
            flight.waiters += 1;
            state.active_waiters += 1;
            return Ok(MaterializationAdmission::Waiter(MaterializationWaiter {
                supervisor: Arc::clone(self),
                key,
                active: true,
            }));
        }
        if state.active_owners >= MAX_STORAGE_OWNERS {
            return Err(SupervisorError::ResourceExhausted("nonterminal operations"));
        }
        state.flights.insert(key.clone(), FlightState::default());
        state.active_owners += 1;
        Ok(MaterializationAdmission::Owner(MaterializationOwner {
            supervisor: Arc::clone(self),
            key,
            active: true,
        }))
    }

    pub(crate) fn shutdown(
        &self,
        deadline: Instant,
        cancellation: &AtomicBool,
    ) -> Result<(), SupervisorError> {
        assert_writer_lock_allows(WriterLockForbiddenWork::WorkerJoin);
        let mut state = self
            .state
            .lock()
            .map_err(|_| SupervisorError::Poisoned("state"))?;
        state.shutting_down = true;
        self.changed.notify_all();
        while state.active_owners != 0
            || state.active_waiters != 0
            || state.active_materialization_targets != 0
            || state.active_metadata_queues != 0
            || !state.flights.is_empty()
        {
            check_wait(deadline, cancellation)?;
            let remaining = deadline.saturating_duration_since(Instant::now());
            let wait = remaining.min(WAIT_SLICE);
            let (next, _) = self
                .changed
                .wait_timeout(state, wait)
                .map_err(|_| SupervisorError::Poisoned("shutdown wait"))?;
            state = next;
        }
        drop(state);
        let worker_pool = self
            .worker_pool
            .lock()
            .map_err(|_| SupervisorError::Poisoned("storage worker pool"))?
            .take();
        drop(worker_pool);
        Ok(())
    }
}

pub(crate) enum MaterializationAdmission {
    Owner(MaterializationOwner),
    Waiter(MaterializationWaiter),
}

pub(crate) struct MaterializationOwner {
    supervisor: Arc<StorageSupervisor>,
    key: String,
    active: bool,
}

impl MaterializationOwner {
    pub(crate) fn acquire_target(
        &self,
        byte_permits: usize,
        deadline: Instant,
        cancellation: &AtomicBool,
    ) -> Result<MaterializationTarget, SupervisorError> {
        assert_writer_lock_allows(WriterLockForbiddenWork::PermitOrFlightWait);
        if byte_permits == 0 || byte_permits > MAX_BYTE_PERMITS {
            return Err(SupervisorError::ResourceExhausted("byte permits"));
        }
        let mut state = self
            .supervisor
            .state
            .lock()
            .map_err(|_| SupervisorError::Poisoned("state"))?;
        loop {
            check_wait(deadline, cancellation)?;
            if state.shutting_down {
                return Err(SupervisorError::ShuttingDown);
            }
            let target_available =
                state.active_materialization_targets < MAX_MATERIALIZATION_TARGETS;
            let bytes_available = state
                .byte_permits_in_use
                .checked_add(byte_permits)
                .is_some_and(|value| value <= MAX_BYTE_PERMITS);
            let fds_available = state
                .fd_permits_in_use
                .checked_add(MAX_OPERATION_FDS)
                .is_some_and(|value| value <= MAX_STORAGE_FDS);
            if target_available && bytes_available && fds_available {
                state.active_materialization_targets += 1;
                state.byte_permits_in_use += byte_permits;
                state.fd_permits_in_use += MAX_OPERATION_FDS;
                return Ok(MaterializationTarget {
                    supervisor: Arc::clone(&self.supervisor),
                    byte_permits,
                    fd_permits: MAX_OPERATION_FDS,
                    workspace_bytes: 0,
                    active: true,
                });
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            let wait = remaining.min(WAIT_SLICE);
            let (next, _) = self
                .supervisor
                .changed
                .wait_timeout(state, wait)
                .map_err(|_| SupervisorError::Poisoned("target wait"))?;
            state = next;
        }
    }
}

impl Drop for MaterializationOwner {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let mut state = self
            .supervisor
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.flights.remove(&self.key);
        state.active_owners = state.active_owners.saturating_sub(1);
        self.active = false;
        drop(state);
        self.supervisor.changed.notify_all();
    }
}

pub(crate) struct MaterializationTarget {
    supervisor: Arc<StorageSupervisor>,
    byte_permits: usize,
    fd_permits: usize,
    workspace_bytes: u64,
    active: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MaterializationWorkspaceProfile {
    pub(crate) allocation_unit: u64,
    pub(crate) byte_limit: u64,
}

impl MaterializationTarget {
    pub(crate) const fn reserved_permits(&self) -> (usize, usize) {
        (self.byte_permits, self.fd_permits)
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn metadata_queue<T>(
        &self,
        capacity: usize,
    ) -> Result<MetadataQueue<T>, SupervisorError> {
        assert_writer_lock_allows(WriterLockForbiddenWork::PermitOrFlightWait);
        if !self.active {
            return Err(SupervisorError::ResourceExhausted(
                "inactive materialization target",
            ));
        }
        if !(1..=MAX_METADATA_QUEUE_ITEMS).contains(&capacity) {
            return Err(SupervisorError::ResourceExhausted("metadata queue items"));
        }
        let mut state = self
            .supervisor
            .state
            .lock()
            .map_err(|_| SupervisorError::Poisoned("state"))?;
        if state.shutting_down {
            return Err(SupervisorError::ShuttingDown);
        }
        state.active_metadata_queues += 1;
        drop(state);
        Ok(MetadataQueue::new(Arc::clone(&self.supervisor), capacity))
    }

    pub(crate) fn reserve_workspace(
        &mut self,
        workspace_bytes: u64,
        deadline: Instant,
        cancellation: &AtomicBool,
    ) -> Result<(), SupervisorError> {
        assert_writer_lock_allows(WriterLockForbiddenWork::PermitOrFlightWait);
        if workspace_bytes == 0 || workspace_bytes > self.supervisor.workspace_byte_limit {
            return Err(SupervisorError::ResourceExhausted(
                "materialization workspace reservation",
            ));
        }
        if self.workspace_bytes != 0 {
            return Err(SupervisorError::ResourceExhausted(
                "duplicate materialization workspace reservation",
            ));
        }
        let mut state = self
            .supervisor
            .state
            .lock()
            .map_err(|_| SupervisorError::Poisoned("state"))?;
        loop {
            check_wait(deadline, cancellation)?;
            if state.shutting_down {
                return Err(SupervisorError::ShuttingDown);
            }
            let aggregate = state
                .workspace_bytes_in_use
                .checked_add(workspace_bytes)
                .ok_or(SupervisorError::ResourceExhausted(
                    "materialization workspace reservation",
                ))?;
            if aggregate <= self.supervisor.workspace_byte_limit {
                drop(state);
                let filesystem = filesystem_space(&self.supervisor.storage_root)?;
                let available = filesystem.available_bytes;
                state = self
                    .supervisor
                    .state
                    .lock()
                    .map_err(|_| SupervisorError::Poisoned("state"))?;
                if state.shutting_down {
                    return Err(SupervisorError::ShuttingDown);
                }
                let aggregate = state
                    .workspace_bytes_in_use
                    .checked_add(workspace_bytes)
                    .ok_or(SupervisorError::ResourceExhausted(
                        "materialization workspace reservation",
                    ))?;
                if aggregate <= self.supervisor.workspace_byte_limit {
                    if aggregate > available {
                        return Err(SupervisorError::MaterializationStorePeak {
                            requested_bytes: workspace_bytes,
                            aggregate_bytes: aggregate,
                            available_bytes: available,
                            capacity_bytes: filesystem.capacity_bytes,
                            workspace_limit_bytes: self.supervisor.workspace_byte_limit,
                        });
                    }
                    state.workspace_bytes_in_use = aggregate;
                    self.workspace_bytes = workspace_bytes;
                    return Ok(());
                }
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            let wait = remaining.min(WAIT_SLICE);
            let (next, _) = self
                .supervisor
                .changed
                .wait_timeout(state, wait)
                .map_err(|_| SupervisorError::Poisoned("workspace wait"))?;
            state = next;
        }
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn run_on_workers<R, F>(&self, work: F) -> Result<R, SupervisorError>
    where
        R: Send,
        F: FnOnce() -> R + Send,
    {
        assert_writer_lock_allows(WriterLockForbiddenWork::WorkerJoin);
        if !self.active {
            return Err(SupervisorError::ResourceExhausted(
                "inactive materialization target",
            ));
        }
        let state = self
            .supervisor
            .state
            .lock()
            .map_err(|_| SupervisorError::Poisoned("state"))?;
        if state.shutting_down {
            return Err(SupervisorError::ShuttingDown);
        }
        let mut worker_pool_guard = self
            .supervisor
            .worker_pool
            .lock()
            .map_err(|_| SupervisorError::Poisoned("storage worker pool"))?;
        let worker_pool = match worker_pool_guard.as_ref() {
            Some(worker_pool) => Arc::clone(worker_pool),
            None => {
                let created = Arc::new(
                    rayon::ThreadPoolBuilder::new()
                        .num_threads(MAX_MATERIALIZATION_TARGETS)
                        .thread_name(|index| format!("layerstack-storage-{index}"))
                        .build()
                        .map_err(|error| SupervisorError::Io(error.to_string()))?,
                );
                *worker_pool_guard = Some(Arc::clone(&created));
                created
            }
        };
        drop(worker_pool_guard);
        drop(state);
        Ok(worker_pool.install(work))
    }
}

#[cfg(target_os = "linux")]
pub(crate) struct MetadataQueue<T> {
    supervisor: Arc<StorageSupervisor>,
    items: Vec<T>,
    capacity: usize,
    encoded_bytes: usize,
    active: bool,
}

#[cfg(target_os = "linux")]
impl<T> MetadataQueue<T> {
    fn new(supervisor: Arc<StorageSupervisor>, capacity: usize) -> Self {
        Self {
            supervisor,
            items: Vec::with_capacity(capacity),
            capacity,
            encoded_bytes: 0,
            active: true,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub(crate) fn is_full(&self) -> bool {
        self.items.len() == self.capacity
    }

    pub(crate) fn encoded_bytes(&self) -> usize {
        self.encoded_bytes
    }

    pub(crate) fn push(&mut self, item: T, encoded_bytes: usize) -> Result<(), SupervisorError> {
        assert_writer_lock_allows(WriterLockForbiddenWork::PermitOrFlightWait);
        let state = self
            .supervisor
            .state
            .lock()
            .map_err(|_| SupervisorError::Poisoned("state"))?;
        if state.shutting_down {
            return Err(SupervisorError::ShuttingDown);
        }
        drop(state);
        if self.is_full() {
            return Err(SupervisorError::ResourceExhausted("metadata queue items"));
        }
        let next_bytes = self
            .encoded_bytes
            .checked_add(encoded_bytes)
            .ok_or(SupervisorError::ResourceExhausted("metadata queue bytes"))?;
        if next_bytes > MAX_METADATA_QUEUE_BYTES {
            return Err(SupervisorError::ResourceExhausted("metadata queue bytes"));
        }
        self.items.push(item);
        self.encoded_bytes = next_bytes;
        Ok(())
    }

    pub(crate) fn take(&mut self) -> Vec<T> {
        self.encoded_bytes = 0;
        std::mem::replace(&mut self.items, Vec::with_capacity(self.capacity))
    }
}

#[cfg(target_os = "linux")]
impl<T> Drop for MetadataQueue<T> {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        self.items.clear();
        self.encoded_bytes = 0;
        let mut state = self
            .supervisor
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.active_metadata_queues = state.active_metadata_queues.saturating_sub(1);
        self.active = false;
        drop(state);
        self.supervisor.changed.notify_all();
    }
}

impl Drop for MaterializationTarget {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let mut state = self
            .supervisor
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.active_materialization_targets =
            state.active_materialization_targets.saturating_sub(1);
        state.byte_permits_in_use = state.byte_permits_in_use.saturating_sub(self.byte_permits);
        state.fd_permits_in_use = state.fd_permits_in_use.saturating_sub(self.fd_permits);
        state.workspace_bytes_in_use = state
            .workspace_bytes_in_use
            .saturating_sub(self.workspace_bytes);
        self.active = false;
        drop(state);
        self.supervisor.changed.notify_all();
    }
}

pub(crate) struct MaterializationWaiter {
    supervisor: Arc<StorageSupervisor>,
    key: String,
    active: bool,
}

impl MaterializationWaiter {
    pub(crate) fn wait(
        mut self,
        deadline: Instant,
        cancellation: &AtomicBool,
    ) -> Result<(), SupervisorError> {
        assert_writer_lock_allows(WriterLockForbiddenWork::PermitOrFlightWait);
        let supervisor = Arc::clone(&self.supervisor);
        let mut state = supervisor
            .state
            .lock()
            .map_err(|_| SupervisorError::Poisoned("state"))?;
        while state.flights.contains_key(&self.key) {
            check_wait(deadline, cancellation)?;
            if state.shutting_down {
                return Err(SupervisorError::ShuttingDown);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            let wait = remaining.min(WAIT_SLICE);
            let (next, _) = supervisor
                .changed
                .wait_timeout(state, wait)
                .map_err(|_| SupervisorError::Poisoned("flight wait"))?;
            state = next;
        }
        self.release_waiter(&mut state);
        Ok(())
    }

    fn release_waiter(&mut self, state: &mut SupervisorState) {
        if !self.active {
            return;
        }
        if let Some(flight) = state.flights.get_mut(&self.key) {
            flight.waiters = flight.waiters.saturating_sub(1);
        }
        state.active_waiters = state.active_waiters.saturating_sub(1);
        self.active = false;
    }
}

impl Drop for MaterializationWaiter {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let supervisor = Arc::clone(&self.supervisor);
        let mut state = supervisor
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.release_waiter(&mut state);
        drop(state);
        supervisor.changed.notify_all();
    }
}

pub(crate) fn shared_supervisor_for_root(
    storage_root: &Path,
) -> Result<Arc<StorageSupervisor>, SupervisorError> {
    let key = super::fs::canonical_key(storage_root);
    let mut supervisors = supervisor_registry()
        .lock()
        .map_err(|_| SupervisorError::Poisoned("registry"))?;
    supervisors.retain(|_, supervisor| supervisor.strong_count() > 0);
    if let Some(supervisor) = supervisors.get(&key).and_then(Weak::upgrade) {
        return Ok(supervisor);
    }
    if supervisors.len() >= MAX_STORAGE_OWNERS {
        return Err(SupervisorError::ResourceExhausted(
            "storage supervisor owners",
        ));
    }
    let supervisor = Arc::new(StorageSupervisor::new(storage_root)?);
    supervisors.insert(key, Arc::downgrade(&supervisor));
    Ok(supervisor)
}

fn supervisor_registry() -> &'static Mutex<HashMap<String, Weak<StorageSupervisor>>> {
    static SUPERVISORS: OnceLock<Mutex<HashMap<String, Weak<StorageSupervisor>>>> = OnceLock::new();
    SUPERVISORS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn check_wait(deadline: Instant, cancellation: &AtomicBool) -> Result<(), SupervisorError> {
    if cancellation.load(Ordering::Acquire) {
        return Err(SupervisorError::Cancelled);
    }
    if Instant::now() >= deadline {
        return Err(SupervisorError::Deadline);
    }
    Ok(())
}

struct FilesystemSpace {
    allocation_unit: u64,
    capacity_bytes: u64,
    available_bytes: u64,
}

fn filesystem_space(storage_root: &Path) -> Result<FilesystemSpace, SupervisorError> {
    let stat = rustix::fs::statvfs(storage_root)
        .map_err(|error| SupervisorError::Io(error.to_string()))?;
    let allocation_unit = if stat.f_frsize == 0 {
        stat.f_bsize
    } else {
        stat.f_frsize
    };
    if allocation_unit == 0 {
        return Err(SupervisorError::ResourceExhausted(
            "filesystem allocation unit",
        ));
    }
    let capacity_bytes =
        stat.f_blocks
            .checked_mul(allocation_unit)
            .ok_or(SupervisorError::ResourceExhausted(
                "filesystem capacity accounting",
            ))?;
    let available_bytes =
        stat.f_bavail
            .checked_mul(allocation_unit)
            .ok_or(SupervisorError::ResourceExhausted(
                "filesystem available-space accounting",
            ))?;
    Ok(FilesystemSpace {
        allocation_unit,
        capacity_bytes,
        available_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn worker_pool_is_lazy_and_still_honors_shutdown() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock precedes Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "layerstack-supervisor-lazy-pool-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&root).expect("create test storage root");

        let supervisor =
            Arc::new(StorageSupervisor::new(&root).expect("create storage supervisor"));
        assert!(
            supervisor
                .worker_pool
                .lock()
                .expect("lock worker pool")
                .is_none(),
            "construction must not create worker threads"
        );

        #[cfg(target_os = "linux")]
        {
            let cancellation = AtomicBool::new(false);
            let deadline = Instant::now() + Duration::from_secs(5);
            let owner = match supervisor
                .admit_materialization("test-key".to_owned(), deadline, &cancellation)
                .expect("admit materialization")
            {
                MaterializationAdmission::Owner(owner) => owner,
                MaterializationAdmission::Waiter(_) => panic!("first admission became a waiter"),
            };
            let target = owner
                .acquire_target(1024, deadline, &cancellation)
                .expect("acquire materialization target");
            assert!(
                supervisor
                    .worker_pool
                    .lock()
                    .expect("lock worker pool")
                    .is_none(),
                "admission without hydration must remain thread-free"
            );

            assert_eq!(
                target
                    .run_on_workers(|| 42_u8)
                    .expect("run hydration worker"),
                42
            );
            assert!(
                supervisor
                    .worker_pool
                    .lock()
                    .expect("lock worker pool")
                    .is_some(),
                "the first real worker job must initialize the pool"
            );

            drop(target);
            drop(owner);
            supervisor
                .shutdown(Instant::now() + Duration::from_secs(5), &cancellation)
                .expect("shutdown storage supervisor");
            assert!(
                supervisor
                    .worker_pool
                    .lock()
                    .expect("lock worker pool")
                    .is_none(),
                "shutdown must release an initialized pool"
            );
        }
        std::fs::remove_dir(&root).expect("remove test storage root");
    }
}
