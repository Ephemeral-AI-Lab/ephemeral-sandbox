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
fn rejects_test_support_attribute_in_source() {
    let root = temp_root("source-test-support-attribute");
    let src = root.join("crate/src");
    fs::create_dir_all(&src).expect("create source dir");
    fs::write(
        src.join("lib.rs"),
        r#"
#[should_panic]
fn helper() {}
"#,
    )
    .expect("write source file");

    let output = run_check(&root);

    fs::remove_dir_all(&root).expect("remove temp root");
    assert!(!output.status.success(), "test support attrs should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("test support attribute"), "{stderr}");
}

#[test]
fn rejects_bench_attribute_in_source() {
    let root = temp_root("source-bench-attribute");
    let src = root.join("crate/src");
    fs::create_dir_all(&src).expect("create source dir");
    fs::write(
        src.join("lib.rs"),
        r#"
#[divan::bench]
fn helper() {}
"#,
    )
    .expect("write source file");

    let output = run_check(&root);

    fs::remove_dir_all(&root).expect("remove temp root");
    assert!(!output.status.success(), "bench attrs should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("bench attribute"), "{stderr}");
}

#[test]
fn rejects_broad_allow_in_source() {
    let root = temp_root("source-broad-allow");
    let src = root.join("crate/src");
    fs::create_dir_all(&src).expect("create source dir");
    fs::write(
        src.join("lib.rs"),
        r#"
#[allow(dead_code)]
fn helper() {}
"#,
    )
    .expect("write source file");

    let output = run_check(&root);

    fs::remove_dir_all(&root).expect("remove temp root");
    assert!(!output.status.success(), "broad allow attrs should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("broad lint suppression"), "{stderr}");
}

#[test]
fn rejects_recommended_broad_allow_lints_in_source() {
    for lint in [
        "unsafe_code",
        "unused_must_use",
        "unreachable_code",
        "unused_variables",
        "unused_mut",
        "unused_assignments",
        "clippy::pedantic",
        "clippy::nursery",
        "clippy::restriction",
        "clippy::unwrap_used",
        "clippy::expect_used",
        "clippy::panic",
        "clippy::todo",
        "clippy::unimplemented",
        "clippy::dbg_macro",
    ] {
        let root = temp_root(&format!(
            "source-broad-allow-{}",
            lint.replace("::", "-").replace('_', "-")
        ));
        let src = root.join("crate/src");
        fs::create_dir_all(&src).expect("create source dir");
        fs::write(
            src.join("lib.rs"),
            format!(
                r#"
#[allow({lint})]
fn helper() {{}}
"#
            ),
        )
        .expect("write source file");

        let output = run_check(&root);

        fs::remove_dir_all(&root).expect("remove temp root");
        assert!(
            !output.status.success(),
            "allow({lint}) should fail in production source"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("broad lint suppression"), "{stderr}");
    }
}

#[test]
fn rejects_cfg_attr_wrapped_broad_allow_in_source() {
    let root = temp_root("source-cfg-attr-broad-allow");
    let src = root.join("crate/src");
    fs::create_dir_all(&src).expect("create source dir");
    fs::write(
        src.join("lib.rs"),
        r#"
#![cfg_attr(
    any(test, feature = "e2e-support"),
    allow(unused_must_use)
)]

fn helper() {}
"#,
    )
    .expect("write source file");

    let output = run_check(&root);

    fs::remove_dir_all(&root).expect("remove temp root");
    assert!(
        !output.status.success(),
        "cfg_attr-wrapped allow attrs should fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("broad lint suppression"), "{stderr}");
}

#[test]
fn rejects_module_layout_escape_hatches_in_source() {
    let root = temp_root("source-module-layout-attribute");
    let src = root.join("crate/src");
    fs::create_dir_all(&src).expect("create source dir");
    fs::write(
        src.join("lib.rs"),
        r#"
#[path = "other.rs"]
mod other;
"#,
    )
    .expect("write source file");

    let output = run_check(&root);

    fs::remove_dir_all(&root).expect("remove temp root");
    assert!(!output.status.success(), "path attrs should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("path attribute"), "{stderr}");
}

#[test]
fn rejects_macro_use_in_source() {
    let root = temp_root("source-macro-use");
    let src = root.join("crate/src");
    fs::create_dir_all(&src).expect("create source dir");
    fs::write(
        src.join("lib.rs"),
        r#"
#[macro_use]
extern crate legacy;
"#,
    )
    .expect("write source file");

    let output = run_check(&root);

    fs::remove_dir_all(&root).expect("remove temp root");
    assert!(!output.status.success(), "macro_use attrs should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("macro_use attribute"), "{stderr}");
}

#[test]
fn rejects_abi_linkage_attribute_in_source() {
    let root = temp_root("source-abi-linkage");
    let src = root.join("crate/src");
    fs::create_dir_all(&src).expect("create source dir");
    fs::write(
        src.join("lib.rs"),
        r#"
#[repr(C, packed)]
struct Wire([u8; 4]);
"#,
    )
    .expect("write source file");

    let output = run_check(&root);

    fs::remove_dir_all(&root).expect("remove temp root");
    assert!(!output.status.success(), "ABI/linkage attrs should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("ABI/linkage attribute"), "{stderr}");
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

#[test]
fn allows_crate_root_benches_directory() {
    let root = temp_root("crate-benches");
    let crate_root = root.join("crate");
    let benches = crate_root.join("benches");
    fs::create_dir_all(&benches).expect("create benches dir");
    fs::write(
        crate_root.join("Cargo.toml"),
        "[package]\nname = \"fake\"\n",
    )
    .expect("write manifest");
    fs::write(
        benches.join("bench.rs"),
        r#"
#[divan::bench]
fn external_bench() {}
"#,
    )
    .expect("write bench file");

    let output = run_check(&root);

    fs::remove_dir_all(&root).expect("remove temp root");
    assert!(
        output.status.success(),
        "crate-root benches/ directories should be allowed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
