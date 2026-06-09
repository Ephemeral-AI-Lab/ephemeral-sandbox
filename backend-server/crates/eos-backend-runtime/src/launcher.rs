//! [`RunLauncher`] — accept a user request, persist backend lifecycle, drive
//! agent-core to completion, and finalize through the [`Reaper`].
//!
//! `launch` writes `run_meta(Accepted)` **before** spawning the run task (so a GET
//! right after the `202` always finds a row), registers a cancellation slot, then
//! spawns. The spawned task is the **sole finalizer**: it races the run future
//! against the request's [`CancellationToken`] via `select!`, so exactly one of
//! {completed, failed, cancelled} resolves and exactly one [`Reaper::reap`] runs.
//! This removes any completion/cancel double-finalize race without a separate
//! claim flag, and [`SandboxManager::release`] is idempotent as a further backstop.
//!
//! Cancellation is backend-local: `cancel` stashes a reason and fires the token.
//! The task then reaps as [`BackendRunStatus::Cancelled`] in `run_meta` only — the
//! backend never writes `cancelled` into agent-core `RequestStatus` (it never calls
//! any agent-core status setter; only `run_request`, inside the host, writes
//! agent-core terminal state, and only `Done`/`Failed`).

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use eos_agent_core::EngineEventSink;
use eos_backend_store::{RunMetaRepo, StoreError};
use eos_backend_types::{BackendRunStatus, CreateUserRequest, RunMeta};
use eos_types::{RequestId, SandboxId, UtcDateTime};

use crate::event_bus::EventBus;
use crate::host::{RunHost, RunOutcome};
use crate::reaper::{Disposition, Reaper};
use crate::sandbox_manager::SandboxManager;

/// Failure to accept a user request (the only synchronous failure is persisting
/// the initial `run_meta` row).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LaunchError {
    /// Writing the `run_meta(Accepted)` row failed.
    #[error("failed to persist run metadata")]
    Store(#[from] StoreError),
}

/// Result of a cancellation request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelOutcome {
    /// The run was active; cancellation was signalled and it will finalize as
    /// cancelled.
    Requested,
    /// No active run for that id (already finished, or never launched).
    NotFound,
}

/// Per-run cancellation slot, registered before the run task spawns so `cancel`
/// can always find it. The reason is stashed before the token fires so the task
/// observes it when it finalizes the cancelled disposition.
#[derive(Debug)]
struct RunSlot {
    token: CancellationToken,
    reason: Mutex<Option<String>>,
}

/// Shared launcher state behind an `Arc` so the spawned run task can drive it.
struct LauncherInner {
    host: Arc<dyn RunHost>,
    manager: Arc<SandboxManager>,
    run_meta: RunMetaRepo,
    event_bus: Arc<EventBus>,
    reaper: Reaper,
    /// In-flight runs keyed by request id, holding each run's cancellation slot.
    /// Registered before the run task spawns so `cancel` can never miss it.
    runs: Mutex<HashMap<RequestId, Arc<RunSlot>>>,
}

// `RunHost` has no `Debug` supertrait, so this cannot derive.
impl std::fmt::Debug for LauncherInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LauncherInner").finish_non_exhaustive()
    }
}

impl LauncherInner {
    /// The whole run lifecycle for one request: acquire the sandbox, mark running,
    /// run to completion (racing cancellation), then reap exactly once.
    async fn run_to_completion(
        self: Arc<Self>,
        request_id: RequestId,
        prompt: String,
        sandbox_id: Option<SandboxId>,
        callback: EngineEventSink,
        slot: Arc<RunSlot>,
    ) {
        // Acquire to completion BEFORE racing cancellation. `acquire` provisions a
        // fresh container outside the manager lock and only records the binding on
        // return; racing the token against that window could drop the future with a
        // container live in the host but untracked by the manager, which `release`
        // (keyed on a recorded binding) would never tear down. So provisioning is
        // non-cancellable — only the run itself races the token below.
        let binding = match self
            .manager
            .acquire(&request_id, sandbox_id.as_ref().map(SandboxId::as_str))
            .await
        {
            Ok(binding) => binding,
            Err(err) => {
                tracing::warn!(
                    request_id = request_id.as_str(),
                    error = %err,
                    "sandbox acquisition failed; run resolves as failed"
                );
                self.reaper.reap(&request_id, Disposition::Failed).await;
                self.runs.lock().remove(&request_id);
                return;
            }
        };
        // Mark Running once the sandbox is bound — before the agent-core request
        // row exists — so the API never observes (Accepted, agent=Running).
        if let Err(err) = self
            .run_meta
            .set_status(&request_id, BackendRunStatus::Running, None, None)
            .await
        {
            tracing::warn!(
                request_id = request_id.as_str(),
                error = %err,
                "failed to mark run running"
            );
        }

        let inner = self.clone();
        let run_request_id = request_id.clone();
        let run = async move {
            match inner
                .host
                .run(run_request_id, prompt, binding.sandbox_id, Some(callback))
                .await
            {
                RunOutcome::Done => Disposition::Done,
                RunOutcome::Failed => Disposition::Failed,
            }
        };

        // The task is the sole finalizer: whichever arm wins, exactly one reap runs.
        // `biased` checks cancellation first, so a pending cancel deterministically
        // wins and the run future is dropped (cancel-safe at its `.await` points).
        // The binding is already recorded, so a cancel here reaps -> release ->
        // teardown with no leak.
        let disposition = tokio::select! {
            biased;
            () = slot.token.cancelled() => Disposition::Cancelled(slot.reason.lock().clone()),
            disposition = run => disposition,
        };
        self.reaper.reap(&request_id, disposition).await;
        self.runs.lock().remove(&request_id);
    }
}

/// Backend run launcher and cancellation entry. Cheap to clone (one `Arc`).
#[derive(Debug, Clone)]
pub struct RunLauncher {
    inner: Arc<LauncherInner>,
}

impl RunLauncher {
    /// Assemble the launcher from the injected collaborators. The reaper shares the
    /// same manager / run-meta repo / event bus (all cheap clones over one state).
    #[must_use]
    pub fn new(
        host: Arc<dyn RunHost>,
        manager: Arc<SandboxManager>,
        run_meta: RunMetaRepo,
        event_bus: Arc<EventBus>,
    ) -> Self {
        let reaper = Reaper::new(manager.clone(), run_meta.clone(), event_bus.clone());
        Self {
            inner: Arc::new(LauncherInner {
                host,
                manager,
                run_meta,
                event_bus,
                reaper,
                runs: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Accept a user request: mint its id, persist `run_meta(Accepted)`, register
    /// the event stream + cancellation slot, and spawn the run task. Returns the
    /// minted request id (the `202` body). Must be called within a Tokio runtime.
    ///
    /// # Errors
    /// [`LaunchError::Store`] if the initial `run_meta` write fails (the run is not
    /// started).
    pub async fn launch(&self, request: CreateUserRequest) -> Result<RequestId, LaunchError> {
        let request_id = RequestId::new_v4();
        let CreateUserRequest {
            prompt,
            sandbox_args,
            client_meta,
        } = request;

        let label = client_meta.as_ref().and_then(|meta| meta.label.clone());
        let client_meta_json = client_meta
            .map(|meta| serde_json::to_value(meta).unwrap_or_else(|_| serde_json::json!({})))
            .unwrap_or_else(|| serde_json::json!({}));

        // run_meta is written BEFORE the run task starts (spec hard item): a GET
        // immediately after the 202 always finds at least the Accepted row.
        let meta = RunMeta {
            request_id: request_id.clone(),
            status: BackendRunStatus::Accepted,
            label,
            client_meta: client_meta_json,
            created_at: UtcDateTime::now(),
            finished_at: None,
            cancel_reason: None,
        };
        self.inner.run_meta.insert(&meta).await?;

        let callback = self.inner.event_bus.register(&request_id);
        let slot = Arc::new(RunSlot {
            token: CancellationToken::new(),
            reason: Mutex::new(None),
        });
        // Registered before spawn so `cancel` can never miss an in-flight run.
        self.inner
            .runs
            .lock()
            .insert(request_id.clone(), slot.clone());

        let sandbox_id = sandbox_args.and_then(|args| args.sandbox_id);
        let inner = self.inner.clone();
        let task_request_id = request_id.clone();
        tokio::spawn(async move {
            inner
                .run_to_completion(task_request_id, prompt, sandbox_id, callback, slot)
                .await;
        });

        Ok(request_id)
    }

    /// Request cancellation of an in-flight run. Stashes `reason` and fires the
    /// run's token; the run task finalizes it as cancelled (backend-local) and the
    /// reaper releases the sandbox. A run that has already finalized returns
    /// [`CancelOutcome::NotFound`].
    #[must_use]
    pub fn cancel(&self, request_id: &RequestId, reason: impl Into<String>) -> CancelOutcome {
        let Some(slot) = self.inner.runs.lock().get(request_id).cloned() else {
            return CancelOutcome::NotFound;
        };
        // Reason before token so the finalizing task observes it.
        *slot.reason.lock() = Some(reason.into());
        slot.token.cancel();
        CancelOutcome::Requested
    }
}

#[cfg(test)]
#[path = "../tests/launcher/mod.rs"]
mod tests;
