use std::collections::BTreeMap;

use super::*;

#[test]
fn prepare_builds_setns_runner_request_without_publish() -> Result<(), Box<dyn std::error::Error>> {
    let scratch_dir = std::env::temp_dir().join(format!(
        "eos-isolated-command-prepare-{}",
        std::process::id()
    ));
    let workspace_root = PathBuf::from("/configured-workspace");
    let _ = std::fs::remove_dir_all(&scratch_dir);

    let prepared = prepare_isolated_command(
        IsolatedCommandPrepareContext {
            workspace_handle_id: "iws-1".to_owned(),
            workspace_root: workspace_root.clone(),
            scratch_dir: scratch_dir.clone(),
            layer_paths: vec![PathBuf::from("/lower/a")],
            upperdir: scratch_dir.join("upper"),
            workdir: scratch_dir.join("work"),
            ns_fds: HashMap::from([
                ("user".to_owned(), 10),
                ("mnt".to_owned(), 11),
                ("pid".to_owned(), 12),
                ("net".to_owned(), 13),
            ]),
            cgroup_path: Some(PathBuf::from("/sys/fs/cgroup/eos/iws-1")),
        },
        PrepareCommandRequest {
            caller_id: "caller-1".to_owned(),
            command_session_id: "cmd-1".to_owned(),
            invocation_id: "inv-1".to_owned(),
            cmd: "pwd".to_owned(),
            timeout_seconds: Some(4.0),
        },
    )?;

    assert_eq!(prepared.run_request["mode"], "set_ns");
    assert_eq!(
        prepared.run_request["workspace_root"],
        workspace_root.to_string_lossy().as_ref()
    );
    assert_eq!(prepared.run_request["ns_fds"]["user"], 10);
    assert_eq!(prepared.run_request["tool_call"]["args"]["command"], "pwd");
    assert_eq!(prepared.run_request["layer_paths"][0], "/lower/a");
    assert_eq!(
        prepared.session_dir,
        scratch_dir.join("command-sessions").join("cmd-1")
    );

    let _ = std::fs::remove_dir_all(scratch_dir);
    Ok(())
}

#[test]
fn finalize_captures_audit_only_changes_without_publish() -> Result<(), Box<dyn std::error::Error>>
{
    let root = std::env::temp_dir().join(format!(
        "eos-isolated-command-finalize-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let upperdir = root.join("upper");
    std::fs::create_dir_all(&upperdir)?;
    std::fs::write(upperdir.join("private.txt"), b"private")?;

    let mut outcome = finalize_isolated_command(
        IsolatedCommandFinalizeContext {
            caller_id: "caller-1".to_owned(),
            workspace_handle_id: "iws-1".to_owned(),
            manifest_version: 7,
            manifest_root_hash: "hash".to_owned(),
            upperdir,
            base_timings: BTreeMap::new(),
        },
        FinalizeCommandRequest {
            runner_result: Some(json!({
                "tool_result": {"timings": {"workspace.mount_s": 0.1, "workspace.tool_s": 0.2}},
                "exit_code": 0,
            })),
            command_elapsed_s: 1.25,
            status: "ok".to_owned(),
            exit_code: Some(0),
            stdout: "done".to_owned(),
            stderr: String::new(),
            command_session_id: Some("cmd-1".to_owned()),
        },
    )?;

    assert_eq!(outcome.mode, WorkspaceMode::Isolated);
    assert!(outcome.success);
    assert_eq!(outcome.changed_paths, vec!["private.txt"]);
    assert_eq!(outcome.changed_path_kinds["private.txt"], "write");
    assert_eq!(outcome.timings["command_exec.occ_apply_s"], 0.0);
    assert_eq!(outcome.timings["command_exec.mount_workspace_s"], 0.1);
    assert_eq!(outcome.metadata["isolated_workspace"]["published"], false);

    let audit = take_isolated_audit(&mut outcome);
    assert_eq!(audit["published"], false);
    assert_eq!(audit["changed_paths"][0], "private.txt");
    assert!(outcome.metadata.get("audit").is_none());

    let _ = std::fs::remove_dir_all(root);
    Ok(())
}
