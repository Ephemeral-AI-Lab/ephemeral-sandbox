use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use sandbox_benchmark::config::BenchmarkPaths;
use sandbox_benchmark::fixtures::{
    default_workspace_profile_directory, load_workspace_profiles, materialize,
    WorkspaceProfileCatalog, FIXTURE_GENERATOR_VERSION,
};
use sandbox_benchmark::model::{ConfigurationScope, OperationPlan, WorkspaceProfileId};
use sandbox_benchmark::plan::{load_plan, slice_default, validate_and_expand_with_profiles};

use crate::support::{create_fake_repository, TestRoot};

#[test]
fn default_profile_catalog_is_strict_versioned_data() {
    let catalog = load_workspace_profiles(&default_workspace_profile_directory())
        .expect("load default workspace profiles");
    let ids = catalog
        .profiles
        .iter()
        .map(|profile| profile.id.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(ids, BTreeSet::from(["large", "medium", "small"]));

    let small = profile(&catalog, "small");
    assert_eq!(small.fixture.file_count, 1_000);
    assert_eq!(small.fixture.logical_bytes, 16 * 1024 * 1024);
    assert_eq!(small.fixture.maximum_depth, 4);
    assert!(small.standard);

    let medium = profile(&catalog, "medium");
    assert_eq!(medium.fixture.file_count, 10_000);
    assert_eq!(medium.fixture.logical_bytes, 256 * 1024 * 1024);
    assert_eq!(medium.fixture.maximum_depth, 8);
    assert!(medium.standard);

    let large = profile(&catalog, "large");
    assert_eq!(large.fixture.file_count, 50_000);
    assert_eq!(large.fixture.logical_bytes, 2 * 1024 * 1024 * 1024);
    assert_eq!(large.fixture.maximum_depth, 12);
    assert!(!large.standard);
}

#[test]
fn metadata_heavy_profile_is_discovered_materialized_and_reused_without_code_registration() {
    let catalog = load_workspace_profiles(&extension_profile_directory())
        .expect("discover extension profile from versioned data");
    assert_eq!(catalog.profiles.len(), 1);
    let extension = profile(&catalog, "metadata_heavy");
    let first_root = TestRoot::new("fixture-first");
    let second_root = TestRoot::new("fixture-second");
    let seed = 0x5eed_u64;

    let first = materialize(first_root.path(), extension, seed)
        .expect("materialize first extension fixture");
    assert!(!first.reused);
    let reused =
        materialize(first_root.path(), extension, seed).expect("reuse first extension fixture");
    assert!(reused.reused);
    assert_eq!(reused.path, first.path);

    let independent = materialize(second_root.path(), extension, seed)
        .expect("materialize independent extension fixture");
    assert!(!independent.reused);
    assert_ne!(independent.path, first.path);
    assert_eq!(
        independent.manifest.fixture_hash,
        first.manifest.fixture_hash
    );
    assert_eq!(independent.manifest.tree_hash, first.manifest.tree_hash);

    let manifest = &first.manifest;
    assert!(
        manifest.fixture_hash.starts_with("sha256:")
            && manifest.fixture_hash.len() == "sha256:".len() + 64
    );
    assert_eq!(manifest.generator_version, FIXTURE_GENERATOR_VERSION);
    assert_eq!(manifest.seed, seed);
    assert_eq!(&manifest.profile_id, &extension.id);
    assert_eq!(manifest.profile_version, extension.version);
    assert_eq!(manifest.requested_file_count, extension.fixture.file_count);
    assert_eq!(manifest.actual_file_count, extension.fixture.file_count);
    assert_eq!(
        manifest.requested_logical_bytes,
        extension.fixture.logical_bytes
    );
    assert_eq!(
        manifest.actual_logical_bytes,
        extension.fixture.logical_bytes
    );
    assert_eq!(
        manifest.requested_maximum_depth,
        extension.fixture.maximum_depth
    );
    assert_eq!(
        manifest.actual_maximum_depth,
        extension.fixture.maximum_depth
    );
    assert_eq!(
        manifest.small_text_files + manifest.medium_binary_files + manifest.large_binary_files,
        extension.fixture.file_count
    );
    assert_eq!(
        count_payload_files(&first.path),
        extension.fixture.file_count
    );
    assert!(manifest.directory_count > 0);

    let different_seed = materialize(first_root.path(), extension, seed + 1)
        .expect("materialize extension fixture with different seed");
    assert_ne!(different_seed.manifest.fixture_hash, manifest.fixture_hash);
    assert_ne!(different_seed.manifest.tree_hash, manifest.tree_hash);
}

#[test]
fn metadata_heavy_flows_through_existing_plan_validation_estimates_and_hashing() {
    let catalog = load_workspace_profiles(&extension_profile_directory())
        .expect("discover extension profile");
    let default_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../defaults/standard-local.yml");
    let default = load_plan(&default_path).expect("load standard default");
    let mut plan = slice_default(&default, ConfigurationScope::Command);
    let OperationPlan::ExecCommand(command) = &mut plan.operations[0] else {
        panic!("command slice contains the command operation")
    };
    command.factors.workspace_profile.values = vec![profile_id("metadata_heavy")];
    command.factors.workspace_profile.control = None;

    let test_root = TestRoot::new("profile-plan");
    let repository = test_root.join("repository");
    create_fake_repository(&repository);
    let paths = BenchmarkPaths::initialize(&test_root.join("workspace"), &repository)
        .expect("initialize isolated benchmark paths");
    let expanded = validate_and_expand_with_profiles(&plan, &paths, &catalog, None)
        .expect("validate extension profile plan");
    assert!(expanded.runnable);
    assert_eq!(expanded.selected_workspace_profiles.len(), 1);
    assert_eq!(
        expanded.selected_workspace_profiles[0].id.as_str(),
        "metadata_heavy"
    );
    assert_eq!(expanded.estimates.estimated_peak_disk_bytes, Some(8 * 1024));
    assert_eq!(
        expanded.estimates.required_free_space_bytes,
        Some(16 * 1024)
    );
    assert!(expanded
        .validation
        .iter()
        .any(|finding| finding.code == "opt_in_workspace_profile"));

    let mut revised_catalog = catalog.clone();
    revised_catalog.profiles[0].version += 1;
    revised_catalog.profiles[0].fixture.logical_bytes += 1;
    let revised = validate_and_expand_with_profiles(&plan, &paths, &revised_catalog, None)
        .expect("validate revised profile data");
    assert_ne!(expanded.plan_hash, revised.plan_hash);
    assert_ne!(expanded.cells[0].cell_id, revised.cells[0].cell_id);
}

#[test]
fn profile_envelopes_reject_unknown_fields() {
    let root = TestRoot::new("profile-strict");
    fs::write(
        root.join("invalid.yml"),
        b"schema_version: 1\nid: invalid\nversion: 1\nlabel: Invalid\nhelp: Invalid test profile.\ngenerator_version: 1\nstandard: false\nfixture:\n  file_count: 1\n  logical_bytes: 1\n  maximum_depth: 0\nexecutor: plugin\n",
    )
    .expect("write invalid profile envelope");
    assert!(load_workspace_profiles(root.path()).is_err());
}

fn extension_profile_directory() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/workspace-profiles")
}

fn profile<'a>(
    catalog: &'a WorkspaceProfileCatalog,
    id: &str,
) -> &'a sandbox_benchmark::fixtures::WorkspaceProfileEnvelope {
    catalog
        .get(&profile_id(id))
        .expect("profile exists in catalog")
}

fn profile_id(value: &str) -> WorkspaceProfileId {
    value.parse().expect("valid profile id")
}

fn count_payload_files(root: &Path) -> u64 {
    let mut count = 0;
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).expect("read fixture directory") {
            let entry = entry.expect("read fixture entry");
            let kind = entry.file_type().expect("read fixture entry type");
            if kind.is_dir() {
                pending.push(entry.path());
            } else if entry.file_name() != "fixture-manifest.json" {
                count += 1;
            }
        }
    }
    count
}
