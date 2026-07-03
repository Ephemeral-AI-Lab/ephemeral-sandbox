//! Daemon↔namespace-runner protocol DTOs.

use std::os::unix::io::RawFd;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(transparent)]
pub struct Fd(pub RawFd);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NsFds {
    pub user: Option<Fd>,
    pub mnt: Option<Fd>,
    pub pid: Option<Fd>,
    pub net: Option<Fd>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NamespaceRunnerRequest {
    pub request_id: String,
    pub args: Value,
    pub workspace_root: PathBuf,
    pub layer_paths: Vec<PathBuf>,
    #[serde(default)]
    pub upperdir: Option<PathBuf>,
    #[serde(default)]
    pub workdir: Option<PathBuf>,
    #[serde(default)]
    pub ns_fds: Option<NsFds>,
    #[serde(default)]
    pub timeout_seconds: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observability_log_path: Option<PathBuf>,
    #[serde(default)]
    pub command_security: CommandSecurityPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunResult {
    pub exit_code: i32,
    pub payload: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CommandSecurityPolicy {
    pub mode: CommandSecurityMode,
}

impl CommandSecurityPolicy {
    #[must_use]
    pub const fn enforce() -> Self {
        Self {
            mode: CommandSecurityMode::Enforce,
        }
    }

    #[must_use]
    pub const fn relaxed() -> Self {
        Self {
            mode: CommandSecurityMode::Relaxed,
        }
    }

    #[must_use]
    pub const fn off() -> Self {
        Self {
            mode: CommandSecurityMode::Off,
        }
    }
}

impl Default for CommandSecurityPolicy {
    fn default() -> Self {
        Self::enforce()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandSecurityMode {
    #[default]
    Enforce,
    Relaxed,
    Off,
}
