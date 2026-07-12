use std::fs;

use sandbox_benchmark::cleanup::{CleanupError, CleanupLedger, OwnedIdentity, OWNERSHIP_MARKER};
use sandbox_benchmark::config::BenchmarkPaths;

use crate::support::{create_fake_repository, TestRoot};

#[test]
fn registered_owned_directory_has_a_marker_and_is_removed_from_the_ledger() {
    let (test_root, paths) = benchmark_paths("cleanup-owned");
    let target = paths.runs.join("run-1/trial-1");
    fs::create_dir_all(&target).expect("create owned trial directory");
    fs::write(target.join("payload"), b"owned").expect("write owned payload");
    let identity = trial_identity("run-1", "trial-1");
    let mut ledger = CleanupLedger::default();

    let canonical = ledger
        .register(&paths, &target, identity.clone())
        .expect("register owned directory");
    assert!(ledger.contains(&canonical));
    let marker: serde_json::Value = serde_json::from_slice(
        &fs::read(canonical.join(OWNERSHIP_MARKER)).expect("read ownership marker"),
    )
    .expect("parse ownership marker");
    assert_eq!(marker["schema_version"], 1);
    assert_eq!(marker["identity"]["run_id"], "run-1");
    assert_eq!(marker["identity"]["trial_id"], "trial-1");

    ledger
        .remove_owned(&paths, &canonical, &identity)
        .expect("remove registered owned directory");
    assert!(!canonical.exists());
    assert!(!ledger.contains(&canonical));
    drop(test_root);
}

#[test]
fn cleanup_requires_both_the_matching_marker_and_active_ledger_entry() {
    let (_test_root, paths) = benchmark_paths("cleanup-identity");
    let target = paths.runs.join("run-1/trial-1");
    fs::create_dir_all(&target).expect("create owned trial directory");
    let identity = trial_identity("run-1", "trial-1");
    let mut ledger = CleanupLedger::default();
    ledger
        .register(&paths, &target, identity.clone())
        .expect("register owned directory");

    let wrong_identity = trial_identity("run-1", "trial-2");
    let error = ledger
        .remove_owned(&paths, &target, &wrong_identity)
        .expect_err("identity mismatch must prevent cleanup");
    assert!(matches!(error, CleanupError::IdentityMismatch(_)));
    assert!(target.exists());

    let mut empty_ledger = CleanupLedger::default();
    let error = empty_ledger
        .remove_owned(&paths, &target, &identity)
        .expect_err("marker without ledger entry must prevent cleanup");
    assert!(matches!(error, CleanupError::NotInLedger(_)));
    assert!(target.exists());

    fs::write(target.join(OWNERSHIP_MARKER), b"not-json").expect("corrupt ownership marker");
    let error = ledger
        .remove_owned(&paths, &target, &identity)
        .expect_err("invalid marker must prevent cleanup");
    assert!(matches!(error, CleanupError::InvalidMarker(_)));
    assert!(target.exists());
}

#[test]
fn restart_recovery_can_adopt_only_an_existing_matching_marker() {
    let (_test_root, paths) = benchmark_paths("cleanup-adopt");
    let target = paths.runs.join("run-1/trial-1");
    fs::create_dir_all(&target).expect("create owned trial directory");
    let identity = trial_identity("run-1", "trial-1");
    let mut original = CleanupLedger::default();
    original
        .register(&paths, &target, identity.clone())
        .expect("write original marker");
    drop(original);

    let mut recovered = CleanupLedger::default();
    let wrong = trial_identity("run-1", "trial-2");
    assert!(matches!(
        recovered.adopt_existing(&paths, &target, &wrong),
        Err(CleanupError::IdentityMismatch(_))
    ));
    assert!(target.exists());

    let adopted = recovered
        .adopt_existing(&paths, &target, &identity)
        .expect("adopt matching marker");
    recovered
        .remove_owned(&paths, &adopted, &identity)
        .expect("remove recovered ownership");
    assert!(!target.exists());
}

#[test]
fn cleanup_never_registers_an_outside_directory() {
    let (test_root, paths) = benchmark_paths("cleanup-outside");
    let outside = test_root.join("outside");
    fs::create_dir_all(&outside).expect("create outside directory");
    let sentinel = outside.join("sentinel");
    fs::write(&sentinel, b"keep").expect("write outside sentinel");
    let mut ledger = CleanupLedger::default();

    let error = ledger
        .register(&paths, &outside, trial_identity("run-1", "trial-1"))
        .expect_err("outside directory must not be registered");
    assert!(matches!(error, CleanupError::OutsideRoot(_)));
    assert_eq!(fs::read(sentinel).expect("read outside sentinel"), b"keep");
    assert!(!outside.join(OWNERSHIP_MARKER).exists());
}

#[cfg(unix)]
#[test]
fn cleanup_rejects_a_symlink_even_when_it_is_located_under_the_runs_root() {
    use std::os::unix::fs::symlink;

    let (test_root, paths) = benchmark_paths("cleanup-symlink");
    let outside = test_root.join("outside");
    fs::create_dir_all(&outside).expect("create symlink target");
    let sentinel = outside.join("sentinel");
    fs::write(&sentinel, b"keep").expect("write symlink target sentinel");
    let link = paths.runs.join("linked-trial");
    symlink(&outside, &link).expect("create trial symlink");
    let mut ledger = CleanupLedger::default();

    let error = ledger
        .register(&paths, &link, trial_identity("run-1", "trial-1"))
        .expect_err("symlink must not be registered");
    assert!(matches!(error, CleanupError::Symlink(_)));
    assert_eq!(
        fs::read(sentinel).expect("read symlink target sentinel"),
        b"keep"
    );
    assert!(!outside.join(OWNERSHIP_MARKER).exists());
}

fn benchmark_paths(label: &str) -> (TestRoot, BenchmarkPaths) {
    let test_root = TestRoot::new(label);
    let repo = test_root.join("repository");
    create_fake_repository(&repo);
    let repo = repo.canonicalize().expect("canonical fake repository");
    let paths = BenchmarkPaths::initialize(&test_root.join("workspace"), &repo)
        .expect("initialize benchmark paths");
    (test_root, paths)
}

fn trial_identity(run_id: &str, trial_id: &str) -> OwnedIdentity {
    OwnedIdentity::RunTrial {
        run_id: run_id.to_owned(),
        trial_id: trial_id.to_owned(),
    }
}
