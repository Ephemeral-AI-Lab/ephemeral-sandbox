use std::collections::HashSet;
use std::path::PathBuf;

use crate::{
    ExitOutcome, IsolatedError, IsolatedSessions, IsolatedSnapshot, ResourceCaps, WorkspaceHandle,
};

/// State-only isolated workspace manager.
///
/// This type owns session lifecycle mechanics while keeping storage lease
/// acquisition and release outside this crate. Snapshot fields enter as plain
/// data, and exit/TTL outcomes return the lease ids for the daemon to release.
pub struct IsolatedManager {
    sessions: IsolatedSessions,
}

impl IsolatedManager {
    #[must_use]
    pub fn with_scratch_root(caps: ResourceCaps, scratch_root: PathBuf) -> Self {
        Self {
            sessions: IsolatedSessions::with_scratch_root(caps, scratch_root),
        }
    }

    /// Reconcile persisted handles and return orphaned lease ids to release.
    ///
    /// # Errors
    ///
    /// Returns [`IsolatedError`] when the underlying session registry cannot
    /// initialize.
    pub fn initialize(&mut self) -> Result<Vec<String>, IsolatedError> {
        self.sessions.initialize()
    }

    /// Enter the caller's isolated workspace against an already acquired
    /// snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`IsolatedError`] when the session registry rejects the enter.
    pub fn enter(
        &mut self,
        caller_id: &str,
        snapshot: IsolatedSnapshot,
    ) -> Result<WorkspaceHandle, IsolatedError> {
        self.sessions.enter(caller_id, snapshot)
    }

    /// Exit the caller's workspace and return the lease id to release.
    ///
    /// # Errors
    ///
    /// Returns [`IsolatedError`] when the caller has no open workspace or
    /// teardown fails.
    pub fn exit(
        &mut self,
        caller_id: &str,
        grace_s: Option<f64>,
    ) -> Result<ExitOutcome, IsolatedError> {
        self.sessions.exit(caller_id, grace_s)
    }

    #[must_use]
    pub fn get_handle(&self, caller_id: &str) -> Option<WorkspaceHandle> {
        self.sessions.get_handle(caller_id)
    }

    #[must_use]
    pub fn list_open_callers(&self) -> Vec<String> {
        self.sessions.list_open_callers()
    }

    pub fn touch(&mut self, caller_id: &str) {
        self.sessions.touch(caller_id);
    }

    #[must_use]
    pub fn ttl_sweep(&mut self, active_callers: &HashSet<String>) -> Vec<ExitOutcome> {
        self.sessions.ttl_sweep(active_callers)
    }

    pub fn reap_orphan_resources(&mut self) {
        self.sessions.reap_orphan_resources();
    }
}
