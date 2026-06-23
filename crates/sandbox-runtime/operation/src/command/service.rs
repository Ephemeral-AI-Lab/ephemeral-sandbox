mod completion;
mod contract;
mod core;
mod exec_command;
pub(crate) mod finalize;
mod helpers;
mod launch;
mod process_store;
mod read_command_lines;
mod status_lookup;
pub mod test_support;
pub(crate) mod transcript;
mod write_command_stdin;

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
