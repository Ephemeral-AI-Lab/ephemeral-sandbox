//! Owned request/result types for the namespace runner.
//!
//! These model the JSON payloads the Rust helpers exchange over stdin/stdout
//! and the namespace request/result files — the tool-call payload, the fresh-ns
//! request file, and the setns stdin payload. The verb-specific `args`
//! stay an opaque [`serde_json::Value`] here (the runner forwards them verbatim
//! to the in-namespace tool primitive); the typed per-verb args/results are the
//! daemon's concern (decoded into `eos-workspace-runtime` contract and
//! runtime types), not modeled here.
//!
//! These DTOs live in `eos-cas` (the shared in-box model crate) so the tokio
//! daemon, which builds requests and parses results, and the single-threaded
//! `eos-ns-child` runner, which executes them, share one contract without the
//! daemon depending on the syscall crate.

use std::os::unix::io::RawFd;
use std::path::PathBuf;

use crate::Intent;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

/// Which namespace strategy the runner uses for this call.
///
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunMode {
    /// Create a brand-new private namespace stack via `unshare`, mount the
    /// overlay, then exec — one tool call per namespace.
    FreshNs,
    /// `setns` into the ns-holder's already-open namespace FDs, then exec.
    SetNs,
}

/// A raw file descriptor handle.
///
/// `#[repr(transparent)]` lets this cross the FFI boundary into the `setns(2)`
/// syscall unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(transparent)]
pub struct Fd(pub RawFd);

/// The validated workspace root the overlay is mounted at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRoot(pub PathBuf);

/// The ns-holder's pre-opened namespace FDs.
///
/// Applied in this exact order:
/// `user` (privilege change), `mnt` (mount table), `pid` (descendants only,
/// before `fork`), `net`. A wrong order breaks the setns sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NsFds {
    /// User namespace FD (`CLONE_NEWUSER`) — applied first.
    pub user: Option<Fd>,
    /// Mount namespace FD (`CLONE_NEWNS`).
    pub mnt: Option<Fd>,
    /// PID namespace FD (`CLONE_NEWPID`) — affects descendants only; set before `fork`.
    pub pid: Option<Fd>,
    /// Network namespace FD (`CLONE_NEWNET`).
    pub net: Option<Fd>,
}

/// Runner-supported tool verbs.
///
/// Unknown verbs stay representable so the runner can preserve the current
/// `unsupported_runner_verb` result instead of failing JSON decoding before
/// dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunnerVerb {
    ExecCommand,
    PluginService,
    Unknown(String),
}

impl RunnerVerb {
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::ExecCommand => "exec_command",
            Self::PluginService => "plugin_service",
            Self::Unknown(verb) => verb,
        }
    }
}

impl From<&str> for RunnerVerb {
    fn from(value: &str) -> Self {
        match value {
            "exec_command" => Self::ExecCommand,
            "plugin_service" => Self::PluginService,
            other => Self::Unknown(other.to_owned()),
        }
    }
}

impl From<String> for RunnerVerb {
    fn from(value: String) -> Self {
        match value.as_str() {
            "exec_command" => Self::ExecCommand,
            "plugin_service" => Self::PluginService,
            _ => Self::Unknown(value),
        }
    }
}

impl Serialize for RunnerVerb {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RunnerVerb {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer).map(Self::from)
    }
}

/// One tool invocation, the runner's view of `ToolCallRequest`.
///
/// `args` is the opaque verb payload forwarded to the in-namespace primitive;
/// `intent` reuses the protocol enum so the runner does not redefine the verb
/// taxonomy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    pub invocation_id: String,
    pub caller_id: String,
    pub verb: RunnerVerb,
    pub intent: Intent,
    pub args: Value,
    #[serde(default)]
    pub background: bool,
}

/// A fully-resolved request to the runner: which mode, the tool call, the
/// overlay layout (fresh-ns), and the held namespace FDs (setns).
///
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunRequest {
    /// Fresh-ns vs setns.
    pub mode: RunMode,
    /// The tool invocation to execute inside the namespace.
    pub tool_call: ToolCall,
    /// Where the overlay is (or will be) mounted; the exec cwd.
    pub workspace_root: WorkspaceRoot,
    /// Overlay lower layers (newest-first), present for [`RunMode::FreshNs`].
    #[serde(default)]
    pub layer_paths: Vec<PathBuf>,
    /// Overlay upperdir (fresh-ns).
    #[serde(default)]
    pub upperdir: Option<PathBuf>,
    /// Overlay workdir (fresh-ns).
    #[serde(default)]
    pub workdir: Option<PathBuf>,
    /// Held namespace FDs to `setns` into, present for [`RunMode::SetNs`].
    #[serde(default)]
    pub ns_fds: Option<NsFds>,
    /// Absolute iws cgroup path; the setns child joins it before `fork` so the
    /// child inherits cgroup membership.
    #[serde(default)]
    pub cgroup_path: Option<PathBuf>,
    /// Hard timeout for the tool call (tool's own timeout + a fixed margin).
    #[serde(default)]
    pub timeout_seconds: Option<f64>,
}

/// The runner's result.
///
/// Contains the in-namespace tool result JSON plus the child's exit code. The
/// runner constructs the tool result object in-namespace and carries it
/// opaquely as [`Value`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunResult {
    /// The in-namespace tool result object, forwarded unchanged.
    pub tool_result: Value,
    /// The child process exit code.
    pub exit_code: i32,
}

#[cfg(test)]
mod tests {
    use super::RunnerVerb;

    #[test]
    fn runner_verb_preserves_wire_strings_and_unknowns() {
        assert_eq!(
            serde_json::to_value(&RunnerVerb::ExecCommand).expect("serialize"),
            serde_json::json!("exec_command")
        );
        assert_eq!(
            serde_json::from_value::<RunnerVerb>(serde_json::json!("plugin_service"))
                .expect("deserialize"),
            RunnerVerb::PluginService
        );
        assert_eq!(
            serde_json::from_value::<RunnerVerb>(serde_json::json!("future_verb"))
                .expect("deserialize"),
            RunnerVerb::Unknown("future_verb".to_owned())
        );
    }
}
