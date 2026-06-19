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

fn run_check(root: &Path, max_lines: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["check-mod-lib-size", "--root"])
        .arg(root)
        .args(["--max-lines", max_lines])
        .output()
        .expect("xtask command should run")
}

fn body_with_lines(line_count: usize) -> String {
    (0..line_count)
        .map(|index| format!("pub const LINE_{index}: usize = {index};"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn rejects_oversized_mod_rs() {
    let root = temp_root("oversized-mod-rs");
    let src = root.join("crate/src/service");
    fs::create_dir_all(&src).expect("create source dir");
    fs::write(src.join("mod.rs"), body_with_lines(4)).expect("write mod file");

    let output = run_check(&root, "3");

    fs::remove_dir_all(&root).expect("remove temp root");
    assert!(!output.status.success(), "oversized mod.rs should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("mod.rs"), "{stderr}");
}

#[test]
fn rejects_oversized_lib_rs() {
    let root = temp_root("oversized-lib-rs");
    let src = root.join("crate/src");
    fs::create_dir_all(&src).expect("create source dir");
    fs::write(src.join("lib.rs"), body_with_lines(4)).expect("write lib file");

    let output = run_check(&root, "3");

    fs::remove_dir_all(&root).expect("remove temp root");
    assert!(!output.status.success(), "oversized lib.rs should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("lib.rs"), "{stderr}");
}

#[test]
fn ignores_other_large_rust_files() {
    let root = temp_root("large-non-facade");
    let src = root.join("crate/src");
    fs::create_dir_all(&src).expect("create source dir");
    fs::write(src.join("large.rs"), body_with_lines(4)).expect("write source file");

    let output = run_check(&root, "3");

    fs::remove_dir_all(&root).expect("remove temp root");
    assert!(
        output.status.success(),
        "non mod.rs/lib.rs files should be ignored: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn ignores_oversized_mod_rs_under_crate_tests() {
    let root = temp_root("oversized-test-mod-rs");
    let crate_root = root.join("crate");
    let tests = crate_root.join("tests/support");
    fs::create_dir_all(&tests).expect("create tests support dir");
    fs::write(
        crate_root.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    )
    .expect("write fixture manifest");
    fs::write(tests.join("mod.rs"), body_with_lines(4)).expect("write test mod file");

    let output = run_check(&root, "3");

    fs::remove_dir_all(&root).expect("remove temp root");
    assert!(
        output.status.success(),
        "test support mod.rs files should be ignored: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
