//! Isolated-workspace runtime lifecycle: enter/exit custody, root rebinding,
//! and the test-reset sweep. The daemon's op adapters are covered by the
//! daemon's own integration tests; these drive the runtime API directly.

use std::path::{Path, PathBuf};

use config::configs::isolated_workspace::IsolatedWorkspaceConfig;
use serde_json::{json, Value};

use super::WorkspaceRuntime;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

#[test]
fn cancel_runs_for_caller_tears_down_entered_handle() -> TestResult {
    // The per-caller workspace-run teardown discards the caller's commands
    // (owned by the command registry, not a side-map here)
    // and removes the handle, releasing its lease.
    let root = test_root("cancel-runs-teardown");
    let scratch = root.join("scratch");
    let runtime = isolated_runtime(&scratch, Path::new("/testbed"));
    seed_empty_stack(&root.join("stack"))?;

    let entered = runtime.enter("caller-command", &root.join("stack"))?;
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
fn enter_uses_workspace_binding_over_configured_workspace_root() -> TestResult {
    let root = test_root("bound-workspace-root");
    let scratch = root.join("scratch");
    let stack_root = root.join("stack");
    let workspace_root = root.join("workspace");
    std::fs::create_dir_all(&workspace_root)?;
    std::fs::write(workspace_root.join("seed.txt"), "seed\n")?;
    layerstack::build_workspace_base(&stack_root, &workspace_root, true)?;
    let runtime = isolated_runtime(&scratch, Path::new("/configured-fallback"));

    let entered = runtime.enter("caller-bound-root", &stack_root)?;

    let expected_workspace_root = workspace_root.to_string_lossy().into_owned();
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
fn enter_rebinds_idle_state_to_new_layer_stack_root() -> TestResult {
    let root = test_root("root-switch");
    let scratch = root.join("scratch");
    let stack_a = root.join("stack-a");
    let stack_b = root.join("stack-b");
    let runtime = isolated_runtime(&scratch, Path::new("/testbed"));
    seed_empty_stack(&stack_a)?;
    seed_empty_stack(&stack_b)?;

    runtime.enter("caller-root-a", &stack_a)?;
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

    runtime.enter("caller-root-b", &stack_b)?;
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
fn test_reset_rewrites_invalid_manager_json() -> TestResult {
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
        std::sync::Arc::new(operation::command::CommandOps::new(
            command::CommandConfig::default(),
        )),
    )
}

fn test_root(label: &str) -> PathBuf {
    let root =
        std::env::temp_dir().join(format!("daemon-workspace-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    root
}

fn seed_empty_stack(root: &Path) -> TestResult {
    std::fs::create_dir_all(root.join("layers"))?;
    std::fs::create_dir_all(root.join("staging"))?;
    std::fs::write(
        root.join("manifest.json"),
        r#"{"schema_version":1,"version":1,"layers":[]}"#,
    )?;
    Ok(())
}
