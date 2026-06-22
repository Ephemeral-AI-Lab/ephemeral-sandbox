use super::*;

use sandbox_runtime_workspace::{WorkspaceEntry, WorkspaceEntryFds};

fn workspace_entry() -> WorkspaceEntry {
    WorkspaceEntry {
        workspace_root: "/workspace".into(),
        layer_paths: vec!["/lower/one".into(), "/lower/two".into()],
        upperdir: "/tmp/eos/upper".into(),
        workdir: "/tmp/eos/work".into(),
        ns_fds: WorkspaceEntryFds {
            user: 10,
            mnt: 11,
            pid: 12,
            net: Some(13),
        },
        cgroup_path: Some("/sys/fs/cgroup/eos".into()),
    }
}

fn workspace_entry_without_cgroup() -> WorkspaceEntry {
    let mut entry = workspace_entry();
    entry.cgroup_path = None;
    entry
}

#[test]
fn process_exposes_identity() {
    let process = CommandProcess::inactive_for_test(CommandProcessSpec {
        id: "cmd_1".to_owned(),
        command: "echo ok".to_owned(),
        cwd: None,
        timeout_seconds: Some(0.001),
    });

    assert_eq!(process.id(), "cmd_1");
    assert_eq!(process.command(), "echo ok");
}

#[test]
fn take_exit_reads_transcript_and_retains_it() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!(
        "command-take-exit-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    std::fs::create_dir_all(&root)?;
    let transcript_path = root.join("transcript.log");
    std::fs::write(&transcript_path, b"captured transcript output")?;

    let writer = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/null")?;
    let process = CommandProcess::with_runtime(
        CommandProcessSpec {
            id: "cmd_1".to_owned(),
            command: "echo ok".to_owned(),
            cwd: None,
            timeout_seconds: None,
        },
        CommandProcessRuntime::new(
            crate::pty::PtyProcess::inactive(writer),
            transcript_path.clone(),
            None,
            sandbox_runtime_workspace::CgroupMonitorConfig::default(),
        ),
    );

    let exit = process.take_exit().expect("inactive process has an exit");
    assert_eq!(exit.stdout, "captured transcript output");
    assert!(exit.kill.is_none());
    assert!(process.take_exit().is_none());
    assert!(transcript_path.exists());

    let _ = std::fs::remove_dir_all(root);
    Ok(())
}

#[test]
fn process_spawn_prepare_owns_command_artifact_layout() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!(
        "command-spawn-prepare-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    let config = CommandConfig {
        scratch_root: root.clone(),
        cgroup_monitor: sandbox_runtime_workspace::CgroupMonitorConfig::default(),
    };

    let spawn = CommandProcessSpawn::prepare("cmd_7", workspace_entry_without_cgroup(), &config)?;

    let command_dir = root.join("cmd_7");
    assert!(command_dir.is_dir());
    assert_eq!(spawn.artifact_dir(), command_dir);
    assert_eq!(
        spawn.transcript_path,
        root.join("cmd_7").join("transcript.log")
    );
    let _ = std::fs::remove_dir_all(root);
    Ok(())
}

#[test]
fn process_spawn_cleanup_removes_prepared_artifacts() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!(
        "command-spawn-cleanup-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    let config = CommandConfig {
        scratch_root: root.clone(),
        cgroup_monitor: sandbox_runtime_workspace::CgroupMonitorConfig::default(),
    };
    let spawn = CommandProcessSpawn::prepare("cmd_8", workspace_entry_without_cgroup(), &config)?;
    std::fs::write(&spawn.transcript_path, b"partial output")?;

    spawn.cleanup_artifacts_after_start_failure()?;

    assert!(!root.join("cmd_8").exists());
    let _ = std::fs::remove_dir_all(root);
    Ok(())
}

#[test]
fn process_spawn_prepare_creates_command_child_cgroup_when_session_parent_exists(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!(
        "command-cgroup-prepare-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    let session_cgroup = root.join("eos").join("sessions").join("ws-1");
    std::fs::create_dir_all(&session_cgroup)?;
    let config = CommandConfig {
        scratch_root: root.join("commands"),
        cgroup_monitor: sandbox_runtime_workspace::CgroupMonitorConfig::default(),
    };
    let mut entry = workspace_entry();
    entry.cgroup_path = Some(session_cgroup.clone());

    let spawn = CommandProcessSpawn::prepare("cmd_9", entry, &config)?;

    let command_cgroup = session_cgroup.join("commands").join("cmd_9");
    assert!(command_cgroup.is_dir());
    assert_eq!(
        spawn.workspace_entry.cgroup_path.as_deref(),
        Some(command_cgroup.as_path())
    );
    assert_eq!(
        spawn
            .cgroup_target()
            .expect("command cgroup target is retained")
            .cgroup_path,
        command_cgroup
    );
    spawn.cleanup_artifacts_after_start_failure()?;
    assert!(!session_cgroup.join("commands").join("cmd_9").exists());

    let _ = std::fs::remove_dir_all(root);
    Ok(())
}

#[test]
fn process_spawn_prepare_rejects_missing_session_cgroup_parent(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!(
        "command-cgroup-missing-parent-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    let config = CommandConfig {
        scratch_root: root.join("commands"),
        cgroup_monitor: sandbox_runtime_workspace::CgroupMonitorConfig::default(),
    };
    let mut entry = workspace_entry();
    entry.cgroup_path = Some(root.join("eos").join("sessions").join("missing"));

    let error = match CommandProcessSpawn::prepare("cmd_missing", entry, &config) {
        Ok(_) => panic!("missing parent cgroup is a launch error"),
        Err(error) => error,
    };

    assert!(error
        .to_string()
        .contains("session cgroup path does not exist"));
    let _ = std::fs::remove_dir_all(root);
    Ok(())
}

#[test]
fn command_cgroup_final_sample_respects_disabled_monitor() -> Result<(), Box<dyn std::error::Error>>
{
    let root = std::env::temp_dir().join(format!(
        "command-cgroup-disabled-final-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    let session_cgroup = root.join("eos").join("sessions").join("ws-1");
    std::fs::create_dir_all(&session_cgroup)?;
    let cgroup = crate::cgroup::CommandCgroup::prepare(
        "cmd_disabled",
        Some(&session_cgroup),
        root.join("upper").as_path(),
    )?
    .expect("command cgroup is prepared");

    let sample = cgroup.final_sample(&sandbox_runtime_workspace::CgroupMonitorConfig {
        enabled: false,
        ..sandbox_runtime_workspace::CgroupMonitorConfig::default()
    });

    assert!(sample.is_none());
    let _ = cgroup.cleanup();
    let _ = std::fs::remove_dir_all(root);
    Ok(())
}

#[test]
fn builds_namespace_runner_request_from_command_spec_and_workspace_entry(
) -> Result<(), Box<dyn std::error::Error>> {
    let request = build_namespace_runner_request(
        &CommandProcessSpec {
            id: "cmd_1".to_owned(),
            command: "printf ok".to_owned(),
            cwd: Some("/workspace/src".into()),
            timeout_seconds: Some(2.5),
        },
        workspace_entry(),
    );

    let request = serde_json::to_value(request)?;
    assert!(request.get("mode").is_none());
    assert_eq!(request["request_id"], "cmd_1");
    assert_eq!(request["args"]["command"], "printf ok");
    assert_eq!(request["args"]["cwd"], "/workspace/src");
    assert_eq!(request["workspace_root"], "/workspace");
    assert_eq!(request["layer_paths"][0], "/lower/one");
    assert_eq!(request["layer_paths"][1], "/lower/two");
    assert_eq!(request["upperdir"], "/tmp/eos/upper");
    assert_eq!(request["workdir"], "/tmp/eos/work");
    assert_eq!(request["ns_fds"]["user"], 10);
    assert_eq!(request["ns_fds"]["mnt"], 11);
    assert_eq!(request["ns_fds"]["pid"], 12);
    assert_eq!(request["ns_fds"]["net"], 13);
    assert_eq!(request["cgroup_path"], "/sys/fs/cgroup/eos");
    assert_eq!(request["timeout_seconds"], 2.5);

    Ok(())
}
