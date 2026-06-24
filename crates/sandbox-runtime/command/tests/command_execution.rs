//! Black-box coverage of `CommandExecution`'s running-vs-terminal read modes
//! over a fake interactive execution.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use sandbox_runtime_command::{CommandExecution, CommandTerminalResult};
use sandbox_runtime_namespace_execution::{
    open_pty_pair, CompletionPromise, ExecutionHandle, InteractiveExecution, NamespaceExecutionId,
    NamespaceExecutionTerminalStatus, PtyMaster,
};
use sandbox_runtime_workspace::WorkspaceSessionId;

struct Fixture {
    command: CommandExecution,
    promise: Arc<CompletionPromise<CommandTerminalResult>>,
    transcript_path: PathBuf,
}

fn fixture(suffix: &str) -> Fixture {
    let dir =
        std::env::temp_dir().join(format!("command-execution-{}-{suffix}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create transcript dir");
    let transcript_path = dir.join("transcript.log");
    let _ = std::fs::remove_file(&transcript_path);

    let promise = Arc::new(CompletionPromise::new());
    let handle = ExecutionHandle::new(
        NamespaceExecutionId("namespace_execution_1".to_owned()),
        Arc::clone(&promise),
    );
    let (master, _slave) = open_pty_pair().expect("openpt pair");
    let pty = PtyMaster::spawn(master, None, Some(transcript_path.clone()), Box::new(|| {}))
        .expect("pty master");
    let exec = InteractiveExecution::new(handle, pty);
    let command = CommandExecution::new(
        exec,
        Some(transcript_path.clone()),
        WorkspaceSessionId("workspace-session".to_owned()),
        PathBuf::from("/workspace/session"),
        Instant::now(),
    );
    Fixture {
        command,
        promise,
        transcript_path,
    }
}

#[test]
fn running_read_uses_the_file_window_and_is_not_finished() {
    let fixture = fixture("running");
    std::fs::write(&fixture.transcript_path, b"alpha\nbeta\n").expect("write transcript");

    assert!(!fixture.command.is_finished());
    assert!(fixture.command.terminal_result().is_none());

    let window = fixture.command.transcript_window(0, usize::MAX);
    let rows = window
        .output
        .iter()
        .map(|row| row.text.as_str())
        .collect::<Vec<_>>();
    assert_eq!(rows, vec!["alpha", "beta"]);
    assert_eq!(
        fixture.command.output_len(),
        std::fs::metadata(&fixture.transcript_path)
            .expect("transcript metadata")
            .len()
    );
}

#[test]
fn terminal_read_resolves_without_consuming() {
    let fixture = fixture("terminal");
    let result = CommandTerminalResult {
        status: NamespaceExecutionTerminalStatus::Ok,
        exit_code: 0,
        command_total_time_seconds: 1.5,
    };
    fixture.promise.resolve(Ok(result));

    assert!(fixture.command.is_finished());

    // resolved() is non-consuming: repeated terminal reads each see the result.
    for _ in 0..2 {
        let observed = fixture
            .command
            .terminal_result()
            .expect("finished implies a resolved result")
            .expect("resolved Ok");
        assert_eq!(observed, result);
    }
}

#[test]
fn workspace_identity_is_exposed_for_reverse_lookup() {
    let fixture = fixture("identity");
    assert_eq!(
        fixture.command.workspace_session_id(),
        &WorkspaceSessionId("workspace-session".to_owned())
    );
    assert_eq!(
        fixture.command.workspace_root(),
        std::path::Path::new("/workspace/session")
    );
    assert_eq!(fixture.command.id().0, "namespace_execution_1");
}
