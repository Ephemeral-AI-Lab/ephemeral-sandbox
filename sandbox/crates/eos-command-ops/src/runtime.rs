//! Process-wide command runtime, configuration, and lifecycle backstops.
//!
//! The daemon schedules these hooks, but command-session runtime policy lives
//! here with [`CommandOps`].

use std::sync::{OnceLock, RwLock};
use std::time::Instant;

use eos_command_session::{CommandResponse, CommandSessionCompletion, CommandSessionConfig};
use serde_json::Value;

use crate::ops::CommandOps;

pub fn command_ops() -> &'static CommandOps {
    static OPS: OnceLock<CommandOps> = OnceLock::new();
    OPS.get_or_init(|| CommandOps::new(command_session_config()))
}

pub fn configure_command_sessions(config: &CommandSessionConfig) {
    let mut guard = command_session_config_cell()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *guard = config.clone();
}

pub fn command_session_config() -> CommandSessionConfig {
    command_session_config_cell()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

pub fn command_session_scratch_root() -> std::path::PathBuf {
    command_session_config().scratch_root
}

fn command_session_config_cell() -> &'static RwLock<CommandSessionConfig> {
    static CONFIG: OnceLock<RwLock<CommandSessionConfig>> = OnceLock::new();
    CONFIG.get_or_init(|| RwLock::new(CommandSessionConfig::default()))
}

#[must_use]
pub fn active_command_sessions_for_caller(caller_id: &str) -> usize {
    let caller_id = caller_id.trim();
    if caller_id.is_empty() {
        return 0;
    }
    command_ops().count_by_caller(Some(caller_id))
}

/// Best-effort lifecycle backstop for callers that bypass the model-facing
/// `RequireNoBackgroundSessions` hook.
pub fn cleanup_command_sessions_for_caller(caller_id: &str, grace_s: Option<f64>) -> usize {
    command_ops().cleanup_caller(caller_id, grace_s)
}

/// Cancel and discard every live command session across all callers (the
/// whole-sandbox cancel sweep). Returns the number cancelled.
pub fn cancel_all_command_sessions(grace_s: Option<f64>) -> usize {
    command_ops().cancel_all(grace_s)
}

/// Periodic reaper (sense-2 §2.4, §3): enforce the per-session timeout backstop
/// and finalize any session whose child has exited without a live poller,
/// parking the completion for the heartbeat. The runner enforces the per-call
/// timeout internally (primary); this is the backstop for a wedged or
/// no-timeout runner and the only finalizer for fire-and-forget sessions. A
/// session started without an explicit `timeout` falls back to the configured
/// wall-clock cap so it can never run forever.
pub fn command_session_reaper_sweep() {
    command_ops().sweep_expired(Instant::now());
}

/// Startup recovery (sense-2 §2.4): a previous daemon may have left ephemeral
/// command-session metadata behind. Park an `orphan_reaped` completion for each
/// so a recovering agent learns the session is dead, then remove the stale dir.
///
/// We deliberately do **not** `killpg` the old children: their pgids are not
/// persisted, so a restarted daemon could otherwise signal a reused PID. Their
/// own runner timeout reclaims them; lease cleanup is left to LayerStack GC.
pub fn recover_orphaned_command_sessions() {
    let dir = command_session_scratch_root();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if let Ok(bytes) = std::fs::read(path.join("metadata.json")) {
            if let Ok(meta) = serde_json::from_slice::<Value>(&bytes) {
                let id = meta
                    .get("command_session_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !id.is_empty() {
                    let caller_id = meta
                        .get("caller_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let command = meta
                        .get("command")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let result = CommandResponse {
                        status: "error".to_owned(),
                        exit_code: Some(1),
                        stdout: String::new(),
                        stderr: "orphan_reaped: daemon restarted".to_owned(),
                        command_session_id: Some(id.to_owned()),
                        workspace: None,
                        metadata: Value::Null,
                    };
                    command_ops().push_completed(CommandSessionCompletion {
                        command_session_id: id.to_owned(),
                        caller_id: caller_id.to_owned(),
                        command: command.to_owned(),
                        result,
                    });
                }
            }
        }
        let _ = std::fs::remove_dir_all(&path);
    }
}
