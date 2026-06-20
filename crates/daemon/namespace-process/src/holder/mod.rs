//! Isolated-workspace namespace holder.

pub(crate) mod namespace;
pub(crate) mod network;

use std::os::fd::RawFd;

use namespace::{rbind_proc, unshare_namespace_stack, HeldNamespaces};
use network::{
    bring_loopback_up, configure_namespace_veth, disable_ipv6_ra, flush_ipv6_default_route,
    parse_network_config, NetworkConfig,
};

pub const NS_UP: &[u8] = b"ns-up\n";

pub const NET_READY: &[u8] = b"net-ready";

pub const READY: &[u8] = b"ready\n";

pub const TEST_HOLDER_CRASH_ENV: &str = "EOS_ISOLATED_WORKSPACE_TEST_HOLDER_CRASH";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamespaceNetwork {
    Shared,
    Isolated,
}

impl NamespaceNetwork {
    const fn is_isolated(self) -> bool {
        matches!(self, Self::Isolated)
    }
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum NsHolderError {
    #[error("failed to unshare namespace stack")]
    Unshare,
    #[error("control pipe closed before net-ready")]
    ControlPipeClosed,
    #[error("control pipe sent unexpected token; expected net-ready prefix")]
    UnexpectedToken,
    #[error("handshake pipe i/o failed")]
    PipeIo(#[source] std::io::Error),
    #[error("namespace setup io failed at {path}")]
    SetupIo {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("namespace network setup failed during {step}")]
    NetworkSetup {
        step: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("test holder crash injected")]
    TestCrash,
}

impl NsHolderError {
    pub const CONTROL_CLOSED_EXIT: i32 = 1;
    pub const UNEXPECTED_TOKEN_EXIT: i32 = 2;
    pub const TEST_CRASH_EXIT: i32 = 7;
}

#[derive(Debug)]
pub(crate) struct Handshake {
    readiness_fd: RawFd,
    control_fd: RawFd,
    network: NamespaceNetwork,
    network_config: Option<NetworkConfig>,
    _namespaces: HeldNamespaces,
}

impl Handshake {
    pub(crate) const fn new(
        readiness_fd: RawFd,
        control_fd: RawFd,
        network: NamespaceNetwork,
        namespaces: HeldNamespaces,
    ) -> Self {
        Self {
            readiness_fd,
            control_fd,
            network,
            network_config: None,
            _namespaces: namespaces,
        }
    }

    pub(crate) fn signal_ns_up(&mut self) -> Result<(), NsHolderError> {
        write_all_fd(self.readiness_fd, NS_UP)
    }

    pub(crate) fn await_net_ready(&mut self) -> Result<(), NsHolderError> {
        let mut buf = [0_u8; 256];
        let mut offset = 0;
        while offset < buf.len() {
            let read = read_fd(self.control_fd, &mut buf[offset..offset + 1])?;
            if read == 0 {
                return Err(NsHolderError::ControlPipeClosed);
            }
            offset += read;
            if buf[offset - 1] == b'\n' {
                break;
            }
        }
        if !buf[..offset].starts_with(NET_READY) {
            return Err(NsHolderError::UnexpectedToken);
        }
        self.network_config = parse_network_config(&buf[..offset]);
        Ok(())
    }

    pub(crate) fn finish_ready(&self) -> Result<(), NsHolderError> {
        if self.network.is_isolated() {
            bring_loopback_up();
            if let Some(config) = &self.network_config {
                configure_namespace_veth(config).map_err(|source| NsHolderError::NetworkSetup {
                    step: "veth",
                    source,
                })?;
            }
            disable_ipv6_ra();
            flush_ipv6_default_route();
        }
        write_all_fd(self.readiness_fd, READY)
    }
}

pub fn run(
    readiness_fd: RawFd,
    control_fd: RawFd,
    network: NamespaceNetwork,
) -> Result<(), NsHolderError> {
    let namespaces = unshare_namespace_stack(readiness_fd, control_fd, network)?;
    rbind_proc();
    let mut handshake = Handshake::new(readiness_fd, control_fd, network, namespaces);
    handshake.signal_ns_up()?;
    if std::env::var(TEST_HOLDER_CRASH_ENV)
        .unwrap_or_default()
        .eq_ignore_ascii_case("true")
    {
        return Err(NsHolderError::TestCrash);
    }
    if network.is_isolated() {
        handshake.await_net_ready()?;
    }
    handshake.finish_ready()?;
    loop {
        // SAFETY: `pause(2)` has no pointer arguments and simply suspends this
        // single-threaded holder process until a signal is delivered.
        unsafe {
            libc::pause();
        }
    }
}

fn write_all_fd(fd: RawFd, mut bytes: &[u8]) -> Result<(), NsHolderError> {
    while !bytes.is_empty() {
        // SAFETY: `bytes.as_ptr()` is valid for `bytes.len()` bytes and the
        // inherited fd is borrowed for the duration of the syscall.
        let written = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        if written < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(NsHolderError::PipeIo(err));
        }
        let written = usize::try_from(written).map_err(|_| {
            NsHolderError::PipeIo(std::io::Error::other("negative write byte count"))
        })?;
        bytes = &bytes[written..];
    }
    Ok(())
}

fn read_fd(fd: RawFd, bytes: &mut [u8]) -> Result<usize, NsHolderError> {
    loop {
        // SAFETY: `bytes.as_mut_ptr()` is valid for `bytes.len()` bytes and the
        // inherited fd is borrowed for the duration of the syscall.
        let read = unsafe { libc::read(fd, bytes.as_mut_ptr().cast(), bytes.len()) };
        if read >= 0 {
            return usize::try_from(read).map_err(|_| {
                NsHolderError::PipeIo(std::io::Error::other("negative read byte count"))
            });
        }
        let err = std::io::Error::last_os_error();
        if err.kind() != std::io::ErrorKind::Interrupted {
            return Err(NsHolderError::PipeIo(err));
        }
    }
}
