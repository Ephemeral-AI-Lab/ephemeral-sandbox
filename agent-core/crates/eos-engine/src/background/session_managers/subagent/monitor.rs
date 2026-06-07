use std::time::Duration;

use tokio::task::JoinHandle;

use super::super::{spawn_monitor_loop, BackgroundSessionMonitor};
use super::SubagentSessionManager;

/// Polls subagent sessions through the shared monitor loop.
pub(in crate::background) struct SubagentSessionMonitor {
    join: JoinHandle<()>,
}

impl Drop for SubagentSessionMonitor {
    fn drop(&mut self) {
        self.join.abort();
    }
}

impl BackgroundSessionMonitor for SubagentSessionMonitor {
    type Manager = SubagentSessionManager;

    fn spawn(manager: Self::Manager, interval: Duration) -> Self {
        Self {
            join: spawn_monitor_loop(manager, interval),
        }
    }
}
