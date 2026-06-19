//! Shared workspace runtime primitives plus concrete workspace isolation
//! profiles.
//!
//! Every profile creates a private mounted workspace: fresh overlay directories
//! plus the holder-owned namespace stack used to run and remount commands.
//! `WorkspaceProfile` selects the isolation profile applied to that workspace; higher
//! layers decide when a workspace is created, destroyed, captured, or published.
//!
//! The host-compatible profile keeps the private workspace overlay and holder
//! namespace stack while preserving host network access. The isolated profile
//! adds a dedicated network boundary with veth, DNS, and network policy.
//! `overlay` holds the filesystem and telemetry contracts both profiles share,
//! while common lifecycle code owns holder, namespace FD, scratch, and cgroup
//! behavior.
#![forbid(unsafe_code)]

pub mod error;
mod isolated_setup;
mod lifecycle;
pub mod model;
mod namespace;
pub mod overlay;
pub mod profile;
pub mod service;

pub use error::WorkspaceError;
pub use model::{
    BaseRevision, CallerId, CaptureChangesRequest, CapturedWorkspaceChanges, ChangedPathKind,
    CreateWorkspaceRequest, DestroyWorkspaceRequest, DestroyWorkspaceResult, LatestSnapshotRequest,
    LayerStackSnapshotRef, LayerStackSnapshotView, LeaseId, ProtectedPathDrop,
    ProtectedPathDropReason, ReadonlySnapshotHandle, RemountWorkspaceRequest,
    RemountWorkspaceResult, WorkspaceEntry, WorkspaceEntryError, WorkspaceEntryFds,
    WorkspaceHandle, WorkspaceId, WorkspaceProfile,
};
pub use service::{WorkspaceRuntimeHooks, WorkspaceRuntimeService};
