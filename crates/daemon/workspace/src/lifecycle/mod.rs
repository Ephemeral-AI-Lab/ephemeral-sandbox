mod create;
mod destroy;
pub(crate) mod leases;
pub(crate) mod recovery;
pub(crate) mod remount;

use std::collections::HashMap;
use std::time::Instant;

use crate::isolated_workspace::manager::WorkspaceHandle;

pub use destroy::ExitOutcome;
pub(crate) use leases::monotonic_seconds;

pub(crate) fn close_handle_fds(handle: &WorkspaceHandle) {
    for fd in handle.ns_fds.values().copied() {
        if fd >= 0 {
            let _ = nix::unistd::close(fd);
        }
    }
    for fd in [handle.readiness_fd, handle.control_fd] {
        if fd >= 0 {
            let _ = nix::unistd::close(fd);
        }
    }
}

pub(crate) fn record_phase_ms(
    phases_ms: &mut HashMap<String, f64>,
    phase: &str,
    started_at: Instant,
) {
    phases_ms.insert(
        phase.to_owned(),
        started_at.elapsed().as_secs_f64() * 1000.0,
    );
}
