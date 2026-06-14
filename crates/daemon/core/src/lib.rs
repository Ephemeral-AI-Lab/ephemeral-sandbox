//! Daemon RPC server: owns transport, dispatch, in-flight tracking, and adapter
//! glue while delegating namespace, workspace, plugin, and checkpoint work to
//! sibling crates.
//!
#![forbid(unsafe_code)]

pub(crate) mod dispatch;
pub(crate) mod op_adapter;
pub(crate) mod runtime;
pub(crate) mod trace;
pub(crate) mod transport;
pub mod wire;

pub(crate) use dispatch::{builtin, dispatcher};
pub(crate) use runtime::{
    context, error, invocation_registry, response, runtime_services, workspace_runtime,
};
pub(crate) use transport::server;

pub use context::DispatchContext;
pub use dispatcher::{dispatch, dispatch_with_context};

pub use invocation_registry::InFlightRegistry;
pub(crate) use invocation_registry::{DEFAULT_REAPER_INTERVAL_S, DEFAULT_TTL_S};
pub use runtime_services::RuntimeServices;
pub use server::{DaemonServer, ServerConfig};
pub use workspace_runtime::{ExitOutcome, WorkspaceRuntime};
pub(crate) use workspace_runtime::{WorkspaceEnterError, WorkspaceRecoveryReport};

#[cfg(test)]
mod dependency_guard {
    #[test]
    fn daemon_manifest_excludes_host_store_and_sqlite_dependencies() {
        let manifest = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"),
        )
        .expect("read daemon manifest");
        for forbidden in ["rusqlite", "host"] {
            assert!(
                !manifest.contains(forbidden),
                "daemon hot path must not depend on {forbidden}"
            );
        }
    }
}
