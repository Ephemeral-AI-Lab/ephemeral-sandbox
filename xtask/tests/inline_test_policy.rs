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
        .args(["check-inline-tests", "--root"])
        .arg(root)
        .output()
        .expect("xtask command should run")
}

#[test]
fn rejects_inline_test_attribute_in_source() {
    let root = temp_root("source-test-attribute");
    let src = root.join("crate/src");
    fs::create_dir_all(&src).expect("create source dir");
    fs::write(
        src.join("lib.rs"),
        r#"
#[test]
fn inline_test() {}
"#,
    )
    .expect("write source file");

    let output = run_check(&root);

    fs::remove_dir_all(&root).expect("remove temp root");
    assert!(!output.status.success(), "inline #[test] should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("inline test attribute"), "{stderr}");
}

#[test]
fn rejects_src_tests_directory() {
    let root = temp_root("src-tests");
    let crate_root = root.join("crate");
    let src_tests = crate_root.join("src/tests");
    fs::create_dir_all(&src_tests).expect("create source tests dir");
    fs::write(
        crate_root.join("Cargo.toml"),
        "[package]\nname = \"fake\"\n",
    )
    .expect("write manifest");
    fs::write(
        src_tests.join("unit.rs"),
        r#"
#[tokio::test(flavor = "current_thread")]
async fn inline_async_test() {}
"#,
    )
    .expect("write source test file");

    let output = run_check(&root);

    fs::remove_dir_all(&root).expect("remove temp root");
    assert!(
        !output.status.success(),
        "crate src/tests directories should still count as inline tests"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("inline test attribute"), "{stderr}");
}

#[test]
fn rejects_namespaced_test_attribute_in_source() {
    let root = temp_root("namespaced-test-attribute");
    let src = root.join("crate/src");
    fs::create_dir_all(&src).expect("create source dir");
    fs::write(
        src.join("lib.rs"),
        r#"
#[async_std::test]
async fn inline_async_test() {}
"#,
    )
    .expect("write source file");

    let output = run_check(&root);

    fs::remove_dir_all(&root).expect("remove temp root");
    assert!(
        !output.status.success(),
        "inline namespaced test attribute should fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("inline test attribute"), "{stderr}");
}

#[test]
fn allows_crate_root_tests_directory() {
    let root = temp_root("crate-tests");
    let crate_root = root.join("crate");
    let tests = crate_root.join("tests");
    fs::create_dir_all(&tests).expect("create tests dir");
    fs::write(
        crate_root.join("Cargo.toml"),
        "[package]\nname = \"fake\"\n",
    )
    .expect("write manifest");
    fs::write(
        tests.join("unit.rs"),
        r#"
#[test]
fn external_test() {}
"#,
    )
    .expect("write test file");

    let output = run_check(&root);

    fs::remove_dir_all(&root).expect("remove temp root");
    assert!(
        output.status.success(),
        "crate-root tests/ directories should be allowed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
