use std::fs;
use std::path::{Path, PathBuf};

use sandbox_benchmark::config::BenchmarkPaths;
use sandbox_benchmark::executors::files::TargetMode;
use sandbox_benchmark::model::OperationId;
use sandbox_benchmark::plan::{load_presets, validate_and_expand};

use crate::support::{create_fake_repository, TestRoot};

#[test]
fn data_only_preset_is_discovered_validated_and_hashed_without_registration_code() {
    let presets = load_presets(&plan_fixture_directory()).expect("discover data-only preset");
    assert_eq!(presets.len(), 1);
    let preset = &presets[0];
    assert_eq!(preset.id, "data-only-high-contention");
    assert_eq!(preset.version, 1);

    let root = TestRoot::new("data-only-preset");
    let repository = root.join("repository");
    create_fake_repository(&repository);
    let paths = BenchmarkPaths::initialize(&root.join("workspace"), &repository)
        .expect("initialize isolated paths");
    let first = validate_and_expand(&preset.plan, &paths, None).expect("expand data-only preset");
    let second =
        validate_and_expand(&preset.plan, &paths, None).expect("repeat canonical expansion");

    assert!(first.runnable);
    assert_eq!(first.plan_hash, second.plan_hash);
    assert_eq!(first.cells.len(), 3);
    assert_eq!(first.estimates.cell_count, 3);
    assert_eq!(first.estimates.trial_batch_count, 9);
    assert_eq!(first.estimates.issued_operation_request_count, 78);
    assert!(first.cells.iter().all(|cell| {
        cell.operation_id == OperationId::FileWrite
            && matches!(
                &cell.operation,
                sandbox_benchmark::model::ExpandedOperationCell::FileWrite(write)
                    if write.target_mode == TargetMode::SameTarget
            )
    }));
}

#[test]
fn preset_envelopes_reject_unknown_fields_and_future_versions() {
    let source = fs::read_to_string(plan_fixture_directory().join("data-only-high-contention.yml"))
        .expect("read immutable preset fixture");

    let unknown_root = TestRoot::new("preset-unknown-field");
    fs::write(
        unknown_root.join("invalid.yml"),
        format!("{source}\nexecutor: runtime-plugin\n"),
    )
    .expect("write invalid preset");
    assert!(load_presets(unknown_root.path()).is_err());

    let future_root = TestRoot::new("preset-future-version");
    fs::write(
        future_root.join("future.yml"),
        source.replacen("schema_version: 1", "schema_version: 2", 1),
    )
    .expect("write future preset");
    assert!(load_presets(future_root.path()).is_err());
}

fn plan_fixture_directory() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plans")
}
