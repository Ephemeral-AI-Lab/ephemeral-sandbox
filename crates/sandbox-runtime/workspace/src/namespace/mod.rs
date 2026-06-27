mod fds;
mod holder;
mod setns_runner;

use std::sync::Arc;

use sandbox_observability::{NoopHook, Observer};
use sandbox_runtime_namespace_execution::NamespaceExecutionEngine;

#[cfg(target_os = "linux")]
use crate::session::WorkspaceManagerError;

const MOUNT_MAX_ACTIVE: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NamespaceNetwork {
    Shared,
    IsolatedNetwork,
}

impl NamespaceNetwork {
    #[cfg(target_os = "linux")]
    pub(crate) const fn holder_arg(self) -> &'static str {
        match self {
            Self::Shared => "shared",
            Self::IsolatedNetwork => "isolated",
        }
    }

    #[cfg(target_os = "linux")]
    pub(crate) const fn requires_net_fd(self) -> bool {
        matches!(self, Self::IsolatedNetwork)
    }
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NamespaceFd {
    User,
    Mnt,
    Pid,
    Net,
}

#[cfg(target_os = "linux")]
impl NamespaceFd {
    pub(crate) fn proc_path(self, holder_pid: i32) -> String {
        match self {
            Self::User => format!("/proc/{holder_pid}/ns/user"),
            Self::Mnt => format!("/proc/{holder_pid}/ns/mnt"),
            Self::Pid => format!("/proc/{holder_pid}/ns/pid_for_children"),
            Self::Net => format!("/proc/{holder_pid}/ns/net"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NamespacePlan {
    pub(crate) network: NamespaceNetwork,
}

impl NamespacePlan {
    pub(crate) const fn shared_network() -> Self {
        Self {
            network: NamespaceNetwork::Shared,
        }
    }

    pub(crate) const fn isolated() -> Self {
        Self {
            network: NamespaceNetwork::IsolatedNetwork,
        }
    }

    #[cfg(target_os = "linux")]
    pub(crate) const fn fds(self) -> &'static [NamespaceFd] {
        if self.network.requires_net_fd() {
            &[
                NamespaceFd::User,
                NamespaceFd::Mnt,
                NamespaceFd::Pid,
                NamespaceFd::Net,
            ]
        } else {
            &[NamespaceFd::User, NamespaceFd::Mnt, NamespaceFd::Pid]
        }
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn setup_error(error: impl std::fmt::Display) -> WorkspaceManagerError {
    WorkspaceManagerError::SetupFailed {
        step: error.to_string(),
    }
}

pub struct NamespaceRuntime {
    engine: Arc<NamespaceExecutionEngine>,
    obs: Observer,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct HolderKillReport {
    pub(crate) holder_was_alive: bool,
    pub(crate) exit_status: Option<i32>,
    pub(crate) signal: Option<i32>,
    pub(crate) status_raw: Option<i32>,
}

impl NamespaceRuntime {
    pub fn new(setup_timeout_s: f64, obs: Observer) -> Self {
        Self {
            engine: Arc::new(NamespaceExecutionEngine::new(
                Arc::new(NoopHook),
                MOUNT_MAX_ACTIVE,
                setup_timeout_s,
            )),
            obs,
        }
    }
}
