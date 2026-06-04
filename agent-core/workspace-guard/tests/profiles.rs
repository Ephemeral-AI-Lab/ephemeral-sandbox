// Guards workspace build profiles against regressing to process-aborting panic
// behavior. The engine recovers from per-task and per-attempt panics, so release
// builds must keep unwinding.

use std::fs;
use std::path::Path;

fn workspace_manifest() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("Cargo.toml");
    fs::read_to_string(path).expect("read workspace Cargo.toml")
}

fn section<'a>(manifest: &'a str, header: &str) -> &'a str {
    let start = manifest
        .find(header)
        .unwrap_or_else(|| panic!("missing section {header}"));
    let after = &manifest[start + header.len()..];
    match after.find("\n[") {
        Some(end) => &after[..end],
        None => after,
    }
}

#[test]
fn release_profile_uses_unwind_and_size_opts() {
    let manifest = workspace_manifest();
    let release = section(&manifest, "[profile.release]");
    assert!(
        release.contains("panic = \"unwind\""),
        "release must not abort"
    );
    assert!(release.contains("lto = \"fat\""), "release lto");
    assert!(
        release.contains("codegen-units = 1"),
        "release codegen-units"
    );
    assert!(release.contains("strip = true"), "release strip");
}

#[test]
fn bench_profile_inherits_release() {
    let manifest = workspace_manifest();
    let bench = section(&manifest, "[profile.bench]");
    assert!(
        bench.contains("inherits = \"release\""),
        "bench must inherit release"
    );
}

#[test]
fn no_abort_panic_anywhere() {
    let manifest = workspace_manifest();
    assert!(
        !manifest.contains("panic = \"abort\""),
        "panic = abort is forbidden because recovery requires unwind"
    );
}
