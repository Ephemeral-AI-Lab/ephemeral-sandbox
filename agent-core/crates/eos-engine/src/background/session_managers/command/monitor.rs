use std::time::Duration;

use tokio::task::JoinHandle;

use super::super::{spawn_monitor_loop, BackgroundSessionMonitor};
use super::CommandSessionManager;

/// Polls command sessions through the shared monitor loop.
pub(in crate::background) struct CommandSessionMonitor {
    join: JoinHandle<()>,
}

impl Drop for CommandSessionMonitor {
    fn drop(&mut self) {
        self.join.abort();
    }
}

impl BackgroundSessionMonitor for CommandSessionMonitor {
    type Manager = CommandSessionManager;

    fn spawn(manager: Self::Manager, interval: Duration) -> Self {
        Self {
            join: spawn_monitor_loop(manager, interval),
        }
    }
}
