use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use sandbox_runtime_namespace_execution::test_support::{NsRunnerLauncher, PtyMaster, RunnerChild};
use sandbox_runtime_namespace_execution::{
    NamespaceExecutionEngine, NamespaceExecutionError, NoopObserver,
};
use sandbox_runtime_namespace_process::runner::protocol::{NamespaceRunnerRequest, RunResult};
use sandbox_runtime_workspace::overlay::dirs::OverlayDirs;
use sandbox_runtime_workspace::profile::{
    RemountProbe, ResourceCaps, WorkspaceModeError, WorkspaceModeFds, WorkspaceModeHandle,
    WorkspaceModeId,
};
use sandbox_runtime_workspace::test_support::{
    insert_handle_for_test, mount_overlay_for_test, namespace_runtime_with_engine_for_test,
    remount_overlay_for_test, remount_with_layers_for_test,
    workspace_mode_manager_with_runtime_for_test, WorkspaceRemountState,
};
use sandbox_runtime_workspace::WorkspaceProfile;
use serde_json::json;

#[derive(Clone, Default)]
struct FakeLauncher {
    state: Arc<Mutex<FakeLauncherState>>,
}

#[derive(Default)]
struct FakeLauncherState {
    outcomes: VecDeque<Result<RunResult, NamespaceExecutionError>>,
    requests: Vec<NamespaceRunnerRequest>,
    mode_flags: Vec<&'static str>,
    setup_timeouts: Vec<f64>,
}

struct FakeChild {
    outcome: Option<Result<RunResult, NamespaceExecutionError>>,
}

impl FakeLauncher {
    fn push_result(&self, result: RunResult) {
        self.state
            .lock()
            .expect("fake launcher mutex poisoned")
            .outcomes
            .push_back(Ok(result));
    }

    fn requests(&self) -> Vec<NamespaceRunnerRequest> {
        self.state
            .lock()
            .expect("fake launcher mutex poisoned")
            .requests
            .clone()
    }

    fn mode_flags(&self) -> Vec<&'static str> {
        self.state
            .lock()
            .expect("fake launcher mutex poisoned")
            .mode_flags
            .clone()
    }
}

impl NsRunnerLauncher for FakeLauncher {
    fn spawn_pty(
        &self,
        _request: NamespaceRunnerRequest,
    ) -> Result<(Box<dyn RunnerChild>, PtyMaster), NamespaceExecutionError> {
        panic!("workspace mount tests never spawn PTY children")
    }

    fn spawn_piped(
        &self,
        mode_flag: &'static str,
        request: NamespaceRunnerRequest,
        setup_timeout_s: f64,
    ) -> Result<Box<dyn RunnerChild>, NamespaceExecutionError> {
        let mut state = self.state.lock().expect("fake launcher mutex poisoned");
        state.mode_flags.push(mode_flag);
        state.setup_timeouts.push(setup_timeout_s);
        state.requests.push(request);
        let outcome = state.outcomes.pop_front().unwrap_or_else(|| {
            Err(NamespaceExecutionError::Completion(
                "missing fake result".into(),
            ))
        });
        Ok(Box::new(FakeChild {
            outcome: Some(outcome),
        }))
    }
}

impl RunnerChild for FakeChild {
    fn wait_completion(&mut self) -> Result<RunResult, NamespaceExecutionError> {
        self.outcome
            .take()
            .expect("fake child is waited exactly once")
    }
}

#[test]
fn mount_overlay_success_uses_engine_request_and_caller_layers() {
    let fake = FakeLauncher::default();
    fake.push_result(run_result(0, json!({"status": "ok", "success": true})));
    let runtime = runtime_with_fake_launcher(&fake);
    let handle = workspace_mode_handle();
    let layer_paths = vec![PathBuf::from("/override/lower")];

    mount_overlay_for_test(&runtime, &handle, &layer_paths).expect("mount succeeds");

    assert_eq!(fake.mode_flags(), vec!["--mount-overlay"]);
    let requests = fake.requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.request_id, "namespace_execution_1");
    assert_eq!(request.layer_paths, layer_paths);
    assert_eq!(request.workspace_root, PathBuf::from("/workspace"));
    assert_eq!(request.upperdir, Some(PathBuf::from("/tmp/eos/upper")));
    assert_eq!(request.workdir, Some(PathBuf::from("/tmp/eos/work")));
    assert_eq!(
        request
            .ns_fds
            .expect("fds are populated")
            .mnt
            .map(|fd| fd.0),
        Some(11)
    );
}

#[test]
fn remount_overlay_success_parses_verified_report_and_probe_args() {
    let fake = FakeLauncher::default();
    fake.push_result(run_result(
        0,
        json!({
            "mount_verified": true,
            "staged_switch": true,
            "staging_verified": true,
            "rollback_unmounted": true
        }),
    ));
    let runtime = runtime_with_fake_launcher(&fake);
    let handle = workspace_mode_handle();
    let probe = RemountProbe {
        path: Some(PathBuf::from("/workspace/probe")),
        expected_content: Some("ready".to_owned()),
    };

    let result =
        remount_overlay_for_test(&runtime, &handle, &[PathBuf::from("/new/lower")], &probe)
            .expect("remount succeeds");

    assert!(result.mount_verified);
    assert_eq!(fake.mode_flags(), vec!["--remount-overlay"]);
    let request = fake
        .requests()
        .into_iter()
        .next()
        .expect("request recorded");
    assert_eq!(request.args["probe_path"], "/workspace/probe");
    assert_eq!(request.args["probe_content"], "ready");
    assert_eq!(request.layer_paths, vec![PathBuf::from("/new/lower")]);
}

#[test]
fn remount_verification_failure_is_caller_error() {
    let fake = FakeLauncher::default();
    fake.push_result(run_result(
        0,
        json!({
            "mount_verified": false,
            "staged_switch": true,
            "staging_verified": true,
            "rollback_unmounted": true,
            "probe_content_matched": false
        }),
    ));
    let runtime = runtime_with_fake_launcher(&fake);
    let mut manager = workspace_mode_manager_with_runtime_for_test(
        "/workspace",
        ResourceCaps::default(),
        temp_path("remount-false"),
        runtime,
    );
    let handle = workspace_mode_handle();
    let workspace_id = handle.workspace_id.clone();
    insert_handle_for_test(&mut manager, handle);

    let error = remount_with_layers_for_test(
        &mut manager,
        &workspace_id,
        vec![PathBuf::from("/new/lower")],
        &RemountProbe::default(),
    )
    .expect_err("caller rejects unverified remount");

    assert!(matches!(
        error,
        WorkspaceModeError::SetupFailed { step }
            if step.contains("remount overlay verification failed")
                && step.contains("mount_verified=false")
    ));
}

#[test]
fn mount_overlay_failure_is_setup_failed_with_payload_detail() {
    let fake = FakeLauncher::default();
    fake.push_result(run_result(1, json!({"error": "mount exploded"})));
    let runtime = runtime_with_fake_launcher(&fake);

    let error = mount_overlay_for_test(
        &runtime,
        &workspace_mode_handle(),
        &[PathBuf::from("/lower")],
    )
    .expect_err("mount failure surfaces as setup failure");

    assert!(matches!(
        error,
        WorkspaceModeError::SetupFailed { step }
            if step.contains("--mount-overlay") && step.contains("mount exploded")
    ));
}

fn runtime_with_fake_launcher(
    fake: &FakeLauncher,
) -> sandbox_runtime_workspace::test_support::NamespaceRuntime {
    let engine: NamespaceExecutionEngine = NamespaceExecutionEngine::with_launcher(
        Box::new(fake.clone()),
        Arc::new(NoopObserver),
        8,
        12.0,
    );
    namespace_runtime_with_engine_for_test(Arc::new(engine))
}

fn run_result(exit_code: i32, payload: serde_json::Value) -> RunResult {
    RunResult { exit_code, payload }
}

fn workspace_mode_handle() -> WorkspaceModeHandle {
    WorkspaceModeHandle {
        workspace_id: WorkspaceModeId("namespace-handle".to_owned()),
        profile: WorkspaceProfile::Isolated,
        lease_id: "lease-1".to_owned(),
        manifest_version: 42,
        manifest_root_hash: "root-hash".to_owned(),
        base_manifest: test_manifest(),
        workspace_root: "/workspace".to_owned(),
        dirs: OverlayDirs {
            run_dir: "/tmp/eos/run".into(),
            upperdir: "/tmp/eos/upper".into(),
            workdir: "/tmp/eos/work".into(),
        },
        layer_paths: vec!["/lower/one".into(), "/lower/two".into()],
        ns_fds: WorkspaceModeFds {
            user: Some(10),
            mnt: Some(11),
            pid: Some(12),
            net: Some(13),
        },
        holder_pid: 1234,
        readiness_fd: 13,
        control_fd: 14,
        veth: None,
        remount_state: WorkspaceRemountState::Active,
        created_at: 1.0,
        last_activity: 2.0,
    }
}

fn test_manifest() -> sandbox_runtime_layerstack::Manifest {
    sandbox_runtime_layerstack::Manifest::new(
        1,
        vec![sandbox_runtime_layerstack::LayerRef {
            layer_id: "L000001-test".to_owned(),
            path: "layers/L000001-test".to_owned(),
        }],
        sandbox_runtime_layerstack::MANIFEST_SCHEMA_VERSION,
    )
    .expect("test manifest is valid")
}

fn temp_path(label: &str) -> PathBuf {
    static NEXT_TEST: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "workspace-setns-{label}-{}-{}",
        std::process::id(),
        NEXT_TEST.fetch_add(1, Ordering::Relaxed)
    ))
}
