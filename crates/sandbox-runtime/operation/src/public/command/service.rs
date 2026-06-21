mod contract;
mod core;
pub(crate) mod finalize;
mod helpers;
mod impls;
mod launch;
mod process_store;
mod status_lookup;
pub(crate) mod transcript;

pub use contract::{
    CancelCommandInput, CommandFinalizedMetadata, CommandLinesOutput, CommandOutputSnapshot,
    CommandPollOutput, CommandSessionId, CommandStatus, CommandStream, CommandTranscriptRow,
    CommandYield, ExecCommandInput, PollCommandInput, ReadCommandLinesInput,
    WriteCommandStdinInput,
};
pub use core::CommandOperationService;
pub use launch::{CommandLaunchDriver, RealCommandLaunchDriver};
pub(crate) use process_store::{
    ActiveCommandProcess, ActiveCommandRef, CancellationState, CommandLifecycleState,
    CommandProcessStore, CommandTerminalResult, CommandTranscriptStore, CompletedCommandRecord,
    FinalizationState, RetainedCommandTranscript,
};

pub(crate) fn operation_entries() -> &'static [crate::operation::OperationEntry] {
    impls::OPERATIONS
}

pub(crate) fn operation_specs() -> &'static [&'static crate::operation::OperationSpec] {
    impls::SPECS
}
