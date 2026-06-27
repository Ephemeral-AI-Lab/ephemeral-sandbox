pub mod engine {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/engine.rs"));
}

pub mod error {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/error.rs"));
}

pub mod execution {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/execution.rs"));
}

pub mod launcher {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/launcher.rs"));
}

pub mod promise {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/promise.rs"));
}

pub mod pty {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/pty.rs"));
}

pub mod registry {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/registry.rs"));
}

pub mod shell {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/shell.rs"));
}

pub mod types {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/types.rs"));
}

pub use engine::NamespaceExecutionEngine;
pub use error::NamespaceExecutionError;
pub use execution::{ExecutionHandle, InteractiveExecution};
pub use registry::ExecutionRegistry;
pub use shell::{NamespaceExecutionTerminalStatus, RunnerOutcome, ShellOperation};
pub use types::{NamespaceExecutionId, NamespaceTarget};
