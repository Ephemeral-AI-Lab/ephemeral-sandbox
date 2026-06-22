#![forbid(unsafe_code)]

pub(crate) use time::OffsetDateTime;

#[path = "../src/cgroup.rs"]
pub mod cgroup;
#[path = "../src/config.rs"]
mod config;
#[path = "../src/contract.rs"]
mod contract;
#[path = "../src/process.rs"]
pub mod process;
#[path = "../src/pty.rs"]
mod pty;
#[path = "../src/transcript.rs"]
mod transcript;
#[path = "../src/yield_wait_loop.rs"]
pub mod yield_wait_loop;

pub use config::CommandConfig;
pub use contract::CommandError;
pub use process::{CommandProcess, CommandProcessSpec};

pub(crate) use process::*;
pub(crate) use pty::*;
pub(crate) use transcript::*;
pub(crate) use yield_wait_loop::*;

#[path = "unit/process.rs"]
mod process_tests;
#[path = "unit/pty.rs"]
mod pty_tests;
#[path = "unit/transcript.rs"]
mod transcript_tests;
#[path = "unit/yield_wait_loop.rs"]
mod yield_wait_loop_tests;
