mod cgroup;
mod fds;
mod holder;
mod setns_runner;

#[cfg(target_os = "linux")]
pub(crate) use setns_runner::{ns_command_request, run_child};

use crate::profile::IsolatedNetworkError;

pub(crate) const TEST_HARNESS_ENV: &str = "EOS_ISOLATED_WORKSPACE_TEST_HARNESS";

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

    pub(crate) const fn requires_net_fd(self) -> bool {
        matches!(self, Self::IsolatedNetwork)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NamespaceFd {
    User,
    Mnt,
    Pid,
    Net,
}

impl NamespaceFd {
    #[cfg(target_os = "linux")]
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

pub(crate) fn setup_error(error: impl std::fmt::Display) -> IsolatedNetworkError {
    IsolatedNetworkError::SetupFailed {
        step: error.to_string(),
    }
}

pub(crate) fn test_harness_enabled() -> bool {
    std::env::var(TEST_HARNESS_ENV)
        .is_ok_and(|value| matches!(value.trim(), "1" | "true" | "TRUE" | "yes" | "YES"))
}

pub(crate) struct NamespaceRuntime {
    pub(crate) stub: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct HolderKillReport {
    pub(crate) holder_was_alive: bool,
    pub(crate) exit_status: Option<i32>,
    pub(crate) signal: Option<i32>,
    pub(crate) status_raw: Option<i32>,
}

impl NamespaceRuntime {
    pub(crate) fn stubbed() -> Self {
        Self { stub: true }
    }
}
