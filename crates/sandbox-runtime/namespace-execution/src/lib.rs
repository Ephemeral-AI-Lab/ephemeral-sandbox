//! Daemon-side namespace execution engine — types and traits (Phase 1 skeleton).
//!
//! Workspace-agnostic: callers pass a `NamespaceTarget`, never a workspace type,
//! so this crate sits below `workspace` in the dependency graph.
//!
//! The execution/promise/registry skeleton has no production caller yet, so it
//! lives behind the `test-support` feature and is exercised through `tests/`
//! until Phase 2/3 wires it into the daemon.

mod error;
mod id;
mod observer;
mod shell;
mod target;

#[cfg(feature = "test-support")]
mod execution;
#[cfg(feature = "test-support")]
mod promise;
#[cfg(feature = "test-support")]
mod registry;

pub use error::NamespaceExecutionError;
pub use id::NamespaceExecutionId;
pub use observer::ExecutionObserver;
pub use shell::{RunnerOutcome, ShellOperation};
pub use target::NamespaceTarget;

#[cfg(feature = "test-support")]
pub use execution::{ExecutionHandle, InteractiveExecution};

/// Internal skeleton types surfaced to this crate's `tests/` suites; available
/// only under the `test-support` feature.
#[cfg(feature = "test-support")]
pub mod test_support {
    pub use crate::promise::CompletionPromise;
    pub use crate::registry::ExecutionRegistry;
}
