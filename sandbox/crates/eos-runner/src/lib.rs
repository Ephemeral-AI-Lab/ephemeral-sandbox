//! Namespace runner: the syscalls the kernel forces into a single-threaded caller.
//!
//! # Invariant this crate owns
//!
//! `eos-runner` is **single-threaded and syscall-only — NO tokio**, and that is a
//! *kernel requirement, not a style choice*: `unshare(CLONE_NEWUSER|…)` (fresh-ns
//! mode) and `setns()` into a user namespace (setns mode) both require the calling
//! process/thread to be the only thread in the process, or the syscall fails with
//! `EINVAL`. Spawning this work inline in the multithreaded tokio daemon would
//! break it; instead the daemon execs a dedicated single-threaded child whose body
//! lives here. The R10 import discipline of the Python helpers
//! (`isolated_workspace/scripts/setns_exec.py:1-12`) — never pull `logging` /
//! `asyncio` / `subprocess` before the syscall — maps in Rust to *not depending on
//! tokio*: this crate's `Cargo.toml` deliberately omits it.
//!
//! # Two modes
//!
//! 1. **Fresh-ns** ([`RunMode::FreshNs`]): `unshare(CLONE_NEWUSER|CLONE_NEWNS|…)` →
//!    write `uid_map`/`gid_map` → mount the overlay (via the [`mount::KernelMountPort`]
//!    port, fulfilled by `eos-overlay`'s `kernel_mount`) → spawn the tool → construct
//!    the result JSON → cleanup. One tool call per fresh namespace.
//! 2. **Setns** ([`RunMode::SetNs`]): per isolated call, `setns()` into the
//!    ns-holder's pre-opened namespace FDs (`user`, then `mnt`, then `pid`, then
//!    `net` — order is load-bearing) → `fork` → the child `execvp`s the command.
//!
//! # Process group / cancellation
//!
//! Both modes start the child in its own session/process group (the equivalent of
//! Python `start_new_session=True`, `overlay/namespace_runner.py:250`) so the daemon
//! can `killpg` the whole group from outside — cancel kills the entire tree, not
//! just the immediate child.
//!
//! # Build-time guarantee
//!
//! Linux-only syscall bodies are gated behind `#[cfg(target_os = "linux")]`; the
//! non-Linux arms return [`RunnerError::Unsupported`] so the workspace stays green
//! on the macOS dev host. No real `unsafe` exists yet — every future `unsafe`
//! syscall site is documented in prose and a `// PORT` anchor; this crate keeps
//! `#![deny(unsafe_op_in_unsafe_fn)]` so the implementer is forced to annotate.
//!
//! Internal deps: `eos-protocol` (verb [`Intent`](eos_protocol::Intent)); `eos-overlay`
//! (`kernel_mount`, consumed through the local [`mount::KernelMountPort`] port until
//! the sibling crate lands).
#![deny(unsafe_op_in_unsafe_fn)]

pub mod error;
pub mod fresh_ns;
pub mod mount;
pub mod request;
pub mod setns;

pub use error::RunnerError;
pub use mount::{KernelMountPort, MountInputs, MountedOverlay};
pub use request::{Fd, NsFds, RunMode, RunRequest, RunResult, ToolCall, WorkspaceRoot};

/// Execute one tool call through the runner, dispatching on [`RunRequest::mode`].
///
/// This is the crate's single entry point: the daemon hands a fully-resolved
/// [`RunRequest`] (already knowing whether it wants a fresh namespace or a setns
/// into an existing one) and the runner performs the syscalls on this
/// single-threaded caller.
///
/// `mount` supplies the overlay-mount port; in fresh-ns mode the runner calls it
/// after `unshare` to build the workspace mount, mirroring the Python entrypoint's
/// `mount_overlay` call.
// PORT backend/src/sandbox/overlay/namespace_runner.py:48 — run_in_namespace dispatch
pub fn run(request: &RunRequest, mount: &dyn KernelMountPort) -> Result<RunResult, RunnerError> {
    match request.mode {
        RunMode::FreshNs => fresh_ns::run_fresh_ns(request, mount),
        RunMode::SetNs => setns::run_setns(request, mount),
    }
}
