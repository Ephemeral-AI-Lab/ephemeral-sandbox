//! `sandbox-daemon` binary entry: subcommand dispatch only.
//!
//! # Invariant this binary owns (`proj-lib-main-split`)
//!
//! `main.rs` holds NO domain logic. It parses argv, routes to one of three
//! subcommand adapters, and maps their typed errors to process exit codes:
//!
//! - `sandbox-daemon serve` -> the async RPC server in `sandbox_daemon`.
//! - `sandbox-daemon ns-runner` -> the single-threaded namespace runner in
//!   `sandbox_runtime_namespace_process::runner`.
//! - `sandbox-daemon ns-holder` -> the single-threaded namespace holder in
//!   `sandbox_runtime_namespace_process::holder`.
//!
//! Three real processes, one static binary. This is the launcher chain:
//! `serve` owns the RPC server, `ns-runner` owns setns command execution,
//! and `ns-holder` owns the persistent isolated namespace holder lifecycle.
//!
//! `anyhow` is allowed here (binary crate); library crates keep `thiserror`. A
//! tiny hand-rolled arg match is used instead of `clap` because the surface is
//! fixed subcommands plus `--version`.
//!
//! # Exit-code contract (preserved through this dispatcher)
//!
//! The library errors carry exit codes that MUST survive to the process exit
//! status; a blanket `anyhow` fallthrough would collapse them all to `1` and
//! silently drop the contract. The dispatcher therefore maps known codes via
//! [`std::process::exit`]:
//! - ns-holder: `1` (control pipe closed), `2` (unexpected token) —
//!   `sandbox_runtime_namespace_process::holder::NsHolderError::{CONTROL_CLOSED_EXIT,
//!   UNEXPECTED_TOKEN_EXIT}`.
#![forbid(unsafe_code)]

mod cgroup_setup;
mod gate_probe;
mod holder;
mod runner;
mod serve;

use anyhow::{anyhow, Result};

fn main() -> Result<()> {
    let mut args = std::env::args();
    let _argv0 = args.next();

    match args.next().as_deref() {
        Some("--version" | "-V") => {
            println!("sandbox-daemon {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("serve") => {
            sandbox_runtime_namespace_process::thp::prepare_server_policy();
            serve::run(args)
        }
        Some("ns-runner") => {
            restore_workload_thp_policy();
            runner::run(args)
        }
        Some("ns-holder") => {
            restore_workload_thp_policy();
            holder::run(args)
        }
        Some("gate-probe") => {
            restore_workload_thp_policy();
            gate_probe::run(args)
        }
        Some(other) => Err(anyhow!(
            "unknown subcommand {other:?}; expected {}",
            expected_subcommands()
        )),
        None => Err(anyhow!(
            "missing subcommand; expected {}",
            expected_subcommands()
        )),
    }
}

fn restore_workload_thp_policy() {
    if let Err(error) =
        sandbox_runtime_namespace_process::thp::set_transparent_huge_pages_disabled(false)
    {
        eprintln!("sandbox-daemon child: failed to restore transparent huge pages: {error}");
    }
}

const fn expected_subcommands() -> &'static str {
    "serve | ns-runner | ns-holder | gate-probe | --version"
}
