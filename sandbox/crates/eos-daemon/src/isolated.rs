//! Daemon-owned isolated-workspace lifecycle state.
//!
//! This module is the first Rust lifecycle slice behind
//! `api.isolated_workspace.*`: it owns one daemon-local `eos-isolated`
//! session, keeps the public routing key as `agent_id`, and exposes cloned
//! command handles to the command-session dispatcher. The session holds only the
//! snapshot/lease hinge and scratch upperdir; no OCC publish path is linked
//! through `eos-isolated`.

mod runtime;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use eos_isolated::{AgentId, IsolatedError, IsolatedSession, JsonlAuditSink, ResourceCaps};
use eos_layerstack::{read_workspace_binding, LayerStack};
use serde_json::{json, Value};

use crate::command;
use crate::dispatcher::DispatchContext;
use crate::error::DaemonError;
#[cfg(target_os = "linux")]
use runtime::command_handle_from;
#[cfg(target_os = "linux")]
pub use runtime::CommandHandle;
use runtime::{DaemonLayerStackPort, DaemonNamespaceRuntime};

const TEST_HARNESS_ENV: &str = "EOS_ISOLATED_WORKSPACE_TEST_HARNESS";
const TEST_SCRATCH_ROOT_ENV: &str = "EOS_ISOLATED_WORKSPACE_TEST_SCRATCH_ROOT";

type DaemonSession = IsolatedSession<DaemonLayerStackPort, DaemonNamespaceRuntime, JsonlAuditSink>;

struct DaemonIsolatedState {
    #[cfg(target_os = "linux")]
    layer_stack_root: PathBuf,
    session: DaemonSession,
    active_command_sessions: HashMap<String, String>,
}

// Dispatcher op handlers share the `Result<Value, DaemonError>` ABI even when
// isolated-workspace failures are represented as structured JSON responses.
#[expect(
    clippy::unnecessary_wraps,
    reason = "dispatcher handlers share a fallible ABI"
)]
pub fn op_enter(args: &Value, _context: DispatchContext<'_>) -> Result<Value, DaemonError> {
    let agent_id = match require_arg(args, "agent_id") {
        Ok(agent_id) => agent_id,
        Err(error) => return Ok(error),
    };
    let root = match require_arg(args, "layer_stack_root") {
        Ok(root) => PathBuf::from(root),
        Err(error) => return Ok(error),
    };
    command::cleanup_command_sessions_for_agent(&agent_id, None);
    match ensure_state(&root)
        .and_then(|()| with_state(|state| state.session.enter(&AgentId(agent_id))))
    {
        Ok(handle) => Ok(json!({
            "success": true,
            "manifest_version": handle.manifest_version,
            "manifest_root_hash": handle.manifest_root_hash,
            "workspace_handle_id": handle.workspace_handle_id.0,
            "workspace_root": handle.workspace_root,
        })),
        Err(error) => Ok(error_payload(&error)),
    }
}

pub fn op_exit(args: &Value, _context: DispatchContext<'_>) -> Result<Value, DaemonError> {
    let agent_id = match require_arg(args, "agent_id") {
        Ok(agent_id) => agent_id,
        Err(error) => return Ok(error),
    };
    let grace_s = args.get("grace_s").and_then(Value::as_f64);
    command::cleanup_command_sessions_for_agent(&agent_id, grace_s);
    with_state(|state| {
        let response = state.session.exit(&AgentId(agent_id.clone()), grace_s)?;
        state
            .active_command_sessions
            .retain(|_, owner| owner != &agent_id);
        Ok(response)
    })
    .map_or_else(|error| Ok(error_payload(&error)), Ok)
}

// Dispatcher op handlers share the fallible ABI even though status misses are
// represented as `{success: true, open: false}`.
#[expect(
    clippy::unnecessary_wraps,
    reason = "dispatcher handlers share a fallible ABI"
)]
pub fn op_status(args: &Value, _context: DispatchContext<'_>) -> Result<Value, DaemonError> {
    let agent_id = match require_arg(args, "agent_id") {
        Ok(agent_id) => agent_id,
        Err(error) => return Ok(error),
    };
    match with_state(|state| Ok(state.session.get_handle(&AgentId(agent_id)))) {
        Ok(Some(handle)) => Ok(json!({
            "success": true,
            "open": true,
            "manifest_version": handle.manifest_version,
            "manifest_root_hash": handle.manifest_root_hash,
            "workspace_root": handle.workspace_root,
            "created_at": handle.created_at,
            "last_activity": handle.last_activity,
        })),
        Ok(None) => Ok(json!({"success": true, "open": false})),
        Err(error) => Ok(error_payload(&error)),
    }
}

// Dispatcher op handlers share the fallible ABI even though disabled state is
// represented as an empty open-agent list.
#[expect(
    clippy::unnecessary_wraps,
    reason = "dispatcher handlers share a fallible ABI"
)]
pub fn op_list_open(_args: &Value, _context: DispatchContext<'_>) -> Result<Value, DaemonError> {
    match with_state(|state| Ok(state.session.list_open_agents())) {
        Ok(open_agent_ids) => Ok(json!({"success": true, "open_agent_ids": open_agent_ids})),
        Err(IsolatedError::FeatureDisabled) => Ok(json!({"success": true, "open_agent_ids": []})),
        Err(error) => Ok(error_payload(&error)),
    }
}

// Dispatcher op handlers share the fallible ABI even though harness gating is
// represented as a structured JSON error.
#[expect(
    clippy::unnecessary_wraps,
    reason = "dispatcher handlers share a fallible ABI"
)]
pub fn op_test_reset(_args: &Value, _context: DispatchContext<'_>) -> Result<Value, DaemonError> {
    if !env_true(TEST_HARNESS_ENV) {
        return Ok(error_json(
            "forbidden",
            "api.isolated_workspace.test_reset requires EOS_ISOLATED_WORKSPACE_TEST_HARNESS=true",
            json!({}),
        ));
    }
    let exited_agents = {
        let mut guard = lock_state_cell();
        let exited_agents = if let Some(state) = guard.as_mut() {
            let agents = state.session.list_open_agents();
            state.active_command_sessions.clear();
            for agent_id in &agents {
                let _ = state.session.exit(&AgentId(agent_id.clone()), Some(0.0));
            }
            state.session.reap_orphan_resources();
            agents
        } else {
            Vec::new()
        };
        *guard = None;
        exited_agents
    };
    reset_test_manager_file();
    Ok(json!({"success": true, "reset": true, "exited_agents": exited_agents}))
}

#[cfg(target_os = "linux")]
pub fn command_handle_for_args(args: &Value) -> Option<CommandHandle> {
    let agent_id = args
        .get("agent_id")
        .and_then(Value::as_str)
        .unwrap_or("default")
        .trim()
        .to_owned();
    if agent_id.is_empty() {
        return None;
    }
    let (layer_stack_root, handle) = {
        let guard = lock_state_cell();
        guard.as_ref().and_then(|state| {
            state
                .session
                .get_handle(&AgentId(agent_id))
                .map(|handle| (state.layer_stack_root.clone(), handle))
        })
    }?;
    Some(command_handle_from(&layer_stack_root, handle))
}

pub fn agent_has_active_handle(agent_id: &str) -> bool {
    let agent_id = agent_id.trim();
    if agent_id.is_empty() {
        return false;
    }
    let guard = lock_state_cell();
    guard
        .as_ref()
        .and_then(|state| state.session.get_handle(&AgentId(agent_id.to_owned())))
        .is_some()
}

pub fn ttl_sweep() -> usize {
    let mut guard = lock_state_cell();
    let Some(state) = guard.as_mut() else {
        return 0;
    };
    let active_agents = state
        .active_command_sessions
        .values()
        .cloned()
        .collect::<HashSet<_>>();
    state.session.ttl_sweep(&active_agents)
}

#[cfg(any(target_os = "linux", test))]
pub fn register_command_session(agent_id: &str, command_session_id: &str) {
    let mut guard = lock_state_cell();
    if let Some(state) = guard.as_mut() {
        state
            .active_command_sessions
            .insert(command_session_id.to_owned(), agent_id.to_owned());
    }
}

#[cfg(target_os = "linux")]
pub fn unregister_command_session(agent_id: &str, command_session_id: &str) {
    let mut guard = lock_state_cell();
    if let Some(state) = guard.as_mut() {
        if state
            .active_command_sessions
            .get(command_session_id)
            .is_some_and(|owner| owner == agent_id)
        {
            state.active_command_sessions.remove(command_session_id);
        }
    }
}

#[cfg(target_os = "linux")]
pub fn record_tool_call(agent_id: &str, payload: Value) {
    let mut guard = lock_state_cell();
    if let Some(state) = guard.as_mut() {
        state
            .session
            .record_tool_call(&AgentId(agent_id.to_owned()), payload);
    }
}

fn ensure_state(root: &Path) -> Result<(), IsolatedError> {
    {
        let mut guard = lock_state_cell();
        if guard.is_none() {
            let mut caps = ResourceCaps::from_env();
            if !caps.enabled {
                return Err(IsolatedError::FeatureDisabled);
            }
            if let Some(binding) = read_workspace_binding(root).map_err(setup_error)? {
                caps.eos_workspace_root = binding.workspace_root;
            }
            let scratch_root = scratch_root();
            let stack = LayerStack::open(root.to_path_buf()).map_err(setup_error)?;
            let mut session = IsolatedSession::with_scratch_root(
                caps,
                DaemonLayerStackPort {
                    stack: Arc::new(Mutex::new(stack)),
                },
                DaemonNamespaceRuntime,
                JsonlAuditSink::from_env(),
                scratch_root,
            );
            session.initialize()?;
            *guard = Some(DaemonIsolatedState {
                #[cfg(target_os = "linux")]
                layer_stack_root: root.to_path_buf(),
                session,
                active_command_sessions: HashMap::new(),
            });
        }
    }
    Ok(())
}

fn with_state<T>(
    f: impl FnOnce(&mut DaemonIsolatedState) -> Result<T, IsolatedError>,
) -> Result<T, IsolatedError> {
    lock_state_cell()
        .as_mut()
        .ok_or(IsolatedError::FeatureDisabled)
        .and_then(f)
}

fn state_cell() -> &'static Mutex<Option<DaemonIsolatedState>> {
    static STATE: OnceLock<Mutex<Option<DaemonIsolatedState>>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(None))
}

fn lock_state_cell() -> MutexGuard<'static, Option<DaemonIsolatedState>> {
    state_cell().lock().unwrap_or_else(PoisonError::into_inner)
}

fn require_arg(args: &Value, key: &str) -> Result<String, Value> {
    let value = args
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    if value.is_empty() {
        return Err(error_json(
            "invalid_argument",
            format!("{key} is required"),
            json!({"key": key}),
        ));
    }
    Ok(value)
}

fn setup_error(error: impl std::fmt::Display) -> IsolatedError {
    IsolatedError::SetupFailed {
        step: error.to_string(),
    }
}

fn error_payload(error: &IsolatedError) -> Value {
    let details = match error {
        IsolatedError::AlreadyOpen {
            created_at,
            last_activity,
        } => json!({
            "created_at": created_at,
            "last_activity": last_activity,
        }),
        IsolatedError::QuotaExceeded { total_cap } => json!({
            "total_cap": total_cap,
        }),
        IsolatedError::HostRamPressure {
            required_bytes,
            budget_bytes,
        } => json!({
            "required_bytes": required_bytes,
            "budget_bytes": budget_bytes,
        }),
        IsolatedError::SetupFailed { step } | IsolatedError::SetupTimeout { step } => json!({
            "failed_step": step,
        }),
        _ => json!({}),
    };
    error_json(error.kind(), error.to_string(), details)
}

fn error_json(kind: &str, message: impl Into<String>, details: Value) -> Value {
    json!({
        "success": false,
        "error": {
            "kind": kind,
            "message": message.into(),
            "details": if details.is_null() { json!({}) } else { details },
        },
    })
}

fn env_true(key: &str) -> bool {
    std::env::var(key)
        .unwrap_or_default()
        .trim()
        .eq_ignore_ascii_case("true")
}

fn test_runtime_stub_enabled() -> bool {
    env_true(TEST_HARNESS_ENV)
        && !std::env::var(TEST_SCRATCH_ROOT_ENV)
            .unwrap_or_default()
            .trim()
            .is_empty()
}

fn scratch_root() -> PathBuf {
    if env_true(TEST_HARNESS_ENV) {
        let root = std::env::var(TEST_SCRATCH_ROOT_ENV)
            .unwrap_or_default()
            .trim()
            .to_owned();
        if !root.is_empty() {
            return PathBuf::from(root);
        }
    }
    PathBuf::from(eos_overlay::OVERLAY_WRITABLE_ROOT)
}

fn reset_test_manager_file() {
    let session_root = scratch_root().join("runtime").join("isolated-workspace");
    let _ = std::fs::remove_dir_all(&session_root);
    if std::fs::create_dir_all(&session_root).is_err() {
        return;
    }
    let _ = std::fs::write(
        session_root.join("manager.json"),
        br#"{"schema_version":1,"handles":[]}"#,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

    #[test]
    fn active_command_session_records_do_not_guard_exit() -> TestResult {
        let _guard = TEST_LOCK.lock().map_err(|_| "test lock poisoned")?;
        let _ = op_test_reset(&json!({}), DispatchContext::empty());
        let root = std::env::temp_dir().join(format!(
            "eos-daemon-iws-command-session-block-{}",
            std::process::id()
        ));
        let scratch = root.join("scratch");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("layers"))?;
        std::fs::create_dir_all(root.join("staging"))?;
        std::fs::write(
            root.join("manifest.json"),
            r#"{"schema_version":1,"version":1,"layers":[]}"#,
        )?;
        set_env("EOS_ISOLATED_WORKSPACE_ENABLED", "true");
        set_env(TEST_HARNESS_ENV, "true");
        set_env(TEST_SCRATCH_ROOT_ENV, &scratch.to_string_lossy());

        let entered = op_enter(
            &json!({"agent_id": "agent-command-session", "layer_stack_root": root}),
            DispatchContext::empty(),
        )?;
        assert_eq!(entered["success"], true);
        register_command_session("agent-command-session", "cmd-block");

        let exited = op_exit(
            &json!({"agent_id": "agent-command-session"}),
            DispatchContext::empty(),
        )?;
        assert_eq!(exited["success"], true);
        assert_eq!(
            exited["inspection"]["handle_registered_after"],
            json!(false)
        );
        let _ = op_test_reset(&json!({}), DispatchContext::empty());
        clear_env("EOS_ISOLATED_WORKSPACE_ENABLED");
        clear_env(TEST_HARNESS_ENV);
        clear_env(TEST_SCRATCH_ROOT_ENV);
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }

    #[test]
    fn enter_uses_workspace_binding_over_eos_workspace_root_env() -> TestResult {
        let _guard = TEST_LOCK.lock().map_err(|_| "test lock poisoned")?;
        let root = std::env::temp_dir().join(format!(
            "eos-daemon-iws-bound-workspace-root-{}",
            std::process::id()
        ));
        let scratch = root.join("scratch");
        let stack_root = root.join("stack");
        let workspace_root = root.join("workspace");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&workspace_root)?;
        std::fs::write(workspace_root.join("seed.txt"), "seed\n")?;
        eos_layerstack::build_workspace_base(&stack_root, &workspace_root, true)?;
        set_env("EOS_ISOLATED_WORKSPACE_ENABLED", "true");
        set_env(TEST_HARNESS_ENV, "true");
        set_env(TEST_SCRATCH_ROOT_ENV, &scratch.to_string_lossy());
        set_env("EOS_WORKSPACE_ROOT", "/configured-fallback");
        let _ = op_test_reset(&json!({}), DispatchContext::empty());

        let entered = op_enter(
            &json!({"agent_id": "agent-bound-root", "layer_stack_root": stack_root}),
            DispatchContext::empty(),
        )?;

        assert_eq!(entered["success"], true);
        let expected_workspace_root = workspace_root.to_string_lossy().into_owned();
        assert_eq!(
            entered["workspace_root"],
            json!(expected_workspace_root.clone())
        );
        let status = op_status(
            &json!({"agent_id": "agent-bound-root"}),
            DispatchContext::empty(),
        )?;
        assert_eq!(status["success"], true);
        assert_eq!(status["open"], true);
        assert_eq!(
            status["workspace_root"],
            json!(expected_workspace_root.clone())
        );

        let exited = op_exit(
            &json!({"agent_id": "agent-bound-root"}),
            DispatchContext::empty(),
        )?;
        assert_eq!(exited["success"], true);
        let _ = op_test_reset(&json!({}), DispatchContext::empty());
        clear_env("EOS_WORKSPACE_ROOT");
        clear_env("EOS_ISOLATED_WORKSPACE_ENABLED");
        clear_env(TEST_HARNESS_ENV);
        clear_env(TEST_SCRATCH_ROOT_ENV);
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }

    #[test]
    fn test_reset_rewrites_invalid_manager_json() -> TestResult {
        let _guard = TEST_LOCK.lock().map_err(|_| "test lock poisoned")?;
        let root = std::env::temp_dir().join(format!(
            "eos-daemon-iws-reset-manager-{}",
            std::process::id()
        ));
        let scratch = root.join("scratch");
        let manager_root = scratch.join("runtime").join("isolated-workspace");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&manager_root)?;
        std::fs::write(
            manager_root.join("manager.json"),
            r#"{"schema_version":999,"handles":[{"workspace_handle_id":"ghost"}]}"#,
        )?;
        set_env(TEST_HARNESS_ENV, "true");
        set_env(TEST_SCRATCH_ROOT_ENV, &scratch.to_string_lossy());

        let reset = op_test_reset(&json!({}), DispatchContext::empty())?;

        assert_eq!(reset["success"], true);
        let rewritten = std::fs::read_to_string(manager_root.join("manager.json"))?;
        assert_eq!(
            serde_json::from_str::<Value>(&rewritten)?,
            json!({"schema_version": 1, "handles": []})
        );
        clear_env(TEST_HARNESS_ENV);
        clear_env(TEST_SCRATCH_ROOT_ENV);
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }

    #[test]
    fn host_ram_pressure_error_keeps_capacity_details() {
        let response = error_payload(&IsolatedError::HostRamPressure {
            required_bytes: 30,
            budget_bytes: 29,
        });
        assert_eq!(response["success"], false);
        assert_eq!(response["error"]["kind"], "host_ram_pressure");
        assert_eq!(response["error"]["details"]["required_bytes"], 30);
        assert_eq!(response["error"]["details"]["budget_bytes"], 29);
    }

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn set_env(key: &str, value: &str) {
        std::env::set_var(key, value);
    }

    fn clear_env(key: &str) {
        std::env::remove_var(key);
    }
}
