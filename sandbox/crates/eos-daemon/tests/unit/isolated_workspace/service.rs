use super::*;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

#[test]
fn exit_tears_down_caller_handle() -> TestResult {
    // op_exit is the per-caller workspace-run teardown: it discards the
    // caller's command sessions (owned by the command-session registry now,
    // not an isolated side-map) and removes the handle.
    let _guard = lock_isolated_test_state();
    let root = std::env::temp_dir().join(format!(
        "eos-daemon-iws-command-session-block-{}",
        std::process::id()
    ));
    let scratch = root.join("scratch");
    configure_test_isolated_workspace(&scratch, Path::new("/testbed"));
    set_env(TEST_HARNESS_ENV, "true");
    let _ = op_test_reset(&json!({}), DispatchContext::empty());
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("layers"))?;
    std::fs::create_dir_all(root.join("staging"))?;
    std::fs::write(
        root.join("manifest.json"),
        r#"{"schema_version":1,"version":1,"layers":[]}"#,
    )?;

    let entered = op_enter(
        &json!({"caller_id": "caller-command-session", "layer_stack_root": root}),
        DispatchContext::empty(),
    )?;
    assert_eq!(entered["success"], true);

    let exited = op_exit(
        &json!({"caller_id": "caller-command-session"}),
        DispatchContext::empty(),
    )?;
    assert_eq!(exited["success"], true);
    assert_eq!(
        exited["inspection"]["handle_registered_after"],
        json!(false)
    );
    let _ = op_test_reset(&json!({}), DispatchContext::empty());
    clear_env(TEST_HARNESS_ENV);
    reset_isolated_workspace_config();
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[test]
fn enter_uses_workspace_binding_over_configured_workspace_root() -> TestResult {
    let _guard = lock_isolated_test_state();
    let root = std::env::temp_dir().join(format!(
        "eos-daemon-iws-bound-workspace-root-{}",
        std::process::id()
    ));
    let scratch = root.join("scratch");
    let stack_root = root.join("stack");
    let workspace_root = root.join("workspace");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&workspace_root)?;
    std::fs::write(workspace_root.join("seed.txt"), "seed\n")?;
    eos_layerstack::build_workspace_base(&stack_root, &workspace_root, true)?;
    configure_test_isolated_workspace(&scratch, Path::new("/configured-fallback"));
    set_env(TEST_HARNESS_ENV, "true");
    let _ = op_test_reset(&json!({}), DispatchContext::empty());

    let entered = op_enter(
        &json!({"caller_id": "caller-bound-root", "layer_stack_root": stack_root}),
        DispatchContext::empty(),
    )?;

    assert_eq!(entered["success"], true);
    let expected_workspace_root = workspace_root.to_string_lossy().into_owned();
    assert_eq!(
        entered["workspace_root"],
        json!(expected_workspace_root.clone())
    );
    let status = op_status(
        &json!({"caller_id": "caller-bound-root"}),
        DispatchContext::empty(),
    )?;
    assert_eq!(status["success"], true);
    assert_eq!(status["open"], true);
    assert_eq!(
        status["workspace_root"],
        json!(expected_workspace_root.clone())
    );

    let exited = op_exit(
        &json!({"caller_id": "caller-bound-root"}),
        DispatchContext::empty(),
    )?;
    assert_eq!(exited["success"], true);
    let _ = op_test_reset(&json!({}), DispatchContext::empty());
    clear_env(TEST_HARNESS_ENV);
    reset_isolated_workspace_config();
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[test]
fn enter_rebinds_idle_state_to_new_layer_stack_root() -> TestResult {
    let _guard = lock_isolated_test_state();
    let root =
        std::env::temp_dir().join(format!("eos-daemon-iws-root-switch-{}", std::process::id()));
    let scratch = root.join("scratch");
    let stack_a = root.join("stack-a");
    let stack_b = root.join("stack-b");
    configure_test_isolated_workspace(&scratch, Path::new("/testbed"));
    set_env(TEST_HARNESS_ENV, "true");
    let _ = op_test_reset(&json!({}), DispatchContext::empty());
    let _ = std::fs::remove_dir_all(&root);
    seed_empty_stack(&stack_a)?;
    seed_empty_stack(&stack_b)?;

    let entered_a = op_enter(
        &json!({"caller_id": "caller-root-a", "layer_stack_root": stack_a}),
        DispatchContext::empty(),
    )?;
    assert_eq!(entered_a["success"], true);
    assert_eq!(
        eos_layerstack::LayerStack::open(stack_a.clone())?.active_lease_count(),
        1
    );
    assert_eq!(
        eos_layerstack::LayerStack::open(stack_b.clone())?.active_lease_count(),
        0
    );
    let exited_a = op_exit(
        &json!({"caller_id": "caller-root-a"}),
        DispatchContext::empty(),
    )?;
    assert_eq!(exited_a["success"], true);

    let entered_b = op_enter(
        &json!({"caller_id": "caller-root-b", "layer_stack_root": stack_b}),
        DispatchContext::empty(),
    )?;
    assert_eq!(entered_b["success"], true);
    assert_eq!(
        eos_layerstack::LayerStack::open(stack_a.clone())?.active_lease_count(),
        0
    );
    assert_eq!(
        eos_layerstack::LayerStack::open(stack_b.clone())?.active_lease_count(),
        1
    );

    let exited_b = op_exit(
        &json!({"caller_id": "caller-root-b"}),
        DispatchContext::empty(),
    )?;
    assert_eq!(exited_b["success"], true);
    let _ = op_test_reset(&json!({}), DispatchContext::empty());
    clear_env(TEST_HARNESS_ENV);
    reset_isolated_workspace_config();
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[test]
fn test_reset_rewrites_invalid_manager_json() -> TestResult {
    let _guard = lock_isolated_test_state();
    let root = std::env::temp_dir().join(format!(
        "eos-daemon-iws-reset-manager-{}",
        std::process::id()
    ));
    let scratch = root.join("scratch");
    let manager_root = scratch.clone();
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&manager_root)?;
    std::fs::write(
        manager_root.join("manager.json"),
        r#"{"schema_version":999,"handles":[{"workspace_handle_id":"ghost"}]}"#,
    )?;
    configure_test_isolated_workspace(&scratch, Path::new("/testbed"));
    set_env(TEST_HARNESS_ENV, "true");

    let reset = op_test_reset(&json!({}), DispatchContext::empty())?;

    assert_eq!(reset["success"], true);
    let rewritten = std::fs::read_to_string(manager_root.join("manager.json"))?;
    assert_eq!(
        serde_json::from_str::<Value>(&rewritten)?,
        json!({"schema_version": 1, "handles": []})
    );
    clear_env(TEST_HARNESS_ENV);
    reset_isolated_workspace_config();
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[test]
fn host_ram_pressure_error_keeps_capacity_details() {
    let response = error_payload(&IsolatedError::HostRamPressure {
        required_bytes: 30,
        budget_bytes: 29,
    });
    assert_eq!(response["success"], false);
    assert_eq!(response["error"]["kind"], "host_ram_pressure");
    assert_eq!(response["error"]["details"]["required_bytes"], 30);
    assert_eq!(response["error"]["details"]["budget_bytes"], 29);
}

fn set_env(key: &str, value: &str) {
    std::env::set_var(key, value);
}

fn clear_env(key: &str) {
    std::env::remove_var(key);
}

fn configure_test_isolated_workspace(scratch_root: &Path, workspace_root: &Path) {
    let mut config = default_isolated_workspace_config();
    config.enabled = true;
    config.scratch_root = scratch_root.to_path_buf();
    config.workspace_root = workspace_root.to_path_buf();
    configure_isolated_workspace(&config);
}

fn reset_isolated_workspace_config() {
    configure_isolated_workspace(&default_isolated_workspace_config());
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
