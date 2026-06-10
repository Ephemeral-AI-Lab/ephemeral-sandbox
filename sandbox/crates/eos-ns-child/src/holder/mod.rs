//! Namespace holder: the dedicated single-threaded child that creates and pins
//! the isolated workspace's namespace stack and runs the readiness handshake.
//!
//! # Architecture invariant
//!
//! While still single-threaded, this process `unshare`s the full namespace
//! stack (`CLONE_NEWUSER | CLONE_NEWNS | CLONE_NEWPID | CLONE_NEWNET`), holds
//! the resulting namespace FDs open for the daemon to wire into, runs the
//! readiness/control pipe handshake, then `pause()`s until `SIGTERM`.
//!
//! The daemon NEVER enters a namespace itself — it stays multi-threaded (tokio)
//! and would fail `unshare(CLONE_NEWUSER)` / `setns` into a user namespace,
//! which the kernel requires the calling task to be single-threaded for. This
//! dedicated child is the one that crosses that boundary, so the daemon can
//! later open `/proc/{holder_pid}/ns/{net,pid,mnt,user}` against a stable PID 1
//! of the pidns.
//!
//! # Build-time guarantee
//!
//! The handshake tokens are owned here as inline byte literals, and the module
//! imports nothing internal — the crate-level NO-tokio invariant (see the crate
//! root) is what makes the single-threaded `unshare(CLONE_NEWUSER)` legal.
//! Linux-only at runtime; non-Linux hosts still compile because Linux syscall
//! bodies are gated by `cfg(target_os = "linux")`.
//!
//! # Handshake
//!
//! 1. write [`NS_UP`] (`"ns-up\n"`) to the readiness FD once we are inside the
//!    new namespace stack; the daemon then opens our ns symlinks and wires the
//!    veth/bridge network.
//! 2. read the control FD until newline and require it to start with
//!    [`NET_READY`] (`"net-ready"`) — a PREFIX check, not equality.
//! 3. apply best-effort loopback and IPv6 hardening hooks, then write [`READY`]
//!    (`"ready\n"`) to the readiness FD.
//! 4. `pause()` until `SIGTERM`, then exit 0.
//!
//! Syscall module — `unsafe` is permitted here for raw libc gaps, and every
//! `unsafe` block carries a focused `// SAFETY:` note.

mod handshake;
mod namespace;
mod network;

pub use handshake::run;

/// Readiness handshake token (`b"ns-up\n"`) written to the readiness FD once the
/// holder is inside the new namespace stack.
pub const NS_UP: &[u8] = b"ns-up\n";

/// Control-pipe token the daemon writes once the network is wired.
///
/// The holder requires the newline-terminated control read to *start with* this
/// prefix; it is a `startswith` check, not an equality compare.
pub const NET_READY: &[u8] = b"net-ready";

/// Final readiness token (`b"ready\n"`) written to the readiness FD after the
/// current best-effort network hardening hooks.
pub const READY: &[u8] = b"ready\n";

/// Test-only holder crash knob.
///
/// When set to `"true"`, the holder exits with
/// [`NsHolderError::TEST_CRASH_EXIT`] after writing [`NS_UP`] and before
/// reading the control pipe, to exercise the daemon's holder-crash recovery
/// path.
pub const TEST_HOLDER_CRASH_ENV: &str = "EOS_ISOLATED_WORKSPACE_TEST_HOLDER_CRASH";

/// `/proc` subtree the holder enumerates to find per-interface IPv6 config dirs.
pub const IPV6_CONF_ROOT: &str = "/proc/sys/net/ipv6/conf";

/// Interface names tried when `/proc/sys/net/ipv6/conf` cannot be listed.
pub const FALLBACK_IPV6_CONF_INTERFACES: [&str; 4] = ["all", "default", "lo", "eth0"];

/// Failures raised by the holder lifecycle.
///
/// The variants carry the holder's exit-code contract so the daemon-side
/// recovery logic (and `eosd`'s `main`) can map them to process exit codes
/// without re-deriving them: the exit codes below, plus `SIGTERM` exiting 0.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum NsHolderError {
    /// `unshare` of the namespace stack failed before the handshake could start.
    #[error("failed to unshare namespace stack")]
    Unshare,
    /// The control pipe reached EOF before a full token arrived.
    #[error("control pipe closed before net-ready")]
    ControlPipeClosed,
    /// The control pipe delivered a line that did not start with [`NET_READY`].
    #[error("control pipe sent unexpected token; expected net-ready prefix")]
    UnexpectedToken,
    /// Writing a readiness token or reading the control pipe failed.
    #[error("handshake pipe i/o failed")]
    PipeIo(#[source] std::io::Error),
    /// Namespace setup opened/wrote a procfs control file unsuccessfully.
    #[error("namespace setup io failed at {path}")]
    SetupIo {
        /// Path being opened or written when namespace setup failed.
        path: String,
        /// Underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// Test-only holder crash injection fired after `ns-up`.
    #[error("test holder crash injected")]
    TestCrash,
}

impl NsHolderError {
    /// Exit code for [`NsHolderError::ControlPipeClosed`].
    pub const CONTROL_CLOSED_EXIT: i32 = 1;
    /// Exit code for [`NsHolderError::UnexpectedToken`].
    pub const UNEXPECTED_TOKEN_EXIT: i32 = 2;
    /// Exit code for the test-only crash knob.
    pub const TEST_CRASH_EXIT: i32 = 7;
}
