pub mod engine {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/engine.rs"));

    #[allow(dead_code)]
    impl<V: Send + 'static> NamespaceExecutionEngine<V> {
        pub(crate) fn with_launcher(
            launcher: Box<dyn crate::launcher::NsRunnerLauncher>,
            observer: std::sync::Arc<dyn crate::observer::ExecutionObserver>,
            max_active: usize,
            setup_timeout_s: f64,
        ) -> Self {
            Self {
                registry: std::sync::Arc::new(crate::registry::ExecutionRegistry::new(max_active)),
                observer,
                launcher,
                next_id: std::sync::atomic::AtomicU64::new(1),
                setup_timeout_s,
            }
        }

        #[must_use]
        pub(crate) fn registry_is_completed(&self, id: &crate::id::NamespaceExecutionId) -> bool {
            self.registry.is_completed(id)
        }
    }
}

pub mod error {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/error.rs"));
}

pub mod execution {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/execution.rs"));
}

pub mod id {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/id.rs"));
}

pub mod launcher {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/launcher.rs"));
}

pub mod observer {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/observer.rs"));
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

pub mod status {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/status.rs"));
}

pub mod target {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/target.rs"));
}

pub use engine::NamespaceExecutionEngine;
pub use error::NamespaceExecutionError;
pub use execution::{ExecutionHandle, InteractiveExecution};
pub use id::NamespaceExecutionId;
pub use observer::{ExecutionObserver, NoopObserver};
pub use registry::ExecutionRegistry;
pub use shell::{RunnerOutcome, ShellOperation};
pub use status::NamespaceExecutionTerminalStatus;
pub use target::NamespaceTarget;
