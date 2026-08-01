use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{symlink, FileExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::time::{Duration, Instant};

use sandbox_runtime_mpla_poc::locator::{
    ForwardLocatorEntry, LocatorDelta, LocatorExtent, LocatorReplacement, LocatorStore,
    PayloadRootId, ReverseLocatorEntry, SealedLocatorStore,
};
use sandbox_runtime_mpla_poc::ref_store::{PairedRefStore, RefCommitOutcome, SealedPairedRefStore};
use sandbox_runtime_mpla_poc::{
    AttributionInput, AttributionRootId, CanonicalDurabilityReceipt, CanonicalRootPair,
    LocatorGeneration, LocatorRefCandidate, NamedFaultInjector, NamedFaultPoint, OperationId,
    PairedRefValue, PocError, PublicationId, RefSequence, RootId, SCHEMA_VERSION,
};
use sha2::{Digest, Sha256};
use uuid::Uuid;

const REF_JOURNAL_DATA_BYTES: u64 = 64 * 1024 * 1024;
const REF_CURSOR_SLOT_BYTES: u64 = 4096;
const REF_JOURNAL_TOTAL_BYTES: u64 = REF_JOURNAL_DATA_BYTES + 2 * REF_CURSOR_SLOT_BYTES;
const REF_LAYOUT_V2: &[u8] =
    b"mpla-poc-paired-ref-layout-v2\njournal-preallocated-bytes=67108864\n";
const REF_LAYOUT_V3: &[u8] = b"mpla-poc-paired-ref-layout-v3\njournal-data-bytes=67108864\ncursor-slot-bytes=4096\ncursor-slots=2\njournal-total-bytes=67117056\n";
const CHILD_COMMIT_ROOT_ENV: &str = "MPLA_PAIRED_REF_CHILD_COMMIT_ROOT";
const CHILD_SEALED_LOCK_PATH_ENV: &str = "MPLA_PAIRED_REF_CHILD_SEALED_LOCK_PATH";
const CHILD_SEALED_LOCK_BLOCKED_ENV: &str = "MPLA_PAIRED_REF_CHILD_SEALED_LOCK_BLOCKED";
const CHILD_SEALED_OPEN_ROOT_ENV: &str = "MPLA_PAIRED_REF_CHILD_SEALED_OPEN_ROOT";

struct TestRoot {
    path: PathBuf,
}

impl TestRoot {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!("mpla-m1-{label}-{}", Uuid::new_v4()));
        std::fs::create_dir(&path).expect("create test root");
        Self { path }
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[test]
fn new_paired_ref_journal_remains_logically_empty_after_provisioning() {
    let root = TestRoot::new("preallocated-paired-ref");
    let refs_root = root.path.join("refs");
    let store = PairedRefStore::open(&refs_root).expect("create paired ref store");
    let metadata = std::fs::metadata(refs_root.join("JOURNAL")).expect("stat journal");

    assert_eq!(metadata.len(), REF_JOURNAL_TOTAL_BYTES);
    assert!(store
        .read("fixture-depth-1")
        .expect("read empty journal")
        .is_none());
    assert!(refs_root.join("LAYOUT").is_file());
    assert_eq!(
        std::fs::read(refs_root.join("LAYOUT")).expect("read layout marker"),
        REF_LAYOUT_V3
    );
    let sealed = SealedPairedRefStore::open(&refs_root).expect("open sealed v3 journal");
    let layout = sealed
        .require_v3_layout()
        .expect("require sealed v3 journal");
    assert_eq!(layout.format, "mpla-poc-paired-ref-layout-v3");
    assert_eq!(layout.journal_data_bytes, REF_JOURNAL_DATA_BYTES);
    assert_eq!(layout.journal_total_bytes, REF_JOURNAL_TOTAL_BYTES);
    assert_eq!(layout.cursor_generation, 1);
    assert_eq!(layout.cursor_slot, 0);
    assert_eq!(layout.logical_end, 0);
    assert_eq!(layout.record_count, 0);
    assert_eq!(layout.last_record_hash, None);
}

#[test]
fn partial_paired_ref_layout_is_recovered_before_use() {
    let root = TestRoot::new("partial-paired-ref");
    let refs_root = root.path.join("refs");
    PairedRefStore::open(&refs_root).expect("create paired ref store");
    std::fs::remove_file(refs_root.join("LAYOUT")).expect("remove completion marker");

    let reopened = PairedRefStore::open(&refs_root).expect("recover partial paired ref store");

    let recovered_length = std::fs::metadata(refs_root.join("JOURNAL"))
        .expect("stat recovered journal")
        .len();
    assert_eq!(recovered_length, REF_JOURNAL_TOTAL_BYTES);
    assert!(refs_root.join("LAYOUT").is_file());
    assert!(reopened
        .read("fixture-depth-1")
        .expect("read recovered journal")
        .is_none());
}

#[cfg(target_os = "linux")]
#[test]
fn new_paired_ref_journal_reserves_blocks_before_first_commit() {
    let root = TestRoot::new("allocated-paired-ref");
    let refs_root = root.path.join("refs");
    PairedRefStore::open(&refs_root).expect("create paired ref store");
    let journal_path = refs_root.join("JOURNAL");
    let metadata = std::fs::metadata(&journal_path).expect("stat journal");

    assert_eq!(metadata.len(), REF_JOURNAL_TOTAL_BYTES);
    assert!(
        metadata.blocks() * 512 >= REF_JOURNAL_TOTAL_BYTES,
        "the complete fixed journal must be physically allocated before its first commit"
    );
    let journal = File::open(journal_path).expect("open journal");
    for offset in [0, REF_JOURNAL_DATA_BYTES / 2, REF_JOURNAL_DATA_BYTES - 1] {
        let mut byte = [1_u8; 1];
        journal
            .read_exact_at(&mut byte, offset)
            .expect("read unwritten journal extent");
        assert_eq!(byte, [0], "fresh preallocated extent must read as zero");
    }
}

#[cfg(unix)]
#[test]
fn sealed_paired_ref_store_reads_without_writable_layout() {
    use std::os::unix::fs::PermissionsExt;

    let root = TestRoot::new("sealed-paired-ref-read");
    let refs_root = root.path.join("refs");
    let store = PairedRefStore::open(&refs_root).expect("create paired ref store");
    drop(store);
    for path in [
        refs_root.join("LOCK"),
        refs_root.join("JOURNAL"),
        refs_root.join("LAYOUT"),
    ] {
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400))
            .expect("seal existing paired ref file");
    }
    std::fs::set_permissions(&refs_root, std::fs::Permissions::from_mode(0o500))
        .expect("seal paired ref directory");
    let store = SealedPairedRefStore::open(&refs_root).expect("open sealed paired ref store");

    assert!(store
        .read("fixture-depth-1")
        .expect("read sealed paired ref store")
        .is_none());
    drop(store);
    std::fs::set_permissions(&refs_root, std::fs::Permissions::from_mode(0o700))
        .expect("unseal paired ref directory for cleanup");
}

#[test]
fn existing_locator_open_never_repairs_an_incomplete_layout() {
    let root = TestRoot::new("existing-locator-no-repair");
    let locator_root = root.path.join("locators");
    std::fs::create_dir(&locator_root).expect("create incomplete locator root");

    assert!(SealedLocatorStore::open(&locator_root).is_err());
    assert_eq!(
        std::fs::read_dir(&locator_root)
            .expect("read unchanged incomplete locator root")
            .count(),
        0
    );
}

#[test]
fn sealed_reader_resolves_v3_without_mutating_layout() {
    let (root, locator_store, refs, expected) = committed_ref_fixture("sealed-reader-v3");
    let locators_root = root.path.join("locators");
    let refs_root = root.path.join("refs");
    drop(locator_store);
    drop(refs);
    let before = ref_layout_snapshot(&refs_root);

    let sealed_locator = SealedLocatorStore::open(&locators_root).expect("open sealed v3 locator");
    let sealed = SealedPairedRefStore::open(&refs_root).expect("open sealed v3 refs");

    assert_eq!(
        sealed.root(),
        std::fs::canonicalize(&refs_root).expect("canonical ref fixture root")
    );
    assert_eq!(
        sealed
            .read("main")
            .expect("read sealed v3 ref")
            .expect("sealed v3 head"),
        expected
    );
    assert_eq!(
        sealed
            .read_resolved("main", &sealed_locator)
            .expect("resolve sealed v3 ref")
            .expect("resolved sealed v3 head")
            .value,
        expected
    );
    drop(sealed);
    assert_eq!(ref_layout_snapshot(&refs_root), before);
}

#[test]
fn sealed_locator_bypasses_warm_cache_and_rejects_corrupt_generation() {
    let (root, locator_store, refs, _expected) =
        committed_ref_fixture("sealed-locator-uncached-corruption");
    drop(refs);
    let selected = locator_store
        .selected()
        .expect("warm ordinary locator cache")
        .expect("selected locator generation");
    let forward_path = root
        .path
        .join("locators")
        .join("generations")
        .join(format!("{:020}", selected.receipt.generation.get()))
        .join("forward.json");
    std::fs::write(&forward_path, b"{}").expect("corrupt locator generation after cache warmup");
    assert!(
        locator_store
            .selected()
            .expect("ordinary cache remains warm")
            .is_some(),
        "test precondition requires ordinary cached state to mask the disk corruption"
    );
    let corrupt_bytes = std::fs::read(&forward_path).expect("read corrupt locator file");

    assert!(SealedLocatorStore::open(root.path.join("locators")).is_err());
    assert_eq!(
        std::fs::read(&forward_path).expect("read unchanged corrupt locator file"),
        corrupt_bytes
    );
}

#[test]
fn sealed_locator_rejects_orphan_generation_without_removing_it() {
    let (root, locator_store, refs, _expected) =
        committed_ref_fixture("sealed-locator-orphan-generation");
    drop(refs);
    let selected = locator_store
        .selected()
        .expect("select locator generation")
        .expect("selected locator generation");
    drop(locator_store);
    let generations_root = root.path.join("locators").join("generations");
    let selected_dir = generations_root.join(format!("{:020}", selected.receipt.generation.get()));
    let orphan_dir =
        generations_root.join(format!("{:020}", selected.receipt.generation.get() + 1));
    std::fs::create_dir(&orphan_dir).expect("create orphan generation");
    for name in ["MANIFEST.json", "forward.json", "reverse.json"] {
        std::fs::copy(selected_dir.join(name), orphan_dir.join(name))
            .expect("copy orphan generation file");
    }

    assert!(SealedLocatorStore::open(root.path.join("locators")).is_err());
    assert!(
        orphan_dir.is_dir(),
        "sealed open must not repair the orphan"
    );
}

#[test]
fn sealed_locator_retains_shared_lock_for_its_lifetime() {
    let (root, locator_store, refs, _expected) =
        committed_ref_fixture("sealed-locator-retained-lock");
    let locators_root = root.path.join("locators");
    drop(refs);
    drop(locator_store);
    let sealed =
        SealedLocatorStore::open(&locators_root).expect("open sealed locator with shared lock");
    let cloned = sealed.clone();

    assert!(run_sealed_lock_child(&locators_root, true).success());
    drop(sealed);
    assert!(run_sealed_lock_child(&locators_root, true).success());
    drop(cloned);
    assert!(run_sealed_lock_child(&locators_root, false).success());
}

#[test]
fn sealed_reader_reads_v2_without_migrating_or_creating_temps() {
    let (root, _locator_store, refs, expected) = committed_ref_fixture("sealed-reader-v2");
    let refs_root = root.path.join("refs");
    drop(refs);
    rewrite_ref_fixture_as_legacy(&refs_root, Some(REF_LAYOUT_V2), true);
    let before = ref_layout_snapshot(&refs_root);

    let sealed = SealedPairedRefStore::open(&refs_root).expect("open sealed v2 refs");

    assert_eq!(
        sealed
            .read("main")
            .expect("read sealed v2 ref")
            .expect("sealed v2 head"),
        expected
    );
    assert!(
        sealed.require_v3_layout().is_err(),
        "a legacy sealed reader must not claim the v3 cursor layout"
    );
    drop(sealed);
    assert_eq!(ref_layout_snapshot(&refs_root), before);
    assert!(!refs_root.join("JOURNAL.v3.tmp").exists());
    assert!(!refs_root.join("LAYOUT.v3.tmp").exists());
}

#[test]
fn sealed_reader_reads_missing_layout_legacy_without_creating_marker() {
    let (root, _locator_store, refs, expected) =
        committed_ref_fixture("sealed-reader-missing-layout");
    let refs_root = root.path.join("refs");
    drop(refs);
    rewrite_ref_fixture_as_legacy(&refs_root, None, false);
    let before = ref_layout_snapshot(&refs_root);

    let sealed = SealedPairedRefStore::open(&refs_root).expect("open markerless sealed refs");

    assert_eq!(
        sealed
            .read("main")
            .expect("read markerless sealed ref")
            .expect("markerless sealed head"),
        expected
    );
    drop(sealed);
    assert_eq!(ref_layout_snapshot(&refs_root), before);
    assert!(!refs_root.join("LAYOUT").exists());
}

#[test]
fn sealed_reader_rejects_corrupt_legacy_without_mutating_layout() {
    let (root, _locator_store, refs, _expected) =
        committed_ref_fixture("sealed-reader-corrupt-legacy");
    let refs_root = root.path.join("refs");
    drop(refs);
    let logical_end = rewrite_ref_fixture_as_legacy(&refs_root, Some(REF_LAYOUT_V2), false);
    write_journal_bytes(&refs_root.join("JOURNAL"), logical_end - 1, &[0xff]);
    let before = ref_layout_snapshot(&refs_root);

    assert!(SealedPairedRefStore::open(&refs_root).is_err());
    assert_eq!(ref_layout_snapshot(&refs_root), before);
}

#[test]
fn sealed_reader_rejects_nonzero_preallocated_legacy_tail() {
    let (root, _locator_store, refs, _expected) =
        committed_ref_fixture("sealed-reader-corrupt-legacy-tail");
    let refs_root = root.path.join("refs");
    drop(refs);
    rewrite_ref_fixture_as_legacy(&refs_root, Some(REF_LAYOUT_V2), true);
    write_journal_bytes(
        &refs_root.join("JOURNAL"),
        REF_JOURNAL_DATA_BYTES - 1,
        &[0x5a],
    );
    let before = ref_layout_snapshot(&refs_root);

    let error = match SealedPairedRefStore::open(&refs_root) {
        Ok(_) => panic!("nonzero legacy tail was accepted"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        PocError::Integrity(ref message)
            if message == "sealed legacy paired ref journal has a nonzero preallocated tail"
    ));
    assert_eq!(ref_layout_snapshot(&refs_root), before);
}

#[test]
fn sealed_reader_rejects_torn_legacy_without_repairing_it() {
    let (root, _locator_store, refs, _expected) =
        committed_ref_fixture("sealed-reader-torn-legacy");
    let refs_root = root.path.join("refs");
    drop(refs);
    rewrite_ref_fixture_as_legacy(&refs_root, None, false);
    OpenOptions::new()
        .append(true)
        .open(refs_root.join("JOURNAL"))
        .expect("open legacy journal for torn tail")
        .write_all(b"MPR")
        .expect("write torn legacy tail");
    let before = ref_layout_snapshot(&refs_root);

    assert!(SealedPairedRefStore::open(&refs_root).is_err());
    assert_eq!(ref_layout_snapshot(&refs_root), before);
}

#[test]
fn sealed_reader_rejects_symlinked_layout_marker() {
    let (root, _locator_store, refs, _expected) =
        committed_ref_fixture("sealed-reader-symlink-layout");
    let refs_root = root.path.join("refs");
    drop(refs);
    rewrite_ref_fixture_as_legacy(&refs_root, None, false);
    let marker_target = root.path.join("legacy-layout-marker");
    std::fs::write(&marker_target, REF_LAYOUT_V2).expect("write external layout marker");
    symlink(&marker_target, refs_root.join("LAYOUT")).expect("symlink layout marker");

    assert!(SealedPairedRefStore::open(&refs_root).is_err());
    assert_eq!(
        std::fs::read_link(refs_root.join("LAYOUT")).expect("read unchanged layout symlink"),
        marker_target
    );
}

#[test]
fn sealed_reader_rejects_symlinked_lock_without_touching_target() {
    let (root, _locator_store, refs, _expected) =
        committed_ref_fixture("sealed-reader-symlink-lock");
    let refs_root = root.path.join("refs");
    drop(refs);
    let lock_target = root.path.join("external-lock");
    std::fs::write(&lock_target, b"external").expect("write external lock target");
    std::fs::remove_file(refs_root.join("LOCK")).expect("remove paired ref lock");
    symlink(&lock_target, refs_root.join("LOCK")).expect("symlink paired ref lock");

    assert!(SealedPairedRefStore::open(&refs_root).is_err());
    assert_eq!(
        std::fs::read(&lock_target).expect("read unchanged external lock target"),
        b"external"
    );
}

#[test]
fn sealed_reader_rejects_partial_temp_without_removing_it() {
    let (root, _locator_store, refs, _expected) =
        committed_ref_fixture("sealed-reader-partial-temp");
    let refs_root = root.path.join("refs");
    drop(refs);
    std::fs::write(refs_root.join("JOURNAL.v3.tmp"), b"partial")
        .expect("write partial sealed ref temp");
    let before = ref_layout_snapshot(&refs_root);

    assert!(SealedPairedRefStore::open(&refs_root).is_err());
    assert_eq!(ref_layout_snapshot(&refs_root), before);
}

#[test]
fn sealed_reader_retains_shared_lock_for_its_lifetime() {
    let (root, _locator_store, refs, _expected) =
        committed_ref_fixture("sealed-reader-retained-lock");
    let refs_root = root.path.join("refs");
    drop(refs);
    let sealed = SealedPairedRefStore::open(&refs_root).expect("open sealed refs with shared lock");
    let cloned = sealed.clone();
    assert!(run_sealed_lock_child(&refs_root, true).success());
    drop(sealed);
    assert!(run_sealed_lock_child(&refs_root, true).success());
    drop(cloned);
    assert!(run_sealed_lock_child(&refs_root, false).success());
}

#[test]
fn sealed_reader_fails_closed_instead_of_waiting_for_an_exclusive_lock() {
    let (root, _locator_store, refs, _expected) =
        committed_ref_fixture("sealed-reader-nonblocking-lock");
    let refs_root = root.path.join("refs");
    drop(refs);
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(refs_root.join("LOCK"))
        .expect("open exclusive lock contender");
    // SAFETY: `lock` owns a valid descriptor for the duration of this call.
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) }, 0);
    assert!(run_sealed_open_child(&refs_root).success());
}

#[test]
fn sealed_ref_open_child_helper() {
    let Some(refs_root) = std::env::var_os(CHILD_SEALED_OPEN_ROOT_ENV).map(PathBuf::from) else {
        return;
    };
    assert!(SealedPairedRefStore::open(refs_root).is_err());
}

#[test]
fn sealed_ref_lock_child_helper() {
    let Some(lock_path) = std::env::var_os(CHILD_SEALED_LOCK_PATH_ENV).map(PathBuf::from) else {
        return;
    };
    let expect_blocked = std::env::var_os(CHILD_SEALED_LOCK_BLOCKED_ENV).is_some();
    let contender = OpenOptions::new()
        .read(true)
        .write(true)
        .open(lock_path)
        .expect("open competing sealed ref lock");

    // SAFETY: `contender` owns a valid descriptor for the duration of this call.
    let locked = unsafe { libc::flock(contender.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if expect_blocked {
        assert_eq!(locked, -1);
        assert_eq!(
            std::io::Error::last_os_error().kind(),
            std::io::ErrorKind::WouldBlock
        );
    } else {
        assert_eq!(locked, 0);
    }
}

#[test]
fn paired_ref_journal_length_is_fixed_across_commits() {
    let root = TestRoot::new("fixed-ref-length");
    let locator_store = LocatorStore::open(root.path.join("locators")).expect("locator store");
    let refs_root = root.path.join("refs");
    let refs = PairedRefStore::open(&refs_root).expect("ref store");
    refs.read("main").expect("warm empty ref cache");
    let before = std::fs::metadata(refs_root.join("JOURNAL"))
        .expect("stat empty journal")
        .len();
    let delta = locator_delta(21, "fixed-ref-length");
    let locator = locator_store
        .install(&delta, &mut NamedFaultInjector::default())
        .expect("install locator");
    let canonical = canonical_receipt(&root.path, "fixed-ref-length");
    let candidate = ref_candidate(&delta, locator.generation, RefSequence::ZERO);
    let RefCommitOutcome::Committed(first_receipt) = refs
        .commit(
            "main",
            &candidate,
            &canonical,
            &locator,
            &locator_store,
            &mut NamedFaultInjector::default(),
        )
        .expect("commit first paired ref")
    else {
        panic!("first paired ref reported stale parent");
    };

    assert_eq!(before, REF_JOURNAL_TOTAL_BYTES);
    assert_eq!(
        std::fs::metadata(refs_root.join("JOURNAL"))
            .expect("stat committed journal")
            .len(),
        before
    );
    let mut second_delta = locator_delta(22, "fixed-ref-length-second");
    second_delta.expected_parent = Some(locator.generation);
    let second_locator = locator_store
        .install(&second_delta, &mut NamedFaultInjector::default())
        .expect("install second locator");
    let second_canonical = canonical_receipt(&root.path, "fixed-ref-length-second");
    let second_candidate = ref_candidate(
        &second_delta,
        second_locator.generation,
        first_receipt.value.sequence,
    );
    refs.commit(
        "main",
        &second_candidate,
        &second_canonical,
        &second_locator,
        &locator_store,
        &mut NamedFaultInjector::default(),
    )
    .expect("commit second paired ref");
    assert_eq!(
        std::fs::metadata(refs_root.join("JOURNAL"))
            .expect("stat twice-committed journal")
            .len(),
        before
    );
    assert_eq!(
        refs.read("main")
            .expect("read through warm cache")
            .expect("committed head")
            .operation_id,
        second_candidate.operation_id
    );
}

#[test]
fn warm_parent_cache_observes_fixed_length_child_process_commit() {
    let root = TestRoot::new("cross-process-cache");
    let refs_root = root.path.join("refs");
    let refs = PairedRefStore::open(&refs_root).expect("open parent ref store");
    assert!(refs.read("main").expect("warm parent cache").is_none());
    let before = std::fs::metadata(refs_root.join("JOURNAL")).expect("stat parent journal");

    let status = Command::new(std::env::current_exe().expect("current test executable"))
        .arg("--exact")
        .arg("paired_ref_child_commit_helper")
        .arg("--nocapture")
        .env(CHILD_COMMIT_ROOT_ENV, &root.path)
        .status()
        .expect("run child paired ref commit");
    assert!(status.success());

    let after = std::fs::metadata(refs_root.join("JOURNAL")).expect("stat child journal");
    assert_eq!(after.len(), before.len());
    assert_eq!(after.ino(), before.ino());
    assert_eq!(
        refs.read("main")
            .expect("refresh parent cache")
            .expect("child committed head")
            .operation_id
            .as_str(),
        "operation-cross-process-child"
    );
}

#[test]
fn paired_ref_child_commit_helper() {
    let Some(root) = std::env::var_os(CHILD_COMMIT_ROOT_ENV).map(PathBuf::from) else {
        return;
    };
    let locator_store =
        LocatorStore::open(root.join("locators")).expect("open child locator store");
    let refs = PairedRefStore::open(root.join("refs")).expect("open child ref store");
    let delta = locator_delta(23, "cross-process-child");
    let locator = locator_store
        .install(&delta, &mut NamedFaultInjector::default())
        .expect("install child locator");
    let canonical = canonical_receipt(&root, "cross-process-child");
    let candidate = ref_candidate(&delta, locator.generation, RefSequence::ZERO);
    refs.commit(
        "main",
        &candidate,
        &canonical,
        &locator,
        &locator_store,
        &mut NamedFaultInjector::default(),
    )
    .expect("commit child paired ref");
}

#[test]
fn paired_ref_reader_ignores_bytes_beyond_active_cursor() {
    let (root, _locator_store, refs, expected) = committed_ref_fixture("cursor-boundary");
    let journal_path = root.path.join("refs/JOURNAL");
    let (_, logical_end) = active_ref_cursor(&journal_path);
    write_journal_bytes(&journal_path, logical_end, b"uncommitted-frame");

    assert_eq!(
        refs.read("main")
            .expect("read committed prefix")
            .expect("committed head"),
        expected
    );
}

#[test]
fn torn_newest_paired_ref_cursor_falls_back_to_older_cursor() {
    let (root, _locator_store, refs, _expected) = committed_ref_fixture("torn-cursor");
    let journal_path = root.path.join("refs/JOURNAL");
    let (slot, _) = active_ref_cursor(&journal_path);
    let cursor_offset = REF_JOURNAL_DATA_BYTES + slot as u64 * REF_CURSOR_SLOT_BYTES;
    write_journal_bytes(&journal_path, cursor_offset, &[0xa5; 17]);

    assert!(refs
        .read("main")
        .expect("fall back to older empty cursor")
        .is_none());
}

#[test]
fn valid_newest_paired_ref_cursor_fails_closed_on_zeroed_header() {
    let (root, _locator_store, refs, _expected) = committed_ref_fixture("zeroed-header");
    let journal_path = root.path.join("refs/JOURNAL");
    write_journal_bytes(&journal_path, 0, &[0_u8; 16]);

    assert!(refs.read("main").is_err());
}

#[test]
fn paired_ref_payload_corruption_fails_closed() {
    let (root, _locator_store, refs, _expected) = committed_ref_fixture("payload-corruption");
    let journal_path = root.path.join("refs/JOURNAL");
    let (_, logical_end) = active_ref_cursor(&journal_path);
    let file = File::open(&journal_path).expect("open journal payload");
    let mut byte = [0_u8; 1];
    file.read_exact_at(&mut byte, logical_end - 1)
        .expect("read journal payload byte");
    byte[0] ^= 0xff;
    write_journal_bytes(&journal_path, logical_end - 1, &byte);

    assert!(refs.read("main").is_err());
}

#[test]
fn legacy_paired_ref_journal_migrates_valid_prefix_and_torn_tail() {
    let (root, _locator_store, refs, expected) = committed_ref_fixture("legacy-migration");
    let refs_root = root.path.join("refs");
    let journal_path = refs_root.join("JOURNAL");
    let (_, logical_end) = active_ref_cursor(&journal_path);
    let source = File::open(&journal_path).expect("open v3 journal");
    let mut legacy = vec![0_u8; usize::try_from(logical_end).expect("legacy length")];
    source
        .read_exact_at(&mut legacy, 0)
        .expect("read committed record prefix");
    legacy.extend_from_slice(b"MPR");
    let mut journal = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&journal_path)
        .expect("replace journal with legacy bytes");
    journal.write_all(&legacy).expect("write legacy journal");
    journal.sync_all().expect("sync legacy journal");
    let mut layout = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(refs_root.join("LAYOUT"))
        .expect("open legacy layout marker");
    layout
        .write_all(REF_LAYOUT_V2)
        .expect("write legacy layout marker");
    layout.sync_all().expect("sync legacy layout marker");
    drop(refs);

    let migrated = PairedRefStore::open(&refs_root).expect("migrate legacy journal");

    assert_eq!(
        std::fs::metadata(&journal_path)
            .expect("stat migrated journal")
            .len(),
        REF_JOURNAL_TOTAL_BYTES
    );
    assert_eq!(
        std::fs::read(refs_root.join("LAYOUT")).expect("read migrated marker"),
        REF_LAYOUT_V3
    );
    assert_eq!(
        migrated
            .read("main")
            .expect("read migrated ref")
            .expect("migrated head"),
        expected
    );
}

#[test]
fn interrupted_v2_marker_after_v3_journal_rename_finishes_in_place() {
    let (root, _locator_store, refs, expected) = committed_ref_fixture("interrupted-migration");
    let refs_root = root.path.join("refs");
    let journal_path = refs_root.join("JOURNAL");
    let before = std::fs::metadata(&journal_path).expect("stat v3 journal");
    std::fs::write(refs_root.join("LAYOUT"), REF_LAYOUT_V2).expect("restore v2 layout marker");
    std::fs::write(refs_root.join("LAYOUT.v3.tmp"), b"stale").expect("write stale layout temp");
    drop(refs);

    let reopened =
        PairedRefStore::open(&refs_root).expect("finish interrupted layout-marker migration");
    let after = std::fs::metadata(&journal_path).expect("stat recovered v3 journal");

    assert_eq!(after.ino(), before.ino());
    assert_eq!(after.len(), REF_JOURNAL_TOTAL_BYTES);
    assert_eq!(
        std::fs::read(refs_root.join("LAYOUT")).expect("read completed v3 marker"),
        REF_LAYOUT_V3
    );
    assert_eq!(
        reopened
            .read("main")
            .expect("read interrupted-migration ref")
            .expect("recovered head"),
        expected
    );
}

#[test]
fn immutable_locator_generations_merge_and_preserve_reverse_accounting() {
    let root = TestRoot::new("locator-generation");
    let store = LocatorStore::open(root.path.join("locators")).expect("open locator store");
    let first = locator_delta(1, "first");
    let first_receipt = store
        .install(&first, &mut NamedFaultInjector::default())
        .expect("install first generation");
    assert_eq!(first_receipt.generation, LocatorGeneration::INITIAL);
    let mut second = locator_delta(2, "second");
    second.expected_parent = Some(first_receipt.generation);
    let second_receipt = store
        .install(&second, &mut NamedFaultInjector::default())
        .expect("install second generation");
    assert_eq!(
        second_receipt.generation,
        LocatorGeneration::INITIAL
            .checked_next()
            .expect("next locator generation")
    );
    let mut stale = locator_delta(3, "stale");
    stale.expected_parent = Some(first_receipt.generation);
    let error = store
        .install(&stale, &mut NamedFaultInjector::default())
        .expect_err("stale locator parent must fail");
    assert!(matches!(error, PocError::OwnerConflict(_)));

    let selected = store.selected().expect("read selected").expect("selector");
    assert_eq!(selected.forward.len(), 2);
    assert_eq!(selected.reverse.len(), 2);
    assert_eq!(
        store
            .resolve(&payload_root(1))
            .expect("resolve first")
            .expect("first locator")
            .allocation_id,
        first.forward[0].allocation_id
    );
    assert_eq!(
        selected
            .reverse
            .iter()
            .map(|entry| entry.accounted_bytes)
            .sum::<u64>(),
        8_192
    );
    store
        .validate_generation_receipt(&first_receipt)
        .expect("old immutable generation remains durable");
}

#[test]
fn locator_fault_replay_never_selects_an_incomplete_generation() {
    for point in [
        NamedFaultPoint::LocatorAfterForward,
        NamedFaultPoint::LocatorAfterReverse,
        NamedFaultPoint::LocatorAfterManifestFsync,
        NamedFaultPoint::LocatorAfterSelectorRename,
        NamedFaultPoint::LocatorAfterSelectorDirFsync,
    ] {
        let root = TestRoot::new(point.as_str());
        let path = root.path.join("locators");
        let store = LocatorStore::open(&path).expect("open locator store");
        let delta = locator_delta(1, point.as_str());
        let mut faults = NamedFaultInjector::armed([(point, 1)]);
        assert!(matches!(
            store.install(&delta, &mut faults),
            Err(PocError::RecoveryRequired(_))
        ));
        if let Some(selected) = LocatorStore::open(&path)
            .expect("reopen locator store")
            .selected()
            .expect("read selector")
        {
            assert_eq!(selected.forward.len(), 1);
            assert_eq!(selected.reverse.len(), 1);
        }

        let reopened = LocatorStore::open(&path).expect("fresh locator process");
        let receipt = reopened
            .install(&delta, &mut NamedFaultInjector::default())
            .expect("durable replay");
        reopened
            .validate_receipt(&receipt)
            .expect("selected complete generation");
        let selected = reopened.selected().expect("read replay").expect("selector");
        assert_eq!(selected.forward.len(), 1);
        assert_eq!(selected.reverse.len(), 1);
    }
}

#[test]
fn exact_locator_replacement_is_atomic_replayable_and_preserves_coverage() {
    for point in [
        NamedFaultPoint::LocatorAfterForward,
        NamedFaultPoint::LocatorAfterReverse,
        NamedFaultPoint::LocatorAfterManifestFsync,
        NamedFaultPoint::LocatorAfterSelectorRename,
        NamedFaultPoint::LocatorAfterSelectorDirFsync,
    ] {
        let root = TestRoot::new(&format!("replacement-{}", point.as_str()));
        let path = root.path.join("locators");
        let store = LocatorStore::open(&path).expect("open locator store");
        let source = locator_delta(1, point.as_str());
        let source_receipt = store
            .install(&source, &mut NamedFaultInjector::default())
            .expect("install source locator");
        let replacement = locator_replacement(&source, source_receipt.generation);
        let mut faults = NamedFaultInjector::armed([(point, 1)]);
        assert!(matches!(
            store.replace_exact(&replacement, &mut faults),
            Err(PocError::RecoveryRequired(_))
        ));

        let reopened = LocatorStore::open(&path).expect("reopen locator store");
        let selected = reopened
            .selected()
            .expect("read selected")
            .expect("selector");
        let observed = selected
            .forward
            .iter()
            .find(|entry| entry.payload_root == replacement.payload_root)
            .expect("selected root remains covered");
        assert!(
            observed.allocation_id == replacement.expected_source_allocation_id
                || observed == &replacement.target
        );
        assert!(selected.reverse.iter().any(|entry| {
            entry.allocation_id == observed.allocation_id
                && entry.owner_epoch == observed.owner_epoch
                && entry.payload_roots.contains(&observed.payload_root)
        }));

        let receipt = reopened
            .replace_exact(&replacement, &mut NamedFaultInjector::default())
            .expect("durable replacement replay");
        reopened
            .validate_receipt(&receipt)
            .expect("replacement generation is complete");
        reopened
            .validate_generation_receipt(&source_receipt)
            .expect("source generation remains immutable");
        assert_eq!(
            reopened
                .resolve(&replacement.payload_root)
                .expect("resolve replacement")
                .expect("target locator"),
            replacement.target
        );
        let selected = reopened.selected().expect("read replay").expect("selector");
        assert!(!selected
            .reverse
            .iter()
            .any(|entry| { entry.allocation_id == replacement.expected_source_allocation_id }));
        assert_eq!(
            selected
                .reverse
                .iter()
                .find(|entry| entry.allocation_id == replacement.target.allocation_id),
            Some(&replacement.target_reverse)
        );
        let stable = reopened
            .replace_exact(&replacement, &mut NamedFaultInjector::default())
            .expect("stable replacement response replay");
        assert_eq!(stable, receipt);
    }
}

#[test]
fn exact_locator_replacement_rejects_stale_source_without_advancing() {
    let root = TestRoot::new("replacement-stale-source");
    let store = LocatorStore::open(root.path.join("locators")).expect("open locator store");
    let source = locator_delta(1, "replacement-stale-source");
    let source_receipt = store
        .install(&source, &mut NamedFaultInjector::default())
        .expect("install source locator");
    let mut replacement = locator_replacement(&source, source_receipt.generation);
    replacement.expected_source_owner_epoch += 1;
    let error = store
        .replace_exact(&replacement, &mut NamedFaultInjector::default())
        .expect_err("stale source epoch must fail");
    assert!(matches!(error, PocError::OwnerConflict(_)));
    let selected = store.selected().expect("read selected").expect("selector");
    assert_eq!(selected.receipt, source_receipt);
    assert_eq!(selected.forward, source.forward);
    assert_eq!(selected.reverse, source.reverse);
}

#[test]
fn paired_ref_faults_replay_to_old_or_complete_new_and_stable_response() {
    for point in [
        NamedFaultPoint::RefBeforeTemp,
        NamedFaultPoint::RefAfterTempFsync,
        NamedFaultPoint::RefAfterReplace,
        NamedFaultPoint::RefAfterParentFsync,
        NamedFaultPoint::ResponseLossPublish,
    ] {
        let root = TestRoot::new(point.as_str());
        let locator_store =
            LocatorStore::open(root.path.join("locators")).expect("open locator store");
        let delta = locator_delta(1, point.as_str());
        let locator = locator_store
            .install(&delta, &mut NamedFaultInjector::default())
            .expect("install locator");
        let refs = PairedRefStore::open(root.path.join("refs")).expect("open refs");
        let canonical = canonical_receipt(&root.path, point.as_str());
        let candidate = ref_candidate(&delta, locator.generation, RefSequence::ZERO);
        let mut faults = NamedFaultInjector::armed([(point, 1)]);
        assert!(matches!(
            refs.commit(
                "main",
                &candidate,
                &canonical,
                &locator,
                &locator_store,
                &mut faults,
            ),
            Err(PocError::RecoveryRequired(_))
        ));

        if let Some(observed) = refs.read("main").expect("read after fault") {
            assert_eq!(observed.roots, candidate.roots);
            assert_eq!(observed.locator_generation, locator.generation);
            assert_eq!(observed.operation_id, candidate.operation_id);
        }

        let reopened_refs =
            PairedRefStore::open(root.path.join("refs")).expect("fresh ref process");
        let result = reopened_refs
            .commit(
                "main",
                &candidate,
                &canonical,
                &locator,
                &locator_store,
                &mut NamedFaultInjector::default(),
            )
            .expect("replay commit");
        let RefCommitOutcome::Committed(receipt) = result else {
            panic!("stable retry did not converge");
        };
        assert_eq!(receipt.value.sequence.get(), 1);
        assert!(receipt.parent_directory_synced);
        let resolved = reopened_refs
            .read_resolved("main", &locator_store)
            .expect("resolve paired ref")
            .expect("paired ref");
        assert_eq!(resolved.value, receipt.value);
        assert_eq!(resolved.canonical, canonical);
        assert_eq!(resolved.locator, locator);

        let stable = reopened_refs
            .commit(
                "main",
                &candidate,
                &canonical,
                &locator,
                &locator_store,
                &mut NamedFaultInjector::default(),
            )
            .expect("stable response replay");
        let RefCommitOutcome::Committed(stable) = stable else {
            panic!("stored result was not returned");
        };
        assert!(stable.idempotent_replay);
        assert_eq!(stable.value, receipt.value);
    }
}

#[test]
fn paired_ref_rejects_incomplete_prerequisites_and_excludes_physical_identity() {
    let root = TestRoot::new("ref-prerequisite");
    let locator_store = LocatorStore::open(root.path.join("locators")).expect("locator store");
    let delta = locator_delta(1, "prerequisite");
    let locator = locator_store
        .install(&delta, &mut NamedFaultInjector::default())
        .expect("install locator");
    let refs = PairedRefStore::open(root.path.join("refs")).expect("ref store");
    let mut canonical = canonical_receipt(&root.path, "prerequisite");
    canonical.manifest_directory_fsynced = false;
    let candidate = ref_candidate(&delta, locator.generation, RefSequence::ZERO);
    assert!(matches!(
        refs.commit(
            "main",
            &candidate,
            &canonical,
            &locator,
            &locator_store,
            &mut NamedFaultInjector::default(),
        ),
        Err(PocError::Integrity(_))
    ));
    assert!(refs.read("main").expect("read empty head").is_none());

    let encoded = serde_json::to_string(&candidate).expect("encode candidate");
    assert!(!encoded.contains(delta.forward[0].allocation_id.as_str()));
    assert!(!encoded.contains(root.path.to_string_lossy().as_ref()));
    assert!(serde_json::to_string(&delta)
        .expect("encode locator")
        .contains(delta.forward[0].allocation_id.as_str()));
}

#[test]
fn sixteen_paired_ref_progressions_all_resolve_complete_durable_state() {
    let root = TestRoot::new("sixteen-progressions");
    let locator_store = LocatorStore::open(root.path.join("locators")).expect("locator store");
    let refs = PairedRefStore::open(root.path.join("refs")).expect("ref store");
    let mut expected = RefSequence::ZERO;
    for index in 1..=16 {
        let delta = locator_delta(index, &format!("progression-{index}"));
        let locator = locator_store
            .install(&delta, &mut NamedFaultInjector::default())
            .expect("install locator progression");
        let canonical = canonical_receipt(&root.path, &format!("progression-{index}"));
        let candidate = ref_candidate(&delta, locator.generation, expected);
        let RefCommitOutcome::Committed(receipt) = refs
            .commit(
                "main",
                &candidate,
                &canonical,
                &locator,
                &locator_store,
                &mut NamedFaultInjector::default(),
            )
            .expect("commit paired ref progression")
        else {
            panic!("sequential paired ref reported stale parent");
        };
        expected = receipt.value.sequence;
        let resolved = refs
            .read_resolved("main", &locator_store)
            .expect("resolve progression")
            .expect("paired ref");
        assert_eq!(resolved.value.sequence, expected);
        assert_eq!(resolved.locator.generation, locator.generation);
    }
    assert_eq!(expected.get(), 16);
    assert_eq!(
        locator_store
            .selected()
            .expect("selected locator")
            .expect("locator")
            .forward
            .len(),
        16
    );
}

#[test]
fn atomic_rollback_selects_target_and_advances_branch_once() {
    let root = TestRoot::new("atomic-rollback");
    let locator_store = LocatorStore::open(root.path.join("locators")).expect("locator store");
    let refs = PairedRefStore::open(root.path.join("refs")).expect("ref store");

    let target_delta = locator_delta(1, "atomic-target");
    let target_locator = locator_store
        .install(&target_delta, &mut NamedFaultInjector::default())
        .expect("install target locator");
    let target_canonical = canonical_receipt(&root.path, "atomic-target");
    let target_candidate =
        ref_candidate_for_payload(&target_delta, target_locator.generation, RefSequence::ZERO);
    let RefCommitOutcome::Committed(target) = refs
        .commit(
            "rollback-target",
            &target_candidate,
            &target_canonical,
            &target_locator,
            &locator_store,
            &mut NamedFaultInjector::default(),
        )
        .expect("commit rollback target")
    else {
        panic!("target commit reported stale parent");
    };

    let mut current_delta = locator_delta(2, "atomic-current");
    current_delta.expected_parent = Some(target_locator.generation);
    let current_locator = locator_store
        .install(&current_delta, &mut NamedFaultInjector::default())
        .expect("install current locator");
    let current_canonical = canonical_receipt(&root.path, "atomic-current");
    let current_candidate = ref_candidate_for_payload(
        &current_delta,
        current_locator.generation,
        RefSequence::ZERO,
    );
    refs.commit(
        "main",
        &current_candidate,
        &current_canonical,
        &current_locator,
        &locator_store,
        &mut NamedFaultInjector::default(),
    )
    .expect("commit current branch");

    let rollback_operation = OperationId::from_string("operation-atomic-rollback");
    let receipt = refs
        .rollback_to_branch(
            "main",
            "rollback-target",
            &rollback_operation,
            &locator_store,
            &mut NamedFaultInjector::default(),
        )
        .expect("atomic rollback");
    assert!(!receipt.idempotent_replay);
    assert_eq!(receipt.value.roots, target.value.roots);
    assert_eq!(receipt.value.sequence.get(), 2);
    assert_eq!(receipt.value.locator_generation, current_locator.generation);

    let replay = refs
        .rollback_to_branch(
            "main",
            "rollback-target",
            &rollback_operation,
            &locator_store,
            &mut NamedFaultInjector::default(),
        )
        .expect("atomic rollback replay");
    assert!(replay.idempotent_replay);
    assert_eq!(replay.value, receipt.value);
    assert!(matches!(
        refs.rollback_to_branch(
            "main",
            "different-rollback-target",
            &rollback_operation,
            &locator_store,
            &mut NamedFaultInjector::default(),
        ),
        Err(PocError::Integrity(_))
    ));
    assert_eq!(
        refs.read("main")
            .expect("read rolled back branch")
            .expect("main branch"),
        receipt.value
    );
}

#[test]
fn atomic_rollback_faults_recover_old_or_complete_new_and_stable_response() {
    for point in [
        NamedFaultPoint::RefBeforeTemp,
        NamedFaultPoint::RefAfterTempFsync,
        NamedFaultPoint::RefAfterReplace,
        NamedFaultPoint::RefAfterParentFsync,
        NamedFaultPoint::ResponseLossRollback,
    ] {
        let root = TestRoot::new(&format!("atomic-rollback-{}", point.as_str()));
        let locator_store = LocatorStore::open(root.path.join("locators")).expect("locator store");
        let refs = PairedRefStore::open(root.path.join("refs")).expect("ref store");
        let delta = locator_delta(3, point.as_str());
        let locator = locator_store
            .install(&delta, &mut NamedFaultInjector::default())
            .expect("install locator");
        let canonical = canonical_receipt(&root.path, point.as_str());
        let candidate = ref_candidate_for_payload(&delta, locator.generation, RefSequence::ZERO);
        for branch in ["rollback-target", "main"] {
            refs.commit(
                branch,
                &candidate,
                &canonical,
                &locator,
                &locator_store,
                &mut NamedFaultInjector::default(),
            )
            .expect("commit rollback fixture branch");
        }
        let operation_id =
            OperationId::from_string(format!("operation-rollback-{}", point.as_str()));
        let mut faults = NamedFaultInjector::armed([(point, 1)]);
        assert!(matches!(
            refs.rollback_to_branch(
                "main",
                "rollback-target",
                &operation_id,
                &locator_store,
                &mut faults,
            ),
            Err(PocError::RecoveryRequired(_))
        ));

        let stable = refs
            .rollback_to_branch(
                "main",
                "rollback-target",
                &operation_id,
                &locator_store,
                &mut NamedFaultInjector::default(),
            )
            .expect("recover atomic rollback");
        assert_eq!(stable.value.operation_id, operation_id);
        assert_eq!(
            refs.read("main")
                .expect("read recovered rollback")
                .expect("main branch"),
            stable.value
        );
        let replay = refs
            .rollback_to_branch(
                "main",
                "rollback-target",
                &operation_id,
                &locator_store,
                &mut NamedFaultInjector::default(),
            )
            .expect("stable rollback response replay");
        assert!(replay.idempotent_replay);
        assert_eq!(replay.value, stable.value);
    }
}

#[test]
fn atomic_rollback_rejects_missing_target_without_advancing() {
    let root = TestRoot::new("atomic-rollback-missing-target");
    let locator_store = LocatorStore::open(root.path.join("locators")).expect("locator store");
    let refs = PairedRefStore::open(root.path.join("refs")).expect("ref store");
    let delta = locator_delta(4, "atomic-missing-target");
    let locator = locator_store
        .install(&delta, &mut NamedFaultInjector::default())
        .expect("install locator");
    let canonical = canonical_receipt(&root.path, "atomic-missing-target");
    let candidate = ref_candidate_for_payload(&delta, locator.generation, RefSequence::ZERO);
    let RefCommitOutcome::Committed(current) = refs
        .commit(
            "main",
            &candidate,
            &canonical,
            &locator,
            &locator_store,
            &mut NamedFaultInjector::default(),
        )
        .expect("commit current branch")
    else {
        panic!("current commit reported stale parent");
    };
    assert!(matches!(
        refs.rollback_to_branch(
            "main",
            "absent-target",
            &OperationId::from_string("operation-missing-target"),
            &locator_store,
            &mut NamedFaultInjector::default(),
        ),
        Err(PocError::Integrity(_))
    ));
    assert_eq!(
        refs.read("main")
            .expect("read unchanged branch")
            .expect("main branch"),
        current.value
    );
}

#[test]
fn atomic_squash_advances_current_branch_once_and_replays() {
    let (_root, locator_store, refs, initial) = committed_squash_ref_fixture("atomic-squash");
    let operation_id = OperationId::from_string("operation-atomic-squash-next");
    let receipt = refs
        .squash_branch(
            "main",
            &operation_id,
            &locator_store,
            &mut NamedFaultInjector::default(),
        )
        .expect("atomic squash");

    assert!(!receipt.idempotent_replay);
    assert_eq!(receipt.value.operation_id, operation_id);
    assert_eq!(receipt.value.roots, initial.roots);
    assert_eq!(receipt.value.sequence.get(), initial.sequence.get() + 1);

    let before_replay = ref_layout_snapshot(refs.root());
    let replay = refs
        .squash_branch(
            "main",
            &operation_id,
            &locator_store,
            &mut NamedFaultInjector::default(),
        )
        .expect("atomic squash replay");
    assert!(replay.idempotent_replay);
    assert_eq!(replay.value, receipt.value);
    assert_eq!(ref_layout_snapshot(refs.root()), before_replay);
    assert_eq!(
        refs.read("main")
            .expect("read squashed branch")
            .expect("main branch"),
        receipt.value
    );
}

#[test]
fn atomic_squash_faults_recover_old_or_complete_new_and_stable_response() {
    for point in [
        NamedFaultPoint::RefBeforeTemp,
        NamedFaultPoint::RefAfterTempFsync,
        NamedFaultPoint::RefAfterReplace,
        NamedFaultPoint::RefAfterParentFsync,
        NamedFaultPoint::ResponseLossPublish,
    ] {
        let label = format!("atomic-squash-{}", point.as_str());
        let (_root, locator_store, refs, initial) = committed_squash_ref_fixture(&label);
        let operation_id = OperationId::from_string(format!("operation-squash-{}", point.as_str()));
        let mut faults = NamedFaultInjector::armed([(point, 1)]);
        let failure = refs
            .squash_branch("main", &operation_id, &locator_store, &mut faults)
            .expect_err("armed squash fault must interrupt the response");
        assert!(
            matches!(failure, PocError::RecoveryRequired(_)),
            "unexpected squash fault result at {}: {failure:?}",
            point.as_str()
        );

        let stable = refs
            .squash_branch(
                "main",
                &operation_id,
                &locator_store,
                &mut NamedFaultInjector::default(),
            )
            .expect("recover atomic squash");
        assert_eq!(stable.value.operation_id, operation_id);
        assert_eq!(stable.value.roots, initial.roots);
        assert_eq!(stable.value.sequence.get(), initial.sequence.get() + 1);
        assert_eq!(
            refs.read("main")
                .expect("read recovered squash")
                .expect("main branch"),
            stable.value
        );

        let replay = refs
            .squash_branch(
                "main",
                &operation_id,
                &locator_store,
                &mut NamedFaultInjector::default(),
            )
            .expect("stable squash response replay");
        assert!(replay.idempotent_replay);
        assert_eq!(replay.value, stable.value);
    }
}

#[test]
fn atomic_squash_rejects_corrupt_current_locator_without_advancing() {
    let (root, locator_store, refs, initial) =
        committed_squash_ref_fixture("atomic-squash-corrupt-locator");
    std::fs::write(root.path.join("locators/CURRENT"), b"{corrupt")
        .expect("corrupt current locator");

    assert!(refs
        .squash_branch(
            "main",
            &OperationId::from_string("operation-squash-corrupt-locator"),
            &locator_store,
            &mut NamedFaultInjector::default(),
        )
        .is_err());
    assert_eq!(
        refs.read("main")
            .expect("read unchanged branch")
            .expect("main branch"),
        initial
    );
}

#[test]
fn atomic_squash_rejects_selected_locator_missing_current_root_without_advancing() {
    let (_root, locator_store, refs, initial) =
        committed_ref_fixture("atomic-squash-missing-current-root");

    assert!(matches!(
        refs.squash_branch(
            "main",
            &OperationId::from_string("operation-squash-missing-current-root-next"),
            &locator_store,
            &mut NamedFaultInjector::default(),
        ),
        Err(PocError::Integrity(_))
    ));
    assert_eq!(
        refs.read("main")
            .expect("read unchanged branch")
            .expect("main branch"),
        initial
    );
}

fn committed_ref_fixture(label: &str) -> (TestRoot, LocatorStore, PairedRefStore, PairedRefValue) {
    let root = TestRoot::new(label);
    let locator_store = LocatorStore::open(root.path.join("locators")).expect("locator store");
    let refs = PairedRefStore::open(root.path.join("refs")).expect("ref store");
    let delta = locator_delta(31, label);
    let locator = locator_store
        .install(&delta, &mut NamedFaultInjector::default())
        .expect("install locator");
    let canonical = canonical_receipt(&root.path, label);
    let candidate = ref_candidate(&delta, locator.generation, RefSequence::ZERO);
    let RefCommitOutcome::Committed(receipt) = refs
        .commit(
            "main",
            &candidate,
            &canonical,
            &locator,
            &locator_store,
            &mut NamedFaultInjector::default(),
        )
        .expect("commit paired ref fixture")
    else {
        panic!("fixture commit reported stale parent");
    };
    (root, locator_store, refs, receipt.value)
}

fn committed_squash_ref_fixture(
    label: &str,
) -> (TestRoot, LocatorStore, PairedRefStore, PairedRefValue) {
    let root = TestRoot::new(label);
    let locator_store = LocatorStore::open(root.path.join("locators")).expect("locator store");
    let refs = PairedRefStore::open(root.path.join("refs")).expect("ref store");
    let delta = locator_delta(31, label);
    let locator = locator_store
        .install(&delta, &mut NamedFaultInjector::default())
        .expect("install locator");
    let canonical = canonical_receipt(&root.path, label);
    let candidate = ref_candidate_for_payload(&delta, locator.generation, RefSequence::ZERO);
    let RefCommitOutcome::Committed(receipt) = refs
        .commit(
            "main",
            &candidate,
            &canonical,
            &locator,
            &locator_store,
            &mut NamedFaultInjector::default(),
        )
        .expect("commit atomic squash fixture")
    else {
        panic!("atomic squash fixture commit reported stale parent");
    };
    (root, locator_store, refs, receipt.value)
}

fn active_ref_cursor(path: &Path) -> (usize, u64) {
    let file = File::open(path).expect("open paired ref cursor");
    let mut selected = None;
    for slot in 0..2_usize {
        let mut prefix = [0_u8; 40];
        file.read_exact_at(
            &mut prefix,
            REF_JOURNAL_DATA_BYTES + slot as u64 * REF_CURSOR_SLOT_BYTES,
        )
        .expect("read paired ref cursor");
        if prefix[..8] != *b"MPRCURS3" {
            continue;
        }
        let generation = u64::from_le_bytes(prefix[16..24].try_into().expect("cursor generation"));
        let logical_end =
            u64::from_le_bytes(prefix[24..32].try_into().expect("cursor logical end"));
        if selected.is_none_or(|(_, selected_generation, _)| generation > selected_generation) {
            selected = Some((slot, generation, logical_end));
        }
    }
    let (slot, _, logical_end) = selected.expect("active paired ref cursor");
    (slot, logical_end)
}

fn write_journal_bytes(path: &Path, offset: u64, bytes: &[u8]) {
    let file = OpenOptions::new()
        .write(true)
        .open(path)
        .expect("open paired ref journal for corruption");
    file.write_all_at(bytes, offset)
        .expect("write paired ref journal bytes");
    file.sync_data().expect("sync paired ref journal bytes");
}

fn run_sealed_lock_child(refs_root: &Path, expect_blocked: bool) -> ExitStatus {
    let mut command = Command::new(std::env::current_exe().expect("current test executable"));
    command
        .arg("--exact")
        .arg("sealed_ref_lock_child_helper")
        .arg("--nocapture")
        .env_remove(CHILD_SEALED_LOCK_PATH_ENV)
        .env_remove(CHILD_SEALED_LOCK_BLOCKED_ENV)
        .env(CHILD_SEALED_LOCK_PATH_ENV, refs_root.join("LOCK"));
    if expect_blocked {
        command.env(CHILD_SEALED_LOCK_BLOCKED_ENV, "1");
    }
    let mut child = command.spawn().expect("spawn sealed ref lock child");
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("poll sealed ref lock child") {
            return status;
        }
        if started.elapsed() >= Duration::from_secs(5) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("sealed ref lock child exceeded five seconds");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn run_sealed_open_child(refs_root: &Path) -> ExitStatus {
    let mut command = Command::new(std::env::current_exe().expect("current test executable"));
    command
        .arg("--exact")
        .arg("sealed_ref_open_child_helper")
        .arg("--nocapture")
        .env_remove(CHILD_SEALED_LOCK_PATH_ENV)
        .env_remove(CHILD_SEALED_LOCK_BLOCKED_ENV)
        .env_remove(CHILD_SEALED_OPEN_ROOT_ENV)
        .env(CHILD_SEALED_OPEN_ROOT_ENV, refs_root);
    let mut child = command.spawn().expect("spawn sealed ref open child");
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("poll sealed ref open child") {
            return status;
        }
        if started.elapsed() >= Duration::from_secs(5) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("sealed ref open child exceeded five seconds");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[derive(Debug, Eq, PartialEq)]
struct RefLayoutFileSnapshot {
    name: String,
    sha256: String,
    inode: u64,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
}

#[derive(Debug, Eq, PartialEq)]
struct RefLayoutSnapshot {
    entries: Vec<String>,
    files: Vec<RefLayoutFileSnapshot>,
}

fn ref_layout_snapshot(root: &Path) -> RefLayoutSnapshot {
    let mut entries = std::fs::read_dir(root)
        .expect("read paired ref layout")
        .map(|entry| {
            entry
                .expect("read paired ref layout entry")
                .file_name()
                .into_string()
                .expect("UTF-8 paired ref layout entry")
        })
        .collect::<Vec<_>>();
    entries.sort();
    let files = entries
        .iter()
        .filter_map(|name| {
            let path = root.join(name);
            let metadata = std::fs::symlink_metadata(&path).expect("stat paired ref layout entry");
            if !metadata.file_type().is_file() {
                return None;
            }
            let mut file = File::open(&path).expect("open paired ref layout entry");
            let mut digest = Sha256::new();
            let mut buffer = vec![0_u8; 1024 * 1024];
            loop {
                let length = file
                    .read(&mut buffer)
                    .expect("read paired ref layout entry");
                if length == 0 {
                    break;
                }
                digest.update(&buffer[..length]);
            }
            Some(RefLayoutFileSnapshot {
                name: name.clone(),
                sha256: format!("{:x}", digest.finalize()),
                inode: metadata.ino(),
                length: metadata.len(),
                modified_seconds: metadata.mtime(),
                modified_nanoseconds: metadata.mtime_nsec(),
            })
        })
        .collect();
    RefLayoutSnapshot { entries, files }
}

fn rewrite_ref_fixture_as_legacy(
    refs_root: &Path,
    layout_marker: Option<&[u8]>,
    preallocated: bool,
) -> u64 {
    let journal_path = refs_root.join("JOURNAL");
    let (_, logical_end) = active_ref_cursor(&journal_path);
    let source = File::open(&journal_path).expect("open v3 journal for legacy fixture");
    let mut legacy = vec![0_u8; usize::try_from(logical_end).expect("legacy journal length")];
    source
        .read_exact_at(&mut legacy, 0)
        .expect("read legacy journal committed prefix");
    let mut journal = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&journal_path)
        .expect("replace journal with legacy fixture");
    journal
        .write_all(&legacy)
        .expect("write legacy journal fixture");
    if preallocated {
        journal
            .set_len(REF_JOURNAL_DATA_BYTES)
            .expect("preallocate legacy journal fixture");
    }
    journal.sync_all().expect("sync legacy journal fixture");
    match layout_marker {
        Some(marker) => {
            std::fs::write(refs_root.join("LAYOUT"), marker).expect("write legacy layout marker");
        }
        None => {
            std::fs::remove_file(refs_root.join("LAYOUT")).expect("remove legacy layout marker");
        }
    }
    logical_end
}

fn locator_delta(seed: u8, label: &str) -> LocatorDelta {
    let operation_id = OperationId::from_string(format!("operation-{label}"));
    let publication_id = PublicationId::from_string(format!("publication-{label}"));
    let allocation_id =
        sandbox_runtime_mpla_poc::AllocationId::from_string(Uuid::new_v4().to_string());
    LocatorDelta {
        schema_version: SCHEMA_VERSION,
        operation_id: operation_id.clone(),
        publication_id: publication_id.clone(),
        expected_parent: None,
        forward: vec![ForwardLocatorEntry {
            payload_root: payload_root(seed),
            allocation_id: allocation_id.clone(),
            owner_epoch: 2,
            extents: vec![LocatorExtent {
                relative_path: format!("payload/{seed}"),
                offset: 0,
                length: 4_096,
            }],
        }],
        reverse: vec![ReverseLocatorEntry {
            allocation_id,
            owner_epoch: 2,
            operation_id,
            publication_id,
            payload_roots: vec![payload_root(seed)],
            accounted_bytes: 4_096,
        }],
    }
}

fn locator_replacement(
    source: &LocatorDelta,
    expected_parent: LocatorGeneration,
) -> LocatorReplacement {
    let operation_id = OperationId::from_string("operation-evacuate");
    let publication_id = PublicationId::from_string("publication-evacuate");
    let allocation_id =
        sandbox_runtime_mpla_poc::AllocationId::from_string(Uuid::new_v4().to_string());
    LocatorReplacement {
        schema_version: SCHEMA_VERSION,
        operation_id: operation_id.clone(),
        publication_id: publication_id.clone(),
        expected_parent,
        payload_root: source.forward[0].payload_root.clone(),
        expected_source_allocation_id: source.forward[0].allocation_id.clone(),
        expected_source_owner_epoch: source.forward[0].owner_epoch,
        target: ForwardLocatorEntry {
            payload_root: source.forward[0].payload_root.clone(),
            allocation_id: allocation_id.clone(),
            owner_epoch: source.forward[0].owner_epoch + 1,
            extents: vec![LocatorExtent {
                relative_path: "payload/evacuated".to_owned(),
                offset: 0,
                length: 4_096,
            }],
        },
        target_reverse: ReverseLocatorEntry {
            allocation_id,
            owner_epoch: source.forward[0].owner_epoch + 1,
            operation_id,
            publication_id,
            payload_roots: vec![source.forward[0].payload_root.clone()],
            accounted_bytes: 4_096,
        },
    }
}

fn ref_candidate(
    delta: &LocatorDelta,
    generation: LocatorGeneration,
    expected_sequence: RefSequence,
) -> LocatorRefCandidate {
    LocatorRefCandidate {
        schema_version: SCHEMA_VERSION,
        operation_id: delta.operation_id.clone(),
        publication_id: delta.publication_id.clone(),
        roots: root_pair(delta.forward[0].payload_root.as_str().as_bytes()[0]),
        locator_generation: generation,
        expected_sequence,
    }
}

fn ref_candidate_for_payload(
    delta: &LocatorDelta,
    generation: LocatorGeneration,
    expected_sequence: RefSequence,
) -> LocatorRefCandidate {
    LocatorRefCandidate {
        schema_version: SCHEMA_VERSION,
        operation_id: delta.operation_id.clone(),
        publication_id: delta.publication_id.clone(),
        roots: CanonicalRootPair {
            root_id: RootId::parse(delta.forward[0].payload_root.as_str()).expect("root ID"),
            attribution_root_id: AttributionRootId::parse("cd".repeat(32))
                .expect("attribution root ID"),
        },
        locator_generation: generation,
        expected_sequence,
    }
}

fn payload_root(seed: u8) -> PayloadRootId {
    PayloadRootId::parse(format!("{seed:02x}").repeat(32)).expect("payload root")
}

fn root_pair(seed: u8) -> CanonicalRootPair {
    CanonicalRootPair {
        root_id: RootId::parse(format!("{seed:02x}").repeat(32)).expect("root ID"),
        attribution_root_id: AttributionRootId::parse(
            format!("{:02x}", seed.wrapping_add(64)).repeat(32),
        )
        .expect("attribution root ID"),
    }
}

fn canonical_receipt(root: &Path, label: &str) -> CanonicalDurabilityReceipt {
    let canonical_dir = root.join("canonical");
    std::fs::create_dir_all(&canonical_dir).expect("create canonical directory");
    let manifest = canonical_dir.join(format!("{label}.json"));
    let file = File::create(&manifest).expect("create canonical manifest");
    file.sync_all().expect("fsync canonical manifest");
    File::open(&canonical_dir)
        .expect("open canonical directory")
        .sync_all()
        .expect("fsync canonical directory");
    CanonicalDurabilityReceipt {
        root_manifest: manifest,
        semantic_attribution: AttributionInput {
            actor_id: "test-actor".to_owned(),
            semantic_operation_id: label.to_owned(),
        },
        immutable_object_count: 2,
        immutable_object_bytes: 8_192,
        object_set_sha256: "ab".repeat(32),
        files_fsynced: true,
        object_directory_fsynced: true,
        manifest_fsynced: true,
        manifest_directory_fsynced: true,
    }
}
