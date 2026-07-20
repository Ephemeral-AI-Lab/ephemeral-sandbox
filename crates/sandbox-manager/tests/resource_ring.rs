use std::fs::{self, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sandbox_manager::{
    ResourceRingStore, ResourceSample, SandboxId, SandboxResourceMetrics, SandboxStore,
    MAX_RESOURCE_RESPONSE_RECORDS, RESOURCE_RECORD_BYTES, RESOURCE_RING_BYTES,
};

const HEADER_BYTES: usize = 64;
const CAPACITY: usize = (RESOURCE_RING_BYTES as usize - HEADER_BYTES) / RESOURCE_RECORD_BYTES;

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "sandbox-resource-ring-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create temp root");
        Self(root)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn id(value: &str) -> SandboxId {
    SandboxId::new(value).expect("valid sandbox id")
}

fn sample(timestamp: i64) -> ResourceSample {
    ResourceSample {
        sampled_at_unix_ms: timestamp,
        metrics: SandboxResourceMetrics {
            cpu_usage_usec: Some(timestamp as u64),
            memory_current_bytes: Some(timestamp as u64 + 1),
            memory_limit_bytes: Some(timestamp as u64 + 2),
            io_read_bytes: Some(timestamp as u64 + 3),
            io_write_bytes: Some(timestamp as u64 + 4),
        },
    }
}

fn overwrite(path: &Path, offset: u64, bytes: &[u8]) {
    let mut file = OpenOptions::new()
        .write(true)
        .open(path)
        .expect("open ring for fault injection");
    file.seek(SeekFrom::Start(offset)).expect("seek ring");
    file.write_all(bytes).expect("overwrite ring");
    file.sync_data().expect("sync injected fault");
}

#[test]
fn ring_root_is_derived_from_registry_parent() {
    let temp = TempRoot::new("root");
    let registry = temp.path().join("manager-state/registry.json");
    let store = SandboxStore::load(registry).expect("load empty registry");
    let ring = ResourceRingStore::for_store(&store);

    assert_eq!(
        ring.root(),
        temp.path().join("manager-state/observability-resources")
    );
}

#[test]
fn ring_wraps_one_hundred_times_without_growth_and_recovers_after_restart() {
    let temp = TempRoot::new("wrap");
    let sandbox = id("wrap-sandbox");
    let isolated = id("isolated-sandbox");
    let ring = ResourceRingStore::new(temp.path().join("rings"));
    let writes = CAPACITY * 100 + 7;

    for timestamp in 0..writes {
        ring.append(&sandbox, sample(timestamp as i64))
            .expect("append resource sample");
    }
    ring.append(&isolated, sample(999_999))
        .expect("append isolated sample");

    let path = ring.path(&sandbox);
    assert_eq!(
        fs::metadata(&path).expect("ring metadata").len(),
        RESOURCE_RING_BYTES
    );
    let header = fs::read(&path).expect("read fixed ring");
    assert_eq!(header.len(), RESOURCE_RING_BYTES as usize);
    assert_eq!(
        u32::from_le_bytes(header[28..32].try_into().expect("count bytes")) as usize,
        CAPACITY
    );

    let read = ring.read_window(&sandbox, i64::MAX);
    assert_eq!(read.error, None);
    assert_eq!(read.samples.len(), MAX_RESOURCE_RESPONSE_RECORDS);
    assert!(read
        .samples
        .windows(2)
        .all(|pair| pair[0].sampled_at_unix_ms < pair[1].sampled_at_unix_ms));
    assert_eq!(
        read.samples.last().map(|entry| entry.sampled_at_unix_ms),
        Some((writes - 1) as i64)
    );

    drop(ring);
    let reopened = ResourceRingStore::new(temp.path().join("rings"));
    assert_eq!(
        reopened
            .read_window(&sandbox, i64::MAX)
            .samples
            .last()
            .map(|entry| entry.sampled_at_unix_ms),
        Some((writes - 1) as i64)
    );
    assert_eq!(
        reopened.read_window(&isolated, i64::MAX).samples,
        vec![sample(999_999)]
    );
    assert_eq!(
        reopened.read_latest(&sandbox),
        sandbox_manager::ResourceRingRead {
            samples: vec![sample((writes - 1) as i64)],
            error: None,
        }
    );
    assert_eq!(
        reopened.read_latest(&isolated).samples,
        vec![sample(999_999)]
    );
}

#[test]
fn latest_read_returns_one_record_without_scanning_or_repairing_old_history() {
    let temp = TempRoot::new("latest");
    let sandbox = id("latest-sandbox");
    let ring = ResourceRingStore::new(temp.path().join("rings"));
    for timestamp in 1..=4 {
        ring.append(&sandbox, sample(timestamp))
            .expect("append resource sample");
    }
    let path = ring.path(&sandbox);
    overwrite(
        &path,
        HEADER_BYTES as u64,
        &[0x5a; RESOURCE_RECORD_BYTES / 2],
    );

    let current = ring.read_latest(&sandbox);
    assert_eq!(current.error, None);
    assert_eq!(current.samples, vec![sample(4)]);
    assert_eq!(
        fs::metadata(path).expect("ring remains fixed-size").len(),
        RESOURCE_RING_BYTES
    );
}

#[test]
fn latest_read_uses_memory_cache_and_remove_invalidates_it() {
    let temp = TempRoot::new("latest-cache");
    let sandbox = id("latest-cache-sandbox");
    let ring = ResourceRingStore::new(temp.path().join("rings"));
    let latest = sample(42);
    ring.append(&sandbox, latest)
        .expect("append cached resource sample");

    fs::remove_file(ring.path(&sandbox)).expect("remove ring behind cache");
    assert_eq!(ring.read_latest(&sandbox).samples, vec![latest]);

    ring.remove(&sandbox).expect("invalidate cached sample");
    let missing = ring.read_latest(&sandbox);
    assert!(missing.samples.is_empty());
    assert_eq!(
        missing.error.as_deref(),
        Some("resource ring is not available yet")
    );
}

#[test]
fn ring_recovers_from_torn_header_unsupported_version_and_partial_length() {
    let temp = TempRoot::new("header-recovery");
    let sandbox = id("header-sandbox");
    let ring = ResourceRingStore::new(temp.path().join("rings"));

    for timestamp in 1..=3 {
        ring.append(&sandbox, sample(timestamp))
            .expect("append initial sample");
    }
    let path = ring.path(&sandbox);

    overwrite(&path, 0, b"TORN");
    let torn = ring.read_window(&sandbox, i64::MAX);
    assert!(torn.samples.is_empty());
    assert!(torn
        .error
        .as_deref()
        .is_some_and(|error| error.contains("recreated")));
    assert_eq!(
        fs::metadata(&path).expect("recreated metadata").len(),
        RESOURCE_RING_BYTES
    );

    ring.append(&sandbox, sample(4))
        .expect("append after header recovery");
    overwrite(&path, 8, &99_u32.to_le_bytes());
    let unsupported = ring.read_window(&sandbox, i64::MAX);
    assert!(unsupported.samples.is_empty());
    assert!(unsupported
        .error
        .as_deref()
        .is_some_and(|error| error.contains("recreated")));

    fs::write(&path, [0_u8; 17]).expect("write partial ring");
    let partial = ring.read_window(&sandbox, i64::MAX);
    assert!(partial.samples.is_empty());
    assert!(partial.error.is_some());
    assert_eq!(
        fs::metadata(&path)
            .expect("partial recovery metadata")
            .len(),
        RESOURCE_RING_BYTES
    );
}

#[test]
fn ring_discards_torn_newest_record_and_teardown_deletes_file() {
    let temp = TempRoot::new("record-recovery");
    let sandbox = id("record-sandbox");
    let ring = ResourceRingStore::new(temp.path().join("rings"));

    for timestamp in 0..CAPACITY {
        ring.append(&sandbox, sample(timestamp as i64))
            .expect("fill resource ring");
    }
    let path = ring.path(&sandbox);
    overwrite(
        &path,
        HEADER_BYTES as u64,
        &[0x5a; RESOURCE_RECORD_BYTES / 2],
    );

    let recovered = ring.read_window(&sandbox, i64::MAX);
    assert!(recovered.error.is_some());
    assert_eq!(recovered.samples.len(), MAX_RESOURCE_RESPONSE_RECORDS);
    assert_eq!(
        recovered
            .samples
            .last()
            .map(|entry| entry.sampled_at_unix_ms),
        Some((CAPACITY - 1) as i64)
    );
    assert!(recovered
        .samples
        .windows(2)
        .all(|pair| pair[0].sampled_at_unix_ms < pair[1].sampled_at_unix_ms));
    assert!(ring.read_window(&sandbox, i64::MAX).samples.is_empty());

    ring.append(&sandbox, sample(42))
        .expect("append after record recovery");
    ring.remove(&sandbox).expect("remove resource ring");
    assert!(!path.exists());
    ring.remove(&sandbox).expect("repeat removal is idempotent");
}
