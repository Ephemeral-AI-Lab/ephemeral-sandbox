mod error;
mod service;

pub use error::CommandServiceError;
pub(crate) use service::{
    ActiveCommandProcess, ActiveCommandRef, CancellationState, CommandLifecycleState,
    CommandProcessStore, CommandTerminalResult, CommandTranscriptStore, CompletedCommandRecord,
    FinalizationState, RetainedCommandTranscript,
};
pub use service::{
    CancelCommandInput, CommandFinalizedMetadata, CommandLinesOutput, CommandOperationService,
    CommandOutputSnapshot, CommandPollOutput, CommandSessionId, CommandStatus, CommandStream,
    CommandTranscriptRow, CommandYield, ExecCommandInput, PollCommandInput, ReadCommandLinesInput,
    WriteCommandStdinInput,
};
pub use service::{CommandLaunchDriver, RealCommandLaunchDriver};

pub(crate) fn operation_entries() -> &'static [crate::operation::OperationEntry] {
    service::operation_entries()
}

pub(crate) fn operation_specs() -> &'static [&'static crate::operation::OperationSpec] {
    service::operation_specs()
}
