//! Black-box coverage of `CommandExecValue`'s retained transcript-window and
//! snapshot-offset accessors over a fake interactive execution. The engine
//! forwards (`is_finished`/`output_len`/`resolved`/...) live on
//! `InteractiveExecution` and are covered by the namespace-execution suite.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use sandbox_runtime::command::{CommandExecValue, CommandTerminalResult};
use sandbox_runtime::workspace_session::FinalizeOutcome;
use sandbox_runtime::WorkspaceSessionId;
use sandbox_runtime_namespace_execution::{
    open_pty_pair, CompletionPromise, ExecutionHandle, InteractiveExecution, NamespaceExecutionId,
    PtyMaster,
};
use sandbox_runtime_workspace::WorkspaceScratchLocator;
use std::sync::OnceLock;

struct Fixture {
    command: CommandExecValue,
    transcript_path: PathBuf,
}

fn fixture(suffix: &str) -> Fixture {
    let dir = std::env::current_dir()
        .expect("current directory")
        .join("target")
        .join(format!(
            "command-exec-value-{}-{suffix}",
            std::process::id()
        ));
    let workspace_session_id = WorkspaceSessionId("workspace-session".to_owned());
    let execution_id = NamespaceExecutionId("namespace_execution_1".to_owned());
    let locator = WorkspaceScratchLocator::new(dir).expect("valid scratch locator");
    let scratch = locator
        .allocate_execution(&workspace_session_id, &execution_id)
        .expect("allocate execution scratch");
    let transcript_path = scratch.transcript_path().to_path_buf();

    let promise = Arc::new(CompletionPromise::<CommandTerminalResult>::new());
    let handle = ExecutionHandle::new(execution_id, promise);
    let (master, _slave) = open_pty_pair().expect("openpt pair");
    let pty = PtyMaster::spawn(
        master,
        None,
        Some(transcript_path.clone()),
        Box::new(|| {}),
        std::time::Duration::from_secs(2),
    )
    .expect("pty master");
    let exec = InteractiveExecution::new(handle, pty);
    let command = CommandExecValue::new(
        exec,
        scratch,
        workspace_session_id,
        Instant::now(),
        "exec_command",
        "printf ok".to_owned(),
        Arc::new(OnceLock::<FinalizeOutcome>::new()),
        1024 * 1024,
    );
    Fixture {
        command,
        transcript_path,
    }
}

#[test]
fn transcript_window_reads_the_file_window() {
    let fixture = fixture("window");
    std::fs::write(&fixture.transcript_path, b"alpha\nbeta\n").expect("write transcript");

    let window = fixture.command.transcript_window(0, usize::MAX);
    let rows = window
        .output
        .iter()
        .map(|row| row.text.as_str())
        .collect::<Vec<_>>();
    assert_eq!(rows, vec!["alpha", "beta"]);
}

#[test]
fn snapshot_offset_accessors_round_trip() {
    let fixture = fixture("offset");
    assert_eq!(fixture.command.take_snapshot_offset(), 0);
    fixture.command.advance_snapshot_offset(42);
    assert_eq!(fixture.command.take_snapshot_offset(), 42);
}

#[test]
fn elapsed_seconds_is_non_negative() {
    let fixture = fixture("elapsed");
    assert!(fixture.command.elapsed_seconds() >= 0.0);
}
