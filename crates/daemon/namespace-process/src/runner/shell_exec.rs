//! Namespace shell execution shared by setns runner modes.

#[cfg(target_os = "linux")]
use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::process::{Command, Stdio};
#[cfg(target_os = "linux")]
use std::time::Instant;

#[cfg(target_os = "linux")]
use super::RunnerError;
#[cfg(target_os = "linux")]
use crate::runner::protocol::{NamespaceCommandRequest, RunResult};
#[cfg(target_os = "linux")]
use serde_json::{json, Value};

#[cfg(target_os = "linux")]
pub(crate) mod request;
#[cfg(target_os = "linux")]
mod wait;

#[cfg(target_os = "linux")]
use request::*;
#[cfg(target_os = "linux")]
use wait::*;

#[cfg(target_os = "linux")]
#[derive(Debug, Default)]
pub(crate) struct RunnerPhaseTimings {
    values: serde_json::Map<String, Value>,
}

#[cfg(target_os = "linux")]
impl RunnerPhaseTimings {
    pub(crate) fn insert_s(&mut self, key: &'static str, value: f64) {
        self.values.insert(key.to_owned(), json!(value));
    }

    fn insert_elapsed(&mut self, key: &'static str, started: Instant) {
        self.insert_s(key, started.elapsed().as_secs_f64());
    }

    fn into_json(mut self, run_start: Instant) -> Value {
        self.insert_elapsed("workspace.command_s", run_start);
        Value::Object(self.values)
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn execute_shell(
    request: &NamespaceCommandRequest,
    mut timings: RunnerPhaseTimings,
    run_start: Instant,
    hidden_paths: Option<&[PathBuf]>,
) -> Result<RunResult, RunnerError> {
    let prepare_start = Instant::now();
    let argv = shell_argv(request)?;
    let cwd = shell_cwd(request)?;
    // Open a handle to /proc before applying the mount mask, so scope-wait can
    // still enumerate same-pgid descendant processes if a custom config hides it.
    let proc_dir = rustix::fs::open(
        "/proc",
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::DIRECTORY | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .ok();
    if let Some(hidden_paths) = hidden_paths {
        super::mask_model_shell_paths(hidden_paths)?;
    }
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .current_dir(cwd)
        .env_clear()
        .envs(command_environment(&request.args))
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    timings.insert_elapsed("workspace.shell_prepare_s", prepare_start);

    let spawn_start = Instant::now();
    let mut child = command.spawn().map_err(RunnerError::Child)?;
    timings.insert_elapsed("workspace.shell_spawn_s", spawn_start);
    let wait_start = Instant::now();
    let (exit_code, timed_out, scope_timing) = match wait_for_command_execution_scope(
        &mut child,
        request.timeout_seconds,
        proc_dir.as_ref().map(std::os::fd::AsFd::as_fd),
    ) {
        Ok((exit_code, timing)) => (exit_code, false, Some(timing)),
        Err(RunnerError::TimedOut) => (124, true, None),
        Err(err) => return Err(err),
    };
    if let Some(scope_timing) = scope_timing {
        record_command_scope_wait_timing(&mut timings, &scope_timing);
    }
    timings.insert_elapsed("workspace.shell_wait_s", wait_start);
    Ok(RunResult {
        exit_code,
        payload: serde_json::json!({
            "success": exit_code == 0,
            "workspace": "shared",
            "timings": timings.into_json(run_start),
            "conflict": null,
            "conflict_reason": null,
            "changed_paths": [],
            "error": null,
            "changed_path_kinds": {},
            "mutation_source": "",
            "status": result_status(exit_code, timed_out),
            "exit_code": exit_code,
            "stdout": "",
            "stderr": "",
            "warnings": [],
        }),
    })
}

#[cfg(target_os = "linux")]
fn record_command_scope_wait_timing(
    timings: &mut RunnerPhaseTimings,
    scope_timing: &CommandExecutionScopeTiming,
) {
    if let Some(value) = scope_timing.root_exit_s {
        timings.insert_s("workspace.shell_wait_root_exit_s", value);
    }
    if let Some(value) = scope_timing.post_root_drain_s {
        timings.insert_s("workspace.shell_wait_post_root_drain_s", value);
    }
    timings.insert_s(
        "workspace.shell_wait_child_try_wait_s",
        scope_timing.child_try_wait_s,
    );
    timings.insert_s("workspace.shell_wait_proc_scan_s", scope_timing.proc_scan_s);
    timings.insert_s(
        "workspace.shell_wait_proc_scan_count",
        scope_timing.proc_scan_count as f64,
    );
    timings.insert_s(
        "workspace.shell_wait_poll_count",
        scope_timing.poll_count as f64,
    );
    timings.insert_s(
        "workspace.shell_wait_poll_sleep_s",
        scope_timing.poll_sleep_s,
    );
}

#[cfg(target_os = "linux")]
const fn result_status(exit_code: i32, timed_out: bool) -> &'static str {
    if timed_out {
        "timed_out"
    } else if exit_code == 0 {
        "ok"
    } else {
        "error"
    }
}
