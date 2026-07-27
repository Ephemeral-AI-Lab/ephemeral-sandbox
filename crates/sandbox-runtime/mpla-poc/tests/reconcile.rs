use std::fs;
use std::path::PathBuf;

use sandbox_runtime_mpla_poc::reconcile::reconcile;
use sandbox_runtime_mpla_poc::{LeakCounts, StorageCategoryRoot};
use uuid::Uuid;

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("mpla-reconcile-{}", Uuid::new_v4()));
        fs::create_dir(&path).expect("test directory");
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn physical_union_deduplicates_hardlinks_and_balances_categories() {
    let temp = TestDirectory::new();
    let payload = temp.0.join("payload");
    let control = temp.0.join("control");
    fs::create_dir(&payload).expect("payload");
    fs::create_dir(&control).expect("control");
    fs::write(payload.join("data"), vec![7_u8; 8192]).expect("payload data");
    fs::hard_link(payload.join("data"), control.join("witness")).expect("hardlink witness");
    fs::write(control.join("metadata"), b"control").expect("control metadata");

    let receipt = reconcile(
        &temp.0,
        &[
            StorageCategoryRoot {
                category: "payload".to_owned(),
                root: payload,
                recursive: true,
            },
            StorageCategoryRoot {
                category: "control".to_owned(),
                root: control,
                recursive: true,
            },
            StorageCategoryRoot {
                category: "scope-root".to_owned(),
                root: temp.0.clone(),
                recursive: false,
            },
        ],
        LeakCounts::default(),
    )
    .expect("reconcile");
    assert!(receipt.balanced);
    assert_eq!(receipt.unexplained_allocated_bytes, 0);
    assert_eq!(receipt.unexplained_inodes, 0);
    assert_eq!(
        receipt.physical_union_allocated_bytes,
        receipt.classified_allocated_bytes
    );
}

#[test]
fn unexplained_object_and_leak_prevent_balance() {
    let temp = TestDirectory::new();
    let known = temp.0.join("known");
    fs::create_dir(&known).expect("known");
    fs::write(temp.0.join("unknown"), b"unclassified").expect("unknown");
    let receipt = reconcile(
        &temp.0,
        &[StorageCategoryRoot {
            category: "known".to_owned(),
            root: known,
            recursive: true,
        }],
        LeakCounts {
            active_mounts: 1,
            ..LeakCounts::default()
        },
    )
    .expect("reconcile");
    assert!(!receipt.balanced);
    assert!(receipt.unexplained_inodes > 0);
    assert!(!receipt.unexplained_paths.is_empty());
}
