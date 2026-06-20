use std::os::fd::AsRawFd;
use std::os::fd::OwnedFd;

use super::Handshake;
use crate::holder::namespace::HeldNamespaces;
use crate::holder::{NamespaceNetwork, NsHolderError, NS_UP, READY};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[test]
fn signal_ns_up_writes_readiness_token() -> TestResult {
    let (readiness_read, readiness_write) = nix::unistd::pipe()?;
    let (_control_read, control_write) = nix::unistd::pipe()?;
    let mut handshake = Handshake::new(
        readiness_write.as_raw_fd(),
        control_write.as_raw_fd(),
        NamespaceNetwork::Isolated,
        held_namespaces_for_test()?,
    );

    handshake.signal_ns_up()?;

    let mut buf = [0_u8; 16];
    let read = nix::unistd::read(readiness_read.as_raw_fd(), &mut buf)?;
    assert_eq!(&buf[..read], NS_UP);
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
        NamespaceNetwork::Isolated,
        held_namespaces_for_test()?,
    );

    handshake.await_net_ready()?;

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
        NamespaceNetwork::Isolated,
        held_namespaces_for_test()?,
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
    let handshake = Handshake::new(
        readiness_write.as_raw_fd(),
        control_write.as_raw_fd(),
        NamespaceNetwork::Shared,
        held_namespaces_for_test()?,
    );

    handshake.finish_ready()?;

    let mut buf = [0_u8; 16];
    let read = nix::unistd::read(readiness_read.as_raw_fd(), &mut buf)?;
    assert_eq!(&buf[..read], READY);
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn finish_ready_does_not_signal_ready_when_required_veth_is_missing() -> TestResult {
    let (_readiness_read, readiness_write) = nix::unistd::pipe()?;
    let (control_read, control_write) = nix::unistd::pipe()?;
    nix::unistd::write(
        &control_write,
        b"net-ready eos-missing-for-test 10.244.0.2 24 10.244.0.1\n",
    )?;
    let mut handshake = Handshake::new(
        readiness_write.as_raw_fd(),
        control_read.as_raw_fd(),
        NamespaceNetwork::Isolated,
        held_namespaces_for_test()?,
    );
    handshake.await_net_ready()?;

    let error = handshake
        .finish_ready()
        .expect_err("missing veth should fail before ready");

    assert!(matches!(error, NsHolderError::NetworkSetup { .. }));
    Ok(())
}

fn held_namespaces_for_test() -> std::io::Result<HeldNamespaces> {
    Ok(HeldNamespaces {
        _user: dev_null_fd()?,
        _mnt: dev_null_fd()?,
        _pid: dev_null_fd()?,
        _net: Some(dev_null_fd()?),
        #[cfg(target_os = "linux")]
        _pid_init: None,
    })
}

fn dev_null_fd() -> std::io::Result<OwnedFd> {
    Ok(std::fs::File::open("/dev/null")?.into())
}
