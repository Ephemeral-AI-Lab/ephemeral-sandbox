use std::collections::BTreeMap;
use std::path::PathBuf;

use eos_protocol::LayerChange;

use super::*;
use crate::ephemeral::{EphemeralRunDirs, PublishStatus};

struct FakePublisher;

impl WorkspacePublisherPort for FakePublisher {
    fn publish_upperdir_changes(
        &self,
        _root: &WorkspaceRoot,
        _snapshot: &EphemeralSnapshot,
        changes: &[LayerChange],
        _path_kinds: &[PathChange],
    ) -> Result<PublishOutcome, EphemeralWorkspaceError> {
        Ok(PublishOutcome {
            status: PublishStatus::Published,
            manifest_version: Some(8),
            published_paths: vec!["result.txt".to_owned()],
            conflicts: Vec::new(),
            timings: BTreeMap::from([("occ.commit.total_s".to_owned(), json!(0.25))]),
            raw: json!({
                "files": changes.iter().map(|change| json!({
                    "path": change.path().as_str(),
                    "status": "committed",
                    "message": "",
                })).collect::<Vec<_>>(),
                "published_manifest_version": 8,
                "timings": {"occ.commit.total_s": 0.25},
            }),
        })
    }
}

fn workspace_with_upperdir(root: &std::path::Path) -> EphemeralWorkspace {
    let upperdir = root.join("upper");
    let workdir = root.join("work");
    let run_dir = root.join("run");
    std::fs::create_dir_all(&upperdir).expect("upperdir");
    std::fs::create_dir_all(&workdir).expect("workdir");
    std::fs::create_dir_all(&run_dir).expect("run_dir");
    std::fs::write(upperdir.join("result.txt"), b"ok").expect("write upperdir file");
    EphemeralWorkspace {
        layer_stack_root: WorkspaceRoot(PathBuf::from("/layers")),
        workspace_root: PathBuf::from("/workspace"),
        caller_id: CallerId("caller-1".to_owned()),
        invocation_id: InvocationId("cmd-1".to_owned()),
        snapshot: EphemeralSnapshot {
            lease_id: "lease-1".to_owned(),
            manifest_version: 7,
            manifest_root_hash: "hash".to_owned(),
            layer_paths: vec![PathBuf::from("/lower/a")],
        },
        dirs: EphemeralRunDirs {
            run_dir,
            upperdir,
            workdir,
            output_path: root.join("runner-result.json"),
            final_path: root.join("final.json"),
            request_path: Some(root.join("runner-request.json")),
            result_path: None,
        },
    }
}

#[test]
fn prepare_builds_fresh_runner_request_and_session_metadata(
) -> Result<(), Box<dyn std::error::Error>> {
    let writable_root = std::env::temp_dir().join(format!(
        "eos-ephemeral-command-prepare-{}",
        std::process::id()
    ));
    let session_dir = writable_root.join("sessions").join("cmd-1");
    let workspace_root = PathBuf::from("/configured-workspace");
    let _ = std::fs::remove_dir_all(&writable_root);

    let prepared = prepare_ephemeral_command(
        EphemeralCommandPrepareContext {
            layer_stack_root: PathBuf::from("/layers"),
            workspace_root: workspace_root.clone(),
            writable_root: writable_root.clone(),
            session_dir: session_dir.clone(),
            final_path: session_dir.join("final.json"),
        },
        EphemeralSnapshot {
            lease_id: "lease-1".to_owned(),
            manifest_version: 7,
            manifest_root_hash: "hash".to_owned(),
            layer_paths: vec![PathBuf::from("/lower/a"), PathBuf::from("/lower/b")],
        },
        PrepareCommandRequest {
            caller_id: "caller-1".to_owned(),
            command_session_id: "cmd-1".to_owned(),
            invocation_id: "inv-1".to_owned(),
            cmd: "printf ok".to_owned(),
            timeout_seconds: Some(2.5),
        },
    )?;

    assert_eq!(prepared.prepared.run_request["mode"], "fresh_ns");
    assert_eq!(
        prepared.prepared.run_request["workspace_root"],
        workspace_root.to_string_lossy().as_ref()
    );
    assert_eq!(
        prepared.prepared.run_request["tool_call"]["args"]["command"],
        "printf ok"
    );
    assert_eq!(prepared.prepared.run_request["layer_paths"][0], "/lower/a");
    assert_eq!(prepared.workspace.snapshot.lease_id, "lease-1");
    let metadata = std::fs::read_to_string(prepared.prepared.session_dir.join("metadata.json"))?;
    assert!(metadata.contains("\"workspace\": \"ephemeral\""));

    let _ = std::fs::remove_dir_all(writable_root);
    Ok(())
}

#[test]
fn finalize_captures_publishes_and_shapes_command_outcome() -> Result<(), Box<dyn std::error::Error>>
{
    let root = std::env::temp_dir().join(format!(
        "eos-ephemeral-command-finalize-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let workspace = workspace_with_upperdir(&root);

    let outcome = finalize_ephemeral_command(
        &FakePublisher,
        workspace,
        BTreeMap::new(),
        FinalizeCommandRequest {
            runner_result: None,
            command_elapsed_s: 1.5,
            status: "ok".to_owned(),
            exit_code: Some(0),
            stdout: "done".to_owned(),
            stderr: String::new(),
            command_session_id: Some("cmd-1".to_owned()),
        },
    )?;

    assert_eq!(outcome.mode, WorkspaceMode::Ephemeral);
    assert!(outcome.success);
    assert_eq!(outcome.changed_paths, vec!["result.txt"]);
    assert_eq!(outcome.changed_path_kinds["result.txt"], "write");
    assert_eq!(outcome.timings["command_exec.occ_apply_s"], 0.25);

    let _ = std::fs::remove_dir_all(root);
    Ok(())
}

#[test]
fn finalize_marks_failed_command_unsuccessful_even_when_publish_succeeds(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!(
        "eos-ephemeral-command-finalize-failed-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let workspace = workspace_with_upperdir(&root);

    let outcome = finalize_ephemeral_command(
        &FakePublisher,
        workspace,
        BTreeMap::new(),
        FinalizeCommandRequest {
            runner_result: None,
            command_elapsed_s: 1.5,
            status: "error".to_owned(),
            exit_code: Some(2),
            stdout: String::new(),
            stderr: "failed".to_owned(),
            command_session_id: Some("cmd-1".to_owned()),
        },
    )?;

    assert!(!outcome.success);
    assert_eq!(outcome.status, "error");
    assert_eq!(outcome.exit_code, Some(2));
    assert_eq!(outcome.changed_paths, vec!["result.txt"]);

    let _ = std::fs::remove_dir_all(root);
    Ok(())
}
