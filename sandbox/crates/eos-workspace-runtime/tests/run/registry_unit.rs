use super::*;

fn sample_completion(id: &str) -> CommandSessionCompletion {
    CommandSessionCompletion {
        command_session_id: id.to_owned(),
        caller_id: "caller".to_owned(),
        command: "cmd".to_owned(),
        result: CommandResponse::error(""),
    }
}

#[cfg(not(target_os = "linux"))]
fn ephemeral_run(id: &str, caller: &str) -> Arc<WorkspaceRun> {
    use std::path::PathBuf;

    use crate::command_session::session::CommandSessionSpec;
    use crate::ephemeral::{
        CallerId, EphemeralRunDirs, SnapshotLease, InvocationId, WorkspaceRoot,
    };

    let session = CommandSession::new(CommandSessionSpec {
        id: id.to_owned(),
        caller_id: caller.to_owned(),
        command: "sleep 1".to_owned(),
        timeout_seconds: None,
    });
    let workspace = EphemeralWorkspace {
        layer_stack_root: WorkspaceRoot(PathBuf::from("/layers")),
        workspace_root: PathBuf::from("/workspace"),
        caller_id: CallerId(caller.to_owned()),
        invocation_id: InvocationId("inv".to_owned()),
        snapshot: SnapshotLease {
            lease_id: "lease".to_owned(),
            manifest_version: 1,
            manifest_root_hash: "hash".to_owned(),
            layer_paths: Vec::new(),
        },
        dirs: EphemeralRunDirs {
            run_dir: PathBuf::from("/run"),
            upperdir: PathBuf::from("/upper"),
            workdir: PathBuf::from("/work"),
            output_path: PathBuf::from("/out.json"),
            final_path: PathBuf::from("/final.json"),
            request_path: None,
            result_path: None,
        },
    };
    Arc::new(WorkspaceRun::Ephemeral(EphemeralRun { session, workspace }))
}

#[cfg(not(target_os = "linux"))]
#[test]
fn insert_get_count_remove_track_caller_runs() {
    let registry = WorkspaceRunRegistry::new();
    registry.insert(ephemeral_run("cmd_1", "caller"));
    registry.insert(ephemeral_run("cmd_2", "caller"));
    registry.insert(ephemeral_run("cmd_3", "other"));

    assert_eq!(registry.count_by_caller(Some("caller")), 2);
    assert_eq!(registry.count_by_caller(Some("other")), 1);
    assert_eq!(registry.count_by_caller(None), 3);
    assert!(registry.get("cmd_2").is_some());
    assert_eq!(registry.caller_sessions("caller").len(), 2);

    assert!(registry.remove("cmd_2").is_some());
    assert_eq!(registry.count_by_caller(Some("caller")), 1);
    assert!(registry.remove("cmd_1").is_some());
    assert_eq!(registry.count_by_caller(Some("caller")), 0);
    assert_eq!(registry.live().len(), 1);
}

#[test]
fn push_completed_evicts_oldest_beyond_cap() {
    let registry = WorkspaceRunRegistry::new();
    let overflow = 5;
    for index in 0..(MAX_COMPLETED_ENTRIES + overflow) {
        registry.push_completed(sample_completion(&format!("cmd_{index}")));
    }

    assert_eq!(lock(&registry.completed).len(), MAX_COMPLETED_ENTRIES);
    for index in 0..overflow {
        assert!(registry
            .take_completed_result(&format!("cmd_{index}"))
            .is_none());
    }
    let newest = format!("cmd_{}", MAX_COMPLETED_ENTRIES + overflow - 1);
    assert!(registry.take_completed_result(&newest).is_some());
}
