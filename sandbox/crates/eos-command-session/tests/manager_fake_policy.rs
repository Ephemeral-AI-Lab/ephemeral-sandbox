use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use eos_command_session::{
    CollectCompleted, CommandResponse, CommandSessionCompletion, CommandSessionConfig,
    CommandSessionManager, ReadCommandProgress, StartCommandSession, WorkspaceRunKind, WriteStdin,
};
use eos_workspace_api::{
    CommandWorkspacePolicy, FinalizeCommandRequest, PrepareCommandRequest,
    PreparedCommandWorkspace, WorkspaceApiError, WorkspaceCommandOutcome, WorkspaceMode,
};
use serde_json::{json, Value};

#[derive(Clone)]
struct FakePolicy {
    finalize_calls: Arc<Mutex<Vec<FinalizeCommandRequest>>>,
    discard_calls: Arc<Mutex<Vec<FinalizeCommandRequest>>>,
    started_calls: Arc<Mutex<Vec<(String, String)>>>,
    finished_calls: Arc<Mutex<Vec<(String, String, String)>>>,
}

impl FakePolicy {
    fn new() -> Self {
        Self {
            finalize_calls: Arc::new(Mutex::new(Vec::new())),
            discard_calls: Arc::new(Mutex::new(Vec::new())),
            started_calls: Arc::new(Mutex::new(Vec::new())),
            finished_calls: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl CommandWorkspacePolicy for FakePolicy {
    fn prepare_command_workspace(
        &self,
        request: PrepareCommandRequest,
    ) -> Result<PreparedCommandWorkspace, WorkspaceApiError> {
        let session_dir = PathBuf::from(format!("/sessions/{}", request.command_session_id));
        Ok(PreparedCommandWorkspace {
            run_request: json!({"cmd": request.cmd}),
            request_path: session_dir.join("runner-request.json"),
            output_path: session_dir.join("runner-result.json"),
            final_path: session_dir.join("final.json"),
            session_dir: session_dir.clone(),
            transcript_path: session_dir.join("transcript.log"),
        })
    }

    fn command_session_started(&self, command_session_id: &str, caller_id: &str) {
        if let Ok(mut calls) = self.started_calls.lock() {
            calls.push((command_session_id.to_owned(), caller_id.to_owned()));
        }
    }

    fn command_session_finished(&self, command_session_id: &str, caller_id: &str, status: &str) {
        if let Ok(mut calls) = self.finished_calls.lock() {
            calls.push((
                command_session_id.to_owned(),
                caller_id.to_owned(),
                status.to_owned(),
            ));
        }
    }

    fn finalize_command_workspace(
        &self,
        request: FinalizeCommandRequest,
    ) -> Result<WorkspaceCommandOutcome, WorkspaceApiError> {
        self.finalize_calls
            .lock()
            .map_err(|error| WorkspaceApiError::new("test_mutex_poisoned", error.to_string()))?
            .push(request.clone());
        Ok(WorkspaceCommandOutcome {
            mode: WorkspaceMode::default(),
            success: request.command_succeeded(),
            status: request.status,
            exit_code: request.exit_code,
            stdout: request.stdout,
            stderr: request.stderr,
            command_session_id: request.command_session_id,
            changed_paths: Vec::new(),
            changed_path_kinds: Default::default(),
            mutation_source: "fake".to_owned(),
            conflict: None,
            conflict_reason: None,
            timings: Default::default(),
            metadata: Value::Null,
        })
    }

    fn discard_command_workspace(
        &self,
        request: FinalizeCommandRequest,
    ) -> Result<WorkspaceCommandOutcome, WorkspaceApiError> {
        self.discard_calls
            .lock()
            .map_err(|error| WorkspaceApiError::new("test_mutex_poisoned", error.to_string()))?
            .push(request.clone());
        Ok(WorkspaceCommandOutcome::discarded(
            WorkspaceMode::default(),
            request,
        ))
    }
}

#[test]
fn manager_starts_boxed_policy_and_counts_by_caller() -> Result<(), Box<dyn std::error::Error>> {
    let manager = CommandSessionManager::new(CommandSessionConfig::default());
    let policy = FakePolicy::new();

    let response = manager.start_boxed(
        start_request("caller-1", "printf ok"),
        Box::new(policy.clone()),
        WorkspaceRunKind::Ephemeral,
    )?;

    assert_eq!(response.status, "running");
    assert_eq!(manager.count_by_caller(Some("caller-1")), 1);
    assert_eq!(manager.count_by_caller(Some("caller-2")), 0);
    assert_eq!(manager.count_by_caller(None), 1);
    let started = lock(&policy.started_calls)?;
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].1, "caller-1");
    Ok(())
}

#[test]
fn ctrl_c_discards_through_policy_and_parks_completion() -> Result<(), Box<dyn std::error::Error>> {
    let policy = FakePolicy::new();
    let manager = CommandSessionManager::new(CommandSessionConfig::default());
    let started = manager.start(
        start_request("caller-1", "cat"),
        policy.clone(),
        WorkspaceRunKind::Ephemeral,
    )?;
    let command_session_id = started.command_session_id.ok_or_else(|| {
        std::io::Error::other("running response should include command_session_id")
    })?;

    let response = manager.write_stdin(WriteStdin {
        command_session_id: command_session_id.clone(),
        chars: "\u{3}".to_owned(),
        yield_time_ms: 1,
    })?;

    assert_eq!(response.status, "cancelled");
    assert_eq!(response.exit_code, Some(130));
    assert_eq!(response.stdout, "");
    assert_eq!(response.workspace_mode, Some(WorkspaceMode::default()));
    assert_eq!(manager.count_by_caller(Some("caller-1")), 0);
    // The invariant: cancel routes to discard and NEVER reaches publish, so a
    // cancelled command can never merge into the shared workspace.
    assert_eq!(lock(&policy.discard_calls)?.len(), 1);
    assert_eq!(lock(&policy.finalize_calls)?.len(), 0);
    assert_eq!(lock(&policy.started_calls)?.len(), 1);
    assert_eq!(lock(&policy.finished_calls)?.len(), 1);

    let completions = manager.collect_completed(&CollectCompleted {
        command_session_ids: Some(vec![command_session_id]),
        caller_id: Some("caller-1".to_owned()),
    });
    assert!(completions.success);
    assert_eq!(completions.completions.len(), 1);
    assert_eq!(completions.completions[0].result.status, "cancelled");
    Ok(())
}

#[test]
fn read_progress_reads_parked_completion_without_consuming(
) -> Result<(), Box<dyn std::error::Error>> {
    let manager = CommandSessionManager::new(CommandSessionConfig::default());
    manager.push_completed(test_completion("cmd_done", "caller-1", "a\nb\nc\n"));

    let first = manager.read_progress(ReadCommandProgress {
        command_session_id: "cmd_done".to_owned(),
        last_n_lines: 2,
    })?;
    assert_eq!(first.status, "ok");
    assert_eq!(first.stdout, "b\nc\n");

    let second = manager.read_progress(ReadCommandProgress {
        command_session_id: "cmd_done".to_owned(),
        last_n_lines: 1,
    })?;
    assert_eq!(second.status, "ok");
    assert_eq!(second.stdout, "c\n");

    let remaining = manager.collect_completed(&CollectCompleted {
        command_session_ids: Some(vec!["cmd_done".to_owned()]),
        caller_id: Some("caller-1".to_owned()),
    });
    assert_eq!(remaining.completions.len(), 1);
    Ok(())
}

fn start_request(caller_id: &str, cmd: &str) -> StartCommandSession {
    StartCommandSession {
        invocation_id: "exec_command".to_owned(),
        caller_id: caller_id.to_owned(),
        cmd: cmd.to_owned(),
        timeout_seconds: None,
        yield_time_ms: 1,
    }
}

fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, Box<dyn std::error::Error>> {
    mutex
        .lock()
        .map_err(|error| format!("mutex poisoned: {error}").into())
}

fn test_completion(id: &str, caller_id: &str, stdout: &str) -> CommandSessionCompletion {
    let result = CommandResponse {
        status: "ok".to_owned(),
        exit_code: Some(0),
        stdout: stdout.to_owned(),
        stderr: String::new(),
        command_session_id: Some(id.to_owned()),
        workspace_mode: None,
        metadata: Value::Null,
    };
    CommandSessionCompletion {
        command_session_id: id.to_owned(),
        caller_id: caller_id.to_owned(),
        command: "test".to_owned(),
        result: result.clone(),
        notification_result: result,
    }
}
