//! Shared Phase 5 test doubles and helpers, declared crate-internally from
//! `lib.rs` so the fakes can implement the `pub(crate)` [`SandboxTeardown`] seam
//! and drive `SandboxManager::with_seams`. Reused by the reaper and launcher test
//! modules.
#![allow(clippy::unwrap_used)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;
use tokio::sync::Notify;

use eos_agent_core::EngineEventSink;
use eos_backend_store::BackendStore;
use eos_backend_types::RunMeta;
use eos_sandbox_port::{
    DaemonOp, RequestProvisioner, RequestSandboxBinding, SandboxPortError, SandboxProvisionError,
    SandboxTransport,
};
use eos_types::{JsonObject, RequestId, SandboxId};

use crate::host::{RunHost, RunOutcome};
use crate::sandbox_manager::{SandboxManager, SandboxManagerError, SandboxTeardown};

// --- sandbox host fakes ------------------------------------------------------

/// A provision gate: `prepare_for_run` signals `entered` then parks on `release`,
/// so a test can fire cancellation deterministically while a provision is in
/// flight (the acquire phase), reproducing the cancel-during-acquire race.
#[derive(Debug, Clone, Default)]
pub struct ProvisionGate {
    pub entered: Arc<Notify>,
    pub release: Arc<Notify>,
}

/// Mints `sb-for-<request>` for a fresh acquisition, echoes the explicit id
/// otherwise. An optional [`ProvisionGate`] parks the provision mid-flight.
#[derive(Debug, Default)]
pub struct FakeProvisioner {
    pub fail: bool,
    pub gate: Option<ProvisionGate>,
}

#[async_trait]
impl RequestProvisioner for FakeProvisioner {
    async fn prepare_for_run(
        &self,
        request_id: &RequestId,
        sandbox_id: Option<&str>,
    ) -> Result<RequestSandboxBinding, SandboxProvisionError> {
        if self.fail {
            return Err(SandboxProvisionError::new("provision boom"));
        }
        // Simulate a slow host create that a test can cancel during.
        if let Some(gate) = &self.gate {
            gate.entered.notify_one();
            gate.release.notified().await;
        }
        let sandbox_id: SandboxId = match sandbox_id {
            Some(id) => id.parse().unwrap(),
            None => format!("sb-for-{request_id}").parse().unwrap(),
        };
        Ok(RequestSandboxBinding {
            sandbox_id,
            request_id: request_id.clone(),
        })
    }
}

/// Records every destroy so tests can assert teardown ran exactly once.
#[derive(Debug, Default)]
pub struct FakeTeardown {
    pub destroyed: Mutex<Vec<SandboxId>>,
}

#[async_trait]
impl SandboxTeardown for FakeTeardown {
    async fn destroy(&self, id: &SandboxId) -> Result<(), SandboxManagerError> {
        self.destroyed.lock().push(id.clone());
        Ok(())
    }
}

/// Unused by lifecycle tests; present only to satisfy the gateway.
#[derive(Debug, Default)]
pub struct FakeTransport;

#[async_trait]
impl SandboxTransport for FakeTransport {
    async fn call(
        &self,
        _sandbox_id: &SandboxId,
        _op: DaemonOp,
        _payload: JsonObject,
        _timeout_s: u32,
    ) -> Result<JsonObject, SandboxPortError> {
        Err(SandboxPortError::transport(None, "fake transport unused"))
    }
}

/// Build a manager over fakes, returning the shared teardown spy.
pub fn manager(
    max_owned: usize,
    destroy_on_finish: bool,
) -> (Arc<SandboxManager>, Arc<FakeTeardown>) {
    manager_with(FakeProvisioner::default(), max_owned, destroy_on_finish)
}

/// Build a manager whose provisioner parks mid-provision on the returned gate, so
/// a test can fire cancellation during the acquire phase (M1 leak regression).
pub fn gated_manager(
    max_owned: usize,
    destroy_on_finish: bool,
) -> (Arc<SandboxManager>, Arc<FakeTeardown>, ProvisionGate) {
    let gate = ProvisionGate::default();
    let (manager, teardown) = manager_with(
        FakeProvisioner {
            fail: false,
            gate: Some(gate.clone()),
        },
        max_owned,
        destroy_on_finish,
    );
    (manager, teardown, gate)
}

/// Build a manager whose provisioner always fails, so a test can drive the
/// launcher's sandbox-acquisition-failure arm.
pub fn failing_manager(
    max_owned: usize,
    destroy_on_finish: bool,
) -> (Arc<SandboxManager>, Arc<FakeTeardown>) {
    manager_with(
        FakeProvisioner {
            fail: true,
            gate: None,
        },
        max_owned,
        destroy_on_finish,
    )
}

fn manager_with(
    provisioner: FakeProvisioner,
    max_owned: usize,
    destroy_on_finish: bool,
) -> (Arc<SandboxManager>, Arc<FakeTeardown>) {
    let teardown = Arc::new(FakeTeardown::default());
    let transport: Arc<dyn SandboxTransport> = Arc::new(FakeTransport);
    let manager = SandboxManager::with_seams(
        Arc::new(provisioner),
        transport,
        teardown.clone(),
        max_owned,
        destroy_on_finish,
    );
    (Arc::new(manager), teardown)
}

// --- run host fake -----------------------------------------------------------

/// A [`RunHost`] that resolves to a configured outcome, optionally parking on a
/// gate so a test can cancel it mid-run. Records start/completion and the bound
/// sandbox id for assertions.
#[derive(Debug)]
pub struct FakeRunHost {
    outcome: RunOutcome,
    gate: Option<Arc<Notify>>,
    started: AtomicBool,
    completed: AtomicBool,
    seen_sandbox: Mutex<Option<SandboxId>>,
}

impl FakeRunHost {
    /// Resolves immediately to `outcome`.
    pub fn resolving(outcome: RunOutcome) -> Arc<Self> {
        Arc::new(Self {
            outcome,
            gate: None,
            started: AtomicBool::new(false),
            completed: AtomicBool::new(false),
            seen_sandbox: Mutex::new(None),
        })
    }

    /// Parks on `gate` (awaiting `notify`) before resolving — lets a test observe
    /// the run in flight and cancel it.
    pub fn gated(outcome: RunOutcome, gate: Arc<Notify>) -> Arc<Self> {
        Arc::new(Self {
            outcome,
            gate: Some(gate),
            started: AtomicBool::new(false),
            completed: AtomicBool::new(false),
            seen_sandbox: Mutex::new(None),
        })
    }

    /// Whether `run` was entered (sandbox acquired, host invoked).
    pub fn started(&self) -> bool {
        self.started.load(Ordering::SeqCst)
    }

    /// Whether `run` resolved (false if it was cancelled mid-run).
    pub fn completed(&self) -> bool {
        self.completed.load(Ordering::SeqCst)
    }

    /// The sandbox id the launcher bound, if `run` was entered.
    pub fn seen_sandbox(&self) -> Option<SandboxId> {
        self.seen_sandbox.lock().clone()
    }
}

#[async_trait]
impl RunHost for FakeRunHost {
    async fn run(
        &self,
        _request_id: RequestId,
        _prompt: String,
        sandbox_id: SandboxId,
        _on_event: Option<EngineEventSink>,
    ) -> RunOutcome {
        self.started.store(true, Ordering::SeqCst);
        *self.seen_sandbox.lock() = Some(sandbox_id);
        if let Some(gate) = &self.gate {
            gate.notified().await;
        }
        self.completed.store(true, Ordering::SeqCst);
        self.outcome
    }
}

// --- store + polling helpers -------------------------------------------------

/// A temp-backed [`BackendStore`]; keep the returned `TempDir` alive for the test.
pub async fn temp_store() -> (BackendStore, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let store = BackendStore::open(tmp.path().join("backend.db"))
        .await
        .unwrap();
    (store, tmp)
}

/// Parse a request id from a test literal.
pub fn rid(s: &str) -> RequestId {
    s.parse().unwrap()
}

/// Poll `run_meta` until the run is finalized (`finished_at` set), returning it.
pub async fn await_run_finished(store: &BackendStore, request_id: &RequestId) -> RunMeta {
    for _ in 0..500 {
        if let Some(meta) = store.run_meta().get(request_id).await.unwrap() {
            if meta.finished_at.is_some() {
                return meta;
            }
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    panic!("run {request_id} did not finalize in time");
}

/// Poll until the fake host's `run` has been entered.
pub async fn await_host_started(host: &FakeRunHost) {
    for _ in 0..500 {
        if host.started() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    panic!("host run did not start in time");
}
