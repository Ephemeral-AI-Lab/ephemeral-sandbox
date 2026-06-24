use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use sandbox_runtime_namespace_execution::{
    NamespaceExecutionError, NamespaceExecutionId, NamespaceTarget, RunnerOutcome,
};
use sandbox_runtime_namespace_process::runner::protocol::{NamespaceRunnerRequest, RunResult};
use sandbox_runtime_workspace::model::WorkspaceHandle;
use sandbox_runtime_workspace::overlay::dirs::OverlayDirs;
use sandbox_runtime_workspace::profile::{
    RemountOverlayResult, RemountProbe, WorkspaceModeError, WorkspaceModeFds, WorkspaceModeHandle,
    WorkspaceModeId, WorkspaceRemountState,
};
use sandbox_runtime_workspace::WorkspaceProfile;
use serde_json::{json, Value};

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

struct TestNamespaceRuntime {
    launcher: FakeLauncher,
    next_id: AtomicU64,
    setup_timeout_s: f64,
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

impl FakeLauncher {
    fn spawn_piped(
        &self,
        mode_flag: &'static str,
        request: NamespaceRunnerRequest,
        setup_timeout_s: f64,
    ) -> Result<RunResult, NamespaceExecutionError> {
        let mut state = self.state.lock().expect("fake launcher mutex poisoned");
        state.mode_flags.push(mode_flag);
        state.setup_timeouts.push(setup_timeout_s);
        state.requests.push(request);
        state.outcomes.pop_front().unwrap_or_else(|| {
            Err(NamespaceExecutionError::Completion(
                "missing fake result".into(),
            ))
        })
    }
}

impl TestNamespaceRuntime {
    fn new(launcher: FakeLauncher, setup_timeout_s: f64) -> Self {
        Self {
            launcher,
            next_id: AtomicU64::new(1),
            setup_timeout_s,
        }
    }

    fn mount_overlay(
        &self,
        handle: &WorkspaceModeHandle,
        layer_paths: &[PathBuf],
    ) -> Result<(), WorkspaceModeError> {
        let mut entry = WorkspaceHandle::from(handle).entry().map_err(setup_error)?;
        entry.layer_paths = layer_paths.to_vec();
        self.run_mount(
            "--mount-overlay",
            NamespaceTarget::from(entry),
            json!({}),
            |_| Ok(()),
        )
    }

    fn remount_overlay(
        &self,
        handle: &WorkspaceModeHandle,
        layer_paths: &[PathBuf],
        probe: &RemountProbe,
    ) -> Result<RemountOverlayResult, WorkspaceModeError> {
        let mut entry = WorkspaceHandle::from(handle).entry().map_err(setup_error)?;
        entry.layer_paths = layer_paths.to_vec();
        let probe_args = json!({
            "probe_path": probe
                .path
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
            "probe_content": probe.expected_content.as_deref(),
        });
        self.run_mount(
            "--remount-overlay",
            NamespaceTarget::from(entry),
            probe_args,
            |outcome| Ok(RemountOverlayResult::from_payload(outcome.payload())),
        )
    }

    fn run_mount<O>(
        &self,
        mode_flag: &'static str,
        target: NamespaceTarget,
        args: Value,
        parse: impl FnOnce(RunnerOutcome) -> Result<O, NamespaceExecutionError>,
    ) -> Result<O, WorkspaceModeError> {
        let id = self.allocate_id();
        let request = build_request(&target, &id, args);
        let result = self
            .launcher
            .spawn_piped(mode_flag, request, self.setup_timeout_s)
            .map_err(setup_error)?;
        let outcome = RunnerOutcome::new(result);
        if outcome.exit_code() != 0 {
            return Err(setup_error(NamespaceExecutionError::Finalize(format!(
                "namespace runner {mode_flag} failed with exit code {}: {}",
                outcome.exit_code(),
                mount_failure_detail(outcome.payload())
            ))));
        }
        parse(outcome).map_err(setup_error)
    }

    fn allocate_id(&self) -> NamespaceExecutionId {
        let next_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        NamespaceExecutionId(format!("namespace_execution_{next_id}"))
    }
}

#[test]
fn mount_overlay_success_uses_engine_request_and_caller_layers() {
    let fake = FakeLauncher::default();
    fake.push_result(run_result(0, json!({"status": "ok", "success": true})));
    let runtime = runtime_with_fake_launcher(&fake);
    let handle = workspace_mode_handle();
    let layer_paths = vec![PathBuf::from("/override/lower")];

    runtime
        .mount_overlay(&handle, &layer_paths)
        .expect("mount succeeds");

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

    let result = runtime
        .remount_overlay(&handle, &[PathBuf::from("/new/lower")], &probe)
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
    let handle = workspace_mode_handle();

    let error = remount_with_layers_for_test(
        &runtime,
        &handle,
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

    let error = runtime
        .mount_overlay(&workspace_mode_handle(), &[PathBuf::from("/lower")])
        .expect_err("mount failure surfaces as setup failure");

    assert!(matches!(
        error,
        WorkspaceModeError::SetupFailed { step }
            if step.contains("--mount-overlay") && step.contains("mount exploded")
    ));
}

fn runtime_with_fake_launcher(fake: &FakeLauncher) -> TestNamespaceRuntime {
    TestNamespaceRuntime::new(fake.clone(), 12.0)
}

fn remount_with_layers_for_test(
    runtime: &TestNamespaceRuntime,
    handle: &WorkspaceModeHandle,
    layer_paths: Vec<PathBuf>,
    probe: &RemountProbe,
) -> Result<WorkspaceModeHandle, WorkspaceModeError> {
    if layer_paths.is_empty() {
        return Err(WorkspaceModeError::InvalidArgument(
            "layer_paths must not be empty".to_owned(),
        ));
    }
    let remount = runtime.remount_overlay(handle, &layer_paths, probe)?;
    if !remount.mount_verified {
        return Err(WorkspaceModeError::SetupFailed {
            step: format!(
                "remount overlay verification failed: {}",
                remount.failure_summary()
            ),
        });
    }
    let mut updated = handle.clone();
    updated.layer_paths = layer_paths;
    updated.remount_state = WorkspaceRemountState::Active;
    Ok(updated)
}

fn build_request(
    target: &NamespaceTarget,
    id: &NamespaceExecutionId,
    args: Value,
) -> NamespaceRunnerRequest {
    NamespaceRunnerRequest {
        request_id: id.0.clone(),
        args,
        workspace_root: target.workspace_root.clone(),
        layer_paths: target.layer_paths.clone(),
        upperdir: target.upperdir.clone(),
        workdir: target.workdir.clone(),
        ns_fds: Some(target.ns_fds),
        timeout_seconds: None,
    }
}

fn mount_failure_detail(payload: &Value) -> String {
    payload
        .get("error")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| payload.to_string())
}

fn setup_error(error: impl std::fmt::Display) -> WorkspaceModeError {
    WorkspaceModeError::SetupFailed {
        step: error.to_string(),
    }
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
