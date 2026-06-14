//! `gateway` binary entry: top-level argv dispatch only.
#![forbid(unsafe_code)]

use anyhow::{bail, Result};

mod catalog;
mod engine;
mod router;
mod serve;
mod transport;
mod wire;

#[cfg(test)]
pub(crate) use catalog::{Catalog, Route, Visibility};
#[cfg(test)]
pub(crate) use engine::Engine;
#[cfg(test)]
pub(crate) use router::{handle, Surface};
#[cfg(test)]
pub(crate) use transport::{handle_connection, operator_socket_path, serve_with_catalog};
#[cfg(test)]
pub(crate) use wire::{parse_request, ClientRequest};

#[cfg(test)]
#[path = "../tests/contract/mod.rs"]
mod contract;

fn main() -> Result<()> {
    let mut args = std::env::args();
    let _argv0 = args.next();
    match args.next().as_deref() {
        Some("--version" | "-V") => {
            println!("sandbox-gateway {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("serve") => serve::run(args),
        Some(other) => bail!("unknown subcommand {other:?}; expected serve | --version"),
        None => bail!("missing subcommand; expected serve | --version"),
    }
}
