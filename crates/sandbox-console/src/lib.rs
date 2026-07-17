//! Web console HTTP server for the EphemeralOS sandbox.
//!
//! A client peer of the three `sandbox-cli` binaries, built on
//! `sandbox_operation_client::GatewayClient` and the merged semantic catalog.
//! It serves the SPA assets and local catalog, validates public operation
//! routes, and bridges `/api/rpc` to the gateway JSON-line protocol and the
//! browser to each sandbox's `daemon_http` surface (the exact read-only
//! `/api/sandboxes/:id/files/list`, and the `/s/:id/...` preview proxy). It
//! reports `/api/sandboxes/:id/health` from manager-owned record state, defines
//! no operation vocabulary, never contacts the daemon RPC endpoint directly,
//! and never exposes the gateway auth token to the browser.
#![forbid(unsafe_code)]

pub mod assets;
pub mod catalog;
pub mod config;
pub mod daemon_api;
pub mod endpoint;
pub mod health;
pub mod proxy;
pub mod response;
pub mod router;
pub mod rpc;
pub mod server;
pub mod state;
