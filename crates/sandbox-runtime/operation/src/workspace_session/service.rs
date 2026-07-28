mod cgroup;
mod core;
mod dispatcher;
mod impls;
mod model;
mod recovery;
mod snapshot;

pub use core::{MplaLifecycleRoots, WorkspaceSessionService};
#[doc(hidden)]
pub use dispatcher::HolderExitDispatcher;
pub(crate) use impls::WorkspaceSessionShutdownOutcome;
pub use impls::{
    AdmittedCommand, SessionExecutionToken, SweptDisposition, SweptSession, TokenSlot,
};
pub(crate) use model::MplaWorkspaceBinding;
pub use model::{
    CreateSessionRequest, FinalizationState, FinalizeOutcome, FinalizePolicy,
    HolderExitDisposition, HolderExitOutcome, HolderLifecycleEvent, HolderLifecycleEventKind,
    HolderLifecycleSnapshot, PublishFailureStage, PublishWorkspaceSessionResult,
    WorkspaceSessionHandler, WorkspaceSessionPublishDetails,
};
