use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_root(name: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be after Unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("eos-xtask-{name}-{}-{nonce}", std::process::id()))
}

fn run_check(root: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["check-test-support", "--root"])
        .arg(root)
        .output()
        .expect("xtask command should run")
}

#[test]
fn rejects_test_support_feature_cfg_in_source() {
    let root = temp_root("source-test-support-feature");
    let crate_root = root.join("crate");
    let src = crate_root.join("src");
    fs::create_dir_all(&src).expect("create source dir");
    fs::write(
        crate_root.join("Cargo.toml"),
        "[package]\nname = \"fake\"\n",
    )
    .expect("write manifest");
    fs::write(
        src.join("lib.rs"),
        r#"
#[cfg(feature = "test-support")]
pub fn helper_for_test() {}
"#,
    )
    .expect("write source file");

    let output = run_check(&root);

    fs::remove_dir_all(&root).expect("remove temp root");
    assert!(!output.status.success(), "test-support cfg should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("test-support"), "{stderr}");
}

#[test]
fn rejects_nested_test_support_feature_cfg_in_source() {
    let root = temp_root("source-nested-test-support-feature");
    let crate_root = root.join("crate");
    let src = crate_root.join("src");
    fs::create_dir_all(&src).expect("create source dir");
    fs::write(
        crate_root.join("Cargo.toml"),
        "[package]\nname = \"fake\"\n",
    )
    .expect("write manifest");
    fs::write(
        src.join("lib.rs"),
        r#"
#[cfg(any(target_os = "linux", feature = "test-support"))]
pub fn helper_for_test() {}
"#,
    )
    .expect("write source file");

    let output = run_check(&root);

    fs::remove_dir_all(&root).expect("remove temp root");
    assert!(
        !output.status.success(),
        "nested test-support cfg should fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("test-support"), "{stderr}");
}

#[test]
fn allows_test_support_feature_cfg_in_crate_root_tests() {
    let root = temp_root("tests-test-support-feature");
    let crate_root = root.join("crate");
    let tests = crate_root.join("tests");
    fs::create_dir_all(&tests).expect("create tests dir");
    fs::write(
        crate_root.join("Cargo.toml"),
        "[package]\nname = \"fake\"\n",
    )
    .expect("write manifest");
    fs::write(
        tests.join("integration.rs"),
        r#"
#[cfg(feature = "test-support")]
fn helper_for_test() {}
"#,
    )
    .expect("write test file");

    let output = run_check(&root);

    fs::remove_dir_all(&root).expect("remove temp root");
    assert!(
        output.status.success(),
        "crate-root tests/ directories may use test-support cfgs: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn workspace_sources_are_free_of_test_support_feature_gates() {
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .arg("check-test-support")
        .output()
        .expect("xtask command should run");

    assert!(
        output.status.success(),
        "crate src/ Rust files must stay free of test-support feature gates:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
