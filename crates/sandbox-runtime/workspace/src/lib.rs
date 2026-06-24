//! Shared workspace runtime primitives plus concrete workspace isolation
//! profiles.
//!
//! Every profile creates a private mounted workspace: fresh overlay directories
//! plus the holder-owned namespace stack used to run commands.
//! `WorkspaceProfile` selects the isolation profile applied to that workspace; higher
//! layers decide when a workspace is created, destroyed, captured, or published.
//!
//! The host-compatible profile keeps the private workspace overlay and holder
//! namespace stack without adding a dedicated network boundary. The isolated
//! profile adds a dedicated network boundary with veth and network policy.
//! `overlay` holds the filesystem contracts both profiles share, while common
//! lifecycle code owns holder, namespace FD, scratch, and teardown behavior.
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
    BaseRevision, CaptureChangesRequest, CapturedWorkspaceChanges, ChangedPathKind,
    CreateWorkspaceRequest, DestroyWorkspaceRequest, DestroyWorkspaceResult, LayerStackSnapshotRef,
    LayerStackSnapshotView, LeaseId, ProtectedPathDrop, ProtectedPathDropReason,
    ReadonlySnapshotHandle, WorkspaceEntry, WorkspaceEntryError, WorkspaceEntryFds,
    WorkspaceHandle, WorkspaceProfile, WorkspaceSessionId,
};
pub use service::{WorkspaceRuntimeHooks, WorkspaceRuntimeService};
