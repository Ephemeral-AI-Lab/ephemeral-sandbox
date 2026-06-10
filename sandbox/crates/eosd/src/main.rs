//! `eosd` binary entry: subcommand dispatch ONLY.
//!
//! # Invariant this binary owns (`proj-lib-main-split`)
//!
//! `main.rs` holds NO domain logic. It parses argv, routes to one of three
//! subcommand adapters, and maps their typed errors to process exit codes:
//!
//! - `eosd daemon`     -> the async RPC server in `eos-daemon`.
//! - `eosd ns-runner`  -> the single-threaded namespace runner in `eos-ns-child::runner`.
//! - `eosd ns-holder`  -> the single-threaded namespace holder in `eos-ns-child::holder`.
//!
//! Three real processes, one static binary. This is the launcher chain:
//! `daemon` owns the RPC server, `ns-runner` owns fresh/setns tool execution,
//! and `ns-holder` owns the persistent isolated namespace holder lifecycle.
//!
//! `anyhow` is allowed here (binary crate); library crates keep `thiserror`. A
//! tiny hand-rolled arg match is used instead of `clap` — the surface is three
//! fixed subcommands plus `--version`.
//!
//! # Exit-code contract (preserved through this dispatcher)
//!
//! The library errors carry exit codes that MUST survive to the process exit
//! status; a blanket `anyhow` fallthrough would collapse them all to `1` and
//! silently drop the contract. The dispatcher therefore maps known codes via
//! [`std::process::exit`]:
//! - ns-holder: `1` (control pipe closed), `2` (unexpected token), `7` (test
//!   crash knob) — `eos_ns_child::holder::NsHolderError::{CONTROL_CLOSED_EXIT,
//!   UNEXPECTED_TOKEN_EXIT, TEST_CRASH_EXIT}`.
//! - thin-client / daemon connect path: `97` (`CONNECT_FAILED`), `98`
//!   (`IO_FAILED`) — `eos_daemon::wire::{CONNECT_FAILED, IO_FAILED}`.
#![forbid(unsafe_code)]

mod daemon;
mod runner;

use std::os::fd::RawFd;

use anyhow::{anyhow, Context, Result};

fn main() -> Result<()> {
    let mut args = std::env::args();
    let _argv0 = args.next();

    match args.next().as_deref() {
        Some("--version" | "-V") => {
            println!("eosd {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("daemon") => daemon::run(args),
        Some("ns-runner") => runner::run(args),
        Some("ns-holder") => run_ns_holder(args),
        // The committed `contract/ops.json`; `cargo xtask check-contract`
        // fails when this output drifts from the checked-in artifact.
        Some("dump-ops") => {
            print!("{}", eos_daemon::wire::ops::ops_json_document());
            Ok(())
        }
        Some(other) => Err(anyhow!(
            "unknown subcommand {other:?}; expected daemon | ns-runner | ns-holder | dump-ops | --version"
        )),
        None => Err(anyhow!(
            "missing subcommand; expected daemon | ns-runner | ns-holder | dump-ops | --version"
        )),
    }
}

/// `eosd ns-holder <readiness_fd> <control_fd>` — become the single-threaded
/// child that creates and pins the isolated workspace's namespace stack and
/// runs the readiness handshake, then `pause()`s until `SIGTERM`.
///
/// Real thin call: `eos-ns-child::holder` already exposes `run(readiness_fd,
/// control_fd)`, and its lib doc sanctions keeping the argv -> FD parsing here.
/// We parse the two positional FD ints and dispatch; the holder's typed errors
/// carry exit codes (`1` / `2` / `7`) that we map onto the process status so the
/// daemon-side crash-recovery sees the same codes as the Rust holder.
fn run_ns_holder(mut args: std::env::Args) -> Result<()> {
    let readiness_fd = parse_fd(args.next(), "readiness_fd")?;
    let control_fd = parse_fd(args.next(), "control_fd")?;

    match eos_ns_child::holder::run(readiness_fd, control_fd) {
        Ok(()) => Ok(()),
        Err(err) => {
            let code = match &err {
                eos_ns_child::holder::NsHolderError::ControlPipeClosed => {
                    eos_ns_child::holder::NsHolderError::CONTROL_CLOSED_EXIT
                }
                eos_ns_child::holder::NsHolderError::UnexpectedToken => {
                    eos_ns_child::holder::NsHolderError::UNEXPECTED_TOKEN_EXIT
                }
                eos_ns_child::holder::NsHolderError::TestCrash => {
                    eos_ns_child::holder::NsHolderError::TEST_CRASH_EXIT
                }
                // Unshare / pipe-i/o failures have no dedicated Rust exit code;
                // surface the message and fall through to the generic status.
                _ => return Err(anyhow::Error::new(err).context("ns-holder failed")),
            };
            // The holder reached a defined non-zero terminal state; reproduce the
            // exact Rust exit code (1 / 2) instead of anyhow's generic 1.
            std::process::exit(code);
        }
    }
}

/// Parse a positional file-descriptor argument shared by the ns-holder arm.
fn parse_fd(value: Option<String>, name: &str) -> Result<RawFd> {
    value
        .ok_or_else(|| anyhow!("missing {name} argument for ns-holder"))?
        .parse::<RawFd>()
        .with_context(|| format!("{name} must be an integer file descriptor"))
}
