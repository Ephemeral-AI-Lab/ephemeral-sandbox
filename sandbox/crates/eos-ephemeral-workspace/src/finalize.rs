use std::time::Instant;

use crate::capture::{capture_for_publish, CapturedUpperdir};
use crate::error::EphemeralWorkspaceError;
use crate::ports::WorkspacePublisherPort;
use crate::timings::EphemeralTimings;
use crate::types::{EphemeralWorkspace, PublishOutcome};

/// Request to finalize a publishable ephemeral workspace.
#[derive(Debug, Clone)]
pub struct FinalizeRequest {
    pub workspace: EphemeralWorkspace,
}

/// Capture and publish result for one ephemeral workspace.
#[derive(Debug, Clone, PartialEq)]
pub struct FinalizeOutcome {
    pub capture: CapturedUpperdir,
    pub publish: PublishOutcome,
    pub timings: EphemeralTimings,
}

/// Capture upperdir changes and publish them through the injected publisher.
///
/// # Errors
///
/// Returns [`EphemeralWorkspaceError`] when capture or publish fails.
pub fn finalize_publishable_workspace<P>(
    publisher: &P,
    request: FinalizeRequest,
) -> Result<FinalizeOutcome, EphemeralWorkspaceError>
where
    P: WorkspacePublisherPort,
{
    let capture = capture_for_publish(&request.workspace.dirs.upperdir)?;
    let publish_start = Instant::now();
    let publish = publisher.publish_upperdir_changes(
        &request.workspace.layer_stack_root,
        &request.workspace.snapshot,
        &capture.changes,
        &capture.path_kinds,
    )?;

    let timings = EphemeralTimings {
        publish_s: Some(publish_start.elapsed().as_secs_f64()),
    };

    Ok(FinalizeOutcome {
        capture,
        publish,
        timings,
    })
}
