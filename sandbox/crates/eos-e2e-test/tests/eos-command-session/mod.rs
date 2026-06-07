#[path = "../support/mod.rs"]
mod support;

const E2E_CONFIG: &str = "crates/eos-e2e-test/tests/eos-command-session/config/default.test.yml";

mod test_eos_command_session_command_matrix;
mod test_eos_command_session_ephemeral_workspace;
mod test_eos_command_session_error_and_backpressure;
mod test_eos_command_session_isolated_workspace;
mod test_eos_command_session_lifecycle;
mod test_eos_command_session_protocol_smoke;
