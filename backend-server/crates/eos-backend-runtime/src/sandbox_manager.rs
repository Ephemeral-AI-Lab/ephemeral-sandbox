//! [`SandboxManager`] — backend-owned sandbox lifecycle, refcounting, and the
//! [`SandboxGateway`] implementation the agent-core request service is wired against.
//!
//! The manager is the single owner of sandbox *setup/destroy policy*: it composes
//! the Docker/daemon host (`eos-sandbox-host`) behind one shared
//! [`ProviderRegistry`] → [`DaemonClient`] → [`SandboxLifecycle`] chain, then
//! exposes the two narrow port handles the gateway promises — the daemon
//! transport ([`SandboxGateway::transport`]) and the request provisioner
//! ([`SandboxGateway::provisioner`]) — which therefore share that one registry by
//! construction (the invariant is *structural*, not test-asserted).
//!
//! All mutable lifecycle state lives in one [`ManagerInner`] behind an `Arc`. The
//! provisioner handed to request orchestration and the manager the backend retains for
//! `release`/`delete`/`view` are clones of that same `Arc`, so acquisition (which
//! the runtime drives through the gateway provisioner) and release/teardown
//! (which the backend reaper drives through the manager) touch the same maps.
//! This matters because each request service call acquires through
//! [`SandboxGateway::provisioner`] while teardown uses the retained manager.
//!
//! ## Refcount and retention model
//!
//! Each tracked sandbox holds a set of *active* request refs plus a count of
//! *retained* refs; `ref_count = active + retained`.
//!
//! - A **fresh** sandbox (no `sandbox_id` supplied) is backend-owned: its
//!   `owner_request_id` is the creating request and it inherits
//!   `SandboxConfig::destroy_on_finish`. When its last active ref is released and
//!   it is ephemeral, it is destroyed.
//! - A **bound, pre-existing** sandbox (caller supplied a `sandbox_id`) is *not*
//!   backend-owned: it gets a retained ref pinning it against destruction and
//!   `destroy_on_finish = false`. **Intentional v1 policy:** because that retained
//!   pin is never released (there is no unpin API in v1), a caller-supplied
//!   sandbox stays in [`SandboxState::Retained`] after its runs finish and is
//!   never destroyed by `DELETE` — the backend only manages its binding, it does
//!   not own its teardown.
//!
//! Acquisition is **idempotent per request**: a request that already holds a ref
//! returns its existing binding without incrementing again, and `release` keyed on
//! the request decrements exactly once, so the count stays exact regardless of
//! whether Phase 5's launcher pre-acquires before the runtime bootstrap does.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;

use eos_backend_config::SandboxConfig;
use eos_backend_types::{SandboxState, SandboxView};
use eos_sandbox_host::{
    DaemonClient, DockerProviderAdapter, ProviderRegistry, RequestSandboxProvisioner,
    SandboxLifecycle,
};
use eos_sandbox_port::{
    RequestProvisioner, RequestSandboxBinding, SandboxGateway, SandboxProvisionError,
    SandboxTransport,
};
use eos_types::{RequestId, SandboxId, UtcDateTime};

/// Why a `DELETE` was refused: the sandbox still has active runs, or it is
/// retained against destruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteRejection {
    /// One or more runs are currently bound to the sandbox.
    Active,
    /// The sandbox is retained (caller-supplied / pinned) and not backend-owned.
    Retained,
}

impl std::fmt::Display for DeleteRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Active => "active",
            Self::Retained => "retained",
        })
    }
}

/// Errors raised by [`SandboxManager`] lifecycle operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SandboxManagerError {
    /// The Docker provider could not be connected at construction time.
    #[error("docker provider connection failed: {0}")]
    Connect(String),
    /// Provisioning (fresh create or explicit start) failed.
    #[error("sandbox provisioning failed: {0}")]
    Provision(String),
    /// The backend-owned sandbox budget is exhausted.
    #[error("sandbox capacity exceeded: {current}/{max} backend-owned sandboxes")]
    CapacityExceeded {
        /// Backend-owned sandboxes currently tracked.
        current: usize,
        /// Configured upper bound.
        max: usize,
    },
    /// A `DELETE` was refused because the sandbox is still referenced.
    #[error("cannot delete sandbox {sandbox_id} while it is {reason}")]
    DeleteRejected {
        /// The sandbox the delete targeted.
        sandbox_id: SandboxId,
        /// Whether it was active or retained.
        reason: DeleteRejection,
    },
    /// A `DELETE` targeted a sandbox the manager does not track.
    #[error("unknown sandbox {0}")]
    UnknownSandbox(SandboxId),
    /// Host teardown of the container failed.
    #[error("sandbox teardown failed: {0}")]
    Teardown(String),
}

/// Backend-internal destroy seam.
///
/// The host's container teardown ([`SandboxLifecycle::delete`]) sits behind the
/// **sealed** `ProviderAdapter`, so it cannot be faked from another crate. This
/// one-method seam lets the manager's refcount/delete logic be exercised with a
/// fake while production wires [`LifecycleTeardown`]. It is deliberately *not* a
/// port in `eos-sandbox-port`: only the backend needs it.
#[async_trait]
pub(crate) trait SandboxTeardown: Send + Sync + std::fmt::Debug {
    /// Destroy the container backing `id`.
    async fn destroy(&self, id: &SandboxId) -> Result<(), SandboxManagerError>;
}

/// Production teardown over the shared [`SandboxLifecycle`].
#[derive(Debug)]
struct LifecycleTeardown {
    lifecycle: Arc<SandboxLifecycle>,
}

#[async_trait]
impl SandboxTeardown for LifecycleTeardown {
    async fn destroy(&self, id: &SandboxId) -> Result<(), SandboxManagerError> {
        self.lifecycle
            .delete(id)
            .await
            .map_err(|err| SandboxManagerError::Teardown(err.to_string()))
    }
}

/// Per-sandbox lifecycle and refcount record.
#[derive(Debug)]
struct SandboxEntry {
    state: SandboxState,
    owner_request_id: Option<RequestId>,
    active: BTreeSet<RequestId>,
    retained_refs: u32,
    destroy_on_finish: bool,
    created_at: UtcDateTime,
    last_used_at: UtcDateTime,
}

impl SandboxEntry {
    /// A freshly bound entry. `fresh` (no caller `sandbox_id`) means backend-owned
    /// and ephemeral-by-config; otherwise it is a retained, caller-supplied pin.
    fn newly_bound(
        fresh: bool,
        request_id: &RequestId,
        config_destroy_on_finish: bool,
        now: UtcDateTime,
    ) -> Self {
        Self {
            state: SandboxState::Active,
            owner_request_id: fresh.then(|| request_id.clone()),
            active: BTreeSet::new(),
            retained_refs: u32::from(!fresh),
            destroy_on_finish: fresh && config_destroy_on_finish,
            created_at: now,
            last_used_at: now,
        }
    }

    fn ref_count(&self) -> u32 {
        self.active.len() as u32 + self.retained_refs
    }

    fn to_view(&self, sandbox_id: &SandboxId) -> SandboxView {
        SandboxView {
            sandbox_id: sandbox_id.clone(),
            state: self.state,
            owner_request_id: self.owner_request_id.clone(),
            active_request_ids: self.active.iter().cloned().collect(),
            ref_count: self.ref_count(),
            created_at: self.created_at,
            last_used_at: self.last_used_at,
            destroy_on_finish: self.destroy_on_finish,
        }
    }
}

/// In-memory lifecycle state: tracked sandboxes plus the request→sandbox index
/// that lets `release` decrement by request id without the caller knowing which
/// sandbox the request bound.
#[derive(Debug, Default)]
struct ManagerState {
    sandboxes: HashMap<SandboxId, SandboxEntry>,
    by_request: HashMap<RequestId, SandboxId>,
    pending_owned: usize,
}

impl ManagerState {
    fn owned_count(&self) -> usize {
        self.sandboxes
            .values()
            .filter(|entry| entry.owner_request_id.is_some())
            .count()
    }

    fn owned_or_pending_count(&self) -> usize {
        self.owned_count() + self.pending_owned
    }

    fn reserve_owned_slot(
        &mut self,
        max_owned_sandboxes: usize,
    ) -> Result<(), SandboxManagerError> {
        let current = self.owned_or_pending_count();
        if current >= max_owned_sandboxes {
            return Err(SandboxManagerError::CapacityExceeded {
                current,
                max: max_owned_sandboxes,
            });
        }
        self.pending_owned += 1;
        Ok(())
    }

    fn release_owned_slot(&mut self) {
        self.pending_owned = self.pending_owned.saturating_sub(1);
    }
}

struct OwnedSlotReservation<'a> {
    state: &'a Mutex<ManagerState>,
    active: bool,
}

impl<'a> OwnedSlotReservation<'a> {
    fn new(state: &'a Mutex<ManagerState>) -> Self {
        Self {
            state,
            active: true,
        }
    }

    fn release_with(&mut self, state: &mut ManagerState) {
        if self.active {
            state.release_owned_slot();
            self.active = false;
        }
    }
}

impl Drop for OwnedSlotReservation<'_> {
    fn drop(&mut self) {
        if self.active {
            self.state.lock().release_owned_slot();
        }
    }
}

/// Shared lifecycle state behind the gateway and the retained manager handle.
struct ManagerInner {
    provisioner: Arc<dyn RequestProvisioner>,
    transport: Arc<dyn SandboxTransport>,
    teardown: Arc<dyn SandboxTeardown>,
    max_owned_sandboxes: usize,
    destroy_on_finish: bool,
    state: Mutex<ManagerState>,
}

// `SandboxTransport` has no `Debug` supertrait, so this cannot derive.
impl std::fmt::Debug for ManagerInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagerInner")
            .field("max_owned_sandboxes", &self.max_owned_sandboxes)
            .field("destroy_on_finish", &self.destroy_on_finish)
            .finish_non_exhaustive()
    }
}

impl ManagerInner {
    /// Resolve a request's sandbox binding and increment its refcount.
    ///
    /// Idempotent per `request_id`: a request already holding a ref returns its
    /// existing binding without re-provisioning or double-counting. Provisioning
    /// runs with no lock held; bookkeeping takes the lock only for synchronous
    /// map mutation (`await_holding_lock`).
    async fn acquire(
        &self,
        request_id: &RequestId,
        sandbox_id: Option<&str>,
    ) -> Result<RequestSandboxBinding, SandboxManagerError> {
        let existing_sandbox_id = sandbox_id.map(str::trim).filter(|id| !id.is_empty());
        let reserve_owned = existing_sandbox_id.is_none();
        let mut reservation = None;

        // Fast path: already bound (idempotent), destroying explicit sandbox, or
        // over the fresh-create budget. A fresh create reserves capacity before
        // the async host round-trip so the cap is a real upper bound under
        // concurrent provisions.
        {
            let mut state = self.state.lock();
            if let Some(sandbox_id) = state.by_request.get(request_id) {
                return Ok(binding(sandbox_id, request_id));
            }
            if let Some(id) = existing_sandbox_id.and_then(parse_sandbox_id) {
                reject_destroying(&state, &id)?;
            }
            if reserve_owned {
                state.reserve_owned_slot(self.max_owned_sandboxes)?;
                reservation = Some(OwnedSlotReservation::new(&self.state));
            }
        }

        // Provision outside the lock (async host round-trip).
        let resolved = match self
            .provisioner
            .prepare_for_run(request_id, existing_sandbox_id)
            .await
        {
            Ok(binding) => binding,
            Err(err) => {
                return Err(SandboxManagerError::Provision(err.message));
            }
        };

        // Bookkeeping under the lock (no await).
        let mut orphaned_fresh = None;
        let committed = {
            let mut state = self.state.lock();
            if let Some(reservation) = reservation.as_mut() {
                reservation.release_with(&mut state);
            }
            if let Some(sandbox_id) = state.by_request.get(request_id) {
                if reserve_owned && *sandbox_id != resolved.sandbox_id {
                    orphaned_fresh = Some(resolved.sandbox_id.clone());
                }
                binding(sandbox_id, request_id)
            } else {
                reject_destroying(&state, &resolved.sandbox_id)?;
                let now = UtcDateTime::now();
                let entry = state
                    .sandboxes
                    .entry(resolved.sandbox_id.clone())
                    .or_insert_with(|| {
                        SandboxEntry::newly_bound(
                            reserve_owned,
                            request_id,
                            self.destroy_on_finish,
                            now,
                        )
                    });
                entry.active.insert(request_id.clone());
                entry.last_used_at = now;
                entry.state = SandboxState::Active;
                state
                    .by_request
                    .insert(request_id.clone(), resolved.sandbox_id.clone());
                resolved
            }
        };
        if let Some(sandbox_id) = orphaned_fresh {
            if let Err(err) = self.teardown.destroy(&sandbox_id).await {
                tracing::warn!(
                    sandbox = sandbox_id.as_str(),
                    request_id = request_id.as_str(),
                    %err,
                    "duplicate fresh acquire created an untracked sandbox; teardown failed"
                );
            }
        }
        Ok(committed)
    }

    /// Release a request's ref exactly once and, when its sandbox falls idle and
    /// is ephemeral, destroy it. A request that holds no ref is a no-op (so a
    /// racing reaper/cancel cannot double-release).
    async fn release(&self, request_id: &RequestId) {
        let destroy_target = {
            let mut state = self.state.lock();
            let Some(sandbox_id) = state.by_request.remove(request_id) else {
                return;
            };
            let now = UtcDateTime::now();
            let mut target = None;
            if let Some(entry) = state.sandboxes.get_mut(&sandbox_id) {
                entry.active.remove(request_id);
                entry.last_used_at = now;
                if entry.active.is_empty() {
                    if entry.retained_refs == 0 && entry.destroy_on_finish {
                        entry.state = SandboxState::Destroying;
                        target = Some(sandbox_id);
                    } else if entry.retained_refs > 0 {
                        entry.state = SandboxState::Retained;
                    } else {
                        entry.state = SandboxState::Ready;
                    }
                }
            }
            target
        };
        if let Some(sandbox_id) = destroy_target {
            if let Err(err) = self.run_teardown(&sandbox_id).await {
                tracing::warn!(
                    sandbox = sandbox_id.as_str(),
                    %err,
                    "destroy-on-finish teardown failed; sandbox left for retry"
                );
            }
        }
    }

    /// Destroy `id` and, on success, drop its tracking entry. On failure the entry
    /// is left (in `Destroying`) so a `DELETE` can retry.
    async fn run_teardown(&self, sandbox_id: &SandboxId) -> Result<(), SandboxManagerError> {
        self.teardown.destroy(sandbox_id).await?;
        self.state.lock().sandboxes.remove(sandbox_id);
        Ok(())
    }
}

fn binding(sandbox_id: &SandboxId, request_id: &RequestId) -> RequestSandboxBinding {
    RequestSandboxBinding {
        sandbox_id: sandbox_id.clone(),
        request_id: request_id.clone(),
    }
}

fn parse_sandbox_id(value: &str) -> Option<SandboxId> {
    value.parse().ok()
}

fn reject_destroying(
    state: &ManagerState,
    sandbox_id: &SandboxId,
) -> Result<(), SandboxManagerError> {
    if matches!(
        state.sandboxes.get(sandbox_id).map(|entry| entry.state),
        Some(SandboxState::Destroying)
    ) {
        return Err(SandboxManagerError::Provision(format!(
            "sandbox {} is being destroyed",
            sandbox_id.as_str()
        )));
    }
    Ok(())
}

/// Backend-owned sandbox lifecycle manager and [`SandboxGateway`] implementation.
///
/// Construct the production manager with [`SandboxManager::with_docker`], inject
/// it into the agent-core request service via `AgentCoreServiceDependencies`, and retain
/// the same handle (an `Arc<SandboxManager>`) to drive [`SandboxManager::release`],
/// [`SandboxManager::delete`], [`SandboxManager::view`], and
/// [`SandboxManager::list`].
#[derive(Debug)]
pub struct SandboxManager {
    inner: Arc<ManagerInner>,
}

impl SandboxManager {
    /// Compose the production manager: connect the Docker provider, seed one
    /// shared registry, and derive the daemon transport, the host provisioner, and
    /// the teardown seam from it. `eosd_artifact_dir` holds the pinned
    /// `eosd-linux-{arch}` binaries the lifecycle uploads (the sandbox `dist`
    /// dir) — *not* the layer-stack root.
    ///
    /// # Errors
    /// [`SandboxManagerError::Connect`] if the Docker Engine cannot be reached.
    pub fn with_docker(
        config: &SandboxConfig,
        eosd_artifact_dir: PathBuf,
    ) -> Result<Self, SandboxManagerError> {
        let registry = Arc::new(ProviderRegistry::new());
        let docker = DockerProviderAdapter::connect()
            .map_err(|err| SandboxManagerError::Connect(err.to_string()))?;
        registry.seed(Arc::new(docker));
        let daemon = Arc::new(DaemonClient::new(registry));
        let lifecycle = Arc::new(SandboxLifecycle::new(daemon.clone(), eosd_artifact_dir));
        let provisioner: Arc<dyn RequestProvisioner> =
            Arc::new(RequestSandboxProvisioner::with_default_snapshot(
                lifecycle.clone(),
                config.default_snapshot.as_deref(),
            ));
        let transport: Arc<dyn SandboxTransport> = daemon;
        let teardown: Arc<dyn SandboxTeardown> = Arc::new(LifecycleTeardown { lifecycle });
        Ok(Self::with_seams(
            provisioner,
            transport,
            teardown,
            config.max_owned_sandboxes,
            config.destroy_on_finish,
        ))
    }

    /// Assemble a manager from pre-built port/teardown handles. The production
    /// path is [`SandboxManager::with_docker`]; this seam exists so the in-memory
    /// refcount/delete logic can be exercised with fakes.
    pub(crate) fn with_seams(
        provisioner: Arc<dyn RequestProvisioner>,
        transport: Arc<dyn SandboxTransport>,
        teardown: Arc<dyn SandboxTeardown>,
        max_owned_sandboxes: usize,
        destroy_on_finish: bool,
    ) -> Self {
        Self {
            inner: Arc::new(ManagerInner {
                provisioner,
                transport,
                teardown,
                max_owned_sandboxes,
                destroy_on_finish,
                state: Mutex::new(ManagerState::default()),
            }),
        }
    }

    /// Acquire (resolve + refcount) the sandbox binding for a run. Idempotent per
    /// `request_id`. This is the typed-error sibling of the gateway provisioner's
    /// `prepare_for_run`.
    ///
    /// # Errors
    /// [`SandboxManagerError::Provision`] on a host failure, or
    /// [`SandboxManagerError::CapacityExceeded`] when the fresh-create budget is
    /// exhausted.
    pub async fn acquire(
        &self,
        request_id: &RequestId,
        sandbox_id: Option<&str>,
    ) -> Result<RequestSandboxBinding, SandboxManagerError> {
        self.inner.acquire(request_id, sandbox_id).await
    }

    /// Release a request's sandbox ref exactly once, destroying the sandbox when it
    /// falls idle and is ephemeral. Safe to call for a request that holds no ref.
    pub async fn release(&self, request_id: &RequestId) {
        self.inner.release(request_id).await;
    }

    /// Destroy a backend-owned sandbox. Rejected while the sandbox has active runs
    /// or is retained; never requires or returns daemon auth material.
    ///
    /// # Errors
    /// [`SandboxManagerError::UnknownSandbox`] if untracked,
    /// [`SandboxManagerError::DeleteRejected`] if active/retained, or
    /// [`SandboxManagerError::Teardown`] if host teardown fails.
    pub async fn delete(&self, sandbox_id: &SandboxId) -> Result<(), SandboxManagerError> {
        {
            let mut state = self.inner.state.lock();
            let Some(entry) = state.sandboxes.get_mut(sandbox_id) else {
                return Err(SandboxManagerError::UnknownSandbox(sandbox_id.clone()));
            };
            if !entry.active.is_empty() {
                return Err(SandboxManagerError::DeleteRejected {
                    sandbox_id: sandbox_id.clone(),
                    reason: DeleteRejection::Active,
                });
            }
            if entry.retained_refs > 0 {
                return Err(SandboxManagerError::DeleteRejected {
                    sandbox_id: sandbox_id.clone(),
                    reason: DeleteRejection::Retained,
                });
            }
            entry.state = SandboxState::Destroying;
        }
        self.inner.run_teardown(sandbox_id).await
    }

    /// The sanitized view of one tracked sandbox, if present.
    #[must_use]
    pub fn view(&self, sandbox_id: &SandboxId) -> Option<SandboxView> {
        self.inner
            .state
            .lock()
            .sandboxes
            .get(sandbox_id)
            .map(|entry| entry.to_view(sandbox_id))
    }

    /// All tracked sandboxes as sanitized views, newest-created first.
    #[must_use]
    pub fn list(&self) -> Vec<SandboxView> {
        let state = self.inner.state.lock();
        let mut views: Vec<SandboxView> = state
            .sandboxes
            .iter()
            .map(|(id, entry)| entry.to_view(id))
            .collect();
        views.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| a.sandbox_id.as_str().cmp(b.sandbox_id.as_str()))
        });
        views
    }
}

impl SandboxGateway for SandboxManager {
    fn transport(&self) -> Arc<dyn SandboxTransport> {
        self.inner.transport.clone()
    }

    fn provisioner(&self) -> Arc<dyn RequestProvisioner> {
        Arc::new(GatewayProvisioner {
            inner: self.inner.clone(),
        })
    }
}

/// The provisioner the gateway hands to agent-core request orchestration: its `prepare_for_run` is
/// the manager's [`ManagerInner::acquire`], so the runtime's bootstrap binding is
/// the refcount-acquiring path. Holds a clone of the shared inner state.
#[derive(Debug)]
struct GatewayProvisioner {
    inner: Arc<ManagerInner>,
}

#[async_trait]
impl RequestProvisioner for GatewayProvisioner {
    async fn prepare_for_run(
        &self,
        request_id: &RequestId,
        sandbox_id: Option<&str>,
    ) -> Result<RequestSandboxBinding, SandboxProvisionError> {
        self.inner
            .acquire(request_id, sandbox_id)
            .await
            .map_err(|err| SandboxProvisionError::new(err.to_string()))
    }
}

// Test bodies live under the crate `tests/` tree (spec §Backend Test Layout);
// included as a private `#[cfg(test)]` submodule so they keep crate-internal
// access to `with_seams` and the `SandboxTeardown` seam.
#[cfg(test)]
#[path = "../tests/sandbox_manager/mod.rs"]
mod tests;
