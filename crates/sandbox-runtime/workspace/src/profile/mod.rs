//! Workspace isolation profiles and shared profile lifecycle.
//!
//! Profile-specific behavior is selected directly by the workspace lifecycle.
//! Shared handle and resource-control types live here.

pub mod handle;
pub mod manager;

pub use handle::{WorkspaceModeFds, WorkspaceModeHandle, WorkspaceModeId, WorkspaceModeSnapshot};
pub(crate) use manager::validate_workspace_root;
pub use manager::{
    ExitOutcome, ResourceCaps, Rfc1918Egress, WorkspaceModeError, WorkspaceModeManager,
};
