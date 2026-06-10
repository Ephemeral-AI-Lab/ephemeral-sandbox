use std::os::fd::RawFd;

use super::namespace::HeldNamespaces;
use super::namespace::{rbind_proc, unshare_namespace_stack};
use super::network::{
    bring_loopback_up, configure_namespace_veth, disable_ipv6_ra, flush_ipv6_default_route,
    parse_network_config, NetworkConfig,
};
use super::{NsHolderError, NET_READY, NS_UP, READY, TEST_HOLDER_CRASH_ENV};

/// Where the handshake driver currently is.
///
/// The holder's handshake driver advances linearly. The transitions
/// are total and ordered:
/// `Unshared → ProcBound → NsUpSent → NetReadyReceived → Ready → Paused`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub(crate) enum HandshakeState {
    /// Namespace stack `unshare`d; FDs not yet pinned.
    Unshared,
    /// Parent `/proc` recursively bound into the new mount namespace.
    ProcBound,
    /// [`NS_UP`] written to the readiness FD.
    NsUpSent,
    /// A [`NET_READY`]-prefixed line was read from the control FD.
    NetReadyReceived,
    /// Best-effort network hardening applied, [`READY`] written to the readiness FD.
    Ready,
    /// `pause()`ing until `SIGTERM`.
    Paused,
}

/// Drives the readiness/control handshake over a pair of inherited pipe FDs.
///
/// Holds the pinned [`HeldNamespaces`] so they outlive the handshake, and
/// tracks the current [`HandshakeState`]. The pipe FDs are passed as `RawFd`
/// because they are inherited (not owned) — the daemon owns the other ends and
/// closes them; the holder reads/writes but does not own their lifetime.
#[derive(Debug)]
pub(crate) struct Handshake {
    readiness_fd: RawFd,
    control_fd: RawFd,
    state: HandshakeState,
    network_config: Option<NetworkConfig>,
    _namespaces: HeldNamespaces,
}

impl Handshake {
    /// Build a handshake driver over the inherited pipe FDs and the freshly
    /// pinned namespaces, starting in [`HandshakeState::Unshared`]. The pipe FDs
    /// are inherited (the daemon owns the far ends), so they are passed as
    /// `RawFd`, not `OwnedFd`.
    #[must_use]
    pub const fn new(readiness_fd: RawFd, control_fd: RawFd, namespaces: HeldNamespaces) -> Self {
        Self {
            readiness_fd,
            control_fd,
            state: HandshakeState::Unshared,
            network_config: None,
            _namespaces: namespaces,
        }
    }

    /// The current handshake position. Test-only accessor used to assert the
    /// state machine advances correctly; production code drives `state` forward
    /// through the step methods and never reads it back.
    #[cfg(test)]
    pub(crate) const fn state(&self) -> HandshakeState {
        self.state
    }

    /// Write [`NS_UP`] to the readiness FD (handshake step 1) and advance to
    /// [`HandshakeState::NsUpSent`].
    ///
    /// # Errors
    ///
    /// Returns [`NsHolderError::PipeIo`] when the readiness pipe write fails.
    pub(crate) fn signal_ns_up(&mut self) -> Result<(), NsHolderError> {
        write_all_fd(self.readiness_fd, NS_UP)?;
        self.state = HandshakeState::NsUpSent;
        Ok(())
    }

    /// Read the control FD until newline and require a [`NET_READY`] prefix
    /// (handshake step 2). EOF before a token → [`NsHolderError::ControlPipeClosed`];
    /// a non-matching token → [`NsHolderError::UnexpectedToken`].
    ///
    /// # Errors
    ///
    /// Returns [`NsHolderError::ControlPipeClosed`] on EOF,
    /// [`NsHolderError::UnexpectedToken`] for a non-`net-ready` token, or
    /// [`NsHolderError::PipeIo`] for read failures.
    pub(crate) fn await_net_ready(&mut self) -> Result<(), NsHolderError> {
        let mut buf = Vec::new();
        while !buf.contains(&b'\n') {
            let mut chunk = [0_u8; 64];
            let read = read_fd(self.control_fd, &mut chunk)?;
            if read == 0 {
                return Err(NsHolderError::ControlPipeClosed);
            }
            buf.extend_from_slice(&chunk[..read]);
        }
        if !buf.starts_with(NET_READY) {
            return Err(NsHolderError::UnexpectedToken);
        }
        self.network_config = parse_network_config(&buf);
        self.state = HandshakeState::NetReadyReceived;
        Ok(())
    }

    /// Apply best-effort loopback and IPv6 hardening, then write [`READY`]
    /// (handshake step 3) and advance to [`HandshakeState::Ready`].
    ///
    /// # Errors
    ///
    /// Returns [`NsHolderError::PipeIo`] when the final readiness pipe write
    /// fails.
    pub(crate) fn finish_ready(&mut self) -> Result<(), NsHolderError> {
        bring_loopback_up();
        if let Some(config) = self.network_config.as_ref() {
            configure_namespace_veth(config);
        }
        disable_ipv6_ra();
        flush_ipv6_default_route();
        write_all_fd(self.readiness_fd, READY)?;
        self.state = HandshakeState::Ready;
        Ok(())
    }
}

/// Holder entry point.
///
/// Takes the two already-parsed pipe FDs (argv → FD parsing stays in `eosd`'s
/// `main`, per the lib/main split). Returns once `SIGTERM` is received.
///
/// Sequence: [`unshare_namespace_stack`] → [`rbind_proc`] → write [`NS_UP`] →
/// (test-crash knob) → await [`NET_READY`] → best-effort network hardening →
/// write [`READY`] → install a `SIGTERM` handler and `pause()`.
///
/// # Errors
///
/// Returns [`NsHolderError`] when namespace setup, handshake pipe I/O, or the
/// test crash knob fails the holder before it reaches the paused state.
pub fn run(readiness_fd: RawFd, control_fd: RawFd) -> Result<(), NsHolderError> {
    let namespaces = unshare_namespace_stack(readiness_fd, control_fd)?;
    rbind_proc();
    let mut handshake = Handshake::new(readiness_fd, control_fd, namespaces);
    handshake.state = HandshakeState::ProcBound;
    handshake.signal_ns_up()?;
    if std::env::var(TEST_HOLDER_CRASH_ENV)
        .unwrap_or_default()
        .eq_ignore_ascii_case("true")
    {
        return Err(NsHolderError::TestCrash);
    }
    handshake.await_net_ready()?;
    handshake.finish_ready()?;
    handshake.state = HandshakeState::Paused;
    loop {
        // SAFETY: `pause(2)` has no pointer arguments and simply suspends this
        // single-threaded holder process until a signal is delivered. The
        // daemon terminates the holder with SIGTERM/SIGKILL during teardown.
        unsafe {
            libc::pause();
        }
    }
}

fn write_all_fd(fd: RawFd, mut bytes: &[u8]) -> Result<(), NsHolderError> {
    while !bytes.is_empty() {
        // SAFETY: `bytes.as_ptr()` is valid for `bytes.len()` bytes for the
        // duration of this call, and `fd` is an inherited pipe descriptor owned
        // by the process that launched this single-threaded holder.
        let written = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        if written < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(NsHolderError::PipeIo(err));
        }
        let written = usize::try_from(written).map_err(|_| {
            NsHolderError::PipeIo(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "pipe write returned a negative byte count after error handling",
            ))
        })?;
        if written == 0 {
            return Err(NsHolderError::PipeIo(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "pipe write returned zero",
            )));
        }
        bytes = &bytes[written..];
    }
    Ok(())
}

fn read_fd(fd: RawFd, bytes: &mut [u8]) -> Result<usize, NsHolderError> {
    loop {
        // SAFETY: `bytes.as_mut_ptr()` is valid for `bytes.len()` bytes for the
        // duration of this call, and `fd` is an inherited pipe descriptor owned
        // by the process that launched this single-threaded holder.
        let read = unsafe { libc::read(fd, bytes.as_mut_ptr().cast(), bytes.len()) };
        if read >= 0 {
            return usize::try_from(read).map_err(|_| {
                NsHolderError::PipeIo(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "pipe read returned a negative byte count after error handling",
                ))
            });
        }
        let err = std::io::Error::last_os_error();
        if err.kind() != std::io::ErrorKind::Interrupted {
            return Err(NsHolderError::PipeIo(err));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsRawFd;

    use super::{Handshake, HandshakeState};
    use crate::holder::namespace::HeldNamespaces;
    use crate::holder::{NsHolderError, NS_UP, READY};

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

    #[test]
    fn signal_ns_up_writes_readiness_token() -> TestResult {
        let (readiness_read, readiness_write) = nix::unistd::pipe()?;
        let (_control_read, control_write) = nix::unistd::pipe()?;
        let mut handshake = Handshake::new(
            readiness_write.as_raw_fd(),
            control_write.as_raw_fd(),
            HeldNamespaces::for_test()?,
        );

        handshake.signal_ns_up()?;

        let mut buf = [0_u8; 16];
        let read = nix::unistd::read(readiness_read.as_raw_fd(), &mut buf)?;
        assert_eq!(&buf[..read], NS_UP);
        assert_eq!(handshake.state(), HandshakeState::NsUpSent);
        Ok(())
    }

    #[test]
    fn await_net_ready_accepts_prefixed_line() -> TestResult {
        let (_readiness_read, readiness_write) = nix::unistd::pipe()?;
        let (control_read, control_write) = nix::unistd::pipe()?;
        nix::unistd::write(&control_write, b"net-ready extra\n")?;
        let mut handshake = Handshake::new(
            readiness_write.as_raw_fd(),
            control_read.as_raw_fd(),
            HeldNamespaces::for_test()?,
        );

        handshake.await_net_ready()?;

        assert_eq!(handshake.state(), HandshakeState::NetReadyReceived);
        Ok(())
    }

    #[test]
    fn await_net_ready_rejects_wrong_token() -> TestResult {
        let (_readiness_read, readiness_write) = nix::unistd::pipe()?;
        let (control_read, control_write) = nix::unistd::pipe()?;
        nix::unistd::write(&control_write, b"wrong\n")?;
        let mut handshake = Handshake::new(
            readiness_write.as_raw_fd(),
            control_read.as_raw_fd(),
            HeldNamespaces::for_test()?,
        );

        let error = match handshake.await_net_ready() {
            Ok(()) => return Err(std::io::Error::other("wrong token was accepted").into()),
            Err(error) => error,
        };

        assert!(matches!(error, NsHolderError::UnexpectedToken));
        Ok(())
    }

    #[test]
    fn finish_ready_writes_ready_token() -> TestResult {
        let (readiness_read, readiness_write) = nix::unistd::pipe()?;
        let (_control_read, control_write) = nix::unistd::pipe()?;
        let mut handshake = Handshake::new(
            readiness_write.as_raw_fd(),
            control_write.as_raw_fd(),
            HeldNamespaces::for_test()?,
        );

        handshake.finish_ready()?;

        let mut buf = [0_u8; 16];
        let read = nix::unistd::read(readiness_read.as_raw_fd(), &mut buf)?;
        assert_eq!(&buf[..read], READY);
        assert_eq!(handshake.state(), HandshakeState::Ready);
        Ok(())
    }
}
