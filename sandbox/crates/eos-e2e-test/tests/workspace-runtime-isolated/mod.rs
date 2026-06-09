#[path = "../support/mod.rs"]
mod support;

const E2E_CONFIG: &str =
    "crates/eos-e2e-test/tests/workspace-runtime-isolated/config/default.test.yml";

mod isolated_workspace_cross_mode_consistency;
mod isolated_workspace_daemon_restart;
mod isolated_workspace_lifecycle;
mod isolated_workspace_network_isolation;
mod isolated_workspace_private_no_publish;
mod isolated_workspace_tool_routing;
