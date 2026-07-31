use std::fs::{File, FileTimes};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::PathBuf;
use std::time::{Duration, UNIX_EPOCH};

use sandbox_runtime_mpla_poc::projection::{select_exact, MAX_RECENT_DELTAS};
use sandbox_runtime_mpla_poc::{
    inherit_projection_root_metadata, AllocationId, AttributionRootId, CanonicalRootPair,
    ProjectionRecipe, RootId, SCHEMA_VERSION,
};
use uuid::Uuid;

fn roots() -> CanonicalRootPair {
    CanonicalRootPair {
        root_id: RootId::parse("11".repeat(32)).expect("root"),
        attribution_root_id: AttributionRootId::parse("22".repeat(32)).expect("attribution"),
    }
}

#[test]
fn exact_projection_is_zero_build_and_bounded() {
    let recipe = ProjectionRecipe {
        schema_version: SCHEMA_VERSION,
        roots: roots(),
        base_allocation_id: AllocationId::new(),
        net_delta_carrier_id: Some(AllocationId::new()),
        recent_delta_ids: (0..MAX_RECENT_DELTAS)
            .map(|_| AllocationId::new())
            .collect(),
    };
    let receipt = select_exact(&recipe).expect("exact selection");
    assert_eq!(receipt.kernel_lower_count, 10);
    assert_eq!(receipt.reconstructed_payload_bytes, 0);
    assert_eq!(receipt.hydrated_payload_bytes, 0);
    assert_eq!(receipt.base_bytes_copied, 0);
    assert!(!receipt.projection_built_during_activation);
}

#[test]
fn projection_rejects_depth_and_aliasing() {
    let base = AllocationId::new();
    let too_deep = ProjectionRecipe {
        schema_version: SCHEMA_VERSION,
        roots: roots(),
        base_allocation_id: base.clone(),
        net_delta_carrier_id: None,
        recent_delta_ids: (0..=MAX_RECENT_DELTAS)
            .map(|_| AllocationId::new())
            .collect(),
    };
    assert!(too_deep.validate().is_err());

    let aliased = ProjectionRecipe {
        schema_version: SCHEMA_VERSION,
        roots: roots(),
        base_allocation_id: base.clone(),
        net_delta_carrier_id: None,
        recent_delta_ids: vec![base],
    };
    assert!(aliased.validate().is_err());
}

#[test]
fn fresh_upper_inherits_projection_root_semantics() {
    let temporary = Temporary::new("activation-root");
    let source = temporary.path.join("source");
    let target = temporary.path.join("target");
    std::fs::create_dir(&source).expect("source root");
    std::fs::create_dir(&target).expect("target root");
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o751))
        .expect("source permissions");
    File::open(&source)
        .expect("open source root")
        .set_times(
            FileTimes::new()
                .set_accessed(UNIX_EPOCH + Duration::from_secs(1_700_000_001))
                .set_modified(UNIX_EPOCH + Duration::from_secs(1_700_000_002)),
        )
        .expect("source timestamps");
    File::open(&target)
        .expect("open target root")
        .set_times(
            FileTimes::new()
                .set_accessed(UNIX_EPOCH + Duration::from_secs(1_800_000_001))
                .set_modified(UNIX_EPOCH + Duration::from_secs(1_800_000_002)),
        )
        .expect("target timestamps");

    inherit_projection_root_metadata(&source, &target).expect("inherit root metadata");

    let source_metadata = std::fs::symlink_metadata(&source).expect("source metadata");
    let target_metadata = std::fs::symlink_metadata(&target).expect("target metadata");
    assert_eq!(
        target_metadata.permissions().mode() & 0o7777,
        source_metadata.permissions().mode() & 0o7777
    );
    assert_eq!(target_metadata.uid(), source_metadata.uid());
    assert_eq!(target_metadata.gid(), source_metadata.gid());
    assert_eq!(target_metadata.atime(), source_metadata.atime());
    assert_eq!(target_metadata.atime_nsec(), source_metadata.atime_nsec());
    assert_eq!(target_metadata.mtime(), source_metadata.mtime());
    assert_eq!(target_metadata.mtime_nsec(), source_metadata.mtime_nsec());
}

struct Temporary {
    path: PathBuf,
}

impl Temporary {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!("{label}-{}", Uuid::new_v4()));
        std::fs::create_dir(&path).expect("temporary directory");
        Self { path }
    }
}

impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
