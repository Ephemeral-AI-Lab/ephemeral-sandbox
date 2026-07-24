use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use sandbox_runtime_namespace_execution::NamespaceExecutionId;
use sandbox_runtime_workspace::{
    ExecutionScratchRoute, LegacyExecutionScratchLocator, WorkspaceScratchError,
    WorkspaceScratchLocator, WorkspaceSessionId, EXECUTIONS_DIRECTORY, TRANSCRIPT_FILE,
};

static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

fn root(label: &str) -> PathBuf {
    std::env::current_dir()
        .expect("current directory")
        .join("target")
        .join(format!(
            "workspace-execution-scratch-{}-{label}-{}",
            std::process::id(),
            NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
        ))
}

fn ids() -> (WorkspaceSessionId, NamespaceExecutionId) {
    (
        WorkspaceSessionId("workspace-session-1".to_owned()),
        NamespaceExecutionId("namespace_execution_1".to_owned()),
    )
}

#[test]
fn execution_transcript_uses_the_workspace_owned_layout_and_private_modes() {
    let root = root("layout");
    let locator = WorkspaceScratchLocator::new(root.clone()).expect("valid locator");
    let (workspace_id, execution_id) = ids();

    let lease = locator
        .allocate_execution(&workspace_id, &execution_id)
        .expect("allocate execution");

    assert_eq!(
        lease.transcript_path(),
        root.join(&workspace_id.0)
            .join(EXECUTIONS_DIRECTORY)
            .join(&execution_id.0)
            .join(TRANSCRIPT_FILE)
    );
    assert_eq!(
        std::fs::metadata(lease.root())
            .expect("execution metadata")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(lease.transcript_path())
            .expect("transcript metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );

    let execution_root = lease.root().to_path_buf();
    lease.release().expect("explicit release");
    assert!(!execution_root.exists());
    assert!(root
        .join(&workspace_id.0)
        .join(EXECUTIONS_DIRECTORY)
        .is_dir());
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn legacy_compatibility_locator_uses_only_the_global_execution_leaf() {
    let root = root("legacy-compat");
    let locator = LegacyExecutionScratchLocator::new(root.clone()).expect("valid locator");
    let (_, execution_id) = ids();

    let lease = locator
        .allocate_execution(&execution_id)
        .expect("allocate legacy execution");

    assert_eq!(
        lease.transcript_path(),
        root.join(&execution_id.0).join(TRANSCRIPT_FILE)
    );
    assert_eq!(lease.route(), ExecutionScratchRoute::LegacyCompat);
    let execution_root = lease.root().to_path_buf();
    lease.release().expect("release legacy execution");
    assert!(!execution_root.exists());
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn locator_rejects_malformed_identifiers_before_creating_children() {
    let root = root("ids");
    let locator = WorkspaceScratchLocator::new(root.clone()).expect("valid locator");
    for invalid in ["", ".", "..", "../escape", "nested/id", "white space"] {
        let error = locator
            .allocate_execution(
                &WorkspaceSessionId(invalid.to_owned()),
                &NamespaceExecutionId("namespace_execution_1".to_owned()),
            )
            .expect_err("invalid workspace id must fail");
        assert!(matches!(error, WorkspaceScratchError::InvalidId { .. }));
    }
    let error = locator
        .allocate_execution(
            &WorkspaceSessionId("workspace-1".to_owned()),
            &NamespaceExecutionId("../escape".to_owned()),
        )
        .expect_err("invalid execution id must fail");
    assert!(matches!(error, WorkspaceScratchError::InvalidId { .. }));
    for invalid in [
        "execution_1",
        "namespace_execution_",
        "namespace_execution_01",
        "namespace_execution_a",
    ] {
        let error = locator
            .allocate_execution(
                &WorkspaceSessionId("workspace-1".to_owned()),
                &NamespaceExecutionId(invalid.to_owned()),
            )
            .expect_err("non-canonical execution id must fail");
        assert!(matches!(error, WorkspaceScratchError::InvalidId { .. }));
    }
    assert!(!root.join("escape").exists());
}

#[test]
fn existing_execution_leaf_fails_closed_without_reusing_transcript_bytes() {
    let root = root("collision");
    let locator = WorkspaceScratchLocator::new(root.clone()).expect("valid locator");
    let (workspace_id, execution_id) = ids();
    let first = locator
        .allocate_execution(&workspace_id, &execution_id)
        .expect("first allocation");
    std::fs::write(first.transcript_path(), b"owned").expect("seed transcript");

    let error = locator
        .allocate_execution(&workspace_id, &execution_id)
        .expect_err("collision must fail");
    assert!(matches!(
        error,
        WorkspaceScratchError::ExecutionCollision(_)
    ));
    assert_eq!(
        std::fs::read(first.transcript_path()).expect("read transcript"),
        b"owned"
    );
    drop(first);
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn session_symlink_is_rejected_without_following_it() {
    let scratch_root = root("symlink");
    let outside = root("outside");
    std::fs::create_dir_all(&scratch_root).expect("create root");
    std::fs::create_dir_all(&outside).expect("create outside");
    symlink(&outside, scratch_root.join("workspace-session-1")).expect("create symlink");
    let locator = WorkspaceScratchLocator::new(scratch_root.clone()).expect("valid locator");
    let (_, execution_id) = ids();

    let error = locator
        .allocate_execution(
            &WorkspaceSessionId("workspace-session-1".to_owned()),
            &execution_id,
        )
        .expect_err("symlink must fail");
    assert!(matches!(error, WorkspaceScratchError::Symlink(_)));
    assert!(std::fs::read_dir(&outside)
        .expect("read outside")
        .next()
        .is_none());
    std::fs::remove_file(scratch_root.join("workspace-session-1")).expect("remove symlink");
    std::fs::remove_dir_all(scratch_root).expect("cleanup root");
    std::fs::remove_dir_all(outside).expect("cleanup outside");
}

#[test]
fn configured_root_symlink_is_rejected() {
    let target = root("root-symlink-target");
    let link = root("root-symlink-link");
    std::fs::create_dir_all(&target).expect("create target");
    symlink(&target, &link).expect("create root symlink");

    let error =
        WorkspaceScratchLocator::new(link.clone()).expect_err("configured root symlink must fail");

    assert!(matches!(error, WorkspaceScratchError::Symlink(path) if path == link));
    std::fs::remove_file(link).expect("remove root symlink");
    std::fs::remove_dir_all(target).expect("cleanup target");
}
