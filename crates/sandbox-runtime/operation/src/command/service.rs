mod completion;
mod contract;
mod core;
pub(crate) mod finalize;
mod helpers;
mod impls;
mod launch;
mod process_store;
mod status_lookup;
pub mod test_support;
pub(crate) mod transcript;

pub use completion::{CommandCompletionPromise, CommandCompletionWaitOutcome};
pub use contract::{
    CommandFinalizedMetadata, CommandLinesOutput, CommandOutputSnapshot,
    CommandPublishFinalization, CommandPublishStatus, CommandSessionId, CommandStatus,
    CommandYield, ExecCommandInput, ReadCommandLinesInput, WriteCommandStdinInput,
};
pub use core::CommandOperationService;
pub use launch::{CommandLaunchDriver, RealCommandLaunchDriver};
pub(crate) use process_store::{
    ActiveCommandProcess, ActiveCommandRef, CancellationState, CommandLifecycleState,
    CommandProcessStore, CommandTerminalResult, CommandTranscriptStore, CommandWorkspaceOwnership,
    CompletedCommandRecord, FinalizationState, RetainedCommandTranscript,
};

pub(crate) fn operation_entries() -> &'static [crate::operation::OperationEntry] {
    impls::OPERATIONS
}
