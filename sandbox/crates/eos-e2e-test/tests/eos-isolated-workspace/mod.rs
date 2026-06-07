#[path = "../support/mod.rs"]
mod support;

const E2E_CONFIG: &str = "crates/eos-e2e-test/tests/eos-isolated-workspace/config/default.test.yml";

mod test_eos_isolated_workspace_cross_mode_consistency;
mod test_eos_isolated_workspace_daemon_restart;
mod test_eos_isolated_workspace_lifecycle;
mod test_eos_isolated_workspace_network_isolation;
mod test_eos_isolated_workspace_private_no_publish;
mod test_eos_isolated_workspace_tool_routing;
