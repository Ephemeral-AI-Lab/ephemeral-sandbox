//! Daemon RPC server: owns transport, dispatch, in-flight tracking, and adapter
//! glue while delegating namespace, workspace, plugin, and checkpoint work to
//! sibling crates.
//!
#![forbid(unsafe_code)]

#[path = "runtime/context.rs"]
pub(crate) mod context;
#[path = "dispatch/dispatcher.rs"]
pub(crate) mod dispatcher;
#[path = "runtime/error.rs"]
pub(crate) mod error;
#[path = "runtime/invocation_registry.rs"]
pub(crate) mod invocation_registry;
pub(crate) mod ops;
#[path = "runtime/request_args.rs"]
pub(crate) mod request_args;
#[path = "runtime/response.rs"]
pub(crate) mod response;
#[path = "transport/server.rs"]
pub(crate) mod server;
pub mod wire;

pub use context::DispatchContext;
pub use dispatcher::OpTable;
pub use invocation_registry::InFlightRegistry;
pub(crate) use invocation_registry::{DEFAULT_REAPER_INTERVAL_S, DEFAULT_TTL_S};
pub use server::{DaemonServer, ServerConfig};

pub(crate) mod config {
    pub(crate) use eos_config::configs::daemon::CommandSessionConfig;
}
