use std::collections::BTreeSet;
use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sandbox_benchmark::checks::{
    bounded_evidence, check_definition, fold_correctness, CheckEvidenceItem, CheckResult,
    CheckVerdict,
};
use sandbox_benchmark::definitions::definition;
use sandbox_benchmark::executors::{
    expand_operation_plan, ExecutorError, OperationOutcome, RuntimeInvocation, TeardownResult,
    Verification,
};
use sandbox_benchmark::model::{
    ExpandedOperationCell, OperationId, OperationPlan, ProductAccess, ProductOperation,
    ResolvedIsolationPolicy, WorkspaceAction,
};
use sandbox_benchmark::resources::{
    Availability, MetricCollector, MetricDefinition, MonotonicInstant, ResourceReading,
    SamplingInterval, RUNNER_RSS,
};
use sandbox_benchmark::scheduler::{
    drive_static_lifecycle, StaticLifecycleConfig, StaticLifecycleDispatch, StaticLifecycleIssue,
    StaticRequestCompletion,
};
use tokio_util::sync::CancellationToken;

use crate::support::TestRoot;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FakeEndpoint {
    ExecCommand,
    FileRead,
    FileWrite,
    FileEdit,
    FileBlame,
    CreateWorkspace,
    DestroyWorkspace,
    SquashLayerstacks,
}

impl FakeEndpoint {
    const fn product_access(self) -> ProductAccess {
        match self {
            Self::ExecCommand => ProductAccess::PublicGateway(ProductOperation::ExecCommand),
            Self::FileRead => ProductAccess::PublicGateway(ProductOperation::FileRead),
            Self::FileWrite => ProductAccess::PublicGateway(ProductOperation::FileWrite),
            Self::FileEdit => ProductAccess::PublicGateway(ProductOperation::FileEdit),
            Self::FileBlame => ProductAccess::PublicGateway(ProductOperation::FileBlame),
            Self::CreateWorkspace => {
                ProductAccess::InternalWorkspace(WorkspaceAction::CreateNoOpSession)
            }
            // This is a test-only destructive lifecycle shape. Production has
            // no destroy operation in the closed v1 catalog.
            Self::DestroyWorkspace => {
                ProductAccess::InternalWorkspace(WorkspaceAction::CreateNoOpSession)
            }
            Self::SquashLayerstacks => {
                ProductAccess::PublicGateway(ProductOperation::SquashLayerstacks)
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FakeDispatch {
    operation: OperationId,
    endpoint: FakeEndpoint,
    isolation: ResolvedIsolationPolicy,
    prepared_sessions: u32,
    invocations: u32,
}

fn fake_dispatch(cell: &ExpandedOperationCell) -> FakeDispatch {
    match cell {
        ExpandedOperationCell::ExecCommand(cell) => FakeDispatch {
            operation: OperationId::ExecCommand,
            endpoint: FakeEndpoint::ExecCommand,
            isolation: cell.resolved_isolation,
            prepared_sessions: 1,
            invocations: cell.concurrent_requests,
        },
        ExpandedOperationCell::FileRead(cell) => FakeDispatch {
            operation: OperationId::FileRead,
            endpoint: FakeEndpoint::FileRead,
            isolation: cell.resolved_isolation,
            prepared_sessions: u32::from(matches!(
                cell.source,
                sandbox_benchmark::executors::files::FileReadSource::Session
            )),
            invocations: cell.concurrent_requests,
        },
        ExpandedOperationCell::FileWrite(cell) => FakeDispatch {
            operation: OperationId::FileWrite,
            endpoint: FakeEndpoint::FileWrite,
            isolation: cell.resolved_isolation,
            prepared_sessions: u32::from(matches!(
                cell.destination,
                sandbox_benchmark::executors::files::MutationDestination::Session
            )),
            invocations: cell.concurrent_requests,
        },
        ExpandedOperationCell::FileEdit(cell) => FakeDispatch {
            operation: OperationId::FileEdit,
            endpoint: FakeEndpoint::FileEdit,
            isolation: cell.resolved_isolation,
            prepared_sessions: u32::from(matches!(
                cell.destination,
                sandbox_benchmark::executors::files::MutationDestination::Session
            )),
            invocations: cell.concurrent_requests,
        },
        ExpandedOperationCell::FileBlame(cell) => FakeDispatch {
            operation: OperationId::FileBlame,
            endpoint: FakeEndpoint::FileBlame,
            isolation: cell.resolved_isolation,
            prepared_sessions: 0,
            invocations: cell.concurrent_requests,
        },
        ExpandedOperationCell::CreateWorkspace(cell) => FakeDispatch {
            operation: OperationId::CreateWorkspace,
            endpoint: FakeEndpoint::CreateWorkspace,
            isolation: cell.resolved_isolation,
            prepared_sessions: 0,
            invocations: cell.workspace_count,
        },
        ExpandedOperationCell::SquashLayerstack(cell) => FakeDispatch {
            operation: OperationId::SquashLayerstack,
            endpoint: FakeEndpoint::SquashLayerstacks,
            isolation: cell.resolved_isolation,
            prepared_sessions: cell.live_sessions,
            invocations: 1,
        },
    }
}

#[derive(Debug, Default)]
struct FakeState {
    calls: Vec<(FakeEndpoint, u32)>,
    live_sessions: BTreeSet<String>,
    maximum_live_sessions: usize,
    phases: Vec<&'static str>,
}

#[derive(Clone, Debug, Default)]
struct FakeAdapter {
    state: Arc<Mutex<FakeState>>,
}

impl FakeAdapter {
    fn prepare(&self, session_count: u32) {
        let mut state = self.state.lock().expect("fake state lock");
        state.phases.push("setup");
        for index in 0..session_count {
            assert!(state.live_sessions.insert(format!("prepared-{index}")));
        }
        state.maximum_live_sessions = state.live_sessions.len();
    }

    fn invoke(&self, endpoint: FakeEndpoint, index: u32) {
        let mut state = self.state.lock().expect("fake state lock");
        match endpoint {
            FakeEndpoint::CreateWorkspace => {
                assert!(state
                    .live_sessions
                    .insert(format!("measured-session-{index}")));
                state.maximum_live_sessions =
                    state.maximum_live_sessions.max(state.live_sessions.len());
            }
            FakeEndpoint::DestroyWorkspace => {
                assert!(state.live_sessions.remove(&format!("prepared-{index}")));
            }
            _ => {}
        }
        state.calls.push((endpoint, index));
    }

    fn mark_operation_complete(&self) {
        self.state
            .lock()
            .expect("fake state lock")
            .phases
            .push("operation");
    }

    fn verify(&self, expected_calls: u32) {
        let mut state = self.state.lock().expect("fake state lock");
        assert_eq!(state.calls.len(), expected_calls as usize);
        state.phases.push("verify");
    }

    fn teardown(&self) -> usize {
        let mut state = self.state.lock().expect("fake state lock");
        let destroyed = state.live_sessions.len();
        state.live_sessions.clear();
        state.phases.push("teardown");
        destroyed
    }
}

#[derive(Clone)]
struct FakeContext {
    adapter: FakeAdapter,
}

#[derive(Debug, Clone, Copy)]
struct FakeCell {
    dispatch: FakeDispatch,
    generated_invocations: u32,
    fail_check: bool,
}

#[derive(Debug)]
struct FakePrepared;

#[derive(Debug, Clone)]
struct FakeInvocation {
    endpoint: FakeEndpoint,
    index: u32,
    request_id: String,
}

impl RuntimeInvocation for FakeInvocation {
    fn request_id(&self) -> &str {
        &self.request_id
    }
}

#[derive(Debug)]
struct FakeOutcome;

struct FakeLifecycle;

impl StaticLifecycleDispatch for FakeLifecycle {
    type Context = FakeContext;
    type Cell = FakeCell;
    type Prepared = FakePrepared;
    type Invocation = FakeInvocation;
    type Outcome = FakeOutcome;

    async fn prepare(
        context: &Self::Context,
        cell: &Self::Cell,
    ) -> Result<Self::Prepared, ExecutorError> {
        context.adapter.prepare(cell.dispatch.prepared_sessions);
        Ok(FakePrepared)
    }

    fn invocations(
        _prepared: &Self::Prepared,
        cell: &Self::Cell,
    ) -> Result<Vec<Self::Invocation>, ExecutorError> {
        Ok((0..cell.generated_invocations)
            .map(|index| FakeInvocation {
                endpoint: cell.dispatch.endpoint,
                index,
                request_id: format!("fake-{:?}-{index}", cell.dispatch.operation),
            })
            .collect())
    }

    async fn invoke_one(context: &Self::Context, invocation: Self::Invocation) -> Self::Outcome {
        context
            .adapter
            .invoke(invocation.endpoint, invocation.index);
        FakeOutcome
    }

    async fn verify(
        context: &Self::Context,
        _prepared: &Self::Prepared,
        cell: &Self::Cell,
        outcomes: &[Self::Outcome],
    ) -> Result<Verification, ExecutorError> {
        context.adapter.mark_operation_complete();
        context.adapter.verify(cell.generated_invocations);
        let check_id = definition(cell.dispatch.operation).checks[0].id;
        let verdict = if cell.fail_check {
            CheckVerdict::Fail
        } else {
            CheckVerdict::Pass
        };
        Ok(Verification {
            checks: vec![CheckResult {
                id: check_id,
                semantic_revision: check_definition(check_id).semantic_revision,
                operation_id: cell.dispatch.operation,
                cell_id: format!("fake-{:?}", cell.dispatch.operation),
                trial_id: "fake-trial-0".to_owned(),
                request_id: outcomes
                    .first()
                    .map(|_| format!("fake-{:?}-0", cell.dispatch.operation)),
                verdict,
                duration_ns: 1,
                evidence: bounded_evidence(
                    check_id,
                    vec![CheckEvidenceItem {
                        expected: "fake expected".to_owned(),
                        actual: if cell.fail_check {
                            "fake failed".to_owned()
                        } else {
                            "fake expected".to_owned()
                        },
                        artifact_id: None,
                    }],
                ),
            }],
        })
    }

    async fn teardown(context: &Self::Context, _prepared: &mut Self::Prepared) -> TeardownResult {
        let destroyed = context.adapter.teardown();
        TeardownResult {
            checks: Vec::new(),
            expected_destroyed_sessions: u32::try_from(destroyed).expect("fake session count fits"),
            destroyed_sessions: u32::try_from(destroyed).expect("fake session count fits"),
            baseline_restored: true,
            errors: Vec::new(),
        }
    }

    fn outcome_succeeded(_outcome: &Self::Outcome) -> bool {
        true
    }
}

#[derive(Default)]
struct FakeMetricCollector;

impl MetricCollector for FakeMetricCollector {
    fn definitions(&self) -> &'static [MetricDefinition] {
        &[RUNNER_RSS]
    }

    fn read(&mut self, at: MonotonicInstant) -> Vec<ResourceReading> {
        vec![ResourceReading {
            schema_version: 1,
            metric_id: RUNNER_RSS.id.to_owned(),
            metric_semantic_revision: RUNNER_RSS.semantic_revision,
            unit: RUNNER_RSS.unit,
            scope: RUNNER_RSS.scope,
            kind: RUNNER_RSS.kind,
            aggregation: RUNNER_RSS.aggregation,
            source: "fake-lifecycle".to_owned(),
            monotonic_offset_ns: at.offset_ns(),
            sampled: false,
            value: Availability::Unavailable {
                source: "fake-lifecycle".to_owned(),
                reason: "test adapter intentionally has no runner process".to_owned(),
            },
        }]
    }
}

async fn run_fake_trial(
    dispatch: FakeDispatch,
    generated_invocations: u32,
    fail_check: bool,
) -> (
    FakeAdapter,
    sandbox_benchmark::scheduler::StaticLifecycleRun<FakeOutcome>,
) {
    let adapter = FakeAdapter::default();
    let context = FakeContext {
        adapter: adapter.clone(),
    };
    let clock = Arc::new(AtomicU64::new(0));
    let now = {
        let clock = Arc::clone(&clock);
        move || MonotonicInstant::from_offset_ns(clock.fetch_add(1, Ordering::Relaxed))
    };
    let run = drive_static_lifecycle::<FakeLifecycle, _, _>(
        &context,
        &FakeCell {
            dispatch,
            generated_invocations,
            fail_check,
        },
        &CancellationToken::new(),
        StaticLifecycleConfig {
            expected_invocations: dispatch.invocations,
            request_timeout: Duration::from_secs(1),
            cancellation_grace: Duration::from_millis(20),
            teardown_timeout: Duration::from_secs(1),
            sampling_interval: SamplingInterval::from_millis(20).expect("valid fake interval"),
        },
        FakeMetricCollector,
        now,
    )
    .await
    .expect("typed fake lifecycle completes");
    (adapter, run)
}

#[tokio::test]
async fn every_closed_operation_expands_and_runs_through_the_fake_lifecycle() {
    let plans: Vec<OperationPlan> = serde_json::from_str(include_str!(
        "../fixtures/plans/closed-operation-matrix.json"
    ))
    .expect("strict seven-operation fixture parses");
    assert_eq!(plans.len(), OperationId::ALL.len());

    let cells = plans
        .iter()
        .flat_map(|plan| {
            expand_operation_plan(plan)
                .expect("fixture operation expands")
                .into_iter()
        })
        .collect::<Vec<_>>();
    assert_eq!(cells.len(), OperationId::ALL.len());

    let expected = [
        (
            OperationId::ExecCommand,
            FakeEndpoint::ExecCommand,
            ResolvedIsolationPolicy::ReusableVerifiedFixture,
            1,
            3,
        ),
        (
            OperationId::FileRead,
            FakeEndpoint::FileRead,
            ResolvedIsolationPolicy::ReusableVerifiedFixture,
            0,
            2,
        ),
        (
            OperationId::FileWrite,
            FakeEndpoint::FileWrite,
            ResolvedIsolationPolicy::FreshSessionsPerTrial,
            1,
            2,
        ),
        (
            OperationId::FileEdit,
            FakeEndpoint::FileEdit,
            ResolvedIsolationPolicy::FreshSessionsPerTrial,
            1,
            2,
        ),
        (
            OperationId::FileBlame,
            FakeEndpoint::FileBlame,
            ResolvedIsolationPolicy::ReusableVerifiedFixture,
            0,
            2,
        ),
        (
            OperationId::CreateWorkspace,
            FakeEndpoint::CreateWorkspace,
            ResolvedIsolationPolicy::PreparedSandboxPerCell,
            0,
            5,
        ),
        (
            OperationId::SquashLayerstack,
            FakeEndpoint::SquashLayerstacks,
            ResolvedIsolationPolicy::FreshTopologyPerTrial,
            20,
            1,
        ),
    ];

    for (cell, (operation, endpoint, isolation, prepared_sessions, invocations)) in
        cells.iter().zip(expected)
    {
        let dispatch = fake_dispatch(cell);
        assert_eq!(dispatch.operation, operation);
        assert_eq!(dispatch.endpoint, endpoint);
        assert_eq!(dispatch.isolation, isolation);
        assert_eq!(dispatch.prepared_sessions, prepared_sessions);
        assert_eq!(dispatch.invocations, invocations);
        assert_eq!(cell.id(), operation);
        assert_eq!(cell.measured_invocation_count(), invocations);
        assert_eq!(
            definition(operation).product_access,
            endpoint.product_access()
        );

        let (adapter, run) = run_fake_trial(dispatch, invocations, false).await;
        assert_eq!(run.barrier_participants, invocations as usize);
        assert_eq!(run.requests.len(), invocations as usize);
        assert!(run
            .requests
            .iter()
            .all(|request| request.completion == StaticRequestCompletion::Completed));
        assert!(run.product_succeeded);
        assert!(run.cleanup_baseline_restored);
        assert!(run.teardown_attempted);
        assert!(run.issues.is_empty());
        assert_eq!(run.checks.len(), 1);
        assert_eq!(run.checks[0].verdict, CheckVerdict::Pass);
        assert!(run.resources.iter().all(|reading| {
            reading.sampled && matches!(reading.value, Availability::Unavailable { .. })
        }));
        assert!(run.lifecycle.setup_ns > 0);
        assert!(run.lifecycle.operation_ns > 0);
        assert!(run.lifecycle.verify_ns > 0);
        assert!(run.lifecycle.teardown_ns > 0);
        let expected_live_sessions = if operation == OperationId::CreateWorkspace {
            invocations
        } else {
            prepared_sessions
        };
        let state = adapter.state.lock().expect("fake state lock");
        assert!(state.live_sessions.is_empty());
        assert_eq!(state.phases, ["setup", "operation", "verify", "teardown"]);
        assert_eq!(state.maximum_live_sessions, expected_live_sessions as usize);
        let teardown = run.teardown.as_ref().expect("teardown result persists");
        assert_eq!(
            teardown.destroyed_sessions, expected_live_sessions,
            "only the prepared or measured sessions are torn down"
        );
        assert_eq!(teardown.expected_destroyed_sessions, expected_live_sessions);

        let failed = OperationOutcome::failed(
            operation,
            format!("fake-{operation:?}"),
            ExecutorError::InvalidRuntime("injected fake failure"),
        );
        assert_eq!(failed.operation_id(), operation);
        assert!(!failed.is_success());
    }
}

#[tokio::test]
async fn generic_fake_driver_records_failed_checks_mismatch_and_destroy_lifecycle() {
    let plans: Vec<OperationPlan> = serde_json::from_str(include_str!(
        "../fixtures/plans/closed-operation-matrix.json"
    ))
    .expect("strict seven-operation fixture parses");
    let command = expand_operation_plan(&plans[0])
        .expect("command fixture expands")
        .remove(0);
    let dispatch = fake_dispatch(&command);

    let (_adapter, failed_check_run) = run_fake_trial(dispatch, dispatch.invocations, true).await;
    assert_eq!(failed_check_run.checks.len(), 1);
    assert_eq!(failed_check_run.checks[0].verdict, CheckVerdict::Fail);
    let correctness = fold_correctness(
        dispatch.operation,
        failed_check_run.product_succeeded,
        failed_check_run.cleanup_baseline_restored,
        &failed_check_run.checks,
    );
    assert_eq!(correctness.failed_check_count, 1);
    assert!(!correctness.eligible_for_latency);

    let (adapter, mismatch_run) = run_fake_trial(dispatch, dispatch.invocations - 1, false).await;
    assert_eq!(mismatch_run.barrier_participants, 0);
    assert!(mismatch_run.requests.is_empty());
    assert!(matches!(
        mismatch_run.issues.as_slice(),
        [StaticLifecycleIssue::InvocationCount(mismatch)]
            if mismatch.expected == dispatch.invocations
                && mismatch.actual == usize::try_from(dispatch.invocations - 1)
                    .expect("fixture invocation count fits usize")
    ));
    assert!(mismatch_run.teardown_attempted);
    assert!(mismatch_run.cleanup_baseline_restored);
    {
        let state = adapter.state.lock().expect("fake state lock");
        assert!(
            state.calls.is_empty(),
            "mismatch must fail before the barrier"
        );
        assert!(
            state.live_sessions.is_empty(),
            "mismatch still tears down setup"
        );
        assert_eq!(state.phases, ["setup", "teardown"]);
    }

    let destroy_dispatch = FakeDispatch {
        operation: OperationId::CreateWorkspace,
        endpoint: FakeEndpoint::DestroyWorkspace,
        isolation: ResolvedIsolationPolicy::PreparedSandboxPerCell,
        prepared_sessions: 5,
        invocations: 5,
    };
    let (adapter, destroy_run) = run_fake_trial(destroy_dispatch, 5, false).await;
    assert_eq!(destroy_run.barrier_participants, 5);
    assert!(destroy_run.product_succeeded);
    let teardown = destroy_run
        .teardown
        .as_ref()
        .expect("destroy teardown persists");
    assert_eq!(teardown.expected_destroyed_sessions, 0);
    assert_eq!(teardown.destroyed_sessions, 0);
    let state = adapter.state.lock().expect("fake state lock");
    assert_eq!(state.maximum_live_sessions, 5);
    assert_eq!(state.calls.len(), 5);
    assert!(state.live_sessions.is_empty());
}

#[derive(Debug, Clone, Copy)]
enum InjectedFailure {
    Setup,
    Operation,
    Verify,
    Cancellation,
    Teardown,
}

#[derive(Debug)]
struct CleanupOutcome {
    teardown_attempted: bool,
    baseline_restored: bool,
    report_eligible: bool,
    abort_campaign: bool,
}

fn drive_failure_path(
    root: &TestRoot,
    injected: InjectedFailure,
    outside_sentinel: &std::path::Path,
) -> CleanupOutcome {
    let owned_root = root.join(format!("owned-{injected:?}"));
    fs::create_dir_all(&owned_root).expect("fake setup creates owned root");
    fs::write(owned_root.join("resource"), b"owned").expect("fake setup creates resource");

    let failed_before_teardown = matches!(
        injected,
        InjectedFailure::Setup
            | InjectedFailure::Operation
            | InjectedFailure::Verify
            | InjectedFailure::Cancellation
    );
    let leave_residue = matches!(injected, InjectedFailure::Teardown);
    let teardown_attempted = true;
    if !leave_residue {
        fs::remove_dir_all(&owned_root).expect("fake teardown removes only its owned root");
    }

    let baseline_restored = !owned_root.exists();
    assert_eq!(
        fs::read(outside_sentinel).expect("outside sentinel remains readable"),
        b"outside"
    );
    CleanupOutcome {
        teardown_attempted,
        baseline_restored,
        report_eligible: !failed_before_teardown && baseline_restored,
        abort_campaign: !baseline_restored,
    }
}

#[test]
fn fake_failures_and_cancellation_always_attempt_owned_cleanup_and_leaks_abort() {
    let root = TestRoot::new("fake-cleanup-matrix");
    let outside_sentinel = root.join("outside-sentinel");
    fs::write(&outside_sentinel, b"outside").expect("create outside sentinel");

    for injected in [
        InjectedFailure::Setup,
        InjectedFailure::Operation,
        InjectedFailure::Verify,
        InjectedFailure::Cancellation,
    ] {
        let outcome = drive_failure_path(&root, injected, &outside_sentinel);
        assert!(outcome.teardown_attempted);
        assert!(outcome.baseline_restored);
        assert!(!outcome.report_eligible);
        assert!(!outcome.abort_campaign);
    }

    let leaked = drive_failure_path(&root, InjectedFailure::Teardown, &outside_sentinel);
    assert!(leaked.teardown_attempted);
    assert!(!leaked.baseline_restored);
    assert!(!leaked.report_eligible);
    assert!(leaked.abort_campaign);
}
