mod engine;
mod error;
mod execution;
mod launcher;
mod promise;
mod pty;
mod registry;
mod shell;
mod timing;
mod transcript_rows;
mod types;

pub use engine::NamespaceExecutionEngine;
pub use error::NamespaceExecutionError;
pub use execution::{ExecutionHandle, InteractiveExecution};
pub use launcher::{NsRunnerLauncher, RunnerChild};
pub use promise::{CompletionPromise, CompletionWaiter};
pub use pty::{open_pty_pair, PtyMaster};
pub use registry::ExecutionRegistry;
pub use shell::{NamespaceExecutionTerminalStatus, RunnerOutcome, ShellOperation};
pub use transcript_rows::{
    required_transcript_window, transcript_window, CommandStream, CommandTranscriptRow,
    CommandTranscriptWindow,
};
pub use types::{ExecutionObserver, NamespaceExecutionId, NamespaceTarget, NoopObserver};
