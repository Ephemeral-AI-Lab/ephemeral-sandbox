mod cgroup;
mod core;
mod dispatcher;
mod impls;
mod model;
mod mpla_policy;
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
    ActivateMplaWorkspaceSessionResult, AttachMplaPreparedFixtureResult, CreateSessionRequest,
    FinalizationState, FinalizeOutcome, FinalizePolicy, ForkMplaWorkspaceSessionResult,
    HolderExitDisposition, HolderExitOutcome, HolderLifecycleEvent, HolderLifecycleEventKind,
    HolderLifecycleSnapshot, MplaActivationTimings, MplaLifecycleReceipt, PublishFailureStage,
    PublishMplaWorkspaceSessionResult, PublishWorkspaceSessionResult,
    RollbackMplaWorkspaceSessionResult, SquashMplaBranchResult, WorkspaceSessionHandler,
    WorkspaceSessionPublishDetails,
};
