use super::*;

#[test]
fn registry_round_trips_records_and_tokens() -> Result<()> {
    let dir = std::env::temp_dir().join(format!("eos-host-registry-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let registry = SandboxRegistry::open(dir.clone())?;
    let record = SandboxRecord::new(
        "sb-1".into(),
        "sb-1".into(),
        "tok".into(),
        37_657,
        "test".into(),
        None,
    );
    let record = registry.insert(record)?;
    assert_eq!(registry.load_token("sb-1")?, "tok");
    assert!(registry.get("sb-1").is_some());
    assert_eq!(registry.list().len(), 1);

    record.cache_endpoint("127.0.0.1:9999".parse().expect("addr"));
    assert!(record.cached_endpoint().is_some());
    record.invalidate_endpoint();
    assert!(record.cached_endpoint().is_none());

    assert!(registry.remove("sb-1").is_some());
    assert!(registry.get("sb-1").is_none());
    assert!(registry.load_token("sb-1").is_err());
    let _ = fs::remove_dir_all(dir);
    Ok(())
}
