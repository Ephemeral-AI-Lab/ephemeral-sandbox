mod error;
mod service;

pub use error::CommandServiceError;
pub use service::test_support;
pub(crate) use service::{
    ActiveCommandProcess, ActiveCommandRef, CancellationState, CommandLifecycleState,
    CommandProcessStore, CommandTerminalResult, CommandTranscriptStore, CommandWorkspaceOwnership,
    CompletedCommandRecord, FinalizationState, RetainedCommandTranscript,
};
pub use service::{
    CommandCompletionPromise, CommandCompletionWaitOutcome, CommandFinalizedMetadata,
    CommandLinesOutput, CommandOperationService, CommandOutputSnapshot, CommandPublishFinalization,
    CommandPublishStatus, CommandSessionId, CommandStatus, CommandYield, ExecCommandInput,
    ReadCommandLinesInput, WriteCommandStdinInput,
};
pub use service::{CommandLaunchDriver, RealCommandLaunchDriver};
