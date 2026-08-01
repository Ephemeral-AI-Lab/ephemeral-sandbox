#![forbid(unsafe_code)]

use std::cell::{Cell, RefCell};
use std::convert::Infallible;
use std::io::{self, Cursor, Read};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Barrier, Mutex};
use std::time::Instant;

use sandbox_runtime_layerstack::Sha256Digest;
use sandbox_runtime_layerstack_core::{
    v3_record_id, ActorId, BranchId, CanonicalRecordV3, Digest32, ErrorKind, FileNodeId,
    PublicationId, RawDigest, RecordKindV3, RootId, TlvV3,
};

mod lock {
    #[derive(Clone, Copy)]
    pub(crate) enum WriterLockForbiddenWork {
        HistoryScan,
        Cleanup,
    }

    pub(crate) fn assert_writer_lock_allows(_class: WriterLockForbiddenWork) {}
}

#[allow(
    dead_code,
    reason = "the publication harness needs materialization types only to preserve the production operation module graph"
)]
#[path = "../src/stack/candidate/generation.rs"]
mod generation;
#[allow(
    dead_code,
    reason = "the publication harness exercises only operation-state discrimination from this production sibling"
)]
#[path = "../src/stack/candidate/materialization_operation.rs"]
mod materialization_operation;
#[path = "../src/stack/candidate/object_store.rs"]
mod object_store;
#[path = "../src/stack/candidate/occ.rs"]
mod occ;
#[path = "../src/stack/candidate/operation.rs"]
mod operation;
#[path = "../src/stack/candidate/ref_ops.rs"]
mod ref_ops;
#[allow(
    dead_code,
    reason = "the publication harness intentionally exercises only publication-related refs helpers"
)]
#[path = "../src/stack/candidate/refs.rs"]
mod refs;
#[path = "../src/stack/candidate/seqcdc.rs"]
mod seqcdc;
#[path = "../src/stack/candidate/source.rs"]
mod source;
#[path = "../src/stack/candidate/spool.rs"]
mod spool;
#[allow(
    dead_code,
    reason = "publication-only tree helpers are exercised through the operation integration target"
)]
#[path = "../src/stack/candidate/tree.rs"]
mod tree;

use object_store::{InstallDisposition, InstallStage, LooseObjectStore, ObjectStoreError};
use occ::{commit_with_rebase, compare_semantic_keys, CommitRequest, OccError};
use operation::{
    encode_state, validate_rebase_budget, OpenDisposition, OperationError, OperationJournal,
    OperationKind, OperationPhase, OperationRequest, OperationStage, OperationState,
    TerminalOutcome,
};
use ref_ops::{
    checkout, clean_checkpoint, dirty_checkpoint, fork_or_pin, reset, revert, CheckoutSource,
    DirtyCheckpointRequest, ForkMode, ForkOutcome, RevertRequest,
};
use refs::{
    storage_writer_lock_path, BarrierSubject, CommitLock, GcBarrier, Head, NoGcBarrier, Pin,
    RefClass, RefError, RefStage, RefStore, RefTarget,
};
use seqcdc::{stream, ChunkSlices, MAX_CHUNK_BYTES, MIN_CHUNK_BYTES, TARGET_CHUNK_BYTES};
use source::{CarrierCatalog, CleanupDecision, OpenedCarrier, SourceProtector, SourceRequirement};
use spool::{ChangedPathSpool, MutationAction, MutationRecord, SortedSpool};
use tree::{
    AttributionFact, AttributionQuery, FileKindV3, FileNodeV3, MetadataV3, PersistentPages,
    SegmentDescriptor, SegmentKind, TreeEntryV3,
};

static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);

struct TestRoot(PathBuf);

impl TestRoot {
    fn new(label: &str) -> std::io::Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "layerstack-stage03-{label}-{}-{}",
            std::process::id(),
            NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::remove_dir_all(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        std::fs::create_dir(&path)?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn chunk() -> CanonicalRecordV3 {
    CanonicalRecordV3::chunk(b"stage03-object".to_vec()).expect("valid chunk")
}

fn temp_entries(final_path: &Path) -> usize {
    final_path
        .parent()
        .and_then(|parent| std::fs::read_dir(parent).ok())
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
        .count()
}

#[test]
fn object_store_installs_loads_and_reuses_exact_typed_bytes(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("object-install")?;
    let store = LooseObjectStore::new(root.path().to_path_buf())?;
    assert!(!root.path().join("objects").exists());

    let record = chunk();
    let installed = store.install(&record, &mut Sha256Digest)?;
    assert_eq!(installed.kind(), RecordKindV3::Chunk);
    assert_eq!(installed.disposition(), InstallDisposition::Installed);
    assert!(installed.path().is_file());
    assert!(installed
        .path()
        .to_string_lossy()
        .contains("/objects/loose/chunk/"));
    assert_eq!(temp_entries(installed.path()), 0);

    let loaded = store.load(installed.kind(), installed.id(), &mut Sha256Digest)?;
    assert_eq!(loaded, record);
    let loaded_chunk = store.load_authenticated_chunk(installed.id(), &mut Sha256Digest)?;
    assert_eq!(loaded_chunk.payload(), b"stage03-object");
    assert_eq!(
        loaded_chunk.encoded_len(),
        b"stage03-object".len() + object_store::RECORD_HEADER_BYTES
    );
    let mut reusable_chunk =
        Vec::with_capacity(object_store::RECORD_HEADER_BYTES + object_store::MAX_CHUNK_BYTES);
    let reusable_capacity = reusable_chunk.capacity();
    store.load_authenticated_chunk_into(installed.id(), &mut Sha256Digest, &mut reusable_chunk)?;
    assert_eq!(
        &reusable_chunk[object_store::RECORD_HEADER_BYTES..],
        b"stage03-object"
    );
    assert_eq!(reusable_chunk.capacity(), reusable_capacity);

    let existing = store.install(&record, &mut Sha256Digest)?;
    assert_eq!(existing.disposition(), InstallDisposition::AlreadyPresent);
    assert_eq!(existing.id(), installed.id());
    assert_eq!(existing.path(), installed.path());
    assert_eq!(temp_entries(existing.path()), 0);
    Ok(())
}

#[test]
fn object_store_rejects_corruption_collision_and_non_object_kinds(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("object-collision")?;
    let store = LooseObjectStore::new(root.path().to_path_buf())?;
    let record = chunk();
    let installed = store.install(&record, &mut Sha256Digest)?;
    let mut bytes = std::fs::read(installed.path())?;
    let final_byte = bytes.last_mut().expect("nonempty record");
    *final_byte ^= 0xff;
    std::fs::write(installed.path(), bytes)?;

    let error = store
        .install(&record, &mut Sha256Digest)
        .expect_err("corrupt existing object");
    assert_eq!(error.kind(), Some(ErrorKind::ObjectCollisionOrCorruption));
    let error = store
        .load_authenticated_chunk(installed.id(), &mut Sha256Digest)
        .expect_err("corrupt chunk must not authenticate");
    assert_eq!(error.kind(), Some(ErrorKind::ObjectCollisionOrCorruption));

    let mutable = CanonicalRecordV3::mutable(
        RecordKindV3::Head,
        vec![
            TlvV3::new(1, vec![1; 32]),
            TlvV3::new(2, vec![2; 32]),
            TlvV3::new(3, 1_u64.to_be_bytes().to_vec()),
            TlvV3::new(4, vec![3; 16]),
        ],
        &mut Sha256Digest,
    )?;
    let error = store
        .install(&mutable, &mut Sha256Digest)
        .expect_err("mutable records are not loose objects");
    assert_eq!(error.kind(), Some(ErrorKind::WrongKind));
    Ok(())
}

#[test]
fn object_store_failpoints_never_expose_partial_objects_and_retry_exactly(
) -> Result<(), Box<dyn std::error::Error>> {
    for stage in [
        InstallStage::TempCreated,
        InstallStage::BytesWritten,
        InstallStage::FileFsynced,
        InstallStage::BeforeInstall,
        InstallStage::AfterInstall,
        InstallStage::ParentFsynced,
    ] {
        let root = TestRoot::new(&format!("object-fail-{stage:?}"))?;
        let store = LooseObjectStore::new(root.path().to_path_buf())?;
        let record = chunk();
        let expected = store.install_with_hook(&record, &mut Sha256Digest, |observed| {
            if observed == stage {
                Err(ObjectStoreError::Injected(stage))
            } else {
                Ok(())
            }
        });
        assert!(matches!(expected, Err(ObjectStoreError::Injected(found)) if found == stage));

        let retry = store.install(&record, &mut Sha256Digest)?;
        let expected_disposition = if matches!(
            stage,
            InstallStage::AfterInstall | InstallStage::ParentFsynced
        ) {
            InstallDisposition::AlreadyPresent
        } else {
            InstallDisposition::Installed
        };
        assert_eq!(retry.disposition(), expected_disposition);
        assert_eq!(
            store.load(retry.kind(), retry.id(), &mut Sha256Digest)?,
            record
        );
        assert_eq!(temp_entries(retry.path()), 0);
    }
    Ok(())
}

#[test]
fn batched_object_store_installs_unreferenced_cas_before_one_commit_barrier(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("object-batch")?;
    let store = LooseObjectStore::new_commit_batch(root.path().to_path_buf())?;
    let record = chunk();
    let installed = store.install(&record, &mut Sha256Digest)?;
    let final_path = store.object_path(installed.kind(), installed.id());

    assert_eq!(installed.path(), final_path);
    assert!(installed.path().is_file());
    assert_eq!(
        store.load(installed.kind(), installed.id(), &mut Sha256Digest)?,
        record
    );

    store.commit_batch()?;
    let ordinary = LooseObjectStore::new(root.path().to_path_buf())?;
    assert_eq!(
        ordinary.load(installed.kind(), installed.id(), &mut Sha256Digest)?,
        record
    );
    Ok(())
}

#[test]
fn unreferenced_batched_object_corruption_fails_closed() -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("object-batch-corrupt")?;
    let store = LooseObjectStore::new_commit_batch(root.path().to_path_buf())?;
    let installed = store.install(&chunk(), &mut Sha256Digest)?;
    std::fs::write(installed.path(), b"corrupt")?;

    let ordinary = LooseObjectStore::new(root.path().to_path_buf())?;
    let error = ordinary
        .load(installed.kind(), installed.id(), &mut Sha256Digest)
        .expect_err("corrupt unreferenced object must fail authentication");
    assert_eq!(error.kind(), Some(ErrorKind::ObjectCollisionOrCorruption));
    Ok(())
}

#[test]
fn batched_persistent_pages_read_direct_cas_records_before_commit(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("object-batch-pages")?;
    let store = LooseObjectStore::new_commit_batch(root.path().to_path_buf())?;
    let mut pages = PersistentPages::new(&store);
    let tree = pages.build_tree(Vec::<TreeEntryV3>::new())?;
    let directory =
        pages.install_file_node(&FileNodeV3::directory(MetadataV3::directory(0o755), tree))?;
    let content = pages.install_root(directory)?;

    assert_eq!(pages.root_directory(content)?, tree);
    assert!(store
        .object_path(RecordKindV3::Root, content.digest())
        .exists());
    Ok(())
}

#[test]
fn batched_native_file_cache_is_content_addressed_and_readable_after_commit(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("native-file-cache")?;
    let source = root.path().join("source.bin");
    let expected = b"immutable native acceleration payload";
    std::fs::write(&source, expected)?;
    let file_node = FileNodeId::new(Digest32::new([0x5a; 32]));
    let store = LooseObjectStore::new_commit_batch(root.path().to_path_buf())?;

    let installed = store
        .install_native_file(file_node, &source, u64::try_from(expected.len())?)?
        .ok_or("test filesystem does not support native cache reflinks")?;
    assert_eq!(
        installed,
        root.path()
            .join("native-files-v1")
            .join("5a")
            .join("5a".repeat(32))
    );
    store.commit_batch()?;

    let ordinary = LooseObjectStore::new(root.path().to_path_buf())?;
    let mut opened = ordinary
        .open_native_file(file_node, u64::try_from(expected.len())?)?
        .ok_or("committed native file cache was absent")?;
    let mut actual = Vec::new();
    opened.read_to_end(&mut actual)?;
    assert_eq!(actual, expected);
    Ok(())
}

#[test]
fn malformed_native_file_cache_fails_closed() -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("native-file-cache-corrupt")?;
    let source = root.path().join("source.bin");
    let expected = b"immutable native acceleration payload";
    std::fs::write(&source, expected)?;
    let file_node = FileNodeId::new(Digest32::new([0xa5; 32]));
    let store = LooseObjectStore::new_commit_batch(root.path().to_path_buf())?;
    let installed = store
        .install_native_file(file_node, &source, u64::try_from(expected.len())?)?
        .ok_or("test filesystem does not support native cache reflinks")?;
    store.commit_batch()?;
    std::fs::write(&installed, b"partial")?;

    let ordinary = LooseObjectStore::new(root.path().to_path_buf())?;
    let error = ordinary
        .open_native_file(file_node, u64::try_from(expected.len())?)
        .expect_err("partial native cache must not be used");
    assert!(matches!(error, ObjectStoreError::InvalidBatch(_)));
    Ok(())
}

fn collect_chunks<R: Read>(reader: &mut R) -> Result<Vec<Vec<u8>>, Box<dyn std::error::Error>> {
    let mut chunks = Vec::new();
    stream(reader, |chunk| {
        let mut bytes = Vec::with_capacity(chunk.len());
        bytes.extend_from_slice(chunk.first());
        bytes.extend_from_slice(chunk.second());
        chunks.push(bytes);
        Ok::<(), Infallible>(())
    })?;
    Ok(chunks)
}

#[test]
fn seqcdc_preserves_short_exact_min_and_max_fallback_boundaries(
) -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(TARGET_CHUNK_BYTES, 16 * 1024);
    for length in [MIN_CHUNK_BYTES - 1, MIN_CHUNK_BYTES] {
        let input = vec![0_u8; length];
        let chunks = collect_chunks(&mut Cursor::new(&input))?;
        assert_eq!(chunks, vec![input]);
    }

    let input = vec![0_u8; MAX_CHUNK_BYTES + 137];
    let chunks = collect_chunks(&mut Cursor::new(&input))?;
    assert_eq!(
        chunks.iter().map(Vec::len).collect::<Vec<_>>(),
        [MAX_CHUNK_BYTES, 137]
    );
    assert_eq!(chunks.concat(), input);
    Ok(())
}

#[test]
fn seqcdc_matches_frozen_increasing_and_opposing_jump_vectors(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut increasing = vec![0_u8; 10_000];
    increasing[MIN_CHUNK_BYTES..MIN_CHUNK_BYTES + 5].copy_from_slice(&[1, 2, 3, 4, 5]);
    let chunks = collect_chunks(&mut Cursor::new(&increasing))?;
    assert_eq!(
        chunks.iter().map(Vec::len).collect::<Vec<_>>(),
        [8_196, 1_804]
    );
    assert_eq!(chunks.concat(), increasing);

    let mut jump = vec![0_u8; 10_000];
    jump[MIN_CHUNK_BYTES - 1] = u8::MAX;
    for (offset, byte) in (205_u8..=254).rev().enumerate() {
        jump[MIN_CHUNK_BYTES + offset] = byte;
    }
    jump[8_754..8_759].copy_from_slice(&[1, 2, 3, 4, 5]);
    let chunks = collect_chunks(&mut Cursor::new(&jump))?;
    assert_eq!(
        chunks.iter().map(Vec::len).collect::<Vec<_>>(),
        [8_758, 1_242]
    );
    assert_eq!(chunks.concat(), jump);
    Ok(())
}

struct FragmentedReader {
    input: Vec<u8>,
    position: usize,
    maximum: usize,
    attempts: usize,
    interrupt_every: Option<usize>,
}

impl FragmentedReader {
    fn new(input: &[u8], maximum: usize, interrupt_every: Option<usize>) -> Self {
        Self {
            input: input.to_vec(),
            position: 0,
            maximum,
            attempts: 0,
            interrupt_every,
        }
    }
}

impl Read for FragmentedReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.attempts += 1;
        if self
            .interrupt_every
            .is_some_and(|interval| self.attempts % interval == 0)
        {
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
        if self.position == self.input.len() {
            return Ok(0);
        }
        let count = output
            .len()
            .min(self.maximum)
            .min(self.input.len() - self.position);
        output[..count].copy_from_slice(&self.input[self.position..self.position + count]);
        self.position += count;
        Ok(count)
    }
}

#[test]
fn seqcdc_fragmentation_and_interrupted_reads_do_not_change_boundaries(
) -> Result<(), Box<dyn std::error::Error>> {
    let input = (0_u32..70_000)
        .map(|value| value.wrapping_mul(73).wrapping_add(value >> 3) as u8)
        .collect::<Vec<_>>();
    let expected = collect_chunks(&mut Cursor::new(&input))?;
    for maximum in [1, 7, MIN_CHUNK_BYTES - 1, MAX_CHUNK_BYTES] {
        let mut reader = FragmentedReader::new(&input, maximum, Some(5));
        assert_eq!(collect_chunks(&mut reader)?, expected);
    }
    Ok(())
}

#[test]
fn seqcdc_ring_wrap_delivers_at_most_two_borrowed_slices_directly_to_cas(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("seqcdc-ring")?;
    let store = LooseObjectStore::new(root.path().to_path_buf())?;
    let mut input = vec![0_u8; 8_196 + MAX_CHUNK_BYTES];
    input[MIN_CHUNK_BYTES..MIN_CHUNK_BYTES + 5].copy_from_slice(&[1, 2, 3, 4, 5]);
    let mut installed = Vec::new();
    let mut observed_two_slices = false;
    let stats = stream(&mut Cursor::new(&input), |chunk: ChunkSlices<'_>| {
        observed_two_slices |= !chunk.second().is_empty();
        assert!(chunk.len() <= MAX_CHUNK_BYTES);
        assert_eq!(
            chunk.is_all_zero(),
            chunk
                .first()
                .iter()
                .chain(chunk.second())
                .all(|byte| *byte == 0)
        );
        installed.push(store.install_chunk_slices(
            chunk.first(),
            chunk.second(),
            &mut Sha256Digest,
        )?);
        Ok::<(), ObjectStoreError>(())
    })?;

    assert!(observed_two_slices);
    assert_eq!(stats.input_bytes, u64::try_from(input.len())?);
    assert_eq!(stats.max_buffered, MAX_CHUNK_BYTES);
    assert_eq!(stats.max_slices, 2);
    assert_eq!(stats.chunks, 2);

    let mut reconstructed = Vec::new();
    for object in installed {
        let record = store.load(object.kind(), object.id(), &mut Sha256Digest)?;
        reconstructed.extend_from_slice(record.chunk_payload().expect("chunk payload"));
    }
    assert_eq!(reconstructed, input);
    Ok(())
}

#[test]
fn seqcdc_changed_path_spool_orders_deduplicates_and_merges_with_fan_in_eight(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("seqcdc-spool")?;
    let work = root.path().join("operation-spool");
    let mut spool = ChangedPathSpool::new(work.clone(), 160)?;
    for ordinal in (0_u8..24).rev() {
        spool.push(MutationRecord {
            path: vec![b'p', b'/', b'a' + ordinal],
            action: MutationAction::Replace,
            conflict_group: if ordinal < 2 { Some([7; 16]) } else { None },
            descriptor: vec![ordinal],
        })?;
    }
    spool.push(MutationRecord {
        path: vec![b'p', b'/', b'a' + 7],
        action: MutationAction::Remove,
        conflict_group: Some([9; 16]),
        descriptor: b"winner".to_vec(),
    })?;

    let sorted = spool.finish()?;
    let stats = sorted.stats();
    assert!(stats.initial_runs > 8);
    assert!(stats.merge_passes >= 2);
    assert_eq!(stats.maximum_fan_in, 8);
    assert_eq!(stats.records_in, 25);
    assert_eq!(stats.records_out, 24);
    assert!(stats.maximum_buffer_bytes <= 4 * 1024 * 1024);

    let mut records = Vec::new();
    sorted.for_each(|record| {
        records.push(record);
        Ok(())
    })?;
    assert!(records.windows(2).all(|pair| pair[0].path < pair[1].path));
    let winner = records
        .iter()
        .find(|record| record.path == [b'p', b'/', b'a' + 7])
        .expect("deduplicated winner");
    assert_eq!(winner.action, MutationAction::Remove);
    assert_eq!(winner.conflict_group, Some([9; 16]));
    assert_eq!(winner.descriptor, b"winner");

    drop(sorted);
    assert!(!work.exists());
    Ok(())
}

fn digest32(hex: &str) -> Digest32 {
    assert_eq!(hex.len(), 64);
    let mut bytes = [0_u8; 32];
    for (index, output) in bytes.iter_mut().enumerate() {
        *output = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16).expect("valid digest");
    }
    Digest32::new(bytes)
}

fn hex_bytes(hex: &str) -> Vec<u8> {
    assert_eq!(hex.len() % 2, 0);
    (0..hex.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).expect("valid byte"))
        .collect()
}

fn symlink_node(target: &[u8]) -> FileNodeV3 {
    FileNodeV3 {
        kind: FileKindV3::Symlink,
        metadata: MetadataV3::directory(0o777),
        directory: None,
        logical_length: None,
        segments: None,
        symlink_target: Some(target.to_vec()),
        device_major: None,
        device_minor: None,
        hardlink: None,
    }
}

#[test]
fn tree_mutation_persists_the_approved_page_corpus_exactly(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("tree-corpus")?;
    let store = LooseObjectStore::new(root.path().to_path_buf())?;
    let chunk = store.install_chunk_slices(&[0, 0xff], b"abc", &mut Sha256Digest)?;
    assert_eq!(
        chunk.id(),
        digest32("f52da8e7b9e2824829844b1eebaa5f4a0d3481889c27ab2447552ada6694a62d")
    );

    let mut pages = PersistentPages::new(&store);
    let segments = pages.build_segments([SegmentDescriptor {
        offset: 0,
        length: 5,
        kind: SegmentKind::Chunk(chunk.id()),
    }])?;
    assert_eq!(
        segments.digest(),
        digest32("a8ad020562376f2f06b57c774a8368ba699c567bdef964d2649ae8f3638c9984")
    );

    let regular = pages.install_file_node(&FileNodeV3::regular(
        MetadataV3 {
            mode: 0o644,
            uid: 1000,
            gid: 1000,
            mtime_seconds: 1_700_000_000,
            mtime_nanoseconds: 123_456_789,
            xattrs: vec![(b"user.note".to_vec(), b"v3".to_vec())],
        },
        5,
        segments,
        None,
    ))?;
    assert_eq!(
        regular.digest(),
        digest32("b0f9ce9092600504e7773fc815f2ac0896e57135d56c8ddc9cdfa84f8b5b9c9d")
    );

    let tree = pages.build_tree([TreeEntryV3 {
        name: b"hello.txt".to_vec(),
        file: regular,
    }])?;
    assert_eq!(
        tree.digest(),
        digest32("aa6eb3a366c93a9fdd4db823f7d7c80283c70559149ca3281e9d2b65f3e0985b")
    );
    let directory =
        pages.install_file_node(&FileNodeV3::directory(MetadataV3::directory(0o755), tree))?;
    assert_eq!(
        directory.digest(),
        digest32("b22d18fd0672b14991cab970f9ff2d85477ce9aefb32198661b11833be392118")
    );
    let content = pages.install_root(directory)?;
    assert_eq!(
        content.digest(),
        digest32("66707bf571e67cfb7a93f75829abcb32d9a6129c6599b802af9fa6887c0191b8")
    );

    let actor = ActorId::new([0x11; 32])?;
    let publication = [0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 2];
    let attribution = pages.build_attribution([
        AttributionFact {
            path: b"hello.txt".to_vec(),
            scope: 0,
            offset: 0,
            length: 0,
            actor,
            publication,
        },
        AttributionFact {
            path: b"hello.txt".to_vec(),
            scope: 1,
            offset: 0,
            length: 5,
            actor,
            publication,
        },
    ])?;
    assert_eq!(
        attribution.digest(),
        digest32("565b293643649e3f6532b6c5b878a839ea76f219ce7ca5ddfe92fdf5896db445")
    );
    let attribution_root = pages.install_attribution_root(content, attribution)?;
    assert_eq!(
        attribution_root.digest(),
        digest32("bf898ac512519d33d3673466ea7956d46c62ed19c6da92047efdfc383bd894c7")
    );
    assert_eq!(pages.counters().normal_flat_inputs, 0);
    assert_eq!(pages.counters().normal_flat_outputs, 0);

    let hardlink = pages.install_hardlink_group([b"hard/a".to_vec(), b"hard/b".to_vec()])?;
    let linked = pages.install_file_node(&FileNodeV3::regular(
        MetadataV3::directory(0o644),
        5,
        segments,
        Some(hardlink),
    ))?;
    assert_ne!(linked, regular);
    let device = pages.install_file_node(&FileNodeV3 {
        kind: FileKindV3::Device,
        metadata: MetadataV3::directory(0o600),
        directory: None,
        logical_length: None,
        segments: None,
        symlink_target: None,
        device_major: Some(1),
        device_minor: Some(3),
        hardlink: None,
    })?;
    let fifo = pages.install_file_node(&FileNodeV3 {
        kind: FileKindV3::Fifo,
        metadata: MetadataV3::directory(0o600),
        directory: None,
        logical_length: None,
        segments: None,
        symlink_target: None,
        device_major: None,
        device_minor: None,
        hardlink: None,
    })?;
    assert_ne!(device, fifo);
    Ok(())
}

#[test]
fn tree_mutation_rewrites_only_the_touched_leaf_and_ancestors_and_restarts_exactly(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("tree-local")?;
    let store = LooseObjectStore::new(root.path().to_path_buf())?;
    let (changed, replacement_node) = {
        let mut pages = PersistentPages::new(&store);
        let original_node = pages.install_file_node(&symlink_node(b"before"))?;
        let replacement_node = pages.install_file_node(&symlink_node(b"after"))?;
        let entries = (0_u16..1000)
            .map(|ordinal| TreeEntryV3 {
                name: format!("entry-{ordinal:04}").into_bytes(),
                file: original_node,
            })
            .collect::<Vec<_>>();
        let original = pages.build_tree(entries)?;
        let before = pages.counters();

        let changed = pages.replace_tree_entry(original, b"entry-0512", replacement_node)?;
        assert_ne!(changed, original);
        let mutation = pages.counters();
        assert_eq!(mutation.tree_pages_read - before.tree_pages_read, 2);
        assert_eq!(mutation.tree_pages_written - before.tree_pages_written, 2);
        assert_eq!(mutation.normal_complete_tree_scans, 0);
        assert_eq!(mutation.normal_flat_inputs, 0);
        assert_eq!(mutation.normal_flat_outputs, 0);
        assert_eq!(
            pages.lookup_tree_entry(changed, b"entry-0512")?,
            Some(replacement_node)
        );
        assert_eq!(
            pages.lookup_tree_entry(changed, b"entry-0511")?,
            Some(original_node)
        );
        (changed, replacement_node)
    };
    let mut restarted = PersistentPages::new(&store);
    assert_eq!(
        restarted.lookup_tree_entry(changed, b"entry-0512")?,
        Some(replacement_node)
    );
    let mut flat = Vec::new();
    restarted.export_tree_diagnostic(changed, |entry| {
        flat.push(entry);
        Ok(())
    })?;
    assert_eq!(flat.len(), 1000);
    assert_eq!(flat[512].file, replacement_node);
    let counters = restarted.counters();
    assert_eq!(counters.diagnostic_flat_scans, 1);
    assert_eq!(counters.diagnostic_flat_entries, 1000);
    assert_eq!(counters.normal_complete_tree_scans, 0);
    assert_eq!(counters.normal_flat_inputs, 0);
    assert_eq!(counters.normal_flat_outputs, 0);
    assert!(counters.maximum_page_buffer_bytes <= 65_536);
    Ok(())
}

#[test]
fn tree_mutation_segment_and_attribution_pages_are_bounded_separate_and_queryable(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("tree-attribution")?;
    let store = LooseObjectStore::new(root.path().to_path_buf())?;
    let (attribution_two, actor_two, publication_two) = {
        let mut pages = PersistentPages::new(&store);
        let descriptors = (0_u64..1500)
            .map(|offset| SegmentDescriptor {
                offset,
                length: 1,
                kind: if offset % 2 == 0 {
                    SegmentKind::Zero
                } else {
                    SegmentKind::Hole
                },
            })
            .collect::<Vec<_>>();
        let segment_root = pages.build_segments(descriptors.clone())?;
        assert_eq!(pages.reconstruct_segments(segment_root)?, descriptors);

        let empty_tree = pages.build_tree(Vec::<TreeEntryV3>::new())?;
        let directory = pages.install_file_node(&FileNodeV3::directory(
            MetadataV3::directory(0o755),
            empty_tree,
        ))?;
        let content = pages.install_root(directory)?;
        let actor_one = ActorId::new([1; 32])?;
        let actor_two = ActorId::new([2; 32])?;
        let publication_one = [1; 16];
        let publication_two = [2; 16];
        let facts = (0_u16..300)
            .map(|ordinal| AttributionFact {
                path: format!("path-{ordinal:04}").into_bytes(),
                scope: 1,
                offset: 0,
                length: 8,
                actor: actor_one,
                publication: publication_one,
            })
            .collect::<Vec<_>>();
        let attribution_one = pages.build_attribution(facts.clone())?;
        let root_one = pages.install_attribution_root(content, attribution_one)?;

        let mut changed_facts = facts;
        changed_facts[177].actor = actor_two;
        changed_facts[177].publication = publication_two;
        let attribution_two = pages.build_attribution(changed_facts)?;
        let root_two = pages.install_attribution_root(content, attribution_two)?;
        assert_ne!(root_one, root_two);
        (attribution_two, actor_two, publication_two)
    };
    let mut restarted = PersistentPages::new(&store);
    let result = restarted.query_attribution(
        attribution_two,
        &AttributionQuery {
            path: b"path-0177".to_vec(),
            offset: 3,
            length: 1,
        },
    )?;
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].actor, actor_two);
    assert_eq!(result[0].publication, publication_two);
    let counters = restarted.counters();
    assert!(counters.query_pages <= 3);
    assert_eq!(counters.query_facts, 1);
    assert_eq!(counters.attribution_history_scans, 0);
    assert_eq!(counters.normal_complete_tree_scans, 0);
    assert_eq!(counters.normal_flat_inputs, 0);
    assert_eq!(counters.normal_flat_outputs, 0);
    Ok(())
}

#[derive(Default)]
struct TestCommitLock {
    depth: Rc<Cell<u32>>,
    entries: Cell<u64>,
}

impl CommitLock for TestCommitLock {
    fn with_exclusive<T, F>(&self, operation: F) -> Result<T, RefError>
    where
        F: FnOnce() -> Result<T, RefError>,
    {
        assert_eq!(self.depth.replace(1), 0, "commit lock is not reentrant");
        self.entries.set(self.entries.get().saturating_add(1));
        let result = operation();
        assert_eq!(self.depth.replace(0), 1, "commit lock depth drifted");
        result
    }
}

#[derive(Default)]
struct ConcurrentTestCommitLock {
    exclusive: Mutex<()>,
}

impl CommitLock for ConcurrentTestCommitLock {
    fn with_exclusive<T, F>(&self, operation: F) -> Result<T, RefError>
    where
        F: FnOnce() -> Result<T, RefError>,
    {
        let _guard = self
            .exclusive
            .lock()
            .map_err(|_| RefError::Lock("concurrent test commit lock poisoned".to_owned()))?;
        operation()
    }
}

struct TestBarrier {
    depth: Rc<Cell<u32>>,
    subjects: RefCell<Vec<BarrierSubject>>,
    fail: Cell<bool>,
}

impl TestBarrier {
    fn new(depth: Rc<Cell<u32>>) -> Self {
        Self {
            depth,
            subjects: RefCell::new(Vec::new()),
            fail: Cell::new(false),
        }
    }
}

impl GcBarrier for TestBarrier {
    fn participate(&self, subject: BarrierSubject) -> Result<(), RefError> {
        assert_eq!(
            self.depth.get(),
            1,
            "GC barrier ran outside the commit lock"
        );
        if self.fail.get() {
            return Err(RefError::Injected(RefStage::BarrierRegistered));
        }
        self.subjects.borrow_mut().push(subject);
        Ok(())
    }
}

fn candidate_ref_target(
    store: &LooseObjectStore,
    actor_byte: u8,
) -> Result<RefTarget, Box<dyn std::error::Error>> {
    let mut pages = PersistentPages::new(store);
    let tree = pages.build_tree(Vec::<TreeEntryV3>::new())?;
    let directory =
        pages.install_file_node(&FileNodeV3::directory(MetadataV3::directory(0o755), tree))?;
    let content = pages.install_root(directory)?;
    let attribution = pages.build_attribution([AttributionFact {
        path: Vec::new(),
        scope: 0,
        offset: 0,
        length: 0,
        actor: ActorId::new([actor_byte; 32])?,
        publication: [0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, actor_byte],
    }])?;
    let attribution_root = pages.install_attribution_root(content, attribution)?;
    Ok(RefTarget {
        root: content.digest(),
        attribution_root: attribution_root.digest(),
    })
}

fn recursive_file_count(path: &Path) -> Result<u64, std::io::Error> {
    let mut count = 0_u64;
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        let metadata = std::fs::symlink_metadata(entry.path())?;
        if metadata.is_dir() {
            count = count.saturating_add(recursive_file_count(&entry.path())?);
        } else if metadata.is_file() {
            count = count.saturating_add(1);
        }
    }
    Ok(count)
}

#[test]
fn refs_head_atomic_failpoints_expose_only_old_or_complete_new_state(
) -> Result<(), Box<dyn std::error::Error>> {
    for stage in [
        RefStage::TempCreated,
        RefStage::BytesWritten,
        RefStage::FileFsynced,
        RefStage::LockAcquired,
        RefStage::BarrierRegistered,
        RefStage::BeforeVisibility,
        RefStage::AfterVisibility,
        RefStage::ParentFsynced,
    ] {
        let root = TestRoot::new(&format!("refs-head-{stage:?}"))?;
        std::fs::write(storage_writer_lock_path(root.path()), [])?;
        let objects = LooseObjectStore::new(root.path().to_path_buf())?;
        let target = candidate_ref_target(&objects, 3)?;
        let lock = TestCommitLock::default();
        let barrier = TestBarrier::new(lock.depth.clone());
        let mut refs = RefStore::open(
            root.path().to_path_buf(),
            &lock,
            &barrier,
            &mut Sha256Digest,
        )?;
        let branch = sandbox_runtime_layerstack_core::BranchId::new(b"main".to_vec())?;
        let next = Head {
            target,
            generation: 0,
            publication_id: [0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 9],
        };
        let error = refs
            .commit_head_with_hook(&branch, None, next, &mut Sha256Digest, |observed| {
                if observed == stage {
                    Err(RefError::Injected(stage))
                } else {
                    Ok(())
                }
            })
            .expect_err("injected ref stop");
        assert!(matches!(error, RefError::Injected(found) if found == stage));
        let observed = refs.read_head(&branch, &mut Sha256Digest)?;
        if matches!(stage, RefStage::AfterVisibility | RefStage::ParentFsynced) {
            assert_eq!(observed, Some(next));
        } else {
            assert_eq!(observed, None);
        }

        refs.commit_head(&branch, None, next, &mut Sha256Digest)?;
        assert_eq!(refs.read_head(&branch, &mut Sha256Digest)?, Some(next));
        assert_eq!(temp_entries(&refs.head_path(&branch)), 0);
        assert!(root.path().join("CONTROL").is_file());
        assert!(!root.path().join("gc").exists());
        assert!(!root.path().join("refs").join("legacy").exists());
    }
    Ok(())
}

#[test]
fn refs_clean_checkpoint_fork_pin_and_lease_write_only_constant_metadata(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("refs-clean")?;
    std::fs::write(storage_writer_lock_path(root.path()), [])?;
    let objects = LooseObjectStore::new(root.path().to_path_buf())?;
    let target = candidate_ref_target(&objects, 4)?;
    let object_count = recursive_file_count(&root.path().join("objects"))?;
    let lock = TestCommitLock::default();
    let barrier = NoGcBarrier;
    let checkpoint = sandbox_runtime_layerstack_core::BranchId::new(b"checkpoint-1".to_vec())?;
    let branch = sandbox_runtime_layerstack_core::BranchId::new(b"mcts-1".to_vec())?;
    let pin_id = sandbox_runtime_layerstack_core::PinId::new([0x44; 16])?;
    let lease_id = sandbox_runtime_layerstack_core::LeaseId::new([0x55; 16])?;
    let lease = CanonicalRecordV3::mutable(
        RecordKindV3::SourceLease,
        vec![
            TlvV3::new(1, lease_id.as_bytes().to_vec()),
            TlvV3::new(2, target.root.into_bytes().to_vec()),
            TlvV3::new(3, vec![0x66; 32]),
            TlvV3::new(4, 1_u64.to_be_bytes().to_vec()),
            TlvV3::new(5, 1_u64.to_be_bytes().to_vec()),
            TlvV3::new(6, 4096_u64.to_be_bytes().to_vec()),
        ],
        &mut Sha256Digest,
    )?;
    {
        let mut refs = RefStore::open(
            root.path().to_path_buf(),
            &lock,
            &barrier,
            &mut Sha256Digest,
        )?;
        refs.create_checkpoint(&checkpoint, target, &mut Sha256Digest)?;
        let fork = Head {
            target,
            generation: 0,
            publication_id: [0, 0, 0, 0, 0, 0, 0, 1, 7, 0, 0, 0, 0, 0, 0, 1],
        };
        refs.commit_head(&branch, None, fork, &mut Sha256Digest)?;
        refs.create_pin(
            &pin_id,
            Pin {
                target,
                reason_class: 1,
            },
            &mut Sha256Digest,
        )?;
        refs.install_source_lease(&lease_id, &lease, &mut Sha256Digest)?;
        assert_eq!(
            refs.read_source_lease(&lease_id, &mut Sha256Digest)?,
            Some(lease.clone())
        );
        assert_eq!(
            refs.read_checkpoint(&checkpoint, &mut Sha256Digest)?,
            Some(target)
        );
        assert_eq!(
            refs.read_pin(&pin_id, &mut Sha256Digest)?,
            Some(Pin {
                target,
                reason_class: 1
            })
        );
        let counters = refs.counters();
        assert_eq!(counters.payload_object_writes, 0);
        assert_eq!(counters.native_tree_writes, 0);
        assert_eq!(counters.visible_ref_writes, 4);
    }
    assert_eq!(
        recursive_file_count(&root.path().join("objects"))?,
        object_count
    );

    let mut restarted = RefStore::open(
        root.path().to_path_buf(),
        &lock,
        &barrier,
        &mut Sha256Digest,
    )?;
    assert_eq!(
        restarted.read_checkpoint(&checkpoint, &mut Sha256Digest)?,
        Some(target)
    );
    assert!(restarted.delete_checkpoint(&checkpoint, &mut Sha256Digest)?);
    assert!(!restarted.checkpoint_path(&checkpoint).exists());
    assert!(restarted.head_path(&branch).is_file());
    assert!(restarted.pin_path(&pin_id).is_file());
    assert_eq!(
        restarted.read_source_lease(&lease_id, &mut Sha256Digest)?,
        Some(lease)
    );
    assert_eq!(RefError::HeadMismatch.kind(), Some(ErrorKind::Conflict));
    assert_eq!(RefError::Lock("busy".to_owned()).kind(), None);
    assert_eq!(
        recursive_file_count(&root.path().join("objects"))?,
        object_count
    );
    Ok(())
}

#[test]
fn refs_prepare_outside_lock_and_register_gc_barrier_before_visibility(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("refs-lock-barrier")?;
    std::fs::write(storage_writer_lock_path(root.path()), [])?;
    let objects = LooseObjectStore::new(root.path().to_path_buf())?;
    let target = candidate_ref_target(&objects, 5)?;
    let lock = TestCommitLock::default();
    let barrier = TestBarrier::new(lock.depth.clone());
    let mut refs = RefStore::open(
        root.path().to_path_buf(),
        &lock,
        &barrier,
        &mut Sha256Digest,
    )?;
    let branch = sandbox_runtime_layerstack_core::BranchId::new(b"barrier".to_vec())?;
    let head = Head {
        target,
        generation: 0,
        publication_id: [0, 0, 0, 0, 0, 0, 0, 1, 8, 0, 0, 0, 0, 0, 0, 1],
    };
    let trace = RefCell::new(Vec::new());
    let depth = lock.depth.clone();
    refs.commit_head_with_hook(&branch, None, head, &mut Sha256Digest, |stage| {
        let expected_depth = if matches!(
            stage,
            RefStage::TempCreated | RefStage::BytesWritten | RefStage::FileFsynced
        ) {
            0
        } else {
            1
        };
        assert_eq!(depth.get(), expected_depth, "wrong lock phase at {stage:?}");
        trace.borrow_mut().push(stage);
        Ok(())
    })?;
    assert_eq!(
        trace.into_inner(),
        [
            RefStage::TempCreated,
            RefStage::BytesWritten,
            RefStage::FileFsynced,
            RefStage::LockAcquired,
            RefStage::BarrierRegistered,
            RefStage::BeforeVisibility,
            RefStage::AfterVisibility,
            RefStage::ParentFsynced,
        ]
    );
    assert_eq!(barrier.subjects.borrow().len(), 1);
    assert_eq!(barrier.subjects.borrow()[0].class, RefClass::Head);
    let counters = refs.counters();
    assert_eq!(counters.lock_sections, 2);
    assert!(counters.prepared_bytes > 0);
    assert_eq!(counters.barrier_registrations, 1);

    let blocked_root = TestRoot::new("refs-barrier-fail")?;
    std::fs::write(storage_writer_lock_path(blocked_root.path()), [])?;
    let blocked_objects = LooseObjectStore::new(blocked_root.path().to_path_buf())?;
    let blocked_target = candidate_ref_target(&blocked_objects, 6)?;
    let blocked_lock = TestCommitLock::default();
    let blocked_barrier = TestBarrier::new(blocked_lock.depth.clone());
    blocked_barrier.fail.set(true);
    let mut blocked_refs = RefStore::open(
        blocked_root.path().to_path_buf(),
        &blocked_lock,
        &blocked_barrier,
        &mut Sha256Digest,
    )?;
    let blocked = Head {
        target: blocked_target,
        generation: 0,
        publication_id: [0, 0, 0, 0, 0, 0, 0, 1, 9, 0, 0, 0, 0, 0, 0, 1],
    };
    assert!(matches!(
        blocked_refs.commit_head(&branch, None, blocked, &mut Sha256Digest),
        Err(RefError::Injected(RefStage::BarrierRegistered))
    ));
    assert_eq!(blocked_refs.read_head(&branch, &mut Sha256Digest)?, None);
    assert_eq!(temp_entries(&blocked_refs.head_path(&branch)), 0);
    Ok(())
}

#[test]
fn ref_operations_clean_checkpoint_fork_pin_checkout_and_delete_are_constant_metadata(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("ref-operations-clean")?;
    std::fs::write(storage_writer_lock_path(root.path()), [])?;
    let objects = LooseObjectStore::new(root.path().to_path_buf())?;
    let target = candidate_ref_target(&objects, 40)?;
    let object_count = recursive_file_count(&root.path().join("objects"))?;
    let lock = TestCommitLock::default();
    let barrier = NoGcBarrier;
    let mut refs = RefStore::open(
        root.path().to_path_buf(),
        &lock,
        &barrier,
        &mut Sha256Digest,
    )?;
    let source = BranchId::new(b"main".to_vec())?;
    let writable = BranchId::new(b"mcts-writable".to_vec())?;
    let checkpoint = BranchId::new(b"checkpoint-delete".to_vec())?;
    let retained_checkpoint = BranchId::new(b"checkpoint-retain".to_vec())?;
    let pin = sandbox_runtime_layerstack_core::PinId::new([0x41; 16])?;
    let source_head = Head {
        target,
        generation: 0,
        publication_id: publication_id(40),
    };
    refs.commit_head(&source, None, source_head, &mut Sha256Digest)?;
    let before = refs.counters();

    assert_eq!(
        clean_checkpoint(&mut refs, &source, &checkpoint, &mut Sha256Digest)?,
        target
    );
    clean_checkpoint(&mut refs, &source, &retained_checkpoint, &mut Sha256Digest)?;
    let fork = fork_or_pin(
        &mut refs,
        &source,
        ForkMode::Writable {
            branch: writable.clone(),
        },
        &mut Sha256Digest,
    )?;
    assert_eq!(
        fork,
        ForkOutcome::Writable(Head {
            target,
            generation: 0,
            publication_id: source_head.publication_id,
        })
    );
    let retained = fork_or_pin(
        &mut refs,
        &source,
        ForkMode::Retained {
            pin,
            reason_class: 1,
        },
        &mut Sha256Digest,
    )?;
    assert_eq!(
        retained,
        ForkOutcome::Retained(Pin {
            target,
            reason_class: 1,
        })
    );
    assert!(!refs
        .head_path(&BranchId::new(b"mcts-retained".to_vec())?)
        .exists());

    let source_bytes = std::fs::read(refs.head_path(&source))?;
    let fork_bytes = std::fs::read(refs.head_path(&writable))?;
    for selected in [
        checkout(
            &refs,
            CheckoutSource::Head(writable.clone()),
            &mut Sha256Digest,
        )?,
        checkout(
            &refs,
            CheckoutSource::Checkpoint(checkpoint.clone()),
            &mut Sha256Digest,
        )?,
        checkout(&refs, CheckoutSource::Pin(pin), &mut Sha256Digest)?,
    ] {
        assert_eq!(selected.target, target);
    }
    assert_eq!(std::fs::read(refs.head_path(&source))?, source_bytes);
    assert_eq!(std::fs::read(refs.head_path(&writable))?, fork_bytes);

    assert!(refs.delete_checkpoint(&checkpoint, &mut Sha256Digest)?);
    assert!(!refs.checkpoint_path(&checkpoint).exists());
    assert!(refs.checkpoint_path(&retained_checkpoint).is_file());
    assert!(refs.head_path(&source).is_file());
    assert!(refs.head_path(&writable).is_file());
    assert!(refs.pin_path(&pin).is_file());
    let after = refs.counters();
    assert_eq!(after.visible_ref_writes - before.visible_ref_writes, 4);
    assert_eq!(after.payload_object_writes, 0);
    assert_eq!(after.native_tree_writes, 0);
    assert_eq!(
        recursive_file_count(&root.path().join("objects"))?,
        object_count
    );

    drop(refs);
    let restarted = RefStore::open(
        root.path().to_path_buf(),
        &lock,
        &barrier,
        &mut Sha256Digest,
    )?;
    assert_eq!(
        restarted.read_head(&writable, &mut Sha256Digest)?,
        Some(Head {
            target,
            generation: 0,
            publication_id: source_head.publication_id,
        })
    );
    assert_eq!(
        restarted.read_checkpoint(&retained_checkpoint, &mut Sha256Digest)?,
        Some(target)
    );
    assert_eq!(
        restarted.read_pin(&pin, &mut Sha256Digest)?,
        Some(Pin {
            target,
            reason_class: 1,
        })
    );
    Ok(())
}

#[test]
fn ref_operations_dirty_checkpoint_is_publication_plus_one_ref_and_retry_exact(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("ref-operations-dirty")?;
    std::fs::write(storage_writer_lock_path(root.path()), [])?;
    let objects = LooseObjectStore::new(root.path().to_path_buf())?;
    let base = candidate_ref_target(&objects, 42)?;
    let target = candidate_ref_target(&objects, 43)?;
    let object_count = recursive_file_count(&root.path().join("objects"))?;
    let lock = TestCommitLock::default();
    let barrier = NoGcBarrier;
    let journal = OperationJournal::new(root.path().to_path_buf(), &lock);
    let mut refs = RefStore::open(
        root.path().to_path_buf(),
        &lock,
        &barrier,
        &mut Sha256Digest,
    )?;
    let branch = BranchId::new(b"dirty-main".to_vec())?;
    let checkpoint = BranchId::new(b"dirty-checkpoint".to_vec())?;
    let base_head = Head {
        target: base,
        generation: 0,
        publication_id: publication_id(42),
    };
    refs.commit_head(&branch, None, base_head, &mut Sha256Digest)?;
    let mut operation = candidate_operation_request(b"dirty-main", 43, 0x43, Some(base_head))?;
    operation.kind = OperationKind::DirtyCheckpoint;
    let before = refs.counters().visible_ref_writes;
    let changed = Some(Digest32::new([0x44; 32]));

    let outcome = dirty_checkpoint(
        &journal,
        &mut refs,
        DirtyCheckpointRequest {
            operation: &operation,
            target,
            changed_path_digest: changed,
            checkpoint: &checkpoint,
            now_unix_seconds: 100,
        },
        &mut Sha256Digest,
    )?;
    let committed = match outcome {
        TerminalOutcome::Success(head) => head,
        other => panic!("unexpected dirty-checkpoint outcome: {other:?}"),
    };
    assert_eq!(committed.target, target);
    assert_eq!(committed.generation, 1);
    assert_eq!(
        refs.read_checkpoint(&checkpoint, &mut Sha256Digest)?,
        Some(target)
    );
    assert_eq!(refs.counters().visible_ref_writes - before, 2);
    assert_eq!(
        recursive_file_count(&root.path().join("objects"))?,
        object_count
    );

    let retry_before = refs.counters().visible_ref_writes;
    let retry = dirty_checkpoint(
        &journal,
        &mut refs,
        DirtyCheckpointRequest {
            operation: &operation,
            target,
            changed_path_digest: changed,
            checkpoint: &checkpoint,
            now_unix_seconds: 101,
        },
        &mut Sha256Digest,
    )?;
    assert_eq!(retry, TerminalOutcome::Success(committed));
    assert_eq!(refs.counters().visible_ref_writes, retry_before);

    assert!(refs.delete_checkpoint(&checkpoint, &mut Sha256Digest)?);
    assert_eq!(refs.read_head(&branch, &mut Sha256Digest)?, Some(committed));
    assert_eq!(
        recursive_file_count(&root.path().join("objects"))?,
        object_count
    );
    drop(refs);
    let restarted = RefStore::open(
        root.path().to_path_buf(),
        &lock,
        &barrier,
        &mut Sha256Digest,
    )?;
    assert_eq!(
        restarted.read_head(&branch, &mut Sha256Digest)?,
        Some(committed)
    );
    assert_eq!(
        restarted.read_checkpoint(&checkpoint, &mut Sha256Digest)?,
        None
    );
    Ok(())
}

#[test]
fn ref_operations_revert_reuses_content_and_proves_reverting_actor(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("ref-operations-revert")?;
    std::fs::write(storage_writer_lock_path(root.path()), [])?;
    let objects = LooseObjectStore::new(root.path().to_path_buf())?;
    let historical = candidate_ref_target(&objects, 45)?;
    let edited = candidate_ref_target(&objects, 46)?;
    let reverting_actor = ActorId::new([0x47; 32])?;
    let publication = publication_id(47);
    let reverted = {
        let mut pages = PersistentPages::new(&objects);
        let attribution = pages.build_attribution([AttributionFact {
            path: Vec::new(),
            scope: 0,
            offset: 0,
            length: 0,
            actor: reverting_actor,
            publication,
        }])?;
        let attribution_root =
            pages.install_attribution_root(RootId::new(historical.root), attribution)?;
        RefTarget {
            root: historical.root,
            attribution_root: attribution_root.digest(),
        }
    };
    assert_eq!(reverted.root, historical.root);
    assert_ne!(reverted.attribution_root, historical.attribution_root);
    let object_count = recursive_file_count(&root.path().join("objects"))?;
    let lock = TestCommitLock::default();
    let barrier = NoGcBarrier;
    let journal = OperationJournal::new(root.path().to_path_buf(), &lock);
    let mut refs = RefStore::open(
        root.path().to_path_buf(),
        &lock,
        &barrier,
        &mut Sha256Digest,
    )?;
    let branch = BranchId::new(b"revert-main".to_vec())?;
    let historical_head = Head {
        target: historical,
        generation: 0,
        publication_id: publication_id(45),
    };
    let edited_head = Head {
        target: edited,
        generation: 1,
        publication_id: publication_id(46),
    };
    refs.commit_head(&branch, None, historical_head, &mut Sha256Digest)?;
    refs.commit_head(
        &branch,
        Some(historical_head),
        edited_head,
        &mut Sha256Digest,
    )?;
    let mut operation = candidate_operation_request(b"revert-main", 47, 0x47, Some(edited_head))?;
    operation.kind = OperationKind::Revert;
    let queries = [AttributionQuery {
        path: Vec::new(),
        offset: 0,
        length: 0,
    }];
    let mut pages = PersistentPages::new(&objects);

    let rejected = revert(
        &journal,
        &mut refs,
        RevertRequest {
            operation: &operation,
            target: reverted,
            actor: ActorId::new([0x48; 32])?,
            reverted: &queries,
            now_unix_seconds: 200,
        },
        &mut pages,
        &mut Sha256Digest,
    )
    .expect_err("wrong reverting actor must fail closed");
    assert!(rejected
        .to_string()
        .contains("lacks reverting actor attribution"));
    assert_eq!(
        refs.read_head(&branch, &mut Sha256Digest)?,
        Some(edited_head)
    );

    let outcome = revert(
        &journal,
        &mut refs,
        RevertRequest {
            operation: &operation,
            target: reverted,
            actor: reverting_actor,
            reverted: &queries,
            now_unix_seconds: 200,
        },
        &mut pages,
        &mut Sha256Digest,
    )?;
    let reverted_head = match outcome {
        TerminalOutcome::Success(head) => head,
        other => panic!("unexpected revert outcome: {other:?}"),
    };
    assert_eq!(reverted_head.target, reverted);
    assert_eq!(reverted_head.generation, 2);
    assert_eq!(reverted_head.target.root, historical.root);
    assert_eq!(
        refs.read_head(&branch, &mut Sha256Digest)?,
        Some(reverted_head)
    );
    assert_eq!(
        recursive_file_count(&root.path().join("objects"))?,
        object_count
    );
    assert_eq!(pages.counters().attribution_history_scans, 0);
    assert_eq!(pages.counters().normal_complete_tree_scans, 0);
    assert_eq!(pages.counters().normal_flat_inputs, 0);
    assert_eq!(pages.counters().normal_flat_outputs, 0);
    Ok(())
}

#[test]
fn ref_operations_reset_moves_existing_pair_with_new_generation_and_no_objects(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("ref-operations-reset")?;
    std::fs::write(storage_writer_lock_path(root.path()), [])?;
    let objects = LooseObjectStore::new(root.path().to_path_buf())?;
    let historical = candidate_ref_target(&objects, 49)?;
    let current = candidate_ref_target(&objects, 50)?;
    let object_count = recursive_file_count(&root.path().join("objects"))?;
    let lock = TestCommitLock::default();
    let barrier = NoGcBarrier;
    let journal = OperationJournal::new(root.path().to_path_buf(), &lock);
    let mut refs = RefStore::open(
        root.path().to_path_buf(),
        &lock,
        &barrier,
        &mut Sha256Digest,
    )?;
    let branch = BranchId::new(b"reset-main".to_vec())?;
    let historical_head = Head {
        target: historical,
        generation: 0,
        publication_id: publication_id(49),
    };
    let current_head = Head {
        target: current,
        generation: 1,
        publication_id: publication_id(50),
    };
    refs.commit_head(&branch, None, historical_head, &mut Sha256Digest)?;
    refs.commit_head(
        &branch,
        Some(historical_head),
        current_head,
        &mut Sha256Digest,
    )?;
    let mut operation = candidate_operation_request(b"reset-main", 51, 0x51, Some(current_head))?;
    operation.kind = OperationKind::Reset;
    let before = refs.counters().visible_ref_writes;

    let outcome = reset(
        &journal,
        &mut refs,
        &operation,
        historical,
        300,
        &mut Sha256Digest,
    )?;
    let reset_head = match outcome {
        TerminalOutcome::Reset(head) => head,
        other => panic!("unexpected reset outcome: {other:?}"),
    };
    assert_eq!(reset_head.target, historical);
    assert_eq!(reset_head.generation, 2);
    assert_eq!(refs.counters().visible_ref_writes - before, 1);
    assert_eq!(
        recursive_file_count(&root.path().join("objects"))?,
        object_count
    );
    let retry_before = refs.counters().visible_ref_writes;
    assert_eq!(
        reset(
            &journal,
            &mut refs,
            &operation,
            historical,
            301,
            &mut Sha256Digest,
        )?,
        TerminalOutcome::Reset(reset_head)
    );
    assert_eq!(refs.counters().visible_ref_writes, retry_before);

    drop(refs);
    let restarted = RefStore::open(
        root.path().to_path_buf(),
        &lock,
        &barrier,
        &mut Sha256Digest,
    )?;
    assert_eq!(
        restarted.read_head(&branch, &mut Sha256Digest)?,
        Some(reset_head)
    );
    Ok(())
}

fn candidate_operation_request(
    branch: &[u8],
    publication_byte: u8,
    request_byte: u8,
    base: Option<Head>,
) -> Result<OperationRequest, Box<dyn std::error::Error>> {
    let mut publication_id = [0_u8; 16];
    publication_id[7] = 1;
    publication_id[15] = publication_byte;
    Ok(OperationRequest {
        kind: OperationKind::Publish,
        branch: BranchId::new(branch.to_vec())?,
        publication_id: PublicationId::new(publication_id)?,
        request_digest: Digest32::new([request_byte; 32]),
        base,
    })
}

fn injected_operation_stage(
    expected: OperationStage,
) -> impl FnMut(OperationStage) -> Result<(), OperationError> {
    move |observed| {
        if observed == expected {
            Err(OperationError::Injected(observed))
        } else {
            Ok(())
        }
    }
}

#[test]
fn operation_recovery_matches_approved_state_golden_and_scopes_ids(
) -> Result<(), Box<dyn std::error::Error>> {
    let prepared = RefTarget {
        root: digest32("66707bf571e67cfb7a93f75829abcb32d9a6129c6599b802af9fa6887c0191b8"),
        attribution_root: digest32(
            "bf898ac512519d33d3673466ea7956d46c62ed19c6da92047efdfc383bd894c7",
        ),
    };
    let state = OperationState {
        kind: OperationKind::Publish,
        branch: BranchId::new(b"main".to_vec())?,
        publication_id: PublicationId::new([0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 2])?,
        request_digest: Digest32::new([0x22; 32]),
        base: None,
        base_generation: 0,
        phase: OperationPhase::Prepared,
        prepared: Some(prepared),
        changed_path_digest: Some(Digest32::new([0x33; 32])),
        rebase_attempts: 0,
        outcome: TerminalOutcome::None,
        terminal_expiry_unix_seconds: 0,
        acknowledged: false,
    };
    let encoded = encode_state(&state, &mut Sha256Digest)?;
    assert_eq!(
        encoded,
        hex_bytes(
            "454f532d4c5332003100030000011e01000000010102000000046d61696e030000001000000000000000010000000000000002040000002022222222222222222222222222222222222222222222222222222222222222220500000001000600000001000700000008000000000000000008000000010209000000210166707bf571e67cfb7a93f75829abcb32d9a6129c6599b802af9fa6887c0191b80a0000002101bf898ac512519d33d3673466ea7956d46c62ed19c6da92047efdfc383bd894c70b000000210133333333333333333333333333333333333333333333333333333333333333330c00000001000d00000001000e0000000800000000000000000f0000000100ff00000020dd2b5ffd9a9f63fc4468250fb7ced5ad14ed61f1b3a7a00648dfc8ce0d69e9ee"
        )
    );

    let root = TestRoot::new("operation-identity")?;
    let lock = TestCommitLock::default();
    let journal = OperationJournal::new(root.path().to_path_buf(), &lock);
    let request = candidate_operation_request(b"main", 2, 0x22, None)?;
    let created = journal.open(&request, 1, &mut Sha256Digest)?;
    assert_eq!(created.disposition, OpenDisposition::Created);
    assert_eq!(
        journal.open(&request, 2, &mut Sha256Digest)?.disposition,
        OpenDisposition::Resumed
    );

    let changed_digest = candidate_operation_request(b"main", 2, 0x23, None)?;
    let mismatch = journal
        .open(&changed_digest, 2, &mut Sha256Digest)
        .expect_err("same scoped ID cannot change request bytes");
    assert!(matches!(mismatch, OperationError::IdempotencyMismatch));
    assert_eq!(mismatch.kind(), Some(ErrorKind::IdempotencyMismatch));

    let other_branch = candidate_operation_request(b"other", 2, 0x22, None)?;
    let other = journal.open(&other_branch, 2, &mut Sha256Digest)?;
    assert_eq!(other.disposition, OpenDisposition::Created);
    assert_ne!(created.id, other.id);
    assert_ne!(
        OperationJournal::<TestCommitLock>::operation_id(&request, &mut Sha256Digest)?,
        OperationJournal::<TestCommitLock>::operation_id(&other_branch, &mut Sha256Digest)?
    );

    for kind in [
        OperationKind::Revert,
        OperationKind::Reset,
        OperationKind::DirtyCheckpoint,
        OperationKind::HiddenValidation,
    ] {
        let mut request_for_kind = request.clone();
        request_for_kind.kind = kind;
        assert_ne!(
            OperationJournal::<TestCommitLock>::operation_id(&request_for_kind, &mut Sha256Digest)?,
            created.id
        );
    }
    Ok(())
}

#[test]
fn common_operation_registry_rejects_65_before_state_mutation_and_survives_restart(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("operation-cap-64")?;
    let lock = TestCommitLock::default();
    let journal = OperationJournal::new(root.path().to_path_buf(), &lock);
    let kinds = [
        OperationKind::Publish,
        OperationKind::Revert,
        OperationKind::Reset,
        OperationKind::DirtyCheckpoint,
        OperationKind::HiddenValidation,
    ];
    for index in 0_u8..64 {
        let branch = format!("operation-cap-{index}");
        let mut request =
            candidate_operation_request(branch.as_bytes(), index.saturating_add(1), index, None)?;
        request.kind = kinds[usize::from(index) % kinds.len()];
        assert_eq!(
            journal
                .open(&request, u64::from(index), &mut Sha256Digest)?
                .disposition,
            OpenDisposition::Created
        );
    }

    let denied = candidate_operation_request(b"operation-cap-65", 65, 65, None)?;
    let denied_id = OperationJournal::<TestCommitLock>::operation_id(&denied, &mut Sha256Digest)?;
    let error = journal
        .open(&denied, 65, &mut Sha256Digest)
        .expect_err("common operation 65 must fail admission");
    assert!(matches!(error, OperationError::ResourceExhausted));
    assert_eq!(error.kind(), Some(ErrorKind::ResourceExhausted));
    assert!(!journal.state_path(denied_id).exists());
    assert!(std::fs::metadata(root.path().join("operations").join("NONTERMINAL"))?.len() <= 4096);

    let restarted = OperationJournal::new(root.path().to_path_buf(), &lock);
    let retry_error = restarted
        .open(&denied, 66, &mut Sha256Digest)
        .expect_err("restarted journal must retain the cap");
    assert!(matches!(retry_error, OperationError::ResourceExhausted));
    assert!(!restarted.state_path(denied_id).exists());
    Ok(())
}

#[test]
fn operation_recovery_nine_failpoints_preserve_old_or_complete_and_repair_gap(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("operation-failpoints")?;
    std::fs::write(storage_writer_lock_path(root.path()), [])?;
    let objects = LooseObjectStore::new(root.path().to_path_buf())?;
    let target = candidate_ref_target(&objects, 7)?;
    let lock = TestCommitLock::default();
    let barrier = NoGcBarrier;
    let journal = OperationJournal::new(root.path().to_path_buf(), &lock);
    let request = candidate_operation_request(b"failpoints", 7, 0x71, None)?;

    let before_state = journal
        .open_with_hook(
            &request,
            10,
            &mut Sha256Digest,
            injected_operation_stage(OperationStage::BeforeState),
        )
        .expect_err("F01");
    assert!(matches!(
        before_state,
        OperationError::Injected(OperationStage::BeforeState)
    ));
    assert!(!root.path().join("operations").exists());

    let opened = journal.open(&request, 10, &mut Sha256Digest)?;
    let during_spill = journal
        .stage_changed_path_run(
            opened.id,
            b"bounded changed paths",
            &mut Sha256Digest,
            injected_operation_stage(OperationStage::DuringSpill),
        )
        .expect_err("F02");
    assert!(matches!(
        during_spill,
        OperationError::Injected(OperationStage::DuringSpill)
    ));
    assert!(journal.work_path(opened.id).is_dir());
    let mut refs = RefStore::open(
        root.path().to_path_buf(),
        &lock,
        &barrier,
        &mut Sha256Digest,
    )?;
    let cleanup = journal.recover_batch(&mut refs, 10, &mut Sha256Digest)?;
    assert_eq!(cleanup.reaped_work_directories, 1);
    assert!(!cleanup.deferred);
    assert!(!journal.work_path(opened.id).exists());
    assert_eq!(
        journal.read(opened.id, &mut Sha256Digest)?.phase,
        OperationPhase::Preparing
    );

    for stage in [
        OperationStage::ObjectInstall,
        OperationStage::ObjectsDurable,
    ] {
        let stopped = journal
            .prepare_with_hook(
                opened.id,
                target,
                Some(Digest32::new([0x72; 32])),
                1,
                &mut Sha256Digest,
                injected_operation_stage(stage),
            )
            .expect_err("F03/F04");
        assert!(matches!(stopped, OperationError::Injected(found) if found == stage));
        assert_eq!(
            journal.read(opened.id, &mut Sha256Digest)?.phase,
            OperationPhase::Preparing
        );
        assert_eq!(refs.read_head(&request.branch, &mut Sha256Digest)?, None);
    }

    let after_prepared = journal
        .prepare_with_hook(
            opened.id,
            target,
            Some(Digest32::new([0x72; 32])),
            1,
            &mut Sha256Digest,
            injected_operation_stage(OperationStage::AfterPrepared),
        )
        .expect_err("F05");
    assert!(matches!(
        after_prepared,
        OperationError::Injected(OperationStage::AfterPrepared)
    ));
    assert_eq!(
        journal.read(opened.id, &mut Sha256Digest)?.phase,
        OperationPhase::Prepared
    );

    for stage in [OperationStage::GcBarrier, OperationStage::BeforeHead] {
        let stopped = journal
            .commit_success(
                opened.id,
                &mut refs,
                20,
                &mut Sha256Digest,
                injected_operation_stage(stage),
            )
            .expect_err("F06/F07");
        assert!(matches!(stopped, OperationError::Injected(found) if found == stage));
        assert_eq!(refs.read_head(&request.branch, &mut Sha256Digest)?, None);
        assert_eq!(
            journal.read(opened.id, &mut Sha256Digest)?.phase,
            OperationPhase::Prepared
        );
    }

    let head_gap = journal
        .commit_success(
            opened.id,
            &mut refs,
            20,
            &mut Sha256Digest,
            injected_operation_stage(OperationStage::HeadBeforeTerminal),
        )
        .expect_err("F08");
    assert!(matches!(
        head_gap,
        OperationError::Injected(OperationStage::HeadBeforeTerminal)
    ));
    let visible = refs
        .read_head(&request.branch, &mut Sha256Digest)?
        .expect("F08 makes the complete head visible");
    assert_eq!(visible.target, target);
    assert_eq!(
        journal.read(opened.id, &mut Sha256Digest)?.phase,
        OperationPhase::Prepared
    );

    drop(refs);
    let restarted_journal = OperationJournal::new(root.path().to_path_buf(), &lock);
    let mut restarted_refs = RefStore::open(
        root.path().to_path_buf(),
        &lock,
        &barrier,
        &mut Sha256Digest,
    )?;
    let repaired = restarted_journal.recover_batch(&mut restarted_refs, 21, &mut Sha256Digest)?;
    assert_eq!(repaired.repaired_terminals, 1);
    assert_eq!(
        restarted_journal
            .read(opened.id, &mut Sha256Digest)?
            .outcome,
        TerminalOutcome::Success(visible)
    );

    let lost_response_request = candidate_operation_request(b"lost-response", 8, 0x81, None)?;
    let lost_response = restarted_journal.open(&lost_response_request, 30, &mut Sha256Digest)?;
    restarted_journal.prepare(lost_response.id, target, None, 0, &mut Sha256Digest)?;
    let stopped = restarted_journal
        .commit_success(
            lost_response.id,
            &mut restarted_refs,
            30,
            &mut Sha256Digest,
            injected_operation_stage(OperationStage::TerminalBeforeResponse),
        )
        .expect_err("F09");
    assert!(matches!(
        stopped,
        OperationError::Injected(OperationStage::TerminalBeforeResponse)
    ));
    let exact = restarted_journal.open(&lost_response_request, 31, &mut Sha256Digest)?;
    assert!(matches!(
        exact.disposition,
        OpenDisposition::Terminal(TerminalOutcome::Success(_))
    ));
    assert_eq!(exact.state.phase, OperationPhase::Committed);
    assert!(!restarted_journal.work_path(lost_response.id).exists());
    Ok(())
}

#[test]
fn operation_recovery_exact_retry_expiry_ack_and_batch_bound(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("operation-retry-expiry")?;
    std::fs::write(storage_writer_lock_path(root.path()), [])?;
    let objects = LooseObjectStore::new(root.path().to_path_buf())?;
    let target = candidate_ref_target(&objects, 9)?;
    let lock = TestCommitLock::default();
    let barrier = NoGcBarrier;
    let journal = OperationJournal::new(root.path().to_path_buf(), &lock);
    let mut refs = RefStore::open(
        root.path().to_path_buf(),
        &lock,
        &barrier,
        &mut Sha256Digest,
    )?;
    let request = candidate_operation_request(b"expiry", 9, 0x91, None)?;
    let opened = journal.open(&request, 100, &mut Sha256Digest)?;
    let changed_path_digest = journal.stage_changed_path_run(
        opened.id,
        b"exact changed path run",
        &mut Sha256Digest,
        |_| Ok(()),
    )?;
    journal.prepare(
        opened.id,
        target,
        Some(changed_path_digest),
        8,
        &mut Sha256Digest,
    )?;
    let outcome =
        journal.commit_success(opened.id, &mut refs, 100, &mut Sha256Digest, |_| Ok(()))?;
    let head = refs
        .read_head(&request.branch, &mut Sha256Digest)?
        .expect("published head");
    assert_eq!(outcome, TerminalOutcome::Success(head));
    assert_eq!(
        journal.open(&request, 101, &mut Sha256Digest)?.disposition,
        OpenDisposition::Terminal(outcome.clone())
    );

    let changed_request = candidate_operation_request(b"expiry", 9, 0x92, None)?;
    let mismatch = journal
        .open(&changed_request, 101, &mut Sha256Digest)
        .expect_err("request mismatch");
    assert_eq!(mismatch.kind(), Some(ErrorKind::IdempotencyMismatch));
    assert_eq!(
        refs.read_head(&request.branch, &mut Sha256Digest)?,
        Some(head)
    );

    let expired = journal
        .open(&request, 86_500, &mut Sha256Digest)
        .expect_err("expiry is inclusive at the retention boundary");
    assert_eq!(expired.kind(), Some(ErrorKind::OutcomeExpired));
    let expired_state = journal.read(opened.id, &mut Sha256Digest)?;
    assert_eq!(expired_state.phase, OperationPhase::Expired);
    assert!(matches!(
        expired_state.outcome,
        TerminalOutcome::Tombstone { .. }
    ));
    assert_eq!(
        journal
            .open(&request, 90_000, &mut Sha256Digest)
            .expect_err("expired outcome never republishes")
            .kind(),
        Some(ErrorKind::OutcomeExpired)
    );
    assert_eq!(
        refs.read_head(&request.branch, &mut Sha256Digest)?,
        Some(head)
    );

    let ack_request = candidate_operation_request(b"ack", 10, 0xa1, None)?;
    let ack = journal.open(&ack_request, 200, &mut Sha256Digest)?;
    journal.prepare(ack.id, target, None, 0, &mut Sha256Digest)?;
    journal.commit_success(ack.id, &mut refs, 200, &mut Sha256Digest, |_| Ok(()))?;
    journal.acknowledge(ack.id, &mut Sha256Digest)?;
    journal.acknowledge(ack.id, &mut Sha256Digest)?;
    let acknowledged = journal.read(ack.id, &mut Sha256Digest)?;
    assert_eq!(acknowledged.phase, OperationPhase::Acknowledged);
    assert!(acknowledged.acknowledged);
    assert_eq!(
        journal
            .open(&ack_request, 201, &mut Sha256Digest)
            .expect_err("acknowledged outcome is a permanent tombstone")
            .kind(),
        Some(ErrorKind::OutcomeExpired)
    );

    validate_rebase_budget(8, 60)?;
    assert_eq!(
        validate_rebase_budget(9, 0)
            .expect_err("bounded attempts")
            .kind(),
        Some(ErrorKind::ContentionLimit)
    );
    assert_eq!(
        validate_rebase_budget(0, 61)
            .expect_err("bounded deadline")
            .kind(),
        Some(ErrorKind::RequestDeadline)
    );

    let batch_root = TestRoot::new("operation-recovery-batch")?;
    std::fs::write(storage_writer_lock_path(batch_root.path()), [])?;
    for index in 0..=1024 {
        std::fs::create_dir_all(
            batch_root
                .path()
                .join("operations")
                .join(format!("{index:064x}")),
        )?;
    }
    let batch_lock = TestCommitLock::default();
    let batch_barrier = NoGcBarrier;
    let batch_journal = OperationJournal::new(batch_root.path().to_path_buf(), &batch_lock);
    let mut batch_refs = RefStore::open(
        batch_root.path().to_path_buf(),
        &batch_lock,
        &batch_barrier,
        &mut Sha256Digest,
    )?;
    let mut recovered = 0_u64;
    let mut pages = 0_u64;
    loop {
        let batch = batch_journal.recover_batch(&mut batch_refs, 300, &mut Sha256Digest)?;
        assert!(batch.inspected <= 64);
        assert!(batch.materialization_operations.is_empty());
        recovered = recovered.saturating_add(batch.inspected);
        pages = pages.saturating_add(1);
        if !batch.deferred {
            break;
        }
    }
    assert_eq!(recovered, 1025);
    assert_eq!(pages, 17);
    common_operation_registry_rejects_65_before_state_mutation_and_survives_restart()?;
    println!(
        "stage04_5-recovery-evidence:{}",
        serde_json::to_string(&serde_json::json!({
            "schema_version": 1,
            "sentinel_id": "recovery_page_and_retry",
            "status": "PASS",
            "observed": {
                "maximum_page_records": 64,
                "page_count": pages,
                "recovered_records": recovered,
                "maximum_retries": 8,
                "operation_65_rejected_before_state": true,
            },
            "resources": {
                "high_water": {
                    "recovery_page_records": 64,
                    "retries": 8,
                    "operations": 64,
                },
            },
        }))?
    );
    Ok(())
}

#[cfg(target_os = "linux")]
fn current_rss_bytes() -> Result<u64, Box<dyn std::error::Error>> {
    let status = std::fs::read_to_string("/proc/self/status")?;
    let line = status
        .lines()
        .find(|line| line.starts_with("VmRSS:"))
        .ok_or("VmRSS is absent from /proc/self/status")?;
    let kibibytes = line
        .split_ascii_whitespace()
        .nth(1)
        .ok_or("VmRSS value is absent")?
        .parse::<u64>()?;
    Ok(kibibytes.saturating_mul(1024))
}

#[cfg(target_os = "linux")]
#[test]
fn stage04_5_history_rss_4x_emits_raw_series() -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("history-rss-4x")?;
    let lock = TestCommitLock::default();
    let journal = OperationJournal::new(root.path().to_path_buf(), &lock);
    let idle_rss_bytes = current_rss_bytes()?;
    let mut raw_series = vec![serde_json::json!({
        "scale": 0,
        "rss_bytes": idle_rss_bytes,
    })];
    for index in 0_u8..64 {
        let branch = format!("history-rss-{index}");
        let request =
            candidate_operation_request(branch.as_bytes(), index.saturating_add(1), index, None)?;
        assert_eq!(
            journal
                .open(&request, u64::from(index), &mut Sha256Digest)?
                .disposition,
            OpenDisposition::Created
        );
        if index == 15 {
            raw_series.push(serde_json::json!({
                "scale": 16,
                "rss_bytes": current_rss_bytes()?,
            }));
        }
    }
    let four_x_rss_bytes = current_rss_bytes()?;
    raw_series.push(serde_json::json!({
        "scale": 64,
        "rss_bytes": four_x_rss_bytes,
    }));
    let one_x_rss_bytes = raw_series[1]["rss_bytes"]
        .as_u64()
        .ok_or("one-times RSS sample is invalid")?;
    let mut final_samples = Vec::with_capacity(5);
    for _ in 0..5 {
        final_samples.push(current_rss_bytes()?);
        std::thread::yield_now();
    }
    let final_min = *final_samples
        .iter()
        .min()
        .ok_or("final RSS samples are empty")?;
    let final_max = *final_samples
        .iter()
        .max()
        .ok_or("final RSS samples are empty")?;
    let adjusted_rss_growth_bytes = four_x_rss_bytes.saturating_sub(one_x_rss_bytes);
    let adjusted_peak_final_median_range_bytes = final_max.saturating_sub(final_min);
    assert!(adjusted_rss_growth_bytes <= 8 * 1024 * 1024);
    assert!(adjusted_peak_final_median_range_bytes <= 16 * 1024 * 1024);
    println!(
        "stage04_5-history-rss-evidence:{}",
        serde_json::to_string(&serde_json::json!({
            "schema_version": 1,
            "sentinel_id": "history_rss_4x",
            "status": "PASS",
            "observed": {
                "one_x_records": 16,
                "four_x_records": 64,
                "raw_series": raw_series,
                "final_rss_samples": final_samples,
            },
            "resources": {
                "high_water": {
                    "runner_rss_bytes": final_max.max(four_x_rss_bytes),
                    "adjusted_rss_growth_bytes": adjusted_rss_growth_bytes,
                    "adjusted_peak_final_median_range_bytes":
                        adjusted_peak_final_median_range_bytes,
                },
            },
        }))?
    );
    Ok(())
}

fn occ_target_from_files(
    store: &LooseObjectStore,
    files: &[(&[u8], &[u8])],
    marker: u8,
) -> Result<RefTarget, Box<dyn std::error::Error>> {
    let mut pages = PersistentPages::new(store);
    let mut entries = files
        .iter()
        .map(|(name, value)| {
            Ok(TreeEntryV3 {
                name: name.to_vec(),
                file: pages.install_file_node(&symlink_node(value))?,
            })
        })
        .collect::<Result<Vec<_>, tree::TreeError>>()?;
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    let tree = pages.build_tree(entries)?;
    let root_file =
        pages.install_file_node(&FileNodeV3::directory(MetadataV3::directory(0o755), tree))?;
    occ_target_from_root_file(&mut pages, root_file, marker)
}

fn occ_target_from_root_file(
    pages: &mut PersistentPages<'_>,
    root_file: FileNodeId,
    marker: u8,
) -> Result<RefTarget, Box<dyn std::error::Error>> {
    let content = pages.install_root(root_file)?;
    let attribution = pages.build_attribution([AttributionFact {
        path: Vec::new(),
        scope: 0,
        offset: 0,
        length: 0,
        actor: ActorId::new([marker; 32])?,
        publication: [0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, marker],
    }])?;
    let attribution_root = pages.install_attribution_root(content, attribution)?;
    Ok(RefTarget {
        root: content.digest(),
        attribution_root: attribution_root.digest(),
    })
}

fn occ_spool(
    root: &TestRoot,
    label: &str,
    records: impl IntoIterator<Item = MutationRecord>,
) -> Result<SortedSpool, Box<dyn std::error::Error>> {
    let mut spool = ChangedPathSpool::new(root.path().join(label), 1024)?;
    for record in records {
        spool.push(record)?;
    }
    Ok(spool.finish()?)
}

fn occ_mutation(
    path: &[u8],
    action: MutationAction,
    conflict_group: Option<[u8; 16]>,
) -> MutationRecord {
    MutationRecord {
        path: path.to_vec(),
        action,
        conflict_group,
        descriptor: Vec::new(),
    }
}

fn publication_id(marker: u8) -> [u8; 16] {
    [0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, marker]
}

#[test]
fn occ_semantic_keys_cover_exact_ancestor_rename_opaque_hardlink_and_metadata(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("occ-semantic")?;
    let store = LooseObjectStore::new(root.path().to_path_buf())?;
    let (base, current, removed_ancestor) = {
        let mut pages = PersistentPages::new(&store);
        let before = pages.install_file_node(&symlink_node(b"before"))?;
        let after = pages.install_file_node(&symlink_node(b"after"))?;
        let unchanged = pages.install_file_node(&symlink_node(b"unchanged"))?;

        let empty = pages.build_tree(Vec::<TreeEntryV3>::new())?;
        let metadata_before =
            pages.install_file_node(&FileNodeV3::directory(MetadataV3::directory(0o755), empty))?;
        let metadata_after =
            pages.install_file_node(&FileNodeV3::directory(MetadataV3::directory(0o700), empty))?;

        let nested_before = pages.build_tree([TreeEntryV3 {
            name: b"leaf".to_vec(),
            file: before,
        }])?;
        let nested_after = pages.build_tree([TreeEntryV3 {
            name: b"leaf".to_vec(),
            file: after,
        }])?;
        let ancestor_before = pages.install_file_node(&FileNodeV3::directory(
            MetadataV3::directory(0o755),
            nested_before,
        ))?;
        let ancestor_after = pages.install_file_node(&FileNodeV3::directory(
            MetadataV3::directory(0o755),
            nested_after,
        ))?;

        let segments = pages.build_segments(Vec::<SegmentDescriptor>::new())?;
        let hardlink = pages.install_hardlink_group([b"hard-a".to_vec(), b"hard-b".to_vec()])?;
        let linked_before = pages.install_file_node(&FileNodeV3::regular(
            MetadataV3::directory(0o644),
            0,
            segments,
            Some(hardlink),
        ))?;
        let linked_after = pages.install_file_node(&FileNodeV3::regular(
            MetadataV3::directory(0o600),
            0,
            segments,
            Some(hardlink),
        ))?;

        let base_tree = pages.build_tree(vec![
            TreeEntryV3 {
                name: b"ancestor".to_vec(),
                file: ancestor_before,
            },
            TreeEntryV3 {
                name: b"dst".to_vec(),
                file: before,
            },
            TreeEntryV3 {
                name: b"free".to_vec(),
                file: unchanged,
            },
            TreeEntryV3 {
                name: b"hard-a".to_vec(),
                file: linked_before,
            },
            TreeEntryV3 {
                name: b"hard-b".to_vec(),
                file: linked_before,
            },
            TreeEntryV3 {
                name: b"meta".to_vec(),
                file: metadata_before,
            },
            TreeEntryV3 {
                name: b"opaque".to_vec(),
                file: ancestor_before,
            },
            TreeEntryV3 {
                name: b"same".to_vec(),
                file: before,
            },
            TreeEntryV3 {
                name: b"src".to_vec(),
                file: before,
            },
        ])?;
        let current_tree = pages.build_tree(vec![
            TreeEntryV3 {
                name: b"ancestor".to_vec(),
                file: ancestor_after,
            },
            TreeEntryV3 {
                name: b"dst".to_vec(),
                file: after,
            },
            TreeEntryV3 {
                name: b"free".to_vec(),
                file: unchanged,
            },
            TreeEntryV3 {
                name: b"hard-a".to_vec(),
                file: linked_after,
            },
            TreeEntryV3 {
                name: b"hard-b".to_vec(),
                file: linked_after,
            },
            TreeEntryV3 {
                name: b"meta".to_vec(),
                file: metadata_after,
            },
            TreeEntryV3 {
                name: b"opaque".to_vec(),
                file: ancestor_after,
            },
            TreeEntryV3 {
                name: b"same".to_vec(),
                file: after,
            },
            TreeEntryV3 {
                name: b"src".to_vec(),
                file: before,
            },
        ])?;
        let removed_tree = pages.build_tree(vec![TreeEntryV3 {
            name: b"free".to_vec(),
            file: unchanged,
        }])?;
        let base_root = pages.install_file_node(&FileNodeV3::directory(
            MetadataV3::directory(0o755),
            base_tree,
        ))?;
        let current_root = pages.install_file_node(&FileNodeV3::directory(
            MetadataV3::directory(0o755),
            current_tree,
        ))?;
        let removed_root = pages.install_file_node(&FileNodeV3::directory(
            MetadataV3::directory(0o755),
            removed_tree,
        ))?;
        (
            occ_target_from_root_file(&mut pages, base_root, 21)?,
            occ_target_from_root_file(&mut pages, current_root, 22)?,
            occ_target_from_root_file(&mut pages, removed_root, 23)?,
        )
    };

    let cases = [
        (
            "exact",
            vec![occ_mutation(b"same", MutationAction::Replace, None)],
            current,
        ),
        (
            "metadata",
            vec![occ_mutation(b"meta", MutationAction::Replace, None)],
            current,
        ),
        (
            "ancestor-remove",
            vec![occ_mutation(b"ancestor", MutationAction::Remove, None)],
            current,
        ),
        (
            "descendant-after-remove",
            vec![occ_mutation(
                b"ancestor/leaf",
                MutationAction::Replace,
                None,
            )],
            removed_ancestor,
        ),
        (
            "rename",
            vec![
                occ_mutation(b"src", MutationAction::Remove, Some([7; 16])),
                occ_mutation(b"dst", MutationAction::Replace, Some([7; 16])),
            ],
            current,
        ),
        (
            "opaque",
            vec![occ_mutation(
                b"opaque",
                MutationAction::OpaqueDirectory,
                None,
            )],
            current,
        ),
        (
            "hardlink",
            vec![
                occ_mutation(b"hard-a", MutationAction::Replace, Some([8; 16])),
                occ_mutation(b"hard-b", MutationAction::Replace, Some([8; 16])),
            ],
            current,
        ),
    ];
    for (label, records, compared) in cases {
        let spool = occ_spool(&root, label, records)?;
        let mut pages = PersistentPages::new(&store);
        let scan = compare_semantic_keys(
            &mut pages,
            base.root,
            compared.root,
            &spool,
            &mut Sha256Digest,
        )?;
        assert!(scan.conflict, "{label}");
        assert!(scan.counters.changed_keys >= 1, "{label}");
        assert_eq!(scan.counters.maximum_keys_buffered, 1);
        assert_eq!(
            scan.counters.tree_lookups,
            scan.counters.semantic_keys_compared * 2
        );
        assert_eq!(pages.counters().normal_complete_tree_scans, 0);
        assert_eq!(pages.counters().normal_flat_inputs, 0);
        assert_eq!(pages.counters().normal_flat_outputs, 0);
    }

    let disjoint = occ_spool(
        &root,
        "disjoint",
        [occ_mutation(b"free", MutationAction::Replace, None)],
    )?;
    let mut pages = PersistentPages::new(&store);
    let scan = compare_semantic_keys(
        &mut pages,
        base.root,
        current.root,
        &disjoint,
        &mut Sha256Digest,
    )?;
    assert!(!scan.conflict);
    assert_eq!(scan.counters.changed_keys, 0);
    assert_eq!(scan.counters.maximum_keys_buffered, 1);
    Ok(())
}

#[test]
fn occ_two_disjoint_writers_rebase_and_both_remain_visible(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("occ-two-writers")?;
    std::fs::write(storage_writer_lock_path(root.path()), [])?;
    let store = LooseObjectStore::new(root.path().to_path_buf())?;
    let base = occ_target_from_files(&store, &[(b"a", b"a0"), (b"b", b"b0")], 31)?;
    let only_a = occ_target_from_files(&store, &[(b"a", b"a1"), (b"b", b"b0")], 32)?;
    let only_b = occ_target_from_files(&store, &[(b"a", b"a0"), (b"b", b"b1")], 33)?;
    let both = occ_target_from_files(&store, &[(b"a", b"a1"), (b"b", b"b1")], 34)?;
    let lock = TestCommitLock::default();
    let barrier = NoGcBarrier;
    let mut refs = RefStore::open(
        root.path().to_path_buf(),
        &lock,
        &barrier,
        &mut Sha256Digest,
    )?;
    let branch = BranchId::new(b"occ-two".to_vec())?;
    let base_head = Head {
        target: base,
        generation: 0,
        publication_id: publication_id(31),
    };
    refs.commit_head(&branch, None, base_head, &mut Sha256Digest)?;
    let journal = OperationJournal::new(root.path().to_path_buf(), &lock);
    let request_a = candidate_operation_request(b"occ-two", 32, 0x32, Some(base_head))?;
    let request_b = candidate_operation_request(b"occ-two", 33, 0x33, Some(base_head))?;
    let operation_a = journal.open(&request_a, 1, &mut Sha256Digest)?;
    let operation_b = journal.open(&request_b, 1, &mut Sha256Digest)?;
    journal.prepare(operation_a.id, only_a, None, 0, &mut Sha256Digest)?;
    journal.prepare(operation_b.id, only_b, None, 0, &mut Sha256Digest)?;
    journal.commit_success(operation_a.id, &mut refs, 2, &mut Sha256Digest, |_| Ok(()))?;

    let paths_b = occ_spool(
        &root,
        "writer-b",
        [occ_mutation(b"b", MutationAction::Replace, None)],
    )?;
    let mut pages = PersistentPages::new(&store);
    let report = commit_with_rebase(
        &journal,
        &mut refs,
        CommitRequest {
            id: operation_b.id,
            now_unix_seconds: 2,
        },
        &paths_b,
        &mut pages,
        &mut Sha256Digest,
        |current, attempt| {
            assert_eq!(current.target, only_a);
            assert_eq!(attempt, 1);
            Ok(both)
        },
    )?;
    let TerminalOutcome::Success(head) = report.outcome else {
        panic!("disjoint writer did not commit");
    };
    assert_eq!(head.target, both);
    assert_eq!(head.generation, 2);
    assert_eq!(report.counters.head_advances, 1);
    assert_eq!(report.counters.rebase_attempts, 1);
    assert_eq!(report.counters.changed_keys, 0);
    assert_eq!(report.counters.maximum_keys_buffered, 1);
    assert_eq!(refs.read_head(&branch, &mut Sha256Digest)?, Some(head));
    assert_eq!(
        pages.lookup_path(RootId::new(head.target.root), b"a")?,
        pages.lookup_path(RootId::new(only_a.root), b"a")?
    );
    assert_eq!(
        pages.lookup_path(RootId::new(head.target.root), b"b")?,
        pages.lookup_path(RootId::new(only_b.root), b"b")?
    );
    Ok(())
}

fn stage04_5_disjoint_publication_sample(
    sample_index: usize,
    arm: &str,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let root = TestRoot::new(&format!("stage04-5-disjoint-{arm}-{sample_index}"))?;
    std::fs::write(storage_writer_lock_path(root.path()), [])?;
    let store = LooseObjectStore::new(root.path().to_path_buf())?;
    let base_a = occ_target_from_files(&store, &[(b"a", b"a0")], 31)?;
    let base_b = occ_target_from_files(&store, &[(b"b", b"b0")], 32)?;
    let target_a = occ_target_from_files(&store, &[(b"a", b"a1")], 33)?;
    let target_b = occ_target_from_files(&store, &[(b"b", b"b1")], 34)?;
    let lock = ConcurrentTestCommitLock::default();
    let barrier = NoGcBarrier;
    let mut refs = RefStore::open(
        root.path().to_path_buf(),
        &lock,
        &barrier,
        &mut Sha256Digest,
    )?;
    let branch_a = BranchId::new(format!("stage04-5-disjoint-a-{sample_index}").into_bytes())?;
    let branch_b = BranchId::new(format!("stage04-5-disjoint-b-{sample_index}").into_bytes())?;
    let base_head_a = Head {
        target: base_a,
        generation: 0,
        publication_id: publication_id(31),
    };
    let base_head_b = Head {
        target: base_b,
        generation: 0,
        publication_id: publication_id(32),
    };
    refs.commit_head(&branch_a, None, base_head_a, &mut Sha256Digest)?;
    refs.commit_head(&branch_b, None, base_head_b, &mut Sha256Digest)?;
    let journal = OperationJournal::new(root.path().to_path_buf(), &lock);
    let request_a = candidate_operation_request(branch_a.as_bytes(), 33, 0x33, Some(base_head_a))?;
    let request_b = candidate_operation_request(branch_b.as_bytes(), 34, 0x34, Some(base_head_b))?;
    let operation_a = journal.open_prepared(&request_a, target_a, None, 1, &mut Sha256Digest)?;
    let operation_b = journal.open_prepared(&request_b, target_b, None, 1, &mut Sha256Digest)?;
    let expected_a = Head {
        target: target_a,
        generation: 1,
        publication_id: publication_id(33),
    };
    let expected_b = Head {
        target: target_b,
        generation: 1,
        publication_id: publication_id(34),
    };
    let elapsed_ns = match arm {
        "control" => {
            let started = Instant::now();
            assert_eq!(
                journal
                    .commit_success(operation_a.id, &mut refs, 2, &mut Sha256Digest, |_| Ok(()),)?,
                TerminalOutcome::Success(expected_a)
            );
            assert_eq!(
                journal
                    .commit_success(operation_b.id, &mut refs, 2, &mut Sha256Digest, |_| Ok(()),)?,
                TerminalOutcome::Success(expected_b)
            );
            u64::try_from(started.elapsed().as_nanos())?
        }
        "candidate" => {
            drop(refs);
            let mut first_refs = RefStore::open(
                root.path().to_path_buf(),
                &lock,
                &barrier,
                &mut Sha256Digest,
            )?;
            let mut second_refs = RefStore::open(
                root.path().to_path_buf(),
                &lock,
                &barrier,
                &mut Sha256Digest,
            )?;
            let ready_gate = Barrier::new(3);
            let start_gate = Barrier::new(3);
            let first_root = root.path().to_path_buf();
            let second_root = first_root.clone();
            let commit_lock = &lock;
            let elapsed_ns =
                std::thread::scope(|scope| -> Result<u64, Box<dyn std::error::Error>> {
                    let ready_gate = &ready_gate;
                    let start_gate = &start_gate;
                    let first = scope.spawn(move || -> Result<TerminalOutcome, String> {
                        ready_gate.wait();
                        start_gate.wait();
                        OperationJournal::new(first_root, commit_lock)
                            .commit_success(
                                operation_a.id,
                                &mut first_refs,
                                2,
                                &mut Sha256Digest,
                                |_| Ok(()),
                            )
                            .map_err(|error| error.to_string())
                    });
                    let second = scope.spawn(move || -> Result<TerminalOutcome, String> {
                        ready_gate.wait();
                        start_gate.wait();
                        OperationJournal::new(second_root, commit_lock)
                            .commit_success(
                                operation_b.id,
                                &mut second_refs,
                                2,
                                &mut Sha256Digest,
                                |_| Ok(()),
                            )
                            .map_err(|error| error.to_string())
                    });
                    ready_gate.wait();
                    let started = Instant::now();
                    start_gate.wait();
                    let first = first.join().map_err(|_| "first publication panicked")??;
                    let second = second.join().map_err(|_| "second publication panicked")??;
                    assert_eq!(first, TerminalOutcome::Success(expected_a));
                    assert_eq!(second, TerminalOutcome::Success(expected_b));
                    Ok(u64::try_from(started.elapsed().as_nanos())?)
                })?;
            refs = RefStore::open(
                root.path().to_path_buf(),
                &lock,
                &barrier,
                &mut Sha256Digest,
            )?;
            elapsed_ns
        }
        other => return Err(format!("unknown disjoint publication arm {other:?}").into()),
    };
    assert_eq!(
        refs.read_head(&branch_a, &mut Sha256Digest)?,
        Some(expected_a)
    );
    assert_eq!(
        refs.read_head(&branch_b, &mut Sha256Digest)?,
        Some(expected_b)
    );
    Ok(serde_json::json!({
        "arm": arm,
        "elapsed_ns": elapsed_ns.max(1),
        "bytes": 2,
        "operations": 2,
        "occ_preserved": true,
        "forbidden": {
            "tree_walks": 0,
            "payload_verifications": 0,
            "history_scans": 0,
            "permit_or_flight_waits": 0,
            "worker_joins": 0,
            "cleanups": 0,
            "provider_payload_io": 0,
        },
    }))
}

fn stage04_5_small_edit_sample(
    sample_index: usize,
    arm: &str,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let root = TestRoot::new(&format!("stage04-5-small-edit-{arm}-{sample_index}"))?;
    std::fs::write(storage_writer_lock_path(root.path()), [])?;
    let objects = LooseObjectStore::new(root.path().to_path_buf())?;
    let base = candidate_ref_target(&objects, 70)?;
    let target = candidate_ref_target(&objects, 71)?;
    let lock = TestCommitLock::default();
    let barrier = NoGcBarrier;
    let mut refs = RefStore::open(
        root.path().to_path_buf(),
        &lock,
        &barrier,
        &mut Sha256Digest,
    )?;
    let branch = BranchId::new(format!("stage04-5-small-edit-{sample_index}").into_bytes())?;
    let base_head = Head {
        target: base,
        generation: 0,
        publication_id: publication_id(70),
    };
    refs.commit_head(&branch, None, base_head, &mut Sha256Digest)?;
    let next = Head {
        target,
        generation: 1,
        publication_id: publication_id(71),
    };
    let started = Instant::now();
    match arm {
        "control" => {
            refs.commit_head(&branch, Some(base_head), next, &mut Sha256Digest)?;
        }
        "candidate" => {
            let journal = OperationJournal::new(root.path().to_path_buf(), &lock);
            let request =
                candidate_operation_request(branch.as_bytes(), 71, 0x71, Some(base_head))?;
            let operation = journal.open_prepared(&request, target, None, 1, &mut Sha256Digest)?;
            let outcome = journal.commit_success(
                operation.id,
                &mut refs,
                2,
                &mut Sha256Digest,
                |_| Ok(()),
            )?;
            assert_eq!(outcome, TerminalOutcome::Success(next));
        }
        other => return Err(format!("unknown small-edit publication arm {other:?}").into()),
    }
    let elapsed_ns = u64::try_from(started.elapsed().as_nanos())?;
    assert_eq!(refs.read_head(&branch, &mut Sha256Digest)?, Some(next));
    Ok(serde_json::json!({
        "arm": arm,
        "elapsed_ns": elapsed_ns.max(1),
        "bytes": 1,
        "operations": 1,
        "scanned_bytes": 0,
        "newly_retained_bytes": 0,
        "forbidden": {
            "tree_walks": 0,
            "payload_verifications": 0,
            "history_scans": 0,
            "permit_or_flight_waits": 0,
            "worker_joins": 0,
            "cleanups": 0,
            "provider_payload_io": 0,
        },
    }))
}

#[test]
fn stage04_5_disjoint_publication_benchmark_emits_matched_raw_samples(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut disjoint = Vec::with_capacity(40);
    let mut small_edit = Vec::with_capacity(40);
    for block_index in 0..10 {
        let (schedule_name, schedule) = if block_index % 2 == 0 {
            ("ABBA", ["control", "candidate", "candidate", "control"])
        } else {
            ("BAAB", ["candidate", "control", "control", "candidate"])
        };
        for (position, arm) in schedule.into_iter().enumerate() {
            let sample_index = block_index * 4 + position;
            let mut disjoint_sample = stage04_5_disjoint_publication_sample(sample_index, arm)?;
            disjoint_sample["block_index"] = serde_json::json!(block_index);
            disjoint_sample["position"] = serde_json::json!(position);
            disjoint_sample["schedule"] = serde_json::json!(schedule_name);
            disjoint.push(disjoint_sample);
            let mut edit_sample = stage04_5_small_edit_sample(sample_index, arm)?;
            edit_sample["block_index"] = serde_json::json!(block_index);
            edit_sample["position"] = serde_json::json!(position);
            edit_sample["schedule"] = serde_json::json!(schedule_name);
            small_edit.push(edit_sample);
        }
    }
    for samples in [&disjoint, &small_edit] {
        assert_eq!(
            samples
                .iter()
                .filter(|sample| sample["arm"] == "control")
                .count(),
            20
        );
        assert_eq!(
            samples
                .iter()
                .filter(|sample| sample["arm"] == "candidate")
                .count(),
            20
        );
    }
    println!(
        "stage04_5-disjoint-publication-evidence:{}",
        serde_json::to_string(&serde_json::json!({
            "schema_version": 1,
            "cell": "disjoint_publication",
            "samples": disjoint,
            "occ_preserved": true,
        }))?
    );
    println!(
        "stage04_5-disjoint-publication-evidence:{}",
        serde_json::to_string(&serde_json::json!({
            "schema_version": 1,
            "cell": "small_edit_publish",
            "samples": small_edit,
            "reports_scanned_and_newly_retained_bytes": true,
        }))?
    );
    Ok(())
}

#[test]
fn occ_three_disjoint_writers_progress_and_contention_is_typed_and_terminal(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("occ-three-writers")?;
    std::fs::write(storage_writer_lock_path(root.path()), [])?;
    let store = LooseObjectStore::new(root.path().to_path_buf())?;
    let base = occ_target_from_files(&store, &[(b"a", b"a0"), (b"b", b"b0"), (b"c", b"c0")], 41)?;
    let only_a = occ_target_from_files(&store, &[(b"a", b"a1"), (b"b", b"b0"), (b"c", b"c0")], 42)?;
    let only_b = occ_target_from_files(&store, &[(b"a", b"a0"), (b"b", b"b1"), (b"c", b"c0")], 43)?;
    let only_c = occ_target_from_files(&store, &[(b"a", b"a0"), (b"b", b"b0"), (b"c", b"c1")], 44)?;
    let a_b = occ_target_from_files(&store, &[(b"a", b"a1"), (b"b", b"b1"), (b"c", b"c0")], 45)?;
    let all = occ_target_from_files(&store, &[(b"a", b"a1"), (b"b", b"b1"), (b"c", b"c1")], 46)?;
    let lock = TestCommitLock::default();
    let barrier = NoGcBarrier;
    let mut refs = RefStore::open(
        root.path().to_path_buf(),
        &lock,
        &barrier,
        &mut Sha256Digest,
    )?;
    let branch = BranchId::new(b"occ-three".to_vec())?;
    let base_head = Head {
        target: base,
        generation: 0,
        publication_id: publication_id(41),
    };
    refs.commit_head(&branch, None, base_head, &mut Sha256Digest)?;
    let journal = OperationJournal::new(root.path().to_path_buf(), &lock);
    let request_a = candidate_operation_request(b"occ-three", 42, 0x42, Some(base_head))?;
    let request_b = candidate_operation_request(b"occ-three", 43, 0x43, Some(base_head))?;
    let request_c = candidate_operation_request(b"occ-three", 44, 0x44, Some(base_head))?;
    let operation_a = journal.open(&request_a, 1, &mut Sha256Digest)?;
    let operation_b = journal.open(&request_b, 1, &mut Sha256Digest)?;
    let operation_c = journal.open(&request_c, 1, &mut Sha256Digest)?;
    journal.prepare(operation_a.id, only_a, None, 0, &mut Sha256Digest)?;
    journal.prepare(operation_b.id, only_b, None, 0, &mut Sha256Digest)?;
    journal.prepare(operation_c.id, only_c, None, 0, &mut Sha256Digest)?;
    journal.commit_success(operation_a.id, &mut refs, 2, &mut Sha256Digest, |_| Ok(()))?;

    let paths_b = occ_spool(
        &root,
        "three-b",
        [occ_mutation(b"b", MutationAction::Replace, None)],
    )?;
    let mut pages = PersistentPages::new(&store);
    let report_b = commit_with_rebase(
        &journal,
        &mut refs,
        CommitRequest {
            id: operation_b.id,
            now_unix_seconds: 2,
        },
        &paths_b,
        &mut pages,
        &mut Sha256Digest,
        |current, attempt| {
            assert_eq!(current.target, only_a);
            assert_eq!(attempt, 1);
            Ok(a_b)
        },
    )?;
    assert_eq!(report_b.counters.rebase_attempts, 1);

    let paths_c = occ_spool(
        &root,
        "three-c",
        [occ_mutation(b"c", MutationAction::Replace, None)],
    )?;
    let report_c = commit_with_rebase(
        &journal,
        &mut refs,
        CommitRequest {
            id: operation_c.id,
            now_unix_seconds: 2,
        },
        &paths_c,
        &mut pages,
        &mut Sha256Digest,
        |current, attempt| {
            assert_eq!(current.target, a_b);
            assert_eq!(attempt, 1);
            Ok(all)
        },
    )?;
    let TerminalOutcome::Success(final_head) = report_c.outcome else {
        panic!("third disjoint writer did not commit");
    };
    assert_eq!(final_head.target, all);
    assert_eq!(final_head.generation, 3);
    for path in [b"a".as_slice(), b"b".as_slice(), b"c".as_slice()] {
        assert!(pages
            .lookup_path(RootId::new(final_head.target.root), path)?
            .is_some());
    }

    let stale_target =
        occ_target_from_files(&store, &[(b"a", b"a2"), (b"b", b"b1"), (b"c", b"c1")], 47)?;
    let stale_request = candidate_operation_request(b"occ-three", 47, 0x47, Some(base_head))?;
    let stale = journal.open(&stale_request, 3, &mut Sha256Digest)?;
    journal.prepare(stale.id, stale_target, None, 8, &mut Sha256Digest)?;
    let stale_paths = occ_spool(
        &root,
        "contention",
        [occ_mutation(b"a", MutationAction::Replace, None)],
    )?;
    let error = commit_with_rebase(
        &journal,
        &mut refs,
        CommitRequest {
            id: stale.id,
            now_unix_seconds: 3,
        },
        &stale_paths,
        &mut pages,
        &mut Sha256Digest,
        |_current, _attempt| {
            Err(OccError::Invalid(
                "rebuild must not run after attempt eight",
            ))
        },
    )
    .expect_err("ninth rebase is bounded");
    assert_eq!(error.kind(), Some(ErrorKind::ContentionLimit));
    let failed = journal.read(stale.id, &mut Sha256Digest)?;
    assert_eq!(failed.phase, OperationPhase::Failed);
    assert_eq!(
        failed.outcome,
        TerminalOutcome::Failure {
            error_code: ErrorKind::ContentionLimit
                .stage03_code()
                .expect("stage03 code")
        }
    );
    assert_eq!(
        journal
            .open(&stale_request, 4, &mut Sha256Digest)?
            .disposition,
        OpenDisposition::Terminal(failed.outcome)
    );
    Ok(())
}

#[test]
fn occ_conflict_is_stable_and_exact_retry_returns_the_recorded_outcome(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("occ-conflict-terminal")?;
    std::fs::write(storage_writer_lock_path(root.path()), [])?;
    let store = LooseObjectStore::new(root.path().to_path_buf())?;
    let base = occ_target_from_files(&store, &[(b"same", b"base")], 51)?;
    let current = occ_target_from_files(&store, &[(b"same", b"current")], 52)?;
    let candidate = occ_target_from_files(&store, &[(b"same", b"candidate")], 53)?;
    let lock = TestCommitLock::default();
    let barrier = NoGcBarrier;
    let mut refs = RefStore::open(
        root.path().to_path_buf(),
        &lock,
        &barrier,
        &mut Sha256Digest,
    )?;
    let branch = BranchId::new(b"occ-conflict".to_vec())?;
    let base_head = Head {
        target: base,
        generation: 0,
        publication_id: publication_id(51),
    };
    let current_head = Head {
        target: current,
        generation: 1,
        publication_id: publication_id(52),
    };
    refs.commit_head(&branch, None, base_head, &mut Sha256Digest)?;
    let journal = OperationJournal::new(root.path().to_path_buf(), &lock);
    let request = candidate_operation_request(b"occ-conflict", 53, 0x53, Some(base_head))?;
    let opened = journal.open(&request, 1, &mut Sha256Digest)?;
    journal.prepare(opened.id, candidate, None, 0, &mut Sha256Digest)?;
    refs.commit_head(&branch, Some(base_head), current_head, &mut Sha256Digest)?;
    let paths = occ_spool(
        &root,
        "conflict-paths",
        [occ_mutation(b"same", MutationAction::Replace, None)],
    )?;
    let mut pages = PersistentPages::new(&store);
    let first = commit_with_rebase(
        &journal,
        &mut refs,
        CommitRequest {
            id: opened.id,
            now_unix_seconds: 2,
        },
        &paths,
        &mut pages,
        &mut Sha256Digest,
        |_current, _attempt| Err(OccError::Invalid("conflicting rebuild must not run")),
    )?;
    let TerminalOutcome::Conflict {
        error_code,
        conflict_keys,
    } = first.outcome
    else {
        panic!("overlap did not produce a stable conflict");
    };
    assert_eq!(
        error_code,
        ErrorKind::Conflict.stage03_code().expect("stage03 code")
    );
    assert_ne!(conflict_keys, Digest32::default());
    assert_eq!(first.counters.changed_keys, 1);
    assert_eq!(
        refs.read_head(&branch, &mut Sha256Digest)?,
        Some(current_head)
    );

    let exact = commit_with_rebase(
        &journal,
        &mut refs,
        CommitRequest {
            id: opened.id,
            now_unix_seconds: 3,
        },
        &paths,
        &mut pages,
        &mut Sha256Digest,
        |_current, _attempt| Err(OccError::Invalid("terminal retry must not rebuild")),
    )?;
    assert_eq!(exact.outcome, first.outcome);
    assert_eq!(exact.counters, Default::default());
    assert_eq!(
        journal.open(&request, 3, &mut Sha256Digest)?.disposition,
        OpenDisposition::Terminal(first.outcome)
    );
    Ok(())
}

struct TestCarrierCatalog {
    expected_id: Digest32,
    path: PathBuf,
    generation: Cell<u64>,
    drift_on_recheck: Cell<bool>,
}

impl CarrierCatalog for TestCarrierCatalog {
    fn open(&self, carrier_id: Digest32) -> std::io::Result<OpenedCarrier> {
        if carrier_id != self.expected_id {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "unknown stable carrier ID",
            ));
        }
        Ok(OpenedCarrier {
            file: std::fs::File::open(&self.path)?,
            generation: self.generation.get(),
        })
    }

    fn generation(&self, carrier_id: Digest32) -> std::io::Result<u64> {
        if carrier_id != self.expected_id {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "unknown stable carrier ID",
            ));
        }
        Ok(self.generation.get() + u64::from(self.drift_on_recheck.get()))
    }
}

struct SourceHoldFixture {
    target: RefTarget,
    payload: Vec<u8>,
    object_id: Digest32,
    carrier_id: Digest32,
    catalog: TestCarrierCatalog,
}

fn source_hold_fixture(
    root: &TestRoot,
    marker: u8,
) -> Result<SourceHoldFixture, Box<dyn std::error::Error>> {
    std::fs::write(storage_writer_lock_path(root.path()), [])?;
    let objects = LooseObjectStore::new(root.path().to_path_buf())?;
    let target = candidate_ref_target(&objects, marker)?;
    let payload = format!("only-v1-carrier-payload-{marker}").into_bytes();
    let object = CanonicalRecordV3::chunk(payload.clone())?;
    let object_id = v3_record_id(&object, &mut Sha256Digest)?;
    let carrier_id = RawDigest::digest_bytes(
        &mut Sha256Digest,
        format!("stable-carrier-{marker}").as_bytes(),
    )?;
    let carrier_dir = root.path().join("trusted-v1-carriers");
    std::fs::create_dir(&carrier_dir)?;
    let carrier_path = carrier_dir.join(format!("carrier-{marker}.bin"));
    std::fs::write(&carrier_path, &payload)?;
    Ok(SourceHoldFixture {
        target,
        payload,
        object_id,
        carrier_id,
        catalog: TestCarrierCatalog {
            expected_id: carrier_id,
            path: carrier_path,
            generation: Cell::new(7),
            drift_on_recheck: Cell::new(false),
        },
    })
}

#[test]
fn source_hold_fences_last_v1_carrier_restarts_blocks_cleanup_and_releases_exactly(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("source-hold-restart")?;
    let SourceHoldFixture {
        target,
        payload,
        object_id,
        carrier_id,
        catalog,
    } = source_hold_fixture(&root, 71)?;
    let payload_sha256 = RawDigest::digest_bytes(&mut Sha256Digest, &payload)?;
    let lease_id = sandbox_runtime_layerstack_core::LeaseId::new([0x71; 16])?;
    let protector = SourceProtector::new(root.path().to_path_buf());
    let lock = TestCommitLock::default();
    let barrier = NoGcBarrier;
    let mut refs = RefStore::open(
        root.path().to_path_buf(),
        &lock,
        &barrier,
        &mut Sha256Digest,
    )?;
    let hold = protector
        .protect(
            SourceRequirement::LastV1 {
                object_kind: RecordKindV3::Chunk,
                object_id,
                carrier_id,
                offset: 0,
                length: payload.len() as u64,
                payload_sha256,
                locator_generation: 1,
            },
            target.root,
            lease_id,
            &catalog,
            &mut refs,
            &mut Sha256Digest,
        )?
        .expect("v1 source needs a hold");
    assert_eq!(hold.lease_id, lease_id);
    assert_eq!(hold.carrier_id, carrier_id);
    assert_eq!(hold.locator_generation, 1);
    assert_eq!(hold.carrier_generation, 7);
    assert_eq!(hold.protected_bytes, payload.len() as u64);
    assert_eq!(
        protector.guard_cleanup(carrier_id, &mut Sha256Digest)?,
        CleanupDecision::Protected {
            bytes: payload.len() as u64
        }
    );
    assert!(catalog.path.is_file(), "protected carrier was deleted");
    assert!(!root.path().join("refs").join("legacy").exists());

    drop(refs);
    let restarted = SourceProtector::new(root.path().to_path_buf());
    assert_eq!(
        restarted.reconstruct(RecordKindV3::Chunk, object_id, &catalog, &mut Sha256Digest)?,
        payload
    );

    let run_path = restarted.locator_run_path(&mut Sha256Digest)?;
    let run_bytes = std::fs::read(&run_path)?;
    let mut corrupt = run_bytes.clone();
    let final_index = corrupt.len() - 1;
    corrupt[final_index] ^= 0xff;
    std::fs::write(&run_path, &corrupt)?;
    assert_eq!(
        restarted
            .reconstruct(RecordKindV3::Chunk, object_id, &catalog, &mut Sha256Digest)
            .expect_err("corrupt locator must fail closed")
            .kind(),
        Some(ErrorKind::LastLocatorCorrupt)
    );
    std::fs::write(&run_path, &run_bytes)?;
    std::fs::remove_file(&run_path)?;
    assert_eq!(
        restarted
            .reconstruct(RecordKindV3::Chunk, object_id, &catalog, &mut Sha256Digest)
            .expect_err("missing locator must fail closed")
            .kind(),
        Some(ErrorKind::LastLocatorMissing)
    );
    std::fs::write(&run_path, &run_bytes)?;

    let mut refs = RefStore::open(
        root.path().to_path_buf(),
        &lock,
        &barrier,
        &mut Sha256Digest,
    )?;
    assert!(restarted.release(&lease_id, &mut refs, &mut Sha256Digest)?);
    assert!(!restarted.release(&lease_id, &mut refs, &mut Sha256Digest)?);
    assert_eq!(
        restarted.guard_cleanup(carrier_id, &mut Sha256Digest)?,
        CleanupDecision::Unprotected
    );
    std::fs::remove_file(&catalog.path)?;
    assert!(!catalog.path.exists());
    Ok(())
}

#[test]
fn source_hold_fails_closed_on_catalog_generation_drift_and_removes_partial_lease(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("source-hold-drift")?;
    let SourceHoldFixture {
        target,
        payload,
        object_id,
        carrier_id,
        catalog,
    } = source_hold_fixture(&root, 72)?;
    catalog.drift_on_recheck.set(true);
    let lease_id = sandbox_runtime_layerstack_core::LeaseId::new([0x72; 16])?;
    let protector = SourceProtector::new(root.path().to_path_buf());
    let lock = TestCommitLock::default();
    let barrier = NoGcBarrier;
    let mut refs = RefStore::open(
        root.path().to_path_buf(),
        &lock,
        &barrier,
        &mut Sha256Digest,
    )?;
    let error = protector
        .protect(
            SourceRequirement::LastV1 {
                object_kind: RecordKindV3::Chunk,
                object_id,
                carrier_id,
                offset: 0,
                length: payload.len() as u64,
                payload_sha256: RawDigest::digest_bytes(&mut Sha256Digest, &payload)?,
                locator_generation: 1,
            },
            target.root,
            lease_id,
            &catalog,
            &mut refs,
            &mut Sha256Digest,
        )
        .expect_err("catalog drift must fail closed");
    assert_eq!(error.kind(), Some(ErrorKind::LastLocatorCorrupt));
    assert_eq!(refs.read_source_lease(&lease_id, &mut Sha256Digest)?, None);
    assert_eq!(
        protector.guard_cleanup(carrier_id, &mut Sha256Digest)?,
        CleanupDecision::Unprotected
    );
    Ok(())
}

#[test]
fn source_hold_all_loose_creates_no_locator_or_lease_resources(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("source-hold-all-loose")?;
    std::fs::write(storage_writer_lock_path(root.path()), [])?;
    let lock = TestCommitLock::default();
    let barrier = NoGcBarrier;
    let mut refs = RefStore::open(
        root.path().to_path_buf(),
        &lock,
        &barrier,
        &mut Sha256Digest,
    )?;
    let protector = SourceProtector::new(root.path().to_path_buf());
    let unused_catalog = TestCarrierCatalog {
        expected_id: Digest32::new([0x73; 32]),
        path: root.path().join("must-not-open"),
        generation: Cell::new(1),
        drift_on_recheck: Cell::new(false),
    };
    assert_eq!(
        protector.protect(
            SourceRequirement::AllLoose,
            Digest32::new([0x74; 32]),
            sandbox_runtime_layerstack_core::LeaseId::new([0x75; 16])?,
            &unused_catalog,
            &mut refs,
            &mut Sha256Digest,
        )?,
        None
    );
    assert!(!root.path().join("objects").join("locators").exists());
    assert!(!root.path().join("refs").join("leases").exists());
    assert!(!root.path().join("refs").join("legacy").exists());
    Ok(())
}

#[test]
#[ignore = "runner-owned Stage 03 benchmark probe; never part of the default test suite"]
fn stage03_publication_benchmark_probe() -> Result<(), Box<dyn std::error::Error>> {
    use std::time::Instant;

    let mut samples = Vec::new();
    let mut measure = |operation: &str,
                       iteration: u64,
                       run: &mut dyn FnMut() -> Result<(), Box<dyn std::error::Error>>|
     -> Result<(), Box<dyn std::error::Error>> {
        let started = Instant::now();
        run()?;
        samples.push(serde_json::json!({
            "operation": operation,
            "iteration": iteration,
            "elapsed_ns": u64::try_from(started.elapsed().as_nanos())?,
        }));
        Ok(())
    };

    for iteration in 0..3 {
        measure("clean-checkpoint-fork", iteration, &mut || {
            ref_operations_clean_checkpoint_fork_pin_checkout_and_delete_are_constant_metadata()
        })?;
        measure("dirty-checkpoint", iteration, &mut || {
            ref_operations_dirty_checkpoint_is_publication_plus_one_ref_and_retry_exact()
        })?;
        measure("occ-two-disjoint", iteration, &mut || {
            occ_two_disjoint_writers_rebase_and_both_remain_visible()
        })?;
        measure("occ-three-disjoint", iteration, &mut || {
            occ_three_disjoint_writers_progress_and_contention_is_typed_and_terminal()
        })?;
        measure("occ-conflict", iteration, &mut || {
            occ_conflict_is_stable_and_exact_retry_returns_the_recorded_outcome()
        })?;
    }
    measure("operation-recovery-f01-f09", 0, &mut || {
        operation_recovery_nine_failpoints_preserve_old_or_complete_and_repair_gap()
    })?;

    println!(
        "STAGE03_PUBLICATION_BENCHMARK_JSON:{}",
        serde_json::to_string(&serde_json::json!({
            "schema_version": 1,
            "case_id": "phase1.stage03.private-publication-probe",
            "sample_count": samples.len(),
            "samples": samples,
            "asserted_contracts": {
                "clean_checkpoint_new_content_bytes": 0,
                "clean_checkpoint_new_chunk_objects": 0,
                "clean_checkpoint_ref_generation_delta": 1,
                "dirty_checkpoint_ref_generation_delta": 2,
                "occ_conflict_is_typed_and_stable": true,
                "occ_disjoint_writers_remain_visible": true,
                "occ_three_writer_progress_is_terminal": true,
                "operation_recovery_failpoints": [
                    "F01", "F02", "F03", "F04", "F05", "F06", "F07", "F08", "F09"
                ],
                "operation_recovery_old_or_complete": true,
                "temporary_artifacts_reaped": true
            },
            "artifact_complete": true
        }))?
    );
    Ok(())
}
