#[test]
fn config_prd_isolated_section_deserializes_and_validates() {
    prd_config()
        .validate()
        .expect("prd isolated network config is valid");
}

#[test]
fn config_default_isolated_section_is_valid() {
    let config = IsolatedNetworkConfig::default();
    config.validate().expect("default config is valid");
}

#[test]
fn config_validation_rejects_invalid_isolated_values() {
    let mut cfg = prd_config();
    cfg.scratch_root = PathBuf::from("relative");
    assert_invalid(cfg, "isolated.scratch_root");

    let mut cfg = prd_config();
    cfg.scratch_root = PathBuf::from("/");
    assert_invalid(cfg, "isolated.scratch_root");

    let mut cfg = prd_config();
    cfg.scratch_root = cfg.workspace_root.clone();
    assert_invalid(cfg, "isolated.scratch_root");

    let mut cfg = prd_config();
    cfg.total_cap = 0;
    assert_invalid(cfg, "isolated.total_cap");

    let mut cfg = prd_config();
    cfg.memavail_fraction = 0.0;
    assert_invalid(cfg, "isolated.memavail_fraction");

    let mut cfg = prd_config();
    cfg.exit_grace_s = -0.1;
    assert_invalid(cfg, "isolated.exit_grace_s");

    let mut cfg = prd_config();
    cfg.sample_interval_s = 0.001;
    assert_invalid(cfg, "isolated.sample_interval_s");
}

fn prd_config() -> IsolatedNetworkConfig {
    crate::load_baseline()
        .expect("prd config loads")
        .section("isolated")
        .expect("isolated section deserializes")
}

fn assert_invalid(config: IsolatedNetworkConfig, field: &str) {
    let err = config.validate().expect_err("config should be invalid");
    let message = err.to_string();
    assert!(message.contains(field), "{message}");
}
