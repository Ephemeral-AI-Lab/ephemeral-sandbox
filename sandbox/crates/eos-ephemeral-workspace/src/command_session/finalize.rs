use serde_json::{json, Value};

use eos_workspace_api::{
    ChangedPathKinds, FinalizeCommandRequest, WorkspaceApiError, WorkspaceCommandOutcome,
    WorkspaceConflict, WorkspaceMode, WorkspaceTimings,
};

use super::types::{EphemeralCommandFinalizeContext, EphemeralCommandSessionPort};
use crate::{
    finalize_publishable_workspace, EphemeralWorkspaceError, FinalizeRequest, PathChangeKind,
    PublishOutcome, TreeResourceStats, WorkspacePublisherPort,
};

pub(super) fn finalize_command_workspace<P>(
    port: &P,
    context: EphemeralCommandFinalizeContext,
    request: FinalizeCommandRequest,
) -> Result<WorkspaceCommandOutcome, WorkspaceApiError>
where
    P: EphemeralCommandSessionPort,
{
    let publisher = CommandPublisher { port };
    let finalize = finalize_publishable_workspace(
        &publisher,
        FinalizeRequest {
            workspace: context.workspace,
            command_started_at: None,
        },
    )
    .map_err(workspace_api_error)?;
    let files = publish_files(&finalize.publish)?;
    let path_kinds = path_changes_to_wire(&finalize.capture.path_kinds);
    let changed_path_kinds = path_kinds.into_iter().collect::<ChangedPathKinds>();
    let first_conflict = files.iter().find(|file| !status_is_success(&file.status));
    let mut timings = context.base_timings;
    timings.insert(
        "resource.command_exec.changed_path_count".to_owned(),
        json!(usize_to_f64_saturating(changed_path_kinds.len())),
    );
    insert_upperdir_resource_timings(&mut timings, &finalize.capture.stats);
    for (key, value) in &finalize.publish.timings {
        timings.insert(key.clone(), value.clone());
    }
    let occ_s = timing_as_f64(&timings, "occ.commit.total_s")
        .or(finalize.timings.publish_s)
        .unwrap_or_default();
    timings.insert(
        "command_exec.capture_upperdir_s".to_owned(),
        json!(finalize.capture.capture_s),
    );
    timings.insert("command_exec.occ_apply_s".to_owned(), json!(occ_s));
    timings.insert(
        "command_exec.total_s".to_owned(),
        json!(request.command_elapsed_s),
    );
    timings.insert(
        "api.exec_command.dispatch_total_s".to_owned(),
        json!(request.command_elapsed_s),
    );

    Ok(WorkspaceCommandOutcome {
        mode: WorkspaceMode::Ephemeral,
        success: files.iter().all(|file| status_is_success(&file.status)),
        status: request.status,
        exit_code: request.exit_code,
        stdout: request.stdout,
        stderr: request.stderr,
        command_session_id: request.command_session_id,
        changed_paths: files
            .iter()
            .filter(|file| status_is_published(&file.status))
            .map(|file| file.path.clone())
            .collect(),
        changed_path_kinds,
        mutation_source: "overlay_capture".to_owned(),
        conflict: first_conflict.map(conflict_from_file),
        conflict_reason: first_conflict.map(|file| conflict_message(file).to_owned()),
        timings,
        metadata: json!({
            "spool_truncated": request.spool_truncated,
        }),
    })
}

struct CommandPublisher<'a, P> {
    port: &'a P,
}

impl<P> WorkspacePublisherPort for CommandPublisher<'_, P>
where
    P: EphemeralCommandSessionPort,
{
    fn publish_upperdir_changes(
        &self,
        root: &crate::WorkspaceRoot,
        snapshot: &crate::EphemeralSnapshot,
        changes: &[eos_protocol::LayerChange],
        path_kinds: &[crate::PathChange],
    ) -> Result<crate::PublishOutcome, EphemeralWorkspaceError> {
        self.port
            .publish_upperdir_changes(root, snapshot, changes, path_kinds)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PublishFile {
    path: String,
    status: String,
    message: String,
}

fn workspace_api_error(error: impl std::fmt::Display) -> WorkspaceApiError {
    WorkspaceApiError::new("ephemeral_command_finalize_failed", error.to_string())
}

fn publish_files(outcome: &PublishOutcome) -> Result<Vec<PublishFile>, WorkspaceApiError> {
    let Some(files) = outcome.raw.get("files").and_then(Value::as_array) else {
        return Ok(outcome
            .published_paths
            .iter()
            .map(|path| PublishFile {
                path: path.clone(),
                status: "committed".to_owned(),
                message: String::new(),
            })
            .collect());
    };
    files
        .iter()
        .map(|file| {
            let object = file.as_object().ok_or_else(|| {
                WorkspaceApiError::new(
                    "ephemeral_command_finalize_failed",
                    "publish file result must be an object",
                )
            })?;
            let path = object.get("path").and_then(Value::as_str).ok_or_else(|| {
                WorkspaceApiError::new(
                    "ephemeral_command_finalize_failed",
                    "publish file result missing path",
                )
            })?;
            let status = object
                .get("status")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    WorkspaceApiError::new(
                        "ephemeral_command_finalize_failed",
                        "publish file result missing status",
                    )
                })?;
            Ok(PublishFile {
                path: path.to_owned(),
                status: status.to_owned(),
                message: object
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            })
        })
        .collect()
}

fn path_changes_to_wire(path_changes: &[crate::PathChange]) -> Vec<(String, String)> {
    path_changes
        .iter()
        .map(|change| {
            (
                change.path.clone(),
                path_change_kind_wire(change.kind).to_owned(),
            )
        })
        .collect()
}

const fn path_change_kind_wire(kind: PathChangeKind) -> &'static str {
    match kind {
        PathChangeKind::Write => "write",
        PathChangeKind::Delete => "delete",
        PathChangeKind::Symlink => "symlink",
        PathChangeKind::OpaqueDir => "opaque_dir",
    }
}

fn status_is_published(status: &str) -> bool {
    matches!(status, "accepted" | "committed")
}

fn status_is_success(status: &str) -> bool {
    matches!(status, "accepted" | "committed" | "dropped")
}

fn conflict_from_file(file: &PublishFile) -> WorkspaceConflict {
    WorkspaceConflict::path(&file.status, &file.path, conflict_message(file))
}

fn conflict_message(file: &PublishFile) -> &str {
    if file.message.is_empty() {
        file.status.as_str()
    } else {
        file.message.as_str()
    }
}

fn timing_as_f64(timings: &WorkspaceTimings, key: &str) -> Option<f64> {
    timings.get(key).and_then(Value::as_f64)
}

fn insert_upperdir_resource_timings(timings: &mut WorkspaceTimings, stats: &TreeResourceStats) {
    let file_entries = stats.files.saturating_add(stats.symlinks);
    let entry_count = file_entries.saturating_add(stats.dirs);
    insert_resource_timing(
        timings,
        "resource.command_exec.upperdir_tree_exists",
        if entry_count > 0 { 1 } else { 0 },
    );
    insert_resource_timing(
        timings,
        "resource.command_exec.upperdir_tree_bytes",
        stats.bytes,
    );
    insert_resource_timing(
        timings,
        "resource.command_exec.upperdir_tree_file_count",
        file_entries,
    );
    insert_resource_timing(
        timings,
        "resource.command_exec.upperdir_tree_dir_count",
        stats.dirs,
    );
    insert_resource_timing(
        timings,
        "resource.command_exec.upperdir_tree_entry_count",
        entry_count,
    );
    insert_resource_timing(timings, "resource.command_exec.upperdir_tree_truncated", 0);
}

fn insert_resource_timing(timings: &mut WorkspaceTimings, key: &str, value: u64) {
    timings.insert(key.to_owned(), json!(u64_to_f64_saturating(value)));
}

fn usize_to_f64_saturating(value: usize) -> f64 {
    u64::try_from(value).map_or(f64::MAX, u64_to_f64_saturating)
}

fn u64_to_f64_saturating(value: u64) -> f64 {
    value.to_string().parse::<f64>().unwrap_or(f64::MAX)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use eos_workspace_api::WorkspaceMode;
    use serde_json::json;

    use super::*;
    use crate::command_session::types::EphemeralCommandFinalizeContext;
    use crate::{
        CallerId, EphemeralRunDirs, EphemeralSnapshot, EphemeralWorkspace, InvocationId,
        PublishStatus, WorkspaceRoot,
    };

    #[derive(Debug, Clone)]
    struct FakePort;

    impl EphemeralCommandSessionPort for FakePort {
        fn prepare_context(
            &self,
            _command_session_id: &str,
        ) -> Result<crate::command_session::types::EphemeralCommandPrepareContext, WorkspaceApiError>
        {
            unreachable!("finalize test does not prepare command workspaces")
        }

        fn acquire_snapshot(
            &self,
            _request_id: &str,
        ) -> Result<EphemeralSnapshot, EphemeralWorkspaceError> {
            unreachable!("finalize test does not acquire snapshots")
        }

        fn release_snapshot(&self, _lease_id: &str) -> Result<(), EphemeralWorkspaceError> {
            unreachable!("finalize test does not release snapshots")
        }

        fn base_timings(&self) -> Result<WorkspaceTimings, WorkspaceApiError> {
            unreachable!("finalize test provides timings through context")
        }

        fn publish_upperdir_changes(
            &self,
            _root: &WorkspaceRoot,
            _snapshot: &EphemeralSnapshot,
            changes: &[eos_protocol::LayerChange],
            _path_kinds: &[crate::PathChange],
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

    #[test]
    fn finalize_captures_publishes_and_shapes_command_outcome(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!(
            "eos-ephemeral-command-finalize-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let upperdir = root.join("upper");
        let workdir = root.join("work");
        let run_dir = root.join("run");
        std::fs::create_dir_all(&upperdir)?;
        std::fs::create_dir_all(&workdir)?;
        std::fs::create_dir_all(&run_dir)?;
        std::fs::write(upperdir.join("result.txt"), b"ok")?;
        let context = EphemeralCommandFinalizeContext {
            workspace: EphemeralWorkspace {
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
            },
            base_timings: BTreeMap::new(),
        };

        let outcome = finalize_command_workspace(
            &FakePort,
            context,
            FinalizeCommandRequest {
                runner_result: None,
                command_elapsed_s: 1.5,
                spool_truncated: true,
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
        assert_eq!(outcome.metadata["spool_truncated"], true);

        let _ = std::fs::remove_dir_all(root);
        Ok(())
    }
}
