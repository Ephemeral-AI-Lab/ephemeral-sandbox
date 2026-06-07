use std::time::Duration;

use tokio::task::JoinHandle;

use super::super::{spawn_monitor_loop, BackgroundSessionMonitor};
use super::WorkflowSessionManager;

/// Polls delegated workflow sessions through the shared monitor loop.
pub(in crate::background) struct WorkflowSessionMonitor {
    join: JoinHandle<()>,
}

impl Drop for WorkflowSessionMonitor {
    fn drop(&mut self) {
        self.join.abort();
    }
}

impl BackgroundSessionMonitor for WorkflowSessionMonitor {
    type Manager = WorkflowSessionManager;

    fn spawn(manager: Self::Manager, interval: Duration) -> Self {
        Self {
            join: spawn_monitor_loop(manager, interval),
        }
    }
}
