mod error;
mod service;

use crate::operation::CliOperationFamilySpec;

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

pub(crate) const COMMAND_FAMILY: CliOperationFamilySpec = CliOperationFamilySpec {
    id: "command",
    title: "Command",
    summary: "Run, interact with, and inspect commands.",
    description: "Run, interact with, and inspect commands inside the active sandbox runtime.",
};

pub(crate) fn operation_entries() -> &'static [crate::operation::OperationEntry] {
    service::operation_entries()
}
