pub mod error {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/error.rs"));
}

pub mod shell {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/shell.rs"));
}

pub mod types {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/types.rs"));
}

pub mod promise {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/promise.rs"));
}

pub mod pty {
    use std::io;

    pub struct PtyMaster;

    impl PtyMaster {
        pub fn write_stdin(&self, _bytes: &[u8]) -> io::Result<()> {
            Ok(())
        }

        pub fn read_output_since(&self, _offset: u64) -> String {
            String::new()
        }

        pub fn output_len(&self) -> u64 {
            0
        }

        pub fn pgid(&self) -> Option<i32> {
            None
        }

        pub fn cancel(&self) {}

        pub fn cancel_handle(&self) -> std::sync::Arc<dyn Fn() + Send + Sync> {
            std::sync::Arc::new(|| {})
        }
    }
}

pub mod execution {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/execution.rs"));
}

pub use execution::{ExecutionHandle, InteractiveExecution};
pub use types::NamespaceExecutionId;
