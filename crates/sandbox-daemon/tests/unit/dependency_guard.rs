#[test]
fn daemon_manifest_excludes_host_store_and_sqlite_dependencies() {
    let manifest = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"),
    )
    .expect("read daemon manifest");
    let dependencies = manifest_section(&manifest, "[dependencies]");
    for forbidden in ["rusqlite", "host"] {
        assert!(
            !dependencies.contains(forbidden),
            "daemon hot path must not depend on {forbidden}"
        );
    }
}

fn manifest_section<'a>(manifest: &'a str, section: &str) -> &'a str {
    let Some(start) = manifest.find(section) else {
        return "";
    };
    let body = &manifest[start + section.len()..];
    let end = body.find("\n[").unwrap_or(body.len());
    &body[..end]
}
