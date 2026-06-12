#![forbid(unsafe_code)]

#[path = "core/lib.rs"]
pub mod core;

#[path = "checkpoint/lib.rs"]
pub mod checkpoint;
#[path = "command/lib.rs"]
pub mod command;
#[path = "control/lib.rs"]
pub mod control;
#[path = "file/lib.rs"]
pub mod file;
#[path = "isolation/lib.rs"]
pub mod isolation;
#[path = "plugin/lib.rs"]
pub mod plugin;
#[path = "workspace_run/lib.rs"]
pub mod workspace_run;

pub use core::{
    ArgProblem, ArgsError, CallerId, ChangedPathKind, ChangedPathKinds, CommandId, FaultDetails,
    InvocationId, MutationCore, MutationSource, MutationStatus, OpError, OpRequest, OpResponse,
    OpResponseError, OpResponseErrorKind, OperationEnvelope, OperationFault, OperationStatus,
    OperationWarning, RequestError, ResourceSummary, ResponseMeta, SourceError, StepSummary,
    TraceRef, V1FlatteningAdapter, WorkspaceConflict, WorkspaceKind, WorkspaceMutationOutcome,
    WorkspaceTimings,
};
