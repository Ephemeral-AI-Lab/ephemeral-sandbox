mod cgroup;
mod fds;
mod holder;
mod plan;
mod setns_runner;

#[cfg(test)]
use std::sync::Arc;

use crate::isolated_workspace::error::IsolatedError;

pub(crate) const TEST_HARNESS_ENV: &str = "EOS_ISOLATED_WORKSPACE_TEST_HARNESS";

pub(crate) fn setup_error(error: impl std::fmt::Display) -> IsolatedError {
    IsolatedError::SetupFailed {
        step: error.to_string(),
    }
}

pub(crate) fn test_harness_enabled() -> bool {
    std::env::var(TEST_HARNESS_ENV)
        .is_ok_and(|value| matches!(value.trim(), "1" | "true" | "TRUE" | "yes" | "YES"))
}

pub(crate) struct NamespaceRuntime {
    pub(crate) stub: bool,
    #[cfg(test)]
    pub(crate) stub_holder_pid: i32,
    #[cfg(test)]
    pub(crate) killed_holders: Option<Arc<std::sync::Mutex<Vec<i32>>>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct HolderKillReport {
    pub(crate) holder_was_alive: bool,
    pub(crate) exit_status: Option<i32>,
    pub(crate) signal: Option<i32>,
    pub(crate) status_raw: Option<i32>,
}

impl NamespaceRuntime {
    pub(crate) fn from_env() -> Self {
        Self {
            stub: test_harness_enabled(),
            #[cfg(test)]
            stub_holder_pid: 0,
            #[cfg(test)]
            killed_holders: None,
        }
    }

    pub(crate) fn stubbed() -> Self {
        Self {
            stub: true,
            #[cfg(test)]
            stub_holder_pid: 0,
            #[cfg(test)]
            killed_holders: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn stubbed_with_holder(
        pid: i32,
        killed_holders: Arc<std::sync::Mutex<Vec<i32>>>,
    ) -> Self {
        Self {
            stub: true,
            stub_holder_pid: pid,
            killed_holders: Some(killed_holders),
        }
    }
}
