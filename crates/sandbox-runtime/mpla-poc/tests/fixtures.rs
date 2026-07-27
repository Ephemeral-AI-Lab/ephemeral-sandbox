use std::fs;
use std::path::{Path, PathBuf};

use sandbox_runtime_mpla_poc::{
    fixture_plan, populate_empty_fixture_root, prepare_fixture, FixtureId, FixtureTier, PocError,
};
use uuid::Uuid;

#[test]
fn plans_preserve_required_smoke_and_heavy_envelopes() {
    let s1 = fixture_plan(FixtureId::S1Code, FixtureTier::Smoke);
    assert_eq!(s1.declared_paths, 10_000);
    assert_eq!(s1.declared_logical_bytes, 128 * 1024 * 1024);
    let s3 = fixture_plan(FixtureId::S3Small, FixtureTier::Heavy);
    assert_eq!(s3.declared_paths, 250_000);
    assert!(s3.maximum_chain_bytes <= 1024 * 1024 * 1024);
    let chain = fixture_plan(FixtureId::S4Chain, FixtureTier::Heavy);
    assert!(chain.maximum_chain_bytes < 10 * 1024 * 1024 * 1024);
}

#[test]
fn empty_fixture_is_durable_and_refuses_overwrite() {
    let temp = TempDirectory::new();
    let root = temp.0.join("S0-empty");
    let receipt =
        prepare_fixture(&root, FixtureId::S0Empty, FixtureTier::Smoke).expect("prepare empty");
    assert_eq!(receipt.observed_paths, 1);
    assert_eq!(receipt.unique_inodes, 1);
    assert_eq!(receipt.stream_buffer_bytes, 32 * 1024);
    assert!(matches!(
        prepare_fixture(&root, FixtureId::S0Empty, FixtureTier::Smoke),
        Err(PocError::Integrity(_))
    ));
}

#[test]
fn empty_existing_allocation_upper_can_be_populated_once() {
    let temp = TempDirectory::new();
    let root = temp.0.join("upper");
    fs::create_dir(&root).expect("create upper");
    let receipt = populate_empty_fixture_root(&root, FixtureId::S0Empty, FixtureTier::Smoke)
        .expect("populate upper");
    assert_eq!(receipt.root, root);
    fs::write(root.join("payload"), b"x").expect("write payload");
    assert!(matches!(
        populate_empty_fixture_root(&root, FixtureId::S0Empty, FixtureTier::Smoke),
        Err(PocError::Integrity(_))
    ));
}

#[test]
fn parsers_fail_closed() {
    assert_eq!(
        FixtureId::parse("S5-semantics").expect("fixture"),
        FixtureId::S5Semantics
    );
    assert_eq!(
        FixtureTier::parse("heavy").expect("tier"),
        FixtureTier::Heavy
    );
    assert!(FixtureId::parse("../S1-code").is_err());
    assert!(FixtureTier::parse("tiny").is_err());
}

struct TempDirectory(PathBuf);

impl TempDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "mpla-fixtures-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        fs::create_dir(&path).expect("create temp directory");
        Self(path)
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        remove_tree(&self.0);
    }
}

fn remove_tree(path: &Path) {
    if path.exists() {
        fs::remove_dir_all(path).expect("remove temp tree");
    }
}
