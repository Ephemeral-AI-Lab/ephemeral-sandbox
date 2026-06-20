mod error;
mod service;

pub use crate::workspace_remount::{
    CommandRemountInspection, CommandRemountQuiesce, ProcessGroupController,
    RemountCancellationToken, RemountSwitchState,
};
pub use error::CommandServiceError;
pub use service::{
    ActiveCommandProcess, ActiveCommandRef, CancellationState, CommandCompletionStore,
    CommandLaunchDriver, CommandLifecycleState, CommandProcessStore, CommandReservation,
    CommandTerminalResult, CommandTranscriptStore, CompletedCommandRecord, FinalizationState,
    RealCommandLaunchDriver, RetainedCommandTranscript, DEFAULT_MAX_ACTIVE_COMMANDS,
};
pub use service::{
    CancelCommandInput, CommandFinalizedMetadata, CommandLinesOutput, CommandOperationService,
    CommandOutputSnapshot, CommandPollOutput, CommandSessionId, CommandStatus, CommandStream,
    CommandTranscriptRow, CommandYield, ExecCommandInput, PollCommandInput, ReadCommandLinesInput,
    WriteCommandStdinInput,
};

pub(crate) fn operation_entries() -> &'static [crate::operation::OperationEntry] {
    service::operation_entries()
}

pub(crate) fn operation_specs() -> &'static [&'static crate::operation::OperationSpec] {
    service::operation_specs()
}
