//! Sandbox tools: `read_file`, `write_file`, `edit_file`, `multi_edit`,
//! `exec_command`, `write_stdin`. Each builds a typed `eos-sandbox-api`
//! request and projects the daemon result into the model-facing output DTO.
//! Command-session tools additionally coordinate running-session registration and
//! exactly-once terminal recovery through the command-session supervisor port.

mod edit_file;
mod exec_command;
mod lib;
mod multi_edit;
mod read_file;
mod write_file;
mod write_stdin;

pub(crate) fn register(
    registry: &mut crate::registry::ToolRegistry,
    config: &crate::registry::config::ToolConfigSet,
    sandbox_service: super::SandboxToolService,
    command_service: super::CommandToolService,
) {
    lib::register(registry, config, sandbox_service, command_service);
}
