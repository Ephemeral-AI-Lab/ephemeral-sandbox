//! Isolated-workspace runtime lifecycle: enter/exit custody, root rebinding,
//! and the test-reset sweep. The daemon's op adapters are covered by the
//! daemon's own integration tests; these drive the runtime API directly.

use std::path::{Path, PathBuf};

use config::configs::isolated_workspace::IsolatedWorkspaceConfig;
use layerstack::{
    service::BoundedCaptureOptions, CommitOptions, LayerChange, LayerPath, LayerStack,
};
use operation::command::CommandOps;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use workspace::RemountProbe;

use super::{WorkspaceFileRouteContext, WorkspaceRemountCompactionAttempt, WorkspaceRuntime};

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

#[test]
fn workspace_runtime_cancel_runs_for_caller_tears_down_entered_handle() -> TestResult {
    // The per-caller workspace-run teardown discards the caller's commands
    // (owned by the command registry, not a side-map here)
    // and removes the handle, releasing its lease.
    let root = test_root("cancel-runs-teardown");
    let scratch = root.join("scratch");
    let workspace_root = root.join("workspace");
    let runtime = isolated_runtime(&scratch, Path::new("/testbed"));
    seed_workspace_base(&root.join("stack"), &workspace_root)?;

    let entered = runtime.enter("caller-command", &workspace_root)?;
    assert_eq!(entered.caller_id, "caller-command");

    let cancel = runtime.cancel_runs_for_caller("caller-command", None);
    let exit = cancel.isolated?;
    assert_eq!(
        exit.isolated.inspection["handle_registered_after"],
        json!(false)
    );
    assert_eq!(exit.lease_released, Some(true));
    assert_eq!(exit.active_leases_after, 0);
    let _ = runtime.test_reset();
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[test]
fn workspace_runtime_enter_uses_workspace_binding_over_configured_workspace_root() -> TestResult {
    let root = test_root("bound-workspace-root");
    let scratch = root.join("scratch");
    let stack_root = root.join("stack");
    let workspace_root = root.join("workspace");
    std::fs::create_dir_all(&workspace_root)?;
    std::fs::write(workspace_root.join("seed.txt"), "seed\n")?;
    layerstack::build_workspace_base(&stack_root, &workspace_root, true)?;
    let runtime = isolated_runtime(&scratch, Path::new("/configured-fallback"));

    let entered = runtime.enter("caller-bound-root", &workspace_root)?;

    let expected_workspace_root = workspace_root
        .canonicalize()?
        .to_string_lossy()
        .into_owned();
    assert_eq!(entered.workspace_root, expected_workspace_root);
    assert!(!entered.dns_configuration.fallback_applied);
    assert_eq!(entered.dns_configuration.previous_first_nameserver, None);
    let status = runtime
        .status("caller-bound-root")?
        .ok_or("status should report the open handle")?;
    assert_eq!(status.workspace_root, expected_workspace_root);
    assert_eq!(status.dns_configuration, entered.dns_configuration);
    assert_eq!(runtime.list_open(), vec!["caller-bound-root".to_owned()]);

    runtime.exit("caller-bound-root", None)?;
    assert!(runtime.status("caller-bound-root")?.is_none());
    let _ = runtime.test_reset();
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[test]
fn workspace_runtime_enter_rebinds_idle_state_to_new_layer_stack_root() -> TestResult {
    let root = test_root("root-switch");
    let scratch = root.join("scratch");
    let stack_a = root.join("stack-a");
    let stack_b = root.join("stack-b");
    let workspace_a = root.join("workspace-a");
    let workspace_b = root.join("workspace-b");
    let runtime = isolated_runtime(&scratch, Path::new("/testbed"));
    seed_workspace_base(&stack_a, &workspace_a)?;
    seed_workspace_base(&stack_b, &workspace_b)?;

    runtime.enter_with_report_legacy_layer_stack_root("caller-root-a", &stack_a)?;
    assert_eq!(
        layerstack::LayerStack::open(stack_a.clone())?.active_lease_count(),
        1,
        "stack A holds the lease while caller A is open"
    );
    assert_eq!(
        layerstack::LayerStack::open(stack_b.clone())?.active_lease_count(),
        0,
        "stack B is untouched before the rebind"
    );
    runtime.exit("caller-root-a", None)?;

    runtime.enter_with_report_legacy_layer_stack_root("caller-root-b", &stack_b)?;
    assert_eq!(
        layerstack::LayerStack::open(stack_a.clone())?.active_lease_count(),
        0,
        "stack A is released after the rebind"
    );
    assert_eq!(
        layerstack::LayerStack::open(stack_b.clone())?.active_lease_count(),
        1,
        "stack B holds the lease for caller B"
    );

    runtime.exit("caller-root-b", None)?;
    let _ = runtime.test_reset();
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[test]
fn workspace_runtime_enter_rebinds_idle_state_when_workspace_root_changes() -> TestResult {
    let root = test_root("same-stack-workspace-rebind");
    let scratch = root.join("scratch");
    let stack_root = root.join("stack");
    let workspace_a = root.join("workspace-a");
    let workspace_b = root.join("workspace-b");
    let runtime = isolated_runtime(&scratch, Path::new("/testbed"));
    seed_workspace_base(&stack_root, &workspace_a)?;

    let first = runtime.enter("caller-root-a", &workspace_a)?;
    assert_eq!(
        first.workspace_root,
        workspace_a.canonicalize()?.to_string_lossy()
    );
    runtime.exit("caller-root-a", None)?;

    std::fs::create_dir_all(&workspace_b)?;
    let mut binding =
        layerstack::read_workspace_binding(&stack_root)?.ok_or("workspace binding should exist")?;
    binding.workspace_root = workspace_b.to_string_lossy().into_owned();
    std::fs::write(
        stack_root.join(layerstack::WORKSPACE_BINDING_FILE),
        serde_json::to_vec_pretty(&binding)?,
    )?;

    let second = runtime.enter("caller-root-b", &workspace_b)?;

    assert_eq!(
        second.workspace_root,
        workspace_b.canonicalize()?.to_string_lossy(),
        "same stack rebinding must refresh manager workspace_root caps"
    );
    runtime.exit("caller-root-b", None)?;
    let _ = runtime.test_reset();
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[test]
fn workspace_runtime_enter_normalizes_snapshot_before_mounting_workspace() -> TestResult {
    let root = test_root("enter-normalizes-snapshot");
    let scratch = root.join("scratch");
    let stack_root = root.join("stack");
    let workspace_root = root.join("workspace");
    seed_workspace_base(&stack_root, &workspace_root)?;
    for index in 0..5 {
        LayerStack::open(stack_root.clone())?.publish_layer(&[LayerChange::Write {
            path: LayerPath::parse("large.txt")?,
            content: vec![u8::try_from(index)?; 1024],
        }])?;
    }
    let runtime = isolated_runtime_with_max_depth(&scratch, Path::new("/testbed"), 2);

    let entered = runtime.enter_with_report("caller-normalized", &workspace_root)?;

    assert!(
        entered.snapshot_normalization.triggered,
        "enter should normalize the command snapshot before mounting"
    );
    assert_eq!(entered.snapshot_normalization.active_depth_before, 6);
    assert_eq!(entered.snapshot_normalization.active_depth_after, 1);
    assert_eq!(
        entered.handle.layer_paths.len(),
        1,
        "mounted lowerdir list should be bounded"
    );
    assert_eq!(
        LayerStack::open(stack_root.clone())?
            .read_active_manifest()?
            .depth(),
        1
    );

    runtime.exit("caller-normalized", None)?;
    let _ = runtime.test_reset();
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[test]
fn workspace_runtime_host_create_acquires_leased_base_revision() -> TestResult {
    let root = test_root("host-create-leased-base");
    let scratch = root.join("scratch");
    let host_scratch = root.join("host-scratch");
    let stack_root = root.join("stack");
    let workspace_root = root.join("workspace");
    seed_workspace_base(&stack_root, &workspace_root)?;
    let runtime = isolated_runtime(&scratch, Path::new("/testbed"));

    let host = runtime
        .create_host_workspace_for_legacy_layer_stack_root_with_scratch_root_for_test(
            "caller-host",
            "invoke-host",
            &stack_root,
            &host_scratch,
        )?;

    assert!(!host.leased_base.lease_id.is_empty());
    assert_eq!(host.leased_base.version, 1);
    assert_eq!(host.leased_base.layer_paths.len(), 1);
    assert_eq!(host.workspace_root, workspace_root.canonicalize()?);
    assert!(host.workspace.dirs().upperdir.is_dir());
    assert_eq!(
        layerstack::LayerStack::open(stack_root.clone())?.active_lease_count(),
        1
    );
    let release = host.lease.release();
    assert_eq!(release.released, Some(true));
    assert_eq!(release.error, None);
    drop(host);
    assert_eq!(
        layerstack::LayerStack::open(stack_root.clone())?.active_lease_count(),
        0
    );
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[test]
fn workspace_runtime_host_create_failure_releases_lease() -> TestResult {
    let root = test_root("host-create-failure-release");
    let scratch = root.join("scratch");
    let host_scratch = root.join("host-scratch");
    let stack_root = root.join("stack");
    let workspace_root = root.join("workspace");
    let invocation_id = "invoke-host-fail";
    seed_workspace_base(&stack_root, &workspace_root)?;
    let runtime = isolated_runtime(&scratch, Path::new("/testbed"));
    let blocked_run_dir = host_scratch
        .join("sandbox-overlay")
        .join(format!("{}-{invocation_id}", std::process::id()));
    std::fs::create_dir_all(blocked_run_dir.parent().ok_or("blocked run parent")?)?;
    std::fs::write(&blocked_run_dir, "not a directory")?;

    let error = runtime
        .create_host_workspace_for_legacy_layer_stack_root_with_scratch_root_for_test(
            "caller-host",
            invocation_id,
            &stack_root,
            &host_scratch,
        )
        .expect_err("workspace dir allocation should fail after lease acquisition");

    assert!(
        error.to_string().contains("lease") && error.to_string().contains("released"),
        "create failure should report lease release: {error}"
    );
    assert_eq!(
        layerstack::LayerStack::open(stack_root.clone())?.active_lease_count(),
        0
    );
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[test]
fn workspace_runtime_host_release_is_exact_once() -> TestResult {
    let root = test_root("host-destroy-exact-once");
    let scratch = root.join("scratch");
    let host_scratch = root.join("host-scratch");
    let stack_root = root.join("stack");
    let workspace_root = root.join("workspace");
    seed_workspace_base(&stack_root, &workspace_root)?;
    let runtime = isolated_runtime(&scratch, Path::new("/testbed"));
    let host = runtime
        .create_host_workspace_for_legacy_layer_stack_root_with_scratch_root_for_test(
            "caller-host",
            "invoke-host-destroy",
            &stack_root,
            &host_scratch,
        )?;
    let lease = host.lease.clone();

    let first = host.lease.release();
    drop(host);
    let second = lease.release();

    assert_eq!(first.released, Some(true));
    assert_eq!(first.error, None);
    assert_eq!(second.released, Some(false));
    assert_eq!(second.error, None);
    assert_eq!(
        layerstack::LayerStack::open(stack_root.clone())?.active_lease_count(),
        0
    );
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[test]
fn workspace_runtime_command_route_selects_isolated_when_caller_has_active_handle() -> TestResult {
    let root = test_root("command-route-isolated");
    let scratch = root.join("scratch");
    let stack_root = root.join("stack");
    let workspace_root = root.join("workspace");
    seed_workspace_base(&stack_root, &workspace_root)?;
    let runtime = isolated_runtime(&scratch, Path::new("/testbed"));
    runtime.enter("caller-route", &workspace_root)?;

    let route = runtime.route_command_context("caller-route", "invoke-route", None)?;

    assert_eq!(route.trace_facts().kind, "isolated_workspace");
    assert_eq!(
        route.trace_facts().reason,
        "caller_has_open_isolated_workspace"
    );
    assert_eq!(route.trace_facts().layer_stack_root, None);
    assert_eq!(route.caller_id(), "caller-route");
    assert!(route.remountable(true));
    drop(route);
    runtime.exit("caller-route", None)?;
    let _ = runtime.test_reset();
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[test]
fn workspace_runtime_command_route_selects_host_when_no_active_handle() -> TestResult {
    let root = test_root("command-route-host");
    let scratch = root.join("scratch");
    let host_scratch = root.join("host-scratch");
    let stack_root = root.join("stack");
    let workspace_root = root.join("workspace");
    seed_workspace_base(&stack_root, &workspace_root)?;
    let runtime = isolated_runtime(&scratch, Path::new("/testbed"));

    let route = runtime.route_command_context_with_scratch_root_for_test(
        "caller-route",
        "invoke-route-host",
        Some(stack_root.clone()),
        &host_scratch,
    )?;

    assert_eq!(route.trace_facts().kind, "ephemeral_workspace");
    assert_eq!(
        route.trace_facts().reason,
        "no_isolated_workspace_for_caller"
    );
    assert_eq!(
        route.trace_facts().layer_stack_root,
        Some(stack_root.clone())
    );
    assert_eq!(route.caller_id(), "caller-route");
    assert!(!route.remountable(true));
    assert_eq!(
        layerstack::LayerStack::open(stack_root.clone())?.active_lease_count(),
        1
    );
    drop(route);
    assert_eq!(
        layerstack::LayerStack::open(stack_root.clone())?.active_lease_count(),
        0
    );
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[test]
fn workspace_runtime_command_route_missing_root_stays_compatible() -> TestResult {
    let root = test_root("command-route-missing-root");
    let scratch = root.join("scratch");
    let host_scratch = root.join("host-scratch");
    let runtime = isolated_runtime(&scratch, Path::new("/testbed"));

    let Err(error) = runtime.route_command_context_with_scratch_root_for_test(
        "caller-route",
        "invoke-route-missing-root",
        None,
        &host_scratch,
    ) else {
        return Err("missing command route root unexpectedly succeeded".into());
    };

    assert!(matches!(
        error,
        workspace::WorkspaceError::InvalidRequest { field, ref message }
            if field == "layer_stack_root" && message == "layer_stack_root is required"
    ));
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[test]
fn workspace_runtime_file_route_selects_direct_when_no_active_handle() -> TestResult {
    let root = test_root("file-route-direct");
    let scratch = root.join("scratch");
    let stack_root = root.join("stack");
    let workspace_root = root.join("workspace");
    seed_workspace_base(&stack_root, &workspace_root)?;
    let runtime = isolated_runtime(&scratch, Path::new("/testbed"));

    let route = runtime.route_file_context("caller-file", Some(&stack_root))?;

    match &route {
        WorkspaceFileRouteContext::Direct { layer_stack_root } => {
            assert_eq!(layer_stack_root, &stack_root);
        }
        WorkspaceFileRouteContext::Isolated { .. } => {
            return Err("file route should be direct without an active handle".into());
        }
    }
    let facts = route.trace_facts();
    assert_eq!(facts.kind, "fast_path");
    assert_eq!(facts.reason, "no_isolated_workspace_for_caller");
    assert_eq!(facts.layer_stack_root, Some(stack_root.clone()));
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[test]
fn workspace_runtime_file_route_selects_isolated_when_caller_has_active_handle() -> TestResult {
    let root = test_root("file-route-isolated");
    let scratch = root.join("scratch");
    let stack_root = root.join("stack");
    let workspace_root = root.join("workspace");
    seed_workspace_base(&stack_root, &workspace_root)?;
    let runtime = isolated_runtime(&scratch, Path::new("/testbed"));
    runtime.enter("caller-file", &workspace_root)?;
    let before = runtime
        .status("caller-file")?
        .ok_or("open handle before file route")?
        .last_activity;

    let route = runtime.route_file_context("caller-file", None)?;

    match &route {
        WorkspaceFileRouteContext::Isolated { binding } => {
            assert_eq!(binding.caller_id, "caller-file");
            assert_eq!(binding.layer_stack_root, stack_root.canonicalize()?);
        }
        WorkspaceFileRouteContext::Direct { .. } => {
            return Err("file route should be isolated with an active handle".into());
        }
    }
    let facts = route.trace_facts();
    assert_eq!(facts.kind, "isolated_workspace");
    assert_eq!(facts.reason, "caller_has_open_isolated_workspace");
    assert_eq!(facts.layer_stack_root, None);
    std::thread::sleep(Duration::from_millis(2));
    runtime.complete_file_route(&route);
    let after = runtime
        .status("caller-file")?
        .ok_or("open handle after file route")?
        .last_activity;
    assert!(after >= before);
    runtime.exit("caller-file", None)?;
    let _ = runtime.test_reset();
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[test]
fn workspace_runtime_file_route_missing_root_stays_compatible() -> TestResult {
    let root = test_root("file-route-missing-root");
    let scratch = root.join("scratch");
    let runtime = isolated_runtime(&scratch, Path::new("/testbed"));

    let Err(error) = runtime.route_file_context("caller-file", None) else {
        return Err("missing file route root unexpectedly succeeded".into());
    };

    assert!(matches!(
        error,
        workspace::WorkspaceError::InvalidRequest { field, ref message }
            if field == "layer_stack_root" && message == "layer_stack_root is required"
    ));
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[test]
fn workspace_runtime_compact_remount_for_test_resolves_workspace_root() -> TestResult {
    let root = test_root("compact-remount-workspace-root");
    let scratch = root.join("scratch");
    let stack_root = root.join("stack");
    let workspace_root = root.join("workspace");
    seed_workspace_base(&stack_root, &workspace_root)?;
    let runtime = isolated_runtime(&scratch, Path::new("/testbed"));
    runtime.enter("caller-remount", &workspace_root)?;

    let attempt = runtime.compact_remount_open_workspace_for_test(
        "caller-remount",
        &workspace_root,
        RemountProbe {
            path: None,
            expected_content: None,
        },
        None,
    )?;

    match attempt {
        WorkspaceRemountCompactionAttempt::Compacted(report) => {
            assert_eq!(report.active_leases_after, 1);
        }
        WorkspaceRemountCompactionAttempt::Blocked(report) => {
            panic!("remount should not be blocked without active commands: {report:?}");
        }
    }
    runtime.exit("caller-remount", None)?;
    let _ = runtime.test_reset();
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[test]
fn workspace_runtime_resolve_workspace_root_rejects_copied_binding_pointing_elsewhere() -> TestResult
{
    let root = test_root("resolve-copied-binding");
    let scratch = root.join("scratch");
    let stack_root = root.join("stack");
    let copied_stack_root = root.join("copied-stack");
    let workspace_root = root.join("workspace");
    seed_workspace_base(&stack_root, &workspace_root)?;
    std::fs::create_dir_all(&copied_stack_root)?;
    std::fs::copy(
        stack_root.join(layerstack::WORKSPACE_BINDING_FILE),
        copied_stack_root.join(layerstack::WORKSPACE_BINDING_FILE),
    )?;
    let runtime = isolated_runtime(&scratch, Path::new("/configured-fallback"));

    let error = runtime
        .resolve_workspace_root(&workspace_root)
        .expect_err("copied binding should not redirect to another stack root");

    assert!(error
        .to_string()
        .contains("points at different layer_stack_root"));
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[test]
fn workspace_runtime_resolve_workspace_root_reads_layer_stack_binding() -> TestResult {
    let root = test_root("resolve-workspace-root");
    let scratch = root.join("scratch");
    let stack_root = root.join("stack");
    let workspace_root = root.join("workspace");
    seed_workspace_base(&stack_root, &workspace_root)?;
    let runtime = isolated_runtime(&scratch, Path::new("/configured-fallback"));

    let resolved = runtime.resolve_workspace_root(&workspace_root)?;

    assert_eq!(resolved.workspace_root, workspace_root.canonicalize()?);
    assert_eq!(resolved.layer_stack_root, stack_root.canonicalize()?);
    assert_eq!(
        resolved.binding.workspace_root,
        workspace_root.to_string_lossy()
    );
    assert_eq!(
        resolved.binding.layer_stack_root,
        stack_root.to_string_lossy()
    );
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[test]
fn workspace_runtime_resolve_workspace_root_rejects_ambiguous_bindings_after_state_is_bound(
) -> TestResult {
    let root = test_root("resolve-ambiguous-bound-root");
    let scratch = root.join("scratch");
    let stack_a = root.join("stack-a");
    let stack_b = root.join("stack-b");
    let workspace_root = root.join("workspace");
    seed_workspace_base(&stack_a, &workspace_root)?;
    let runtime = isolated_runtime(&scratch, Path::new("/configured-fallback"));
    runtime.enter("caller-bound", &workspace_root)?;
    seed_workspace_base(&stack_b, &workspace_root)?;

    let error = runtime
        .resolve_workspace_root(&workspace_root)
        .expect_err("bound state should not bypass ambiguity checks");

    assert!(error.to_string().contains("ambiguous workspace_root"));
    runtime.exit("caller-bound", None)?;
    let _ = runtime.test_reset();
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[test]
fn workspace_runtime_resolve_legacy_layer_stack_root_reads_compatibility_binding() -> TestResult {
    let root = test_root("resolve-legacy-root");
    let scratch = root.join("scratch");
    let stack_root = root.join("stack");
    let workspace_root = root.join("workspace");
    seed_workspace_base(&stack_root, &workspace_root)?;
    let runtime = isolated_runtime(&scratch, Path::new("/configured-fallback"));

    let resolved = runtime.resolve_legacy_layer_stack_root(&stack_root)?;

    assert_eq!(resolved.workspace_root, workspace_root.canonicalize()?);
    assert_eq!(resolved.layer_stack_root, stack_root.canonicalize()?);
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[test]
fn workspace_runtime_resolve_workspace_root_rejects_ambiguous_bindings() -> TestResult {
    let root = test_root("resolve-ambiguous-root");
    let scratch = root.join("scratch");
    let workspace_root = root.join("workspace");
    seed_workspace_base(&root.join("stack-a"), &workspace_root)?;
    seed_workspace_base(&root.join("stack-b"), &workspace_root)?;
    let runtime = isolated_runtime(&scratch, Path::new("/configured-fallback"));

    let error = runtime
        .resolve_workspace_root(&workspace_root)
        .expect_err("ambiguous workspace bindings should fail");

    assert!(error.to_string().contains("ambiguous workspace_root"));
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[test]
fn workspace_runtime_test_reset_rewrites_invalid_manager_json() -> TestResult {
    let root = test_root("reset-manager");
    let scratch = root.join("scratch");
    std::fs::create_dir_all(&scratch)?;
    std::fs::write(
        scratch.join("manager.json"),
        r#"{"schema_version":999,"handles":[{"workspace_handle_id":"ghost"}]}"#,
    )?;
    let runtime = isolated_runtime(&scratch, Path::new("/testbed"));

    let recovery = runtime.test_reset_report();

    assert_eq!(recovery.exited_callers, Vec::<String>::new());
    assert!(
        recovery
            .manager_json_error
            .as_deref()
            .is_some_and(|error| error.contains("expected schema_version 1, got 999")),
        "invalid manager schema must be retained in the recovery report"
    );
    let rewritten = std::fs::read_to_string(scratch.join("manager.json"))?;
    assert_eq!(
        serde_json::from_str::<Value>(&rewritten)?,
        json!({"schema_version": 1, "handles": []})
    );
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

fn isolated_runtime(scratch_root: &Path, workspace_root: &Path) -> WorkspaceRuntime {
    isolated_runtime_with_max_depth(
        scratch_root,
        workspace_root,
        layerstack::CommitOptions::default().auto_squash_max_depth,
    )
}

fn isolated_runtime_with_max_depth(
    scratch_root: &Path,
    workspace_root: &Path,
    auto_squash_max_depth: usize,
) -> WorkspaceRuntime {
    isolated_runtime_with_command(
        scratch_root,
        workspace_root,
        command_ops_with_max_depth(auto_squash_max_depth),
    )
}

fn isolated_runtime_with_command(
    scratch_root: &Path,
    workspace_root: &Path,
    command: Arc<CommandOps>,
) -> WorkspaceRuntime {
    // The namespace/network layer stubs itself under the isolated-workspace
    // test harness env; every test in this binary drives the stubbed setup, so
    // the variable is set once and never removed (no cross-test race).
    std::env::set_var("EOS_ISOLATED_WORKSPACE_TEST_HARNESS", "true");
    WorkspaceRuntime::new(
        IsolatedWorkspaceConfig {
            enabled: true,
            scratch_root: scratch_root.to_path_buf(),
            workspace_root: workspace_root.to_path_buf(),
            ..IsolatedWorkspaceConfig::default()
        },
        command,
    )
}

fn command_ops_with_max_depth(auto_squash_max_depth: usize) -> Arc<CommandOps> {
    Arc::new(CommandOps::with_commit_options_and_capture_options(
        command::CommandConfig::default(),
        CommitOptions::new(auto_squash_max_depth),
        BoundedCaptureOptions::default(),
    ))
}

fn test_root(label: &str) -> PathBuf {
    let root =
        std::env::temp_dir().join(format!("daemon-workspace-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    root
}

fn seed_workspace_base(root: &Path, workspace_root: &Path) -> TestResult {
    std::fs::create_dir_all(workspace_root)?;
    std::fs::write(workspace_root.join("seed.txt"), "seed\n")?;
    layerstack::build_workspace_base(root, workspace_root, true)?;
    Ok(())
}
