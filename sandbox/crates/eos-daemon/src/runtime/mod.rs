//! Runtime support shared by daemon handlers and listeners.

pub mod context;
pub mod error;
pub mod invocation_registry;
pub(crate) mod ns_runner;
pub(crate) mod request_args;
pub(crate) mod response;

pub mod config {
    pub use eos_config::configs::daemon::*;
}
