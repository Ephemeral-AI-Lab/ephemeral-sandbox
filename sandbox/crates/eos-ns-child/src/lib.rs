//! The two single-threaded namespace children: [`holder`] and [`runner`].
//!
//! # Invariant this crate owns
//!
//! Everything here runs as a dedicated single-threaded child process of the
//! daemon, and the crate is **single-threaded and syscall-only — NO tokio**.
//! That is a *kernel requirement, not a style choice*: `unshare(CLONE_NEWUSER)`
//! and `setns()` into a user namespace both require the calling task to be the
//! only thread in the process, or the syscall fails with `EINVAL`. The
//! multithreaded tokio daemon NEVER crosses that boundary itself; it execs
//! `eosd ns-holder` / `eosd ns-runner`, whose bodies live here. The build-time
//! enforcement is this crate's `Cargo.toml` deliberately omitting tokio.
//!
//! # The two children
//!
//! - [`holder`]: created once per isolated workspace. Unshares the full
//!   namespace stack, holds the namespace FDs open for the daemon to wire
//!   into, runs the readiness handshake, then `pause()`s until `SIGTERM`.
//! - [`runner`]: created once per tool call. Either unshares a fresh
//!   user+mount namespace and mounts the workspace overlay, or `setns`es into
//!   the holder's pinned namespaces, then execs the tool and reports a
//!   [`RunResult`](eos_cas::RunResult).
//!
//! The two children share this charter but no code: the holder produces the
//! pinned namespaces, the daemon brokers the FDs, and the runner consumes
//! them. The daemon↔runner wire DTOs live in `eos-cas` so the daemon does not
//! depend on this syscall crate.
//!
//! Linux-only syscall bodies are gated behind `#[cfg(target_os = "linux")]`;
//! non-Linux arms keep the workspace compiling on the macOS dev host. Raw
//! syscall sites carry focused `// SAFETY:` notes, and
//! `#![deny(unsafe_op_in_unsafe_fn)]` keeps that annotation discipline
//! enforced.
#![deny(unsafe_op_in_unsafe_fn)]

pub mod holder;
pub mod runner;
