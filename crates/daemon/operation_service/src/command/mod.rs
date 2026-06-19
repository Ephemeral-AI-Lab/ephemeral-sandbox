#[path = "service/contract.rs"]
pub mod contract;
pub mod error;
#[path = "service/launch.rs"]
mod launch;
#[path = "service/process_store.rs"]
pub mod process_store;
#[path = "service/registry.rs"]
pub mod registry;
pub(crate) mod remount;
pub mod service;
#[path = "service/transcript.rs"]
mod transcript;

#[path = "service/finalize.rs"]
pub(crate) mod finalize;

pub use contract::{
    CancelCommandInput, CommandCallContext, CommandFinalizationOutcome, CommandFinalizedMetadata,
    CommandFinalizedPolicy, CommandId, CommandLinesOutput, CommandOutputSnapshot,
    CommandPollOutput, CommandStatus, CommandStream, CommandTranscriptRow,
    CommandWorkspaceDestroyMetadata, CommandYield, ExecCommandInput, OperationTraceContext,
    PollCommandInput, ReadCommandLinesInput, WriteStdinInput,
};
pub use error::CommandServiceError;
pub use launch::{CommandLaunchDriver, RealCommandLaunchDriver};
pub use process_store::{
    ActiveCommandProcess, ActiveCommandRef, CancellationState, CommandCompletionStore,
    CommandFinalizePolicy, CommandLifecycleState, CommandProcessStore, CommandReservation,
    CommandTerminalResult, CommandTraceOrigin, CommandTranscriptStore, CompletedCommandRecord,
    FinalizationState, RetainedCommandTranscript, DEFAULT_MAX_ACTIVE_COMMANDS,
};
pub use registry::CommandRegistry;
pub use remount::{
    CommandRemountInspection, CommandRemountQuiesce, ProcessGroupController,
    RemountCancellationToken, RemountSwitchState,
};
pub use service::{CommandFinalizationOptions, CommandOperationService};
