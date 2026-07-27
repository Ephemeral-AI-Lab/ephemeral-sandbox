use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use sandbox_runtime_mpla_poc::inventory::{capture_inventory, capture_stable_pair};
use sandbox_runtime_mpla_poc::{
    AllocationDescriptor, AllocationHandle, ManagedProcessTree, OperationId, PocError,
    SCHEMA_VERSION,
};

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!("mpla-poc-{label}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&path).expect("create test directory");
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn double_inventory_is_stable_and_detects_later_mutation() {
    let root = TestDirectory::new("inventory");
    let allocation = allocation_handle(&root.0);
    fs::create_dir_all(allocation.upper_dir.join("nested")).expect("create nested directory");
    fs::write(allocation.upper_dir.join("nested/file"), b"first").expect("write fixture");

    let (before, after) = capture_stable_pair(&allocation).expect("stable inventory");
    assert_eq!(before, after);
    assert_eq!(before.physical.file_count, 1);

    fs::write(allocation.upper_dir.join("nested/file"), b"second").expect("mutate fixture");
    let changed = capture_inventory(&allocation).expect("capture changed inventory");
    assert_ne!(before.inventory_sha256, changed.inventory_sha256);
}

#[cfg(unix)]
#[test]
fn managed_process_tree_executes_then_fences_admission() {
    let root = TestDirectory::new("process-tree");
    let mut tree = ManagedProcessTree::new(root.0.clone(), None);
    let args = vec!["-c".to_owned(), "printf ok > sentinel".to_owned()];
    let receipt = tree
        .run(
            std::path::Path::new("/bin/sh"),
            &args,
            Duration::from_secs(2),
        )
        .expect("run managed command");
    assert!(receipt.success);
    assert_eq!(
        fs::read_to_string(root.0.join("sentinel")).expect("read sentinel"),
        "ok"
    );

    tree.fence();
    let error = tree
        .run(
            std::path::Path::new("/bin/sh"),
            &args,
            Duration::from_secs(2),
        )
        .expect_err("fenced admission must fail");
    assert!(matches!(error, PocError::Integrity(_)));
    tree.stop_kill_reap().expect("clean process groups");
}

fn allocation_handle(root: &std::path::Path) -> AllocationHandle {
    let allocation_root = root.join("allocations").join("aa").join("fixture");
    let upper_dir = allocation_root.join("upper");
    let work_dir = allocation_root.join("work");
    let owner_dir = allocation_root.join("owner");
    for path in [&upper_dir, &work_dir, &owner_dir] {
        fs::create_dir_all(path).expect("create allocation path");
    }
    AllocationHandle {
        descriptor: AllocationDescriptor {
            schema_version: SCHEMA_VERSION,
            allocation_id: sandbox_runtime_mpla_poc::AllocationId::from_string("fixture"),
            created_by_operation: OperationId::from_string("create-fixture"),
            created_unix_ms: 1,
        },
        allocation_root,
        upper_dir,
        work_dir,
        owner_dir,
    }
}
