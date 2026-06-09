use serde_json::json;

use super::*;

fn runner_ok() -> Option<CommandRunnerResult> {
    CommandRunnerResult::from_value(json!({"exit_code": 0, "tool_result": {"status": "ok"}}))
}

#[test]
fn kill_reason_maps_to_terminal_status() {
    let exit = CommandProcessExit::unwaitable();
    let runner = runner_ok();

    let ok = CommandCompletionStatus::from_process_and_runner(exit, runner.as_ref(), None);
    assert_eq!((ok.status(), ok.exit_code()), ("ok", 0));

    let cancelled = CommandCompletionStatus::from_process_and_runner(
        exit,
        runner.as_ref(),
        Some(KillReason::Cancelled),
    );
    assert_eq!(
        (cancelled.status(), cancelled.exit_code()),
        ("cancelled", 130)
    );

    let timed_out = CommandCompletionStatus::from_process_and_runner(
        exit,
        runner.as_ref(),
        Some(KillReason::TimedOut),
    );
    assert_eq!(
        (timed_out.status(), timed_out.exit_code()),
        ("timed_out", 124)
    );
}
