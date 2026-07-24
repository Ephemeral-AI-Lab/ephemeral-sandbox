//! Shared workspace runtime primitives plus the concrete workspace network
//! modes.
//!
//! Every workspace is a private mounted workspace: fresh overlay directories
//! plus the holder-owned namespace stack used to run commands.
//! `NetworkProfile` selects the network boundary applied to that workspace; higher
//! layers decide when a workspace is created, destroyed, captured, or published.
//!
//! The shared mode keeps the private workspace overlay and holder namespace
//! stack and joins the host network namespace. The isolated mode adds a
//! dedicated network boundary with veth and bridge-port isolation.
//! `overlay` holds the filesystem contracts both modes share, while common
//! lifecycle code owns holder, namespace FD, scratch, and teardown behavior.
#![forbid(unsafe_code)]

pub mod error;
mod isolated_network_setup;
mod lifecycle;
pub mod model;
mod namespace;
pub mod overlay;
pub mod scratch;
pub mod service;
pub mod session;

pub use error::WorkspaceError;
#[doc(hidden)]
pub use lifecycle::{classify_remount_report, ReportClassification};
pub use lifecycle::{
    probe_and_set_live_remount_gate, set_live_remount_gate, ReapedSession, RemountOutcome,
};
pub use model::{
    BaseRevision, CaptureChangesRequest, CapturedWorkspaceChanges, ChangedPathKind,
    CreateWorkspaceRequest, DestroyWorkspaceRequest, DestroyWorkspaceResult, LayerStackSnapshotRef,
    LayerStackSnapshotView, LeaseId, NetworkProfile, ProtectedPathDrop, ProtectedPathDropReason,
    ReadonlySnapshotHandle, WorkspaceEntry, WorkspaceEntryError, WorkspaceEntryFds,
    WorkspaceHandle, WorkspaceHolderIdentity, WorkspaceOwnershipSnapshot, WorkspaceSessionId,
};
#[doc(hidden)]
pub use namespace::holder::{
    HolderFinalization, HolderFinalizationProof, HolderFinalizationUnknownClass, HolderProbe,
    HolderProbeUnknownClass,
};
pub use sandbox_runtime_namespace_process::runner::file_op::{
    decode_file_op_payload, run_result_err, run_result_ok, FileRunnerDirEntry,
    FileRunnerDirEntryKind, FileRunnerEntryKind, FileRunnerError, FileRunnerOp, FileRunnerResult,
};
#[doc(hidden)]
pub use scratch::{
    ExecutionScratchLease, ExecutionScratchRoute, LegacyExecutionScratchLocator,
    WorkspaceScratchError, WorkspaceScratchLocator, EXECUTIONS_DIRECTORY, SCRATCH_LAYOUT_VERSION,
    TRANSCRIPT_FILE,
};
#[doc(hidden)]
pub use service::{
    holder_exit_channel, HolderExitListener, HolderExitNotifier, HolderExitShutdown,
    HolderExitSubscription, HolderExitWait,
};
pub use service::{WorkspaceRuntimeHooks, WorkspaceRuntimeService, WorkspaceRuntimeShutdownReport};
