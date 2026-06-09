//! Caller-keyed workspace-run tier: the composition tier that drives ephemeral
//! and isolated command-session runs over the local PTY substrate and the local
//! `ephemeral`/`isolated` lifecycle modules.
//!
//! This crate is `eos-occ`-free by construction — the build-time no-publish
//! guard. The three daemon-resident seams (the OCC single-writer publish, the
//! per-finalize resource telemetry, and the isolated-session audit sink) are
//! injected via [`WorkspaceRunHostPorts`] so the run lifecycle stays here while
//! the OCC writer + daemon-global state stay in the daemon process.
//!
//! The daemon owns the [`WorkspaceRunManager`] singleton, the config bridge, the
//! RPC/op facade, and the §7 cancel coordinator; this crate owns the run
//! container ([`registry`]) and the lifecycle orchestration ([`manager`]).
#![forbid(unsafe_code)]

mod command_handle;
#[cfg(target_os = "linux")]
mod manager;
mod ports;
#[cfg(any(target_os = "linux", test))]
mod registry;

pub use command_handle::CommandHandle;
pub use ports::WorkspaceRunHostPorts;

#[cfg(target_os = "linux")]
pub use manager::{StartTarget, WorkspaceRunManager};
