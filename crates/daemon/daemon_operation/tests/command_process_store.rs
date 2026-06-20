use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use daemon_operation::command::{
    ActiveCommandProcess, CancellationState, CommandCompletionStore, CommandFinalizedMetadata,
    CommandLifecycleState, CommandProcessStore, CommandServiceError, CommandSessionId,
    CommandStatus, CommandTerminalResult, CommandTranscriptStore, CompletedCommandRecord,
    FinalizationState, RetainedCommandTranscript,
};
use workspace::WorkspaceSessionId;

fn command_session_id(id: &str) -> CommandSessionId {
    CommandSessionId(id.to_owned())
}

fn workspace_session_id(id: &str) -> WorkspaceSessionId {
    WorkspaceSessionId(id.to_owned())
}

fn inactive_process(command_session_id: &CommandSessionId) -> command::CommandProcess {
    command::CommandProcess::inactive_for_test(command::CommandProcessSpec {
        id: command_session_id.0.clone(),
        command: "echo ok".to_owned(),
        cwd: None,
        timeout_seconds: None,
    })
}

fn active_record(
    command_session_id: CommandSessionId,
    workspace_session_id: WorkspaceSessionId,
) -> ActiveCommandProcess {
    ActiveCommandProcess {
        command_session_id: command_session_id.clone(),
        workspace_session_id: workspace_session_id.clone(),
        workspace_root: PathBuf::from("/workspace"),
        process: Arc::new(inactive_process(&command_session_id)),
        transcript: CommandTranscriptStore {
            transcript_path: Some(PathBuf::from("/tmp/transcript.jsonl")),
        },
        lifecycle_state: CommandLifecycleState::Running,
        cancellation: CancellationState::None,
        remount_cancellation: None,
        remount_switch_state: None,
        finalization: FinalizationState::NotStarted,
        started_at: Instant::now(),
    }
}

fn completed_record(
    command_session_id: CommandSessionId,
    workspace_session_id: WorkspaceSessionId,
) -> CompletedCommandRecord {
    CompletedCommandRecord {
        command_session_id,
        workspace_session_id,
        result: CommandTerminalResult {
            status: CommandStatus::Completed,
            exit_code: Some(0),
            stdout: "ok\n".to_owned(),
        },
        transcript: RetainedCommandTranscript {
            transcript_path: Some(PathBuf::from("/tmp/retained-transcript.jsonl")),
        },
        finalization: FinalizationState::Complete,
        finalized: Some(CommandFinalizedMetadata),
        completed_at: Instant::now(),
    }
}

#[test]
fn command_process_store_allocates_monotonic_command_session_ids() {
    let store = CommandProcessStore::new();

    assert_eq!(
        store.allocate_command_session_id(),
        command_session_id("cmd_1")
    );
    assert_eq!(
        store.allocate_command_session_id(),
        command_session_id("cmd_2")
    );
    assert_eq!(
        store.allocate_command_session_id(),
        command_session_id("cmd_3")
    );
}

#[test]
fn command_process_store_reservation_drop_releases_admission_slot() {
    let store = CommandProcessStore::with_max_active(1);
    let reservation = store.try_reserve().expect("first reservation succeeds");

    let error = store
        .try_reserve()
        .expect_err("second reservation at cap is rejected");
    assert!(matches!(
        error,
        CommandServiceError::CommandAdmissionLimit { active: 1, max: 1 }
    ));

    drop(reservation);

    store
        .try_reserve()
        .expect("dropped reservation releases capacity");
}

#[test]
fn command_process_store_active_records_are_command_session_id_keyed() {
    let store = CommandProcessStore::with_max_active(1);
    let cmd_id = command_session_id("cmd_active");
    let ws_id = workspace_session_id("workspace-1");
    let reservation = store.try_reserve().expect("reservation succeeds");

    store
        .insert_active(reservation, active_record(cmd_id.clone(), ws_id.clone()))
        .expect("active insert succeeds");
    let error = store
        .try_reserve()
        .expect_err("active command keeps admission slot consumed");
    assert!(matches!(
        error,
        CommandServiceError::CommandAdmissionLimit { active: 1, max: 1 }
    ));

    {
        let active = store.active(&cmd_id).expect("active command exists");
        assert_eq!(active.command_session_id, cmd_id);
        assert_eq!(active.workspace_session_id, ws_id);
    }

    let removed = store
        .complete_active(completed_record(
            cmd_id.clone(),
            workspace_session_id("workspace-1"),
        ))
        .expect("active command completion succeeds")
        .expect("active command removed");
    assert_eq!(removed.command_session_id, command_session_id("cmd_active"));
    assert!(store.active(&command_session_id("cmd_active")).is_none());
    store
        .try_reserve()
        .expect("removing active command releases capacity");
}

#[test]
fn command_process_store_rejects_duplicate_active_command_session_id() {
    let store = CommandProcessStore::with_max_active(2);
    let command_session_id = command_session_id("cmd_duplicate");
    let reservation = store.try_reserve().expect("first reservation succeeds");

    store
        .insert_active(
            reservation,
            active_record(
                command_session_id.clone(),
                workspace_session_id("workspace-1"),
            ),
        )
        .expect("first insert succeeds");
    let reservation = store.try_reserve().expect("second reservation succeeds");
    let error = store
        .insert_active(
            reservation,
            active_record(
                command_session_id.clone(),
                workspace_session_id("workspace-2"),
            ),
        )
        .expect_err("duplicate active id is rejected");

    assert!(matches!(
        error,
        CommandServiceError::DuplicateCommandSessionId { command_session_id: duplicate }
            if duplicate == command_session_id
    ));
    store
        .try_reserve()
        .expect("failed duplicate insert releases reservation");
}

#[test]
fn command_process_store_rejects_reservation_from_different_store() {
    let reserving_store = CommandProcessStore::with_max_active(1);
    let inserting_store = CommandProcessStore::with_max_active(1);
    let command_session_id = command_session_id("cmd_wrong_store");
    let reservation = reserving_store.try_reserve().expect("reservation succeeds");

    let error = inserting_store
        .insert_active(
            reservation,
            active_record(
                command_session_id.clone(),
                workspace_session_id("workspace-1"),
            ),
        )
        .expect_err("reservation from a different store is rejected");

    assert!(matches!(
        error,
        CommandServiceError::ReservationStoreMismatch
    ));
    assert!(inserting_store.active(&command_session_id).is_none());
    reserving_store
        .try_reserve()
        .expect("rejected reservation releases original store capacity");
}

#[test]
fn command_process_store_completed_records_retain_workspace_during_active_completion() {
    let store = CommandProcessStore::with_max_active(1);
    let command_session_id = command_session_id("cmd_completed");
    let workspace_session_id = workspace_session_id("workspace-1");
    let reservation = store.try_reserve().expect("reservation succeeds");

    store
        .insert_active(
            reservation,
            active_record(command_session_id.clone(), workspace_session_id.clone()),
        )
        .expect("active insert succeeds");
    let removed = store
        .complete_active(completed_record(
            command_session_id.clone(),
            workspace_session_id.clone(),
        ))
        .expect("completed record retained")
        .expect("active record removed during completion");

    assert_eq!(removed.command_session_id, command_session_id);
    assert!(store.active(&command_session_id).is_none());
    let completed = store
        .completed(&command_session_id)
        .expect("completed record remains available");
    assert_eq!(completed.workspace_session_id, workspace_session_id);
    assert_eq!(completed.result.status, CommandStatus::Completed);
    store
        .try_reserve()
        .expect("completion releases active admission slot");
}

#[test]
fn command_process_store_rejects_completed_record_with_mismatched_workspace() {
    let store = CommandProcessStore::with_max_active(1);
    let command_session_id = command_session_id("cmd_completed");
    let original_workspace_session_id = workspace_session_id("workspace-1");
    let reservation = store.try_reserve().expect("reservation succeeds");

    store
        .insert_active(
            reservation,
            active_record(
                command_session_id.clone(),
                original_workspace_session_id.clone(),
            ),
        )
        .expect("active insert succeeds");

    let rewritten_workspace_session_id = workspace_session_id("workspace-other");
    let error = match store.complete_active(completed_record(
        command_session_id.clone(),
        rewritten_workspace_session_id.clone(),
    )) {
        Err(error) => error,
        Ok(_) => panic!("completed record cannot rewrite workspace session"),
    };

    assert!(matches!(
        error,
            CommandServiceError::CommandWorkspaceSessionMismatch { command_session_id: id, expected, actual }
            if id == command_session_id
                && expected == original_workspace_session_id
                && actual == rewritten_workspace_session_id
    ));
    assert!(store.active(&command_session_id).is_some());
    assert!(store.completed(&command_session_id).is_none());
}

#[test]
fn command_process_store_completion_store_rejects_duplicate_command_session_id() {
    let completion_store = CommandCompletionStore::new();
    let command_session_id = command_session_id("cmd_completed");

    completion_store
        .insert(completed_record(
            command_session_id.clone(),
            workspace_session_id("workspace-1"),
        ))
        .expect("first completed insert succeeds");
    let error = completion_store
        .insert(completed_record(
            command_session_id.clone(),
            workspace_session_id("workspace-1"),
        ))
        .expect_err("duplicate completed id is rejected");

    assert!(matches!(
        error,
        CommandServiceError::DuplicateCommandSessionId { command_session_id: duplicate }
            if duplicate == command_session_id
    ));
}
