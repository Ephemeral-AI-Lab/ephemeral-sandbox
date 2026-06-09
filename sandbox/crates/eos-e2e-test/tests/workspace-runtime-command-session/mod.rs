#[path = "../support/mod.rs"]
mod support;

const E2E_CONFIG: &str =
    "crates/eos-e2e-test/tests/workspace-runtime-command-session/config/default.test.yml";

mod command_session_cancel_runs;
mod command_session_command_matrix;
mod command_session_ephemeral_workspace;
mod command_session_error_and_backpressure;
mod command_session_external_process_death;
mod command_session_isolated_workspace;
mod command_session_lifecycle;
mod command_session_protocol_smoke;
