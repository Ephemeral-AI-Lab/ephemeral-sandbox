use sandbox_runtime_mpla_poc::{PocConfig, RunId, INTERFACE_VERSION};

#[test]
fn fixed_config_round_trips() {
    let config = PocConfig::default();
    config.validate().expect("fixed config must validate");
    let encoded = serde_json::to_vec(&config).expect("config must encode");
    let decoded: PocConfig = serde_json::from_slice(&encoded).expect("config must decode");
    assert_eq!(decoded, config);
    assert_eq!(INTERFACE_VERSION, "m2-iface-v1");
}

#[test]
fn run_id_rejects_unsafe_targets() {
    for invalid in ["", "/", "~", "all/run", "*.json", "-leading"] {
        assert!(RunId::parse(invalid).is_err(), "{invalid}");
    }
    assert!(RunId::parse("m0-20260727T130703p0800").is_ok());
}
