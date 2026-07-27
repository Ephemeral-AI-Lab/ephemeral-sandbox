use std::path::PathBuf;

use sandbox_runtime_mpla_poc::semantic::record::{NodeKind, NodeRecord, SemanticRecord};
use sandbox_runtime_mpla_poc::semantic::spool::BoundedSpool;
use uuid::Uuid;

#[test]
fn external_sort_uses_bounded_runs_fan_in_and_file_descriptors() {
    let temporary = Temporary::new("semantic-spool");
    let mut spool = BoundedSpool::new(temporary.path.join("runs"), 12 * 1024)
        .expect("test operation must succeed");
    for index in (0..3_000_u32).rev() {
        spool
            .push_record(node(index))
            .expect("test operation must succeed");
    }
    let sorted = spool.finish().expect("test operation must succeed");
    let stats = sorted.stats();
    assert_eq!(stats.records_in, 3_000);
    assert_eq!(stats.records_out, 3_000);
    assert!(stats.initial_runs > 1);
    assert!(stats.merge_passes > 0);
    assert!(stats.max_fan_in <= 8);
    assert!(stats.maximum_buffer_bytes <= 12 * 1024);
    assert!(stats.peak_open_files <= 9);

    let mut previous = None;
    let mut count = 0_u64;
    sorted
        .for_each(|key, payload| {
            assert!(previous
                .as_ref()
                .is_none_or(|value: &Vec<u8>| value.as_slice() < key));
            assert_eq!(
                SemanticRecord::decode(payload)
                    .expect("test operation must succeed")
                    .key_digest()
                    .expect("test operation must succeed")
                    .as_slice(),
                key
            );
            previous = Some(key.to_vec());
            count += 1;
            Ok(())
        })
        .expect("test operation must succeed");
    assert_eq!(count, 3_000);
}

#[test]
fn duplicate_canonical_keys_fail_closed() {
    let temporary = Temporary::new("semantic-spool-duplicates");
    let mut spool = BoundedSpool::new(temporary.path.join("runs"), 4 * 1024)
        .expect("test operation must succeed");
    spool
        .push_record(node(7))
        .expect("test operation must succeed");
    spool
        .push_record(node(7))
        .expect("test operation must succeed");
    assert!(spool.finish().is_err());
}

fn node(index: u32) -> SemanticRecord {
    SemanticRecord::Node(NodeRecord {
        path: format!("file-{index:08}").into_bytes(),
        kind: NodeKind::Regular,
        mode: 0o644,
        uid: 1000,
        gid: 1000,
        mtime_seconds: 1_700_000_000,
        mtime_nanoseconds: index,
        logical_size: 0,
        symlink_target: Vec::new(),
        device_major: 0,
        device_minor: 0,
    })
}

struct Temporary {
    path: PathBuf,
}

impl Temporary {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!("{label}-{}", Uuid::new_v4()));
        std::fs::create_dir(&path).expect("test operation must succeed");
        Self { path }
    }
}

impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
