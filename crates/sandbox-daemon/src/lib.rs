//! Sandbox daemon server: `AF_UNIX` plus optional loopback TCP, one framed
//! request per connection, dispatch through sandbox-runtime operations, and
//! token-driven shutdown.
#![forbid(unsafe_code)]

pub(crate) mod observability;
mod server;
pub(crate) mod timing;

pub use server::{SandboxDaemonError, SandboxDaemonServer, ServerConfig};
