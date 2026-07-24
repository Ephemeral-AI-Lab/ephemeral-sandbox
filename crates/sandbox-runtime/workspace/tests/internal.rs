#![forbid(unsafe_code)]
#![allow(
    dead_code,
    unused_imports,
    unfulfilled_lint_expectations,
    reason = "the integration harness path-includes production modules and exercises target-specific internals"
)]

#[path = "../src/error.rs"]
pub mod error;
#[path = "../src/isolated_network_setup/mod.rs"]
mod isolated_network_setup;
#[path = "../src/lifecycle/mod.rs"]
mod lifecycle;
#[path = "../src/model.rs"]
pub mod model;
#[path = "../src/namespace/mod.rs"]
mod namespace;
#[path = "../src/overlay/mod.rs"]
pub mod overlay;
#[path = "../src/scratch.rs"]
pub mod scratch;
mod service {
    pub use sandbox_runtime_workspace::{
        holder_exit_channel, HolderExitNotifier, HolderExitSubscription,
    };
}
#[path = "../src/session/mod.rs"]
pub mod session;

pub use error::WorkspaceError;
pub use lifecycle::{
    classify_remount_report, probe_and_set_live_remount_gate, set_live_remount_gate, ReapedSession,
    RemountOutcome, ReportClassification,
};
pub use model::{
    BaseRevision, CaptureChangesRequest, CapturedWorkspaceChanges, ChangedPathKind,
    CreateWorkspaceRequest, DestroyWorkspaceRequest, DestroyWorkspaceResult, LayerStackSnapshotRef,
    LayerStackSnapshotView, LeaseId, NetworkProfile, ProtectedPathDrop, ProtectedPathDropReason,
    ReadonlySnapshotHandle, WorkspaceEntry, WorkspaceEntryError, WorkspaceEntryFds,
    WorkspaceHandle, WorkspaceHolderIdentity, WorkspaceOwnershipSnapshot, WorkspaceSessionId,
};
pub use sandbox_runtime_namespace_process::runner::file_op::{
    decode_file_op_payload, run_result_err, run_result_ok, FileRunnerDirEntry,
    FileRunnerDirEntryKind, FileRunnerEntryKind, FileRunnerError, FileRunnerOp, FileRunnerResult,
};
#[path = "internal/destroy.rs"]
mod destroy_tests;
#[path = "internal/holder.rs"]
mod holder_tests;
