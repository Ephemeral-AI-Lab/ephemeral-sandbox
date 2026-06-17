use std::path::PathBuf;
use std::time::Instant;

use operation_service::command::{
    ActiveCommandProcess, CancellationState, CommandCompletionStore, CommandFinalizePolicy,
    CommandId, CommandLifecycleState, CommandProcessStore, CommandServiceError, CommandStatus,
    CommandTerminalResult, CommandTraceOrigin, CommandTranscriptStore, CompletedCommandRecord,
    FinalizationState, RetainedCommandTranscript,
};
use workspace::{CallerId, WorkspaceId};

fn command_id(id: &str) -> CommandId {
    CommandId(id.to_owned())
}

fn caller_id(id: &str) -> CallerId {
    CallerId(id.to_owned())
}

fn workspace_id(id: &str) -> WorkspaceId {
    WorkspaceId(id.to_owned())
}

fn inactive_process(command_id: &CommandId, caller_id: &CallerId) -> command::CommandProcess {
    command::CommandProcess::new(command::CommandProcessSpec {
        id: command_id.0.clone(),
        caller_id: caller_id.0.clone(),
        command: "echo ok".to_owned(),
        timeout_seconds: None,
    })
}

fn active_record(
    command_id: CommandId,
    caller_id: CallerId,
    workspace_id: WorkspaceId,
) -> ActiveCommandProcess {
    ActiveCommandProcess {
        command_id: command_id.clone(),
        caller_id: caller_id.clone(),
        workspace_id: workspace_id.clone(),
        process: inactive_process(&command_id, &caller_id),
        transcript: CommandTranscriptStore {
            transcript_path: Some(PathBuf::from("/tmp/transcript.jsonl")),
        },
        finalize_policy: CommandFinalizePolicy::Session { workspace_id },
        lifecycle_state: CommandLifecycleState::Running,
        cancellation: CancellationState::None,
        finalization: FinalizationState::NotStarted,
        trace_origin: CommandTraceOrigin,
        started_at: Instant::now(),
    }
}

fn completed_record(
    command_id: CommandId,
    caller_id: CallerId,
    workspace_id: WorkspaceId,
) -> CompletedCommandRecord {
    CompletedCommandRecord {
        command_id,
        caller_id,
        workspace_id,
        result: CommandTerminalResult {
            status: CommandStatus::Completed,
            exit_code: Some(0),
            stdout: "ok\n".to_owned(),
        },
        transcript: RetainedCommandTranscript {
            transcript_path: Some(PathBuf::from("/tmp/retained-transcript.jsonl")),
        },
        finalization: FinalizationState::Complete,
        completed_at: Instant::now(),
    }
}

#[test]
fn command_process_store_allocates_monotonic_command_ids() {
    let store = CommandProcessStore::new();

    assert_eq!(store.allocate_command_id(), command_id("cmd_1"));
    assert_eq!(store.allocate_command_id(), command_id("cmd_2"));
    assert_eq!(store.allocate_command_id(), command_id("cmd_3"));
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
fn command_process_store_active_records_are_command_id_keyed() {
    let store = CommandProcessStore::with_max_active(1);
    let cmd_id = command_id("cmd_active");
    let caller_id = caller_id("caller-1");
    let ws_id = workspace_id("workspace-1");
    let reservation = store.try_reserve().expect("reservation succeeds");

    store
        .insert_active(
            reservation,
            active_record(cmd_id.clone(), caller_id.clone(), ws_id.clone()),
        )
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
        assert_eq!(active.command_id, cmd_id);
        assert_eq!(active.caller_id, caller_id);
        assert_eq!(active.workspace_id, ws_id);
        assert_eq!(
            active.finalize_policy,
            CommandFinalizePolicy::Session {
                workspace_id: workspace_id("workspace-1")
            }
        );
    }

    let removed = store
        .complete_active(completed_record(
            cmd_id.clone(),
            caller_id.clone(),
            workspace_id("workspace-1"),
        ))
        .expect("active command completion succeeds")
        .expect("active command removed");
    assert_eq!(removed.command_id, command_id("cmd_active"));
    assert!(store.active(&command_id("cmd_active")).is_none());
    store
        .try_reserve()
        .expect("removing active command releases capacity");
}

#[test]
fn command_process_store_rejects_duplicate_active_command_id() {
    let store = CommandProcessStore::with_max_active(2);
    let command_id = command_id("cmd_duplicate");
    let reservation = store.try_reserve().expect("first reservation succeeds");

    store
        .insert_active(
            reservation,
            active_record(
                command_id.clone(),
                caller_id("caller-1"),
                workspace_id("workspace-1"),
            ),
        )
        .expect("first insert succeeds");
    let reservation = store.try_reserve().expect("second reservation succeeds");
    let error = store
        .insert_active(
            reservation,
            active_record(
                command_id.clone(),
                caller_id("caller-2"),
                workspace_id("workspace-2"),
            ),
        )
        .expect_err("duplicate active id is rejected");

    assert!(matches!(
        error,
        CommandServiceError::DuplicateCommandId { command_id: duplicate }
            if duplicate == command_id
    ));
    store
        .try_reserve()
        .expect("failed duplicate insert releases reservation");
}

#[test]
fn command_process_store_rejects_reservation_from_different_store() {
    let reserving_store = CommandProcessStore::with_max_active(1);
    let inserting_store = CommandProcessStore::with_max_active(1);
    let command_id = command_id("cmd_wrong_store");
    let reservation = reserving_store.try_reserve().expect("reservation succeeds");

    let error = inserting_store
        .insert_active(
            reservation,
            active_record(
                command_id.clone(),
                caller_id("caller-1"),
                workspace_id("workspace-1"),
            ),
        )
        .expect_err("reservation from a different store is rejected");

    assert!(matches!(
        error,
        CommandServiceError::ReservationStoreMismatch
    ));
    assert!(inserting_store.active(&command_id).is_none());
    reserving_store
        .try_reserve()
        .expect("rejected reservation releases original store capacity");
}

#[test]
fn command_process_store_completed_records_retain_caller_during_active_completion() {
    let store = CommandProcessStore::with_max_active(1);
    let command_id = command_id("cmd_completed");
    let caller_id = caller_id("caller-owner");
    let workspace_id = workspace_id("workspace-1");
    let reservation = store.try_reserve().expect("reservation succeeds");

    store
        .insert_active(
            reservation,
            active_record(command_id.clone(), caller_id.clone(), workspace_id.clone()),
        )
        .expect("active insert succeeds");
    let removed = store
        .complete_active(completed_record(
            command_id.clone(),
            caller_id.clone(),
            workspace_id.clone(),
        ))
        .expect("completed record retained")
        .expect("active record removed during completion");

    assert_eq!(removed.command_id, command_id);
    assert!(store.active(&command_id).is_none());
    let completed = store
        .completed(&command_id)
        .expect("completed record remains available");
    assert_eq!(completed.caller_id, caller_id);
    assert_eq!(completed.workspace_id, workspace_id);
    assert_eq!(completed.result.status, CommandStatus::Completed);
    store
        .try_reserve()
        .expect("completion releases active admission slot");
}

#[test]
fn command_process_store_rejects_completed_record_with_mismatched_owner() {
    let store = CommandProcessStore::with_max_active(1);
    let command_id = command_id("cmd_completed");
    let owner = caller_id("caller-owner");
    let workspace_id = workspace_id("workspace-1");
    let reservation = store.try_reserve().expect("reservation succeeds");

    store
        .insert_active(
            reservation,
            active_record(command_id.clone(), owner.clone(), workspace_id.clone()),
        )
        .expect("active insert succeeds");

    let error = match store.complete_active(completed_record(
        command_id.clone(),
        caller_id("caller-other"),
        workspace_id,
    )) {
        Err(error) => error,
        Ok(_) => panic!("completed record cannot rewrite caller ownership"),
    };

    assert!(matches!(
        error,
        CommandServiceError::CommandCallerMismatch { command_id: id, expected, actual }
            if id == command_id
                && expected == owner
                && actual == caller_id("caller-other")
    ));
    assert!(store.active(&command_id).is_some());
    assert!(store.completed(&command_id).is_none());
}

#[test]
fn command_process_store_rejects_completed_record_with_mismatched_workspace() {
    let store = CommandProcessStore::with_max_active(1);
    let command_id = command_id("cmd_completed");
    let caller_id = caller_id("caller-owner");
    let original_workspace_id = workspace_id("workspace-1");
    let reservation = store.try_reserve().expect("reservation succeeds");

    store
        .insert_active(
            reservation,
            active_record(
                command_id.clone(),
                caller_id.clone(),
                original_workspace_id.clone(),
            ),
        )
        .expect("active insert succeeds");

    let rewritten_workspace_id = workspace_id("workspace-other");
    let error = match store.complete_active(completed_record(
        command_id.clone(),
        caller_id,
        rewritten_workspace_id.clone(),
    )) {
        Err(error) => error,
        Ok(_) => panic!("completed record cannot rewrite workspace ownership"),
    };

    assert!(matches!(
        error,
            CommandServiceError::CommandWorkspaceMismatch { command_id: id, expected, actual }
            if id == command_id
                && expected == original_workspace_id
                && actual == rewritten_workspace_id
    ));
    assert!(store.active(&command_id).is_some());
    assert!(store.completed(&command_id).is_none());
}

#[test]
fn command_process_store_completion_store_rejects_duplicate_command_id() {
    let completion_store = CommandCompletionStore::new();
    let command_id = command_id("cmd_completed");

    completion_store
        .insert(completed_record(
            command_id.clone(),
            caller_id("caller-1"),
            workspace_id("workspace-1"),
        ))
        .expect("first completed insert succeeds");
    let error = completion_store
        .insert(completed_record(
            command_id.clone(),
            caller_id("caller-1"),
            workspace_id("workspace-1"),
        ))
        .expect_err("duplicate completed id is rejected");

    assert!(matches!(
        error,
        CommandServiceError::DuplicateCommandId { command_id: duplicate }
            if duplicate == command_id
    ));
}
