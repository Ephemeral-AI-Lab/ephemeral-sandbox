//! Pure `tool_api` helpers: each builds a daemon payload from a typed request,
//! calls a [`SandboxTransport`](crate::SandboxTransport), and parses the JSON
//! envelope into a typed result. No audit wrapping (that lives in `eos-tools`)
//! and no clock/dispatch-timing recording (the caller records that).

pub(crate) mod parse;

mod command;
mod control;
mod edit;
mod glob;
mod grep;
mod read;
mod write;

pub use command::{
    cancel_command_session, collect_command_completions, exec_command, exec_stdin, write_stdin,
};
pub use control::{cancel, command_session_count, heartbeat, inflight_count, isolated_active};
pub use edit::edit_file;
pub use glob::glob;
pub use grep::grep;
pub use read::read_file;
pub use write::write_file;
