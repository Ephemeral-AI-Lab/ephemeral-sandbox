//! Sandbox daemon server: `AF_UNIX` plus optional loopback TCP, one framed
//! request per connection, dispatch through sandbox-runtime operations, and
//! token-driven shutdown.
#![forbid(unsafe_code)]

pub mod server;
pub mod telemetry;

pub use server::{SandboxDaemonError, SandboxDaemonServer, ServerConfig};
