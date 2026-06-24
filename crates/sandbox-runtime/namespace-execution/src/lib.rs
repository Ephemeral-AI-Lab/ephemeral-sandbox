//! Daemon-side namespace execution engine (Phase 2).
//!
//! Workspace-agnostic: callers pass a `NamespaceTarget`, never a workspace type,
//! so this crate sits below `workspace` in the dependency graph.
//!
//! `NamespaceExecutionEngine` drives both families over one Template-Method
//! dispatch (reserve → spawn → `on_running` → watcher{ wait → finalize →
//! `complete` → `resolve` → `on_terminal` }) against a `pub(crate)` launcher
//! Bridge seam. The seam, promise, and PTY substrate stay `pub(crate)`; they are
//! surfaced to this crate's `tests/` suites (whose fakes live in `tests/support`)
//! through the `test-support`-gated `test_support` re-export facade.

mod engine;
mod error;
mod execution;
mod id;
mod launcher;
mod observer;
mod promise;
mod pty;
mod registry;
mod shell;
mod status;
mod target;

pub use engine::NamespaceExecutionEngine;
pub use error::NamespaceExecutionError;
pub use execution::{ExecutionHandle, InteractiveExecution};
pub use id::NamespaceExecutionId;
pub use observer::{ExecutionObserver, NoopObserver};
pub use registry::{CompletedExecution, ExecutionRegistry};
pub use shell::{RunnerOutcome, ShellOperation};
pub use status::NamespaceExecutionTerminalStatus;
pub use target::NamespaceTarget;

/// The `pub(crate)` production seam surfaced to this crate's `tests/` suites —
/// the launcher Bridge, completion promise, and PTY substrate that the fakes in
/// `tests/support` build on. A re-export facade only (no test logic); available
/// solely under the `test-support` feature.
#[cfg(feature = "test-support")]
pub mod test_support {
    pub use crate::launcher::{NsRunnerLauncher, RunnerChild};
    pub use crate::promise::CompletionPromise;
    pub use crate::pty::{open_pty_pair, PtyMaster};
}
