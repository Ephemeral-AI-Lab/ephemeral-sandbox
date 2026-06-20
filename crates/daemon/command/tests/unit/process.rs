use super::*;

use serde_json::json;
use workspace::{WorkspaceEntry, WorkspaceEntryFds};

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
fn take_exit_reads_transcript_and_persist_removes_it() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!(
        "command-take-exit-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    std::fs::create_dir_all(&root)?;
    let transcript_path = root.join("transcript.log");
    let final_path = root.join("final.json");
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
            root.join("runner-result.json"),
            final_path.clone(),
            transcript_path.clone(),
        ),
    );

    let exit = process.take_exit().expect("inactive process has an exit");
    assert_eq!(exit.stdout, "captured transcript output");
    assert!(exit.kill.is_none());
    assert!(process.take_exit().is_none());

    let response = json!({
        "status": "ok",
        "exit_code": 0,
        "output": {
            "stdout": exit.stdout,
            "stderr": "",
        },
        "command_session_id": "cmd_1",
        "workspace": "shared",
    });
    let persistence = process.persist_final(&response);

    assert!(final_path.exists());
    assert_eq!(
        persistence.final_response,
        Some(CommandFinalResponsePersistence::Persisted {
            path: final_path.clone(),
            bytes: std::fs::metadata(&final_path)?.len().try_into()?,
        })
    );
    assert_eq!(persistence.transcript_error, None);
    let final_response: serde_json::Value = serde_json::from_slice(&std::fs::read(&final_path)?)?;
    assert_eq!(
        final_response
            .get("output")
            .and_then(|output| output.get("stdout"))
            .and_then(serde_json::Value::as_str),
        Some("captured transcript output")
    );
    assert!(!transcript_path.exists());

    let _ = std::fs::remove_dir_all(root);
    Ok(())
}

#[test]
fn spawn_reports_command_request_artifact_write_failure() -> Result<(), Box<dyn std::error::Error>>
{
    let root = std::env::temp_dir().join(format!(
        "command-spawn-artifact-failure-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    let request_path = root.join("missing-parent").join("command-request.json");
    let error = match CommandProcess::spawn(
        CommandProcessSpec {
            id: "cmd_1".to_owned(),
            command: "echo ok".to_owned(),
            cwd: None,
            timeout_seconds: None,
        },
        CommandProcessSpawn {
            workspace_entry: workspace_entry(),
            request_path: request_path.clone(),
            output_path: root.join("runner-result.json"),
            final_path: root.join("final.json"),
            transcript_path: root.join("transcript.log"),
        },
    ) {
        Ok(_) => panic!("spawn should fail before opening a PTY"),
        Err(error) => error,
    };

    match error {
        CommandError::ArtifactWrite {
            artifact,
            path,
            error,
        } => {
            assert_eq!(artifact, "command_request");
            assert_eq!(path, request_path);
            assert!(!error.is_empty());
        }
        other => panic!("expected artifact write failure, got {other:?}"),
    }

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
    };

    let spawn = CommandProcessSpawn::prepare("cmd_7", workspace_entry(), &config)?;

    let command_dir = root.join("cmd_7");
    assert!(command_dir.is_dir());
    assert_eq!(spawn.artifact_dir(), command_dir);
    assert_eq!(
        spawn.request_path,
        root.join("cmd_7").join("command-request.json")
    );
    assert_eq!(
        spawn.output_path,
        root.join("cmd_7").join("runner-result.json")
    );
    assert_eq!(spawn.final_path, root.join("cmd_7").join("final.json"));
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
    };
    let spawn = CommandProcessSpawn::prepare("cmd_8", workspace_entry(), &config)?;
    std::fs::write(&spawn.transcript_path, b"partial output")?;

    spawn.cleanup_artifacts_after_start_failure()?;

    assert!(!root.join("cmd_8").exists());
    let _ = std::fs::remove_dir_all(root);
    Ok(())
}

#[test]
fn builds_namespace_runner_request_from_command_spec_and_workspace_entry(
) -> Result<(), Box<dyn std::error::Error>> {
    let request = build_namespace_command_request(
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

#[test]
fn write_process_metadata_records_process_group_id() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!(
        "command-process-metadata-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    std::fs::create_dir_all(&root)?;
    let path = root.join(PROCESS_METADATA_FILE);

    write_process_metadata(&path, Some(12345))?;

    let metadata = CommandProcessMetadata::from_slice(&std::fs::read(&path)?)?;
    assert_eq!(metadata, CommandProcessMetadata::new(Some(12345)));

    let _ = std::fs::remove_dir_all(root);
    Ok(())
}

#[test]
fn persist_final_reports_final_and_transcript_failures() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!(
        "command-persist-failures-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    std::fs::create_dir_all(&root)?;
    let final_path = root.join("final-as-dir");
    let transcript_path = root.join("transcript-as-dir");
    std::fs::create_dir_all(&final_path)?;
    std::fs::create_dir_all(&transcript_path)?;

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
            root.join("runner-result.json"),
            final_path.clone(),
            transcript_path.clone(),
        ),
    );

    let persistence = process.persist_final(&json!({"status": "ok"}));

    match persistence.final_response {
        Some(CommandFinalResponsePersistence::Failed { path, error }) => {
            assert_eq!(path, final_path);
            assert!(!error.is_empty());
        }
        other => panic!("expected final persistence failure, got {other:?}"),
    }
    let transcript_error = persistence
        .transcript_error
        .expect("directory transcript removal reports failure");
    assert_eq!(transcript_error.path, transcript_path);
    assert!(!transcript_error.error.is_empty());

    let _ = std::fs::remove_dir_all(root);
    Ok(())
}
