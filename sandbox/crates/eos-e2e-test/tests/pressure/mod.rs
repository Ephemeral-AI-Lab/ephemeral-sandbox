#[path = "../support/mod.rs"]
mod support;

const E2E_CONFIG: &str = "crates/eos-e2e-test/tests/pressure/config/default.test.yml";

mod helpers;
mod test_pressure_cross_mode_consistency;
mod test_pressure_cross_subsystem_ladders;
mod test_pressure_failure_recovery;
mod test_pressure_file_ops_concurrency;
mod test_pressure_multi_caller;
mod test_pressure_plugin_refresh_and_isolated_cap;
mod test_pressure_resource_report;
mod test_pressure_soak;
