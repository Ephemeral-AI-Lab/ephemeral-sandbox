use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use sandbox_runtime::WorkspaceSessionId;
use sandbox_runtime_namespace_execution::NamespaceExecutionId;
use sandbox_runtime_workspace::{
    WorkspaceScratchError, WorkspaceScratchLocator, EXECUTIONS_DIRECTORY, TRANSCRIPT_FILE,
};

static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

fn root(label: &str) -> PathBuf {
    std::env::current_dir()
        .expect("current directory")
        .join("target")
        .join(format!(
            "operation-workspace-execution-scratch-{}-{label}-{}",
            std::process::id(),
            NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
        ))
}

#[test]
fn workspace_execution_scratch_is_workspace_contained_and_private() {
    let root = root("layout");
    let workspace_id = WorkspaceSessionId("workspace-session-1".to_owned());
    let execution_id = NamespaceExecutionId("namespace_execution_1".to_owned());
    let locator = WorkspaceScratchLocator::new(root.clone()).expect("valid locator");

    let lease = locator
        .allocate_execution(&workspace_id, &execution_id)
        .expect("allocate workspace execution scratch");
    let expected = root
        .join(&workspace_id.0)
        .join(EXECUTIONS_DIRECTORY)
        .join(&execution_id.0)
        .join(TRANSCRIPT_FILE);

    assert_eq!(lease.transcript_path(), expected);
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
    lease.release().expect("release execution scratch");
    assert!(!execution_root.exists());
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn workspace_execution_scratch_rejects_legacy_or_escaping_ids() {
    let root = root("ids");
    let locator = WorkspaceScratchLocator::new(root.clone()).expect("valid locator");
    let workspace_id = WorkspaceSessionId("workspace-session-1".to_owned());

    for id in [
        "",
        ".",
        "..",
        "../namespace_execution_1",
        "namespace_execution_01",
        "namespace_execution_a",
    ] {
        let error = locator
            .allocate_execution(&workspace_id, &NamespaceExecutionId(id.to_owned()))
            .expect_err("invalid execution id must fail");
        assert!(matches!(error, WorkspaceScratchError::InvalidId { .. }));
    }
    assert!(!root.join("namespace_execution_1").exists());
}
