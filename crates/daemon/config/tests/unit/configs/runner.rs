use super::*;
use std::path::Path;

#[test]
fn config_prd_runner_section_deserializes_and_validates() {
    prd_config().validate().expect("prd runner config is valid");
}

#[test]
fn config_prd_runner_mount_mask_leaves_proc_visible() {
    let config = prd_config();

    assert!(
        !config
            .mount_mask
            .hidden_paths
            .iter()
            .any(|path| path == Path::new("/proc")),
        "masking /proc with an empty tmpfs adds measurable Python startup latency"
    );
}

#[test]
fn config_validation_rejects_invalid_runner_values() {
    let mut cfg = prd_config();
    cfg.child_wait_poll_ms = 0;
    assert_invalid(cfg, "runner.child_wait_poll_ms");

    let mut cfg = prd_config();
    cfg.env.inherit_keys.push(String::new());
    assert_invalid(cfg, "runner.env.inherit_keys");

    let mut cfg = prd_config();
    cfg.env.default_path.clear();
    assert_invalid(cfg, "runner.env.default_path");

    let mut cfg = prd_config();
    cfg.mount_mask.hidden_paths.clear();
    assert_invalid(cfg, "runner.mount_mask.hidden_paths");

    let mut cfg = prd_config();
    cfg.mount_mask.hidden_paths.push(PathBuf::from("relative"));
    assert_invalid(cfg, "runner.mount_mask.hidden_paths");
}

fn prd_config() -> RunnerConfig {
    crate::load_prd()
        .expect("prd config loads")
        .section("runner")
        .expect("runner section deserializes")
}

fn assert_invalid(config: RunnerConfig, field: &str) {
    let err = config.validate().expect_err("config should be invalid");
    let message = err.to_string();
    assert!(message.contains(field), "{message}");
}
