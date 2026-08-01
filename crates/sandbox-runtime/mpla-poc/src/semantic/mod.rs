pub mod allocation;
pub mod attribution;
pub mod chunk;
pub mod record;
pub mod scan;
pub mod spool;
pub mod trie;

use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;
#[cfg(unix)]
use std::{ffi::OsString, os::unix::ffi::OsStringExt};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::config::{
    MAX_DATA_WORKERS, RESIDENT_POOL_BYTES, SEMANTIC_MAX_DATA_FDS, SEMANTIC_MERGE_FAN_IN,
    SEMANTIC_SCAN_TRANSFER_BYTES, SEMANTIC_SCAN_WINDOW_BYTES, SEMANTIC_SPOOL_RUN_BYTES,
    SEMANTIC_TRIE_FAN_OUT,
};
use crate::m1_contract::SEMANTIC_FORMAT_VERSION;
use crate::recovery::reach_real_operation;
use crate::{
    AttributionInput, CanonicalDurabilityReceipt, CanonicalRootPair, NamedFaultInjector,
    NamedFaultPoint, OperationId, PocError, PocResult, SemanticBuildReceipt, SemanticBuildRequest,
    SemanticPhaseSpan, SCHEMA_VERSION,
};

use self::record::{RecordMutation, RecordStreamReader, SemanticRecord};
use self::scan::ScanStats;
use self::spool::{BoundedSpool, SortedSpool, SpoolStats};
use self::trie::{ImmutableObjectStore, TrieRoots};

const MAIN_SPOOL_MEMORY_BYTES: usize = 3 * 1024 * 1024;
const HARDLINK_SPOOL_MEMORY_BYTES: usize = 1024 * 1024;
const DELTA_MAGIC: &[u8; 8] = b"MPLADLT1";
const MANIFEST_VERSION: u32 = 1;
const MAX_AFFECTED_STREAM_BYTES: u64 = 16 * 1024 * 1024;
const MAX_AFFECTED_RECORDS: u64 = 4_096;
const MAX_INCREMENTAL_MUTATION_BATCH_BYTES: usize = 1024 * 1024;
const INCREMENTAL_MUTATION_BATCH_MANAGED_BYTES: usize = 2 * 1024 * 1024;
const SEMANTIC_SPOOL_PEAK_DATA_FDS: usize = SEMANTIC_MERGE_FAN_IN + 3;
const IMMUTABLE_SOURCE_CHAIN_FILE: &str = "immutable-source-chain.json";
const IMMUTABLE_SOURCE_CHAIN_VERSION: u32 = 1;
const MAX_IMMUTABLE_SOURCE_ROOTS: usize = 64;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SemanticResourceMaxima {
    pub application_pool_bytes: u64,
    pub peak_managed_bytes: u64,
    pub scan_window_bytes: usize,
    pub spool_run_bytes: usize,
    pub merge_fan_in: usize,
    pub peak_open_data_fds: u16,
    pub peak_data_workers: u16,
    pub trie_fan_out: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticPhaseMaxima {
    pub peak_managed_bytes: u64,
    pub peak_open_data_fds: u16,
    pub peak_data_workers: u16,
}

impl SemanticResourceMaxima {
    /// Preserve the fixed semantic configuration while accounting for an
    /// earlier, strictly sequential phase of the same publication.
    pub fn with_sequential_phase(&self, phase: SemanticPhaseMaxima) -> Self {
        Self {
            application_pool_bytes: self.application_pool_bytes,
            peak_managed_bytes: self.peak_managed_bytes.max(phase.peak_managed_bytes),
            scan_window_bytes: self.scan_window_bytes,
            spool_run_bytes: self.spool_run_bytes,
            merge_fan_in: self.merge_fan_in,
            peak_open_data_fds: self.peak_open_data_fds.max(phase.peak_open_data_fds),
            peak_data_workers: self.peak_data_workers.max(phase.peak_data_workers),
            trie_fan_out: self.trie_fan_out,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticBuildOutput {
    pub receipt: SemanticBuildReceipt,
    pub record_stream_path: PathBuf,
    pub root_manifest_path: PathBuf,
    pub resource_maxima: SemanticResourceMaxima,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IncrementalBuildRequest {
    pub schema_version: u32,
    pub operation_id: OperationId,
    pub prior_manifest: PathBuf,
    pub expected_prior_roots: CanonicalRootPair,
    pub expected_prior_record_stream_sha256: String,
    pub affected_stream: PathBuf,
    pub affected_stream_sha256: String,
    pub affected_ranges_complete: bool,
    pub canonical_object_dir: PathBuf,
    pub attribution: AttributionInput,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IncrementalBuildOutput {
    pub receipt: SemanticBuildReceipt,
    pub root_manifest_path: PathBuf,
    pub affected_record_count: u64,
    pub affected_input_bytes: u64,
    pub prior_node_bytes_read: u64,
    pub immutable_payload_bytes_read: u64,
    pub resource_maxima: SemanticResourceMaxima,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AffectedPathSnapshot {
    pub paths: Vec<PathBuf>,
    pub records: Vec<SemanticRecord>,
    pub payload_bytes_read: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct RootManifest {
    version: u32,
    semantic_format: String,
    content_root: String,
    attribution_root: String,
    record_stream_sha256: String,
    entry_count: u64,
    attribution_descriptor_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct StreamRecipe {
    version: u32,
    prior_record_stream_sha256: String,
    affected_stream_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct ImmutableSourceChain {
    version: u32,
    source_roots: Vec<PathBuf>,
}

pub fn build(request: &SemanticBuildRequest) -> PocResult<SemanticBuildReceipt> {
    build_with_output(request).map(|output| output.receipt)
}

pub fn build_with_output(request: &SemanticBuildRequest) -> PocResult<SemanticBuildOutput> {
    build_with_output_using_scan_and_trie(request, scan::scan_tree, trie::build_from_sorted_records)
}

/// Build a semantic receipt in the holder-namespace storage helper without
/// creating worker threads after that helper has installed its fixed seccomp
/// policy. This is intentionally for initial fixture sealing only; regular
/// publications retain their bounded concurrent hot path.
pub fn build_with_output_serial(request: &SemanticBuildRequest) -> PocResult<SemanticBuildOutput> {
    build_with_output_using_scan_and_trie(
        request,
        scan::scan_tree_serial,
        trie::build_from_sorted_records_serial,
    )
}

fn build_with_output_using_scan_and_trie(
    request: &SemanticBuildRequest,
    scan_tree: fn(&Path, &mut BoundedSpool, &mut BoundedSpool) -> PocResult<ScanStats>,
    build_trie: fn(
        &SortedSpool,
        &AttributionInput,
        &mut ImmutableObjectStore,
    ) -> PocResult<TrieRoots>,
) -> PocResult<SemanticBuildOutput> {
    validate_full_request(request)?;
    prepare_empty_directory(&request.spool_dir)?;
    prepare_canonical_object_directory(&request.canonical_object_dir)?;
    validate_full_path_isolation(request)?;
    let mut named_faults = NamedFaultInjector::default().with_physical_context(
        request.operation_id.as_str(),
        [
            request.canonical_object_dir.clone(),
            request.sealed_tree.clone(),
        ],
    );

    let started = Instant::now();
    let scan_started = Instant::now();
    let record_spool_dir = request.spool_dir.join("records");
    let hardlink_spool_dir = request.spool_dir.join("hardlinks");
    let mut records = BoundedSpool::new(record_spool_dir, MAIN_SPOOL_MEMORY_BYTES)?;
    let mut hardlinks = BoundedSpool::new(hardlink_spool_dir, HARDLINK_SPOOL_MEMORY_BYTES)?;
    let scan = scan_tree(&request.sealed_tree, &mut records, &mut hardlinks)?;
    scan::append_hardlink_records(&mut records, hardlinks.finish()?, &request.spool_dir)?;
    let scan_elapsed = elapsed_ns(scan_started);

    let sort_started = Instant::now();
    let sorted = records.finish()?;
    let sort_elapsed = elapsed_ns(sort_started);
    let spool_stats = sorted.stats();

    let hash_started = Instant::now();
    let mut store = ImmutableObjectStore::new(&request.canonical_object_dir)?;
    reach_real_operation(
        &mut named_faults,
        NamedFaultPoint::CanonicalBeforeInstall,
        &request.operation_id,
        [request.canonical_object_dir.clone()],
        Some(&request.sealed_tree),
        true,
    )?;
    let roots = build_trie(&sorted, &request.attribution, &mut store)?;
    store.sync_files()?;
    reach_real_operation(
        &mut named_faults,
        NamedFaultPoint::CanonicalAfterObjectFsync,
        &request.operation_id,
        [request.canonical_object_dir.clone()],
        Some(&request.sealed_tree),
        true,
    )?;
    let hash_elapsed = elapsed_ns(hash_started);

    let install_started = Instant::now();
    let manifest = manifest_for(&roots, spool_stats.records_out, &request.attribution);
    let record_stream_path =
        materialize_sorted_record_stream(&manifest, &request.canonical_object_dir, &sorted)?;
    store.sync_directory()?;
    reach_real_operation(
        &mut named_faults,
        NamedFaultPoint::CanonicalAfterObjectDirFsync,
        &request.operation_id,
        [
            request.canonical_object_dir.clone(),
            record_stream_path.clone(),
        ],
        Some(&request.sealed_tree),
        true,
    )?;
    let object_set_sha256 = store.object_set_sha256()?;
    let root_manifest_path = install_manifest(&request.canonical_object_dir, &manifest)?;
    reach_real_operation(
        &mut named_faults,
        NamedFaultPoint::CanonicalAfterRootManifestFsync,
        &request.operation_id,
        [root_manifest_path.clone()],
        Some(&request.sealed_tree),
        true,
    )?;
    let install_elapsed = elapsed_ns(install_started);
    let durability = durability_receipt(
        &root_manifest_path,
        &store,
        object_set_sha256,
        request.attribution.clone(),
    )?;
    let receipt = build_receipt(
        request.operation_id.clone(),
        &roots,
        &manifest,
        &scan,
        &spool_stats,
        durability,
        vec![
            phase("scan", scan_elapsed),
            phase("sort", sort_elapsed),
            phase("hash", hash_elapsed),
            phase("canonical-install", install_elapsed),
            phase("semantic-total", elapsed_ns(started)),
        ],
    )?;
    let resource_maxima = resource_maxima(
        &spool_stats,
        scan.peak_open_data_fds,
        scan.peak_data_workers,
    );
    Ok(SemanticBuildOutput {
        receipt,
        record_stream_path,
        root_manifest_path,
        resource_maxima,
    })
}

pub fn build_incremental(request: &IncrementalBuildRequest) -> PocResult<IncrementalBuildOutput> {
    if request.schema_version != SCHEMA_VERSION || !request.affected_ranges_complete {
        return Err(PocError::Integrity(
            "incremental semantic input is incomplete or has an unsupported schema".to_owned(),
        ));
    }
    if request.affected_stream_sha256 != sha256_file(&request.affected_stream)? {
        return Err(PocError::Integrity(
            "affected semantic stream digest mismatch".to_owned(),
        ));
    }
    let prior = load_manifest(&request.prior_manifest)?;
    validate_prior_manifest(request, &prior)?;
    // The root-manifest parent must be present before the final root-directory
    // durability barrier below.  Creating it here keeps that preparation
    // outside the publication interval without weakening the later
    // data-before-reference order.
    prepare_canonical_object_directory(&request.canonical_object_dir)?;
    let source_roots =
        incremental_source_roots(&request.prior_manifest, &request.canonical_object_dir)?;
    let mut named_faults = NamedFaultInjector::default().with_physical_context(
        request.operation_id.as_str(),
        [
            request.canonical_object_dir.clone(),
            request.prior_manifest.clone(),
        ],
    );

    let started = Instant::now();
    let affected_stream_open_started = Instant::now();
    let mut reader = DeltaStreamReader::open(&request.affected_stream)?;
    let affected_stream_open_elapsed = elapsed_ns(affected_stream_open_started);
    let store_open_started = Instant::now();
    let mut store =
        ImmutableObjectStore::new_incremental(&request.canonical_object_dir, &source_roots)?;
    let store_open_elapsed = elapsed_ns(store_open_started);
    reach_real_operation(
        &mut named_faults,
        NamedFaultPoint::CanonicalBeforeInstall,
        &request.operation_id,
        [request.canonical_object_dir.clone()],
        None,
        true,
    )?;
    let mut roots = TrieRoots::from_hex(&prior.content_root, &prior.attribution_root)?;
    let validate_apply_started = Instant::now();
    trie::validate_roots(&roots, &mut store)?;
    let mut entry_count = prior.entry_count;
    let mut affected_record_count = 0_u64;
    let mut previous_key = None;
    let mut batch = Vec::new();
    let mut batch_bytes = 0_usize;
    while let Some(mutation) = reader.next_mutation()? {
        let key = mutation.key_digest()?;
        if previous_key
            .as_ref()
            .is_some_and(|previous| previous >= &key)
        {
            return Err(PocError::Integrity(
                "affected semantic stream is not strictly key-sorted".to_owned(),
            ));
        }
        previous_key = Some(key);
        let mutation_bytes = mutation.encode()?.len();
        if !batch.is_empty()
            && batch_bytes.saturating_add(mutation_bytes) > MAX_INCREMENTAL_MUTATION_BATCH_BYTES
        {
            let outcome =
                trie::apply_mutation_batch(&roots, &batch, &request.attribution, &mut store)?;
            roots = outcome.roots;
            entry_count = apply_entry_count_delta(entry_count, outcome.entry_count_delta)?;
            batch.clear();
            batch_bytes = 0;
        }
        batch_bytes = batch_bytes.saturating_add(mutation_bytes);
        batch.push(mutation);
        affected_record_count = affected_record_count.saturating_add(1);
    }
    if !batch.is_empty() {
        let outcome = trie::apply_mutation_batch(&roots, &batch, &request.attribution, &mut store)?;
        roots = outcome.roots;
        entry_count = apply_entry_count_delta(entry_count, outcome.entry_count_delta)?;
    }
    let affected_input_bytes = reader.bytes_read();
    drop(reader);
    let validate_apply_elapsed = elapsed_ns(validate_apply_started);
    let staged_commit_started = Instant::now();
    store.commit_incremental_roots(&roots)?;
    let staged_commit_elapsed = elapsed_ns(staged_commit_started);
    // Each newly created pack and index was individually fsynced before its
    // immutable install, and the catalog installation below durably publishes
    // their names.  A filesystem-wide syncfs here adds no additional
    // data-before-reference edge; it only flushes unrelated filesystem work.
    reach_real_operation(
        &mut named_faults,
        NamedFaultPoint::CanonicalAfterObjectFsync,
        &request.operation_id,
        [request.canonical_object_dir.clone()],
        None,
        true,
    )?;
    let update_elapsed = elapsed_ns(started);
    let manifest = manifest_for(&roots, entry_count, &request.attribution);
    let install_started = Instant::now();
    install_incremental_stream_recipe(request, &prior, &manifest)?;
    install_immutable_source_chain(&request.canonical_object_dir, &source_roots)?;
    store.sync_directory()?;
    reach_real_operation(
        &mut named_faults,
        NamedFaultPoint::CanonicalAfterObjectDirFsync,
        &request.operation_id,
        [request.canonical_object_dir.clone()],
        None,
        true,
    )?;
    let object_set_sha256 = store.object_set_sha256()?;
    let root_manifest_path = install_manifest(&request.canonical_object_dir, &manifest)?;
    reach_real_operation(
        &mut named_faults,
        NamedFaultPoint::CanonicalAfterRootManifestFsync,
        &request.operation_id,
        [root_manifest_path.clone()],
        None,
        true,
    )?;
    let install_elapsed = elapsed_ns(install_started);
    let durability = durability_receipt(
        &root_manifest_path,
        &store,
        object_set_sha256,
        request.attribution.clone(),
    )?;
    let spool_stats = SpoolStats::default();
    let receipt = SemanticBuildReceipt {
        schema_version: SCHEMA_VERSION,
        semantic_format: SEMANTIC_FORMAT_VERSION.to_owned(),
        operation_id: request.operation_id.clone(),
        roots: roots.to_root_pair()?,
        record_stream_sha256: manifest.record_stream_sha256,
        entry_count,
        bytes_read: affected_input_bytes,
        spool_runs: 0,
        spool_bytes: 0,
        peak_open_data_fds: trie::INCREMENTAL_PEAK_DATA_FDS
            .try_into()
            .map_err(|_| PocError::Integrity("incremental FD maximum overflow".to_owned()))?,
        peak_data_workers: trie::INCREMENTAL_DATA_WORKERS,
        phase_spans: vec![
            phase(
                "incremental-affected-stream-open",
                affected_stream_open_elapsed,
            ),
            phase("incremental-store-open", store_open_elapsed),
            phase("incremental-validate-apply", validate_apply_elapsed),
            phase("incremental-staged-commit", staged_commit_elapsed),
            phase("incremental-validate-update", update_elapsed),
            phase("canonical-install", install_elapsed),
            phase("semantic-total", elapsed_ns(started)),
        ],
        durability,
    };
    Ok(IncrementalBuildOutput {
        receipt,
        root_manifest_path,
        affected_record_count,
        affected_input_bytes,
        prior_node_bytes_read: store.bytes_read(),
        immutable_payload_bytes_read: 0,
        resource_maxima: resource_maxima(
            &spool_stats,
            trie::INCREMENTAL_PEAK_DATA_FDS,
            trie::INCREMENTAL_DATA_WORKERS,
        ),
    })
}

fn apply_entry_count_delta(entry_count: u64, delta: i64) -> PocResult<u64> {
    if delta >= 0 {
        entry_count
            .checked_add(u64::try_from(delta).unwrap_or(u64::MAX))
            .ok_or_else(|| PocError::Integrity("incremental entry count overflow".to_owned()))
    } else {
        entry_count
            .checked_sub(delta.unsigned_abs())
            .ok_or_else(|| PocError::Integrity("incremental entry count underflow".to_owned()))
    }
}

pub fn write_affected_stream(
    path: &Path,
    mutations: impl IntoIterator<Item = RecordMutation>,
) -> PocResult<String> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| PocError::io("create affected semantic stream", path, error))?;
    let mut writer = BufWriter::new(file);
    writer
        .write_all(DELTA_MAGIC)
        .map_err(|error| PocError::io("write affected semantic stream", path, error))?;
    let mut previous = None;
    for mutation in mutations {
        let key = mutation.key_digest()?;
        if previous.as_ref().is_some_and(|value| value >= &key) {
            return Err(PocError::Integrity(
                "affected semantic mutations must be strictly key-sorted".to_owned(),
            ));
        }
        previous = Some(key);
        let encoded = mutation.encode()?;
        let length = u32::try_from(encoded.len())
            .map_err(|_| PocError::Integrity("affected mutation exceeds u32".to_owned()))?;
        writer
            .write_all(&length.to_be_bytes())
            .and_then(|()| writer.write_all(&encoded))
            .map_err(|error| PocError::io("write affected semantic stream", path, error))?;
    }
    writer
        .flush()
        .map_err(|error| PocError::io("flush affected semantic stream", path, error))?;
    writer
        .get_ref()
        .sync_all()
        .map_err(|error| PocError::io("fsync affected semantic stream", path, error))?;
    sha256_file(path)
}

pub fn capture_affected_paths(
    tree: &Path,
    paths: &[PathBuf],
    work_dir: &Path,
) -> PocResult<AffectedPathSnapshot> {
    capture_affected_paths_with_maxima(tree, paths, work_dir).map(|(snapshot, _)| snapshot)
}

pub fn capture_affected_paths_with_maxima(
    tree: &Path,
    paths: &[PathBuf],
    work_dir: &Path,
) -> PocResult<(AffectedPathSnapshot, SemanticPhaseMaxima)> {
    let scanned = scan::scan_selected_paths(tree, paths, work_dir)?;
    let peak_managed_bytes = scan::SELECTED_RECORD_SPOOL_MEMORY_BYTES
        .saturating_add(scan::SELECTED_HARDLINK_SPOOL_MEMORY_BYTES)
        .saturating_add(
            SEMANTIC_SCAN_TRANSFER_BYTES.saturating_mul(usize::from(scanned.peak_data_workers)),
        )
        .saturating_add(scan::MAX_XATTR_TRANSIENT_BYTES);
    let phase_maxima = SemanticPhaseMaxima {
        peak_managed_bytes: u64::try_from(peak_managed_bytes).unwrap_or(u64::MAX),
        peak_open_data_fds: u16::try_from(scanned.peak_open_data_fds).unwrap_or(u16::MAX),
        peak_data_workers: scanned.peak_data_workers,
    };
    Ok((
        AffectedPathSnapshot {
            paths: paths.to_vec(),
            records: scanned.records,
            payload_bytes_read: scanned.bytes_read,
        },
        phase_maxima,
    ))
}

pub fn write_affected_stream_from_snapshots(
    path: &Path,
    before: &AffectedPathSnapshot,
    after: &AffectedPathSnapshot,
) -> PocResult<String> {
    if before.paths != after.paths {
        return Err(PocError::Integrity(
            "affected path snapshots name different path sets".to_owned(),
        ));
    }
    let before = keyed_records(&before.records)?;
    let after = keyed_records(&after.records)?;
    let keys = before
        .keys()
        .chain(after.keys())
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    let mutations = keys
        .into_iter()
        .filter_map(|key| match (before.get(&key), after.get(&key)) {
            (Some(old), Some(new)) if old == new => None,
            (_, Some(new)) => Some(RecordMutation::Replace((*new).clone())),
            (Some(old), None) => Some(RecordMutation::Delete {
                canonical_key: old
                    .canonical_key()
                    .expect("validated semantic record has a canonical key"),
            }),
            (None, None) => None,
        })
        .collect::<Vec<_>>();
    if mutations.is_empty() {
        return Err(PocError::Integrity(
            "affected path snapshots contain no semantic change".to_owned(),
        ));
    }
    write_affected_stream(path, mutations)
}

fn keyed_records(
    records: &[SemanticRecord],
) -> PocResult<std::collections::BTreeMap<[u8; 32], &SemanticRecord>> {
    let mut keyed = std::collections::BTreeMap::new();
    for record in records {
        let key = record.key_digest()?;
        if keyed.insert(key, record).is_some() {
            return Err(PocError::Integrity(
                "affected path snapshot repeats a canonical key".to_owned(),
            ));
        }
    }
    Ok(keyed)
}

pub fn affected_stream_paths(path: &Path) -> PocResult<Vec<PathBuf>> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| PocError::io("stat affected semantic stream", path, error))?;
    if !metadata.is_file() || metadata.len() > MAX_AFFECTED_STREAM_BYTES {
        return Err(PocError::Integrity(
            "affected semantic stream is not a bounded regular file".to_owned(),
        ));
    }
    let mut reader = DeltaStreamReader::open(path)?;
    let mut previous_key = None;
    let mut paths = Vec::new();
    let mut record_count = 0_u64;
    while let Some(mutation) = reader.next_mutation()? {
        let key = mutation.key_digest()?;
        if previous_key
            .as_ref()
            .is_some_and(|previous| previous >= &key)
        {
            return Err(PocError::Integrity(
                "affected semantic stream is not strictly key-sorted".to_owned(),
            ));
        }
        previous_key = Some(key);
        paths.push(path_from_semantic_bytes(mutation.affected_path()?)?);
        record_count = record_count.saturating_add(1);
        if record_count > MAX_AFFECTED_RECORDS {
            return Err(PocError::Integrity(
                "affected semantic stream exceeds the record bound".to_owned(),
            ));
        }
    }
    if record_count == 0 {
        return Err(PocError::Integrity(
            "receipt-hit affected semantic stream is empty".to_owned(),
        ));
    }
    paths.sort();
    paths.dedup();
    if paths.len() > 64 {
        return Err(PocError::Integrity(
            "affected semantic stream exceeds the path bound".to_owned(),
        ));
    }
    Ok(paths)
}

pub fn materialize_record_stream(
    manifest_path: &Path,
    canonical_object_dir: &Path,
) -> PocResult<PathBuf> {
    let manifest = load_manifest(manifest_path)?;
    let source_roots = materialization_source_roots(canonical_object_dir)?;
    materialize_record_stream_id(
        &manifest.record_stream_sha256,
        canonical_object_dir,
        &source_roots,
        Some((&manifest.content_root, &manifest.attribution_root)),
        Some(manifest.entry_count),
        0,
    )
}

fn materialize_record_stream_id(
    record_stream_sha256: &str,
    canonical_object_dir: &Path,
    source_roots: &[PathBuf],
    legacy_roots: Option<(&str, &str)>,
    expected_count: Option<u64>,
    depth: usize,
) -> PocResult<PathBuf> {
    if depth > 64 {
        return Err(PocError::Integrity(
            "semantic record-stream recipe chain exceeds fixed bound".to_owned(),
        ));
    }
    let streams = canonical_object_dir.join("streams");
    std::fs::create_dir_all(&streams)
        .map_err(|error| PocError::io("create semantic streams directory", &streams, error))?;
    let path = streams.join(format!("{record_stream_sha256}.records"));
    if path.exists() {
        return Ok(path);
    }
    if let Some(source_path) = find_stream_artifact(
        canonical_object_dir,
        source_roots,
        &format!("{record_stream_sha256}.records"),
    ) {
        return Ok(source_path);
    }
    if let Some(recipe_path) = find_stream_artifact(
        canonical_object_dir,
        source_roots,
        &format!("{record_stream_sha256}.recipe.json"),
    ) {
        let bytes = std::fs::read(&recipe_path)
            .map_err(|error| PocError::io("read semantic stream recipe", &recipe_path, error))?;
        let recipe: StreamRecipe = serde_json::from_slice(&bytes)?;
        if recipe.version != MANIFEST_VERSION {
            return Err(PocError::Integrity(
                "semantic stream recipe has unsupported version".to_owned(),
            ));
        }
        let affected = recipe_path
            .parent()
            .ok_or_else(|| {
                PocError::Integrity("semantic stream recipe has no streams directory".to_owned())
            })?
            .join(format!("{record_stream_sha256}.delta"));
        if sha256_file(&affected)? != recipe.affected_stream_sha256 {
            return Err(PocError::Integrity(
                "semantic stream recipe delta digest mismatch".to_owned(),
            ));
        }
        let prior = materialize_record_stream_id(
            &recipe.prior_record_stream_sha256,
            canonical_object_dir,
            source_roots,
            None,
            None,
            depth + 1,
        )?;
        return install_merged_record_stream(&path, &streams, &prior, &affected, expected_count);
    }
    let Some((content_root, attribution_root)) = legacy_roots else {
        return Err(PocError::RecoveryRequired(format!(
            "semantic record stream {record_stream_sha256} has neither materialized bytes nor a durable recipe"
        )));
    };
    let roots = TrieRoots::from_hex(content_root, attribution_root)?;
    let temporary = streams.join(format!(".{record_stream_sha256}-{}.tmp", Uuid::new_v4(),));
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| PocError::io("create semantic record stream", &temporary, error))?;
    let mut writer = BufWriter::new(file);
    let mut store =
        ImmutableObjectStore::new_with_read_only_sources(canonical_object_dir, source_roots)?;
    trie::visit_records(&roots, &mut store, |record| {
        let frame = record.encode_frame()?;
        writer
            .write_all(&frame)
            .map_err(|error| PocError::io("write semantic record stream", &temporary, error))
    })?;
    finish_record_stream_install(writer, &temporary, &path, &streams)
}

fn find_stream_artifact(
    canonical_object_dir: &Path,
    source_roots: &[PathBuf],
    file_name: &str,
) -> Option<PathBuf> {
    std::iter::once(canonical_object_dir)
        .chain(source_roots.iter().map(PathBuf::as_path))
        .map(|root| root.join("streams").join(file_name))
        .find(|path| path.exists())
}

fn materialize_sorted_record_stream(
    manifest: &RootManifest,
    canonical_object_dir: &Path,
    sorted: &SortedSpool,
) -> PocResult<PathBuf> {
    let streams = canonical_object_dir.join("streams");
    std::fs::create_dir_all(&streams)
        .map_err(|error| PocError::io("create semantic streams directory", &streams, error))?;
    let path = streams.join(format!("{}.records", manifest.record_stream_sha256));
    if path.exists() {
        return Ok(path);
    }
    let temporary = streams.join(format!(
        ".{}-{}.tmp",
        manifest.record_stream_sha256,
        Uuid::new_v4()
    ));
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| PocError::io("create semantic record stream", &temporary, error))?;
    let mut writer = BufWriter::new(file);
    let mut count = 0_u64;
    sorted.for_each(|key, payload| {
        let record = SemanticRecord::decode(payload)?;
        if record.key_digest()?.as_slice() != key {
            return Err(PocError::Integrity(
                "semantic sorted record stream key mismatch".to_owned(),
            ));
        }
        writer
            .write_all(&record.encode_frame()?)
            .map_err(|error| PocError::io("write semantic record stream", &temporary, error))?;
        count = count.saturating_add(1);
        Ok(())
    })?;
    if count != manifest.entry_count {
        return Err(PocError::Integrity(
            "semantic record stream count disagrees with root manifest".to_owned(),
        ));
    }
    finish_record_stream_install(writer, &temporary, &path, &streams)
}

fn install_incremental_stream_recipe(
    request: &IncrementalBuildRequest,
    prior: &RootManifest,
    manifest: &RootManifest,
) -> PocResult<()> {
    let streams = request.canonical_object_dir.join("streams");
    std::fs::create_dir_all(&streams)
        .map_err(|error| PocError::io("create semantic streams directory", &streams, error))?;
    let materialized = streams.join(format!("{}.records", manifest.record_stream_sha256));
    if materialized.exists() {
        return Ok(());
    }
    let delta = streams.join(format!("{}.delta", manifest.record_stream_sha256));
    install_immutable_file_copy(&request.affected_stream, &delta)?;
    let recipe = StreamRecipe {
        version: MANIFEST_VERSION,
        prior_record_stream_sha256: prior.record_stream_sha256.clone(),
        affected_stream_sha256: request.affected_stream_sha256.clone(),
    };
    let recipe_path = streams.join(format!("{}.recipe.json", manifest.record_stream_sha256));
    install_immutable_bytes(&serde_json::to_vec(&recipe)?, &recipe_path)?;
    sync_directory(&streams)
}

fn install_merged_record_stream(
    path: &Path,
    streams: &Path,
    prior_path: &Path,
    affected_path: &Path,
    expected_count: Option<u64>,
) -> PocResult<PathBuf> {
    if path.exists() {
        return Ok(path.to_path_buf());
    }
    let temporary = streams.join(format!(
        ".{}-{}.tmp",
        path.file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("semantic-stream"),
        Uuid::new_v4()
    ));
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| PocError::io("create semantic record stream", &temporary, error))?;
    let mut writer = BufWriter::new(file);
    let prior_file = File::open(prior_path)
        .map_err(|error| PocError::io("open prior semantic record stream", prior_path, error))?;
    let mut prior = RecordStreamReader::new(BufReader::new(prior_file));
    let mut affected = DeltaStreamReader::open(affected_path)?;
    let mut prior_head = prior.next_record()?;
    let mut affected_head = affected.next_mutation()?;
    let mut count = 0_u64;
    let mut previous = None;
    while prior_head.is_some() || affected_head.is_some() {
        let prior_key = prior_head
            .as_ref()
            .map(SemanticRecord::key_digest)
            .transpose()?;
        let affected_key = affected_head
            .as_ref()
            .map(RecordMutation::key_digest)
            .transpose()?;
        let next = match (prior_key, affected_key) {
            (Some(left), Some(right)) if left < right => {
                let record = prior_head
                    .take()
                    .ok_or_else(|| PocError::Integrity("prior stream head vanished".to_owned()))?;
                prior_head = prior.next_record()?;
                Some(record)
            }
            (Some(left), Some(right)) if left == right => {
                let mutation = affected_head.take().ok_or_else(|| {
                    PocError::Integrity("affected stream head vanished".to_owned())
                })?;
                prior_head = prior.next_record()?;
                affected_head = affected.next_mutation()?;
                match mutation {
                    RecordMutation::Replace(record) => Some(record),
                    RecordMutation::Delete { .. } => None,
                }
            }
            (Some(_), Some(_)) | (None, Some(_)) => {
                let mutation = affected_head.take().ok_or_else(|| {
                    PocError::Integrity("affected stream head vanished".to_owned())
                })?;
                affected_head = affected.next_mutation()?;
                match mutation {
                    RecordMutation::Replace(record) => Some(record),
                    RecordMutation::Delete { .. } => {
                        return Err(PocError::Integrity(
                            "semantic stream recipe deletes a missing record".to_owned(),
                        ))
                    }
                }
            }
            (Some(_), None) => {
                let record = prior_head
                    .take()
                    .ok_or_else(|| PocError::Integrity("prior stream head vanished".to_owned()))?;
                prior_head = prior.next_record()?;
                Some(record)
            }
            (None, None) => None,
        };
        if let Some(record) = next {
            let key = record.key_digest()?;
            if previous.as_ref().is_some_and(|value| value >= &key) {
                return Err(PocError::Integrity(
                    "materialized semantic stream is not strictly ordered".to_owned(),
                ));
            }
            writer
                .write_all(&record.encode_frame()?)
                .map_err(|error| PocError::io("write semantic record stream", &temporary, error))?;
            previous = Some(key);
            count = count.saturating_add(1);
        }
    }
    if expected_count.is_some_and(|expected| expected != count) {
        return Err(PocError::Integrity(
            "materialized semantic stream count disagrees with manifest".to_owned(),
        ));
    }
    finish_record_stream_install(writer, &temporary, path, streams)
}

fn finish_record_stream_install(
    mut writer: BufWriter<File>,
    temporary: &Path,
    path: &Path,
    streams: &Path,
) -> PocResult<PathBuf> {
    writer
        .flush()
        .map_err(|error| PocError::io("flush semantic record stream", temporary, error))?;
    writer
        .get_ref()
        .sync_all()
        .map_err(|error| PocError::io("fsync semantic record stream", temporary, error))?;
    match std::fs::hard_link(temporary, path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(PocError::io("install semantic record stream", path, error));
        }
    }
    std::fs::remove_file(temporary).map_err(|error| {
        PocError::io(
            "remove installed semantic record stream temporary",
            temporary,
            error,
        )
    })?;
    sync_directory(streams)?;
    Ok(path.to_path_buf())
}

fn install_immutable_file_copy(source: &Path, path: &Path) -> PocResult<()> {
    if path.exists() {
        if sha256_file(path)? == sha256_file(source)? {
            return Ok(());
        }
        return Err(PocError::Integrity(
            "immutable semantic delta collision".to_owned(),
        ));
    }
    let temporary = path.with_file_name(format!(
        ".{}-{}.tmp",
        path.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("semantic-delta"),
        Uuid::new_v4()
    ));
    std::fs::copy(source, &temporary)
        .map_err(|error| PocError::io("copy semantic delta", &temporary, error))?;
    File::open(&temporary)
        .and_then(|file| file.sync_all())
        .map_err(|error| PocError::io("fsync semantic delta", &temporary, error))?;
    install_immutable_temporary(&temporary, path)
}

fn install_immutable_bytes(bytes: &[u8], path: &Path) -> PocResult<()> {
    if path.exists() {
        let existing = std::fs::read(path)
            .map_err(|error| PocError::io("read immutable semantic metadata", path, error))?;
        if existing == bytes {
            return Ok(());
        }
        return Err(PocError::Integrity(
            "immutable semantic metadata collision".to_owned(),
        ));
    }
    let temporary = path.with_file_name(format!(
        ".{}-{}.tmp",
        path.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("semantic-metadata"),
        Uuid::new_v4()
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| PocError::io("create immutable semantic metadata", &temporary, error))?;
    file.write_all(bytes)
        .map_err(|error| PocError::io("write immutable semantic metadata", &temporary, error))?;
    file.sync_all()
        .map_err(|error| PocError::io("fsync immutable semantic metadata", &temporary, error))?;
    drop(file);
    install_immutable_temporary(&temporary, path)
}

fn install_immutable_temporary(temporary: &Path, path: &Path) -> PocResult<()> {
    let install = match std::fs::hard_link(temporary, path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if sha256_file(temporary)? != sha256_file(path)? {
                Err(PocError::Integrity(
                    "immutable semantic file collision".to_owned(),
                ))
            } else {
                Ok(())
            }
        }
        Err(error) => Err(PocError::io("install immutable semantic file", path, error)),
    };
    let cleanup = std::fs::remove_file(temporary)
        .map_err(|error| PocError::io("remove immutable semantic temporary", temporary, error));
    install?;
    cleanup
}

fn validate_full_request(request: &SemanticBuildRequest) -> PocResult<()> {
    if request.schema_version != SCHEMA_VERSION {
        return Err(PocError::Integrity(format!(
            "unsupported semantic build schema {}",
            request.schema_version
        )));
    }
    let metadata = std::fs::metadata(&request.sealed_tree)
        .map_err(|error| PocError::io("stat sealed semantic tree", &request.sealed_tree, error))?;
    if !metadata.is_dir() {
        return Err(PocError::Integrity(
            "sealed semantic tree is not a directory".to_owned(),
        ));
    }
    Ok(())
}

fn prepare_empty_directory(path: &Path) -> PocResult<()> {
    match std::fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let mut entries = std::fs::read_dir(path)
                .map_err(|source| PocError::io("read semantic spool directory", path, source))?;
            if entries.next().is_some() {
                return Err(PocError::Integrity(
                    "semantic spool directory must be empty".to_owned(),
                ));
            }
            Ok(())
        }
        Err(error) => Err(PocError::io("create semantic spool directory", path, error)),
    }
}

fn validate_full_path_isolation(request: &SemanticBuildRequest) -> PocResult<()> {
    let sealed_tree = std::fs::canonicalize(&request.sealed_tree).map_err(|error| {
        PocError::io(
            "canonicalize sealed semantic tree",
            &request.sealed_tree,
            error,
        )
    })?;
    let spool_dir = std::fs::canonicalize(&request.spool_dir).map_err(|error| {
        PocError::io(
            "canonicalize semantic spool directory",
            &request.spool_dir,
            error,
        )
    })?;
    let object_dir = std::fs::canonicalize(&request.canonical_object_dir).map_err(|error| {
        PocError::io(
            "canonicalize semantic object directory",
            &request.canonical_object_dir,
            error,
        )
    })?;
    for (left_label, left, right_label, right) in [
        ("sealed tree", &sealed_tree, "spool directory", &spool_dir),
        ("sealed tree", &sealed_tree, "object directory", &object_dir),
        (
            "spool directory",
            &spool_dir,
            "object directory",
            &object_dir,
        ),
    ] {
        if left.starts_with(right) || right.starts_with(left) {
            return Err(PocError::Integrity(format!(
                "semantic {left_label} and {right_label} must not overlap"
            )));
        }
    }
    Ok(())
}

fn manifest_for(
    roots: &TrieRoots,
    entry_count: u64,
    attribution: &AttributionInput,
) -> RootManifest {
    RootManifest {
        version: MANIFEST_VERSION,
        semantic_format: SEMANTIC_FORMAT_VERSION.to_owned(),
        content_root: roots.content_hex(),
        attribution_root: roots.attribution_hex(),
        record_stream_sha256: roots.record_stream_sha256(),
        entry_count,
        attribution_descriptor_sha256: attribution::descriptor_sha256(attribution),
    }
}

fn prepare_canonical_object_directory(object_dir: &Path) -> PocResult<()> {
    std::fs::create_dir_all(object_dir)
        .map_err(|error| PocError::io("create canonical object directory", object_dir, error))?;
    let manifests = object_dir.join("manifests");
    std::fs::create_dir_all(&manifests)
        .map_err(|error| PocError::io("create root manifest directory", &manifests, error))
}

fn install_manifest(object_dir: &Path, manifest: &RootManifest) -> PocResult<PathBuf> {
    let manifests = object_dir.join("manifests");
    let metadata = std::fs::metadata(&manifests)
        .map_err(|error| PocError::io("stat root manifest directory", &manifests, error))?;
    if !metadata.is_dir() {
        return Err(PocError::Integrity(
            "root manifest parent is not a directory".to_owned(),
        ));
    }
    let path = manifests.join(format!(
        "{}-{}.json",
        manifest.content_root, manifest.attribution_root
    ));
    let bytes = serde_json::to_vec(manifest)?;
    if path.exists() {
        let existing = std::fs::read(&path)
            .map_err(|error| PocError::io("read existing root manifest", &path, error))?;
        if existing != bytes {
            return Err(PocError::Integrity(
                "immutable root manifest collision".to_owned(),
            ));
        }
        return Ok(path);
    }
    let temporary = manifests.join(format!(
        ".{}-{}-{}.tmp",
        manifest.content_root,
        manifest.attribution_root,
        Uuid::new_v4()
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| PocError::io("create root manifest", &temporary, error))?;
    file.write_all(&bytes)
        .map_err(|error| PocError::io("write root manifest", &temporary, error))?;
    file.sync_all()
        .map_err(|error| PocError::io("fsync root manifest", &temporary, error))?;
    match std::fs::hard_link(&temporary, &path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = std::fs::read(&path)
                .map_err(|source| PocError::io("read concurrent root manifest", &path, source))?;
            if existing != bytes {
                return Err(PocError::Integrity(
                    "immutable root manifest collision".to_owned(),
                ));
            }
        }
        Err(error) => {
            return Err(PocError::io("install root manifest", &path, error));
        }
    }
    std::fs::remove_file(&temporary)
        .map_err(|error| PocError::io("remove root manifest temporary", &temporary, error))?;
    sync_directory(&manifests)?;
    Ok(path)
}

fn load_manifest(path: &Path) -> PocResult<RootManifest> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| PocError::io("stat semantic root manifest", path, error))?;
    if !metadata.is_file() || metadata.len() > 16 * 1024 {
        return Err(PocError::Integrity(
            "semantic root manifest is not a bounded regular file".to_owned(),
        ));
    }
    let bytes = std::fs::read(path)
        .map_err(|error| PocError::io("read semantic root manifest", path, error))?;
    let manifest: RootManifest = serde_json::from_slice(&bytes)?;
    if manifest.version != MANIFEST_VERSION || manifest.semantic_format != SEMANTIC_FORMAT_VERSION {
        return Err(PocError::Integrity(
            "unsupported semantic root manifest".to_owned(),
        ));
    }
    Ok(manifest)
}

fn validate_prior_manifest(
    request: &IncrementalBuildRequest,
    manifest: &RootManifest,
) -> PocResult<()> {
    if manifest.content_root != request.expected_prior_roots.root_id.as_str()
        || manifest.attribution_root != request.expected_prior_roots.attribution_root_id.as_str()
        || manifest.record_stream_sha256 != request.expected_prior_record_stream_sha256
        || manifest.attribution_descriptor_sha256
            != attribution::descriptor_sha256(&request.attribution)
    {
        return Err(PocError::Integrity(
            "incremental prior state does not match its validated handles".to_owned(),
        ));
    }
    Ok(())
}

fn incremental_source_roots(
    prior_manifest: &Path,
    canonical_object_dir: &Path,
) -> PocResult<Vec<PathBuf>> {
    let manifests = prior_manifest.parent().ok_or_else(|| {
        PocError::Integrity("incremental prior manifest has no manifest directory".to_owned())
    })?;
    if manifests.file_name().and_then(|name| name.to_str()) != Some("manifests") {
        return Err(PocError::Integrity(
            "incremental prior manifest has an unexpected parent directory".to_owned(),
        ));
    }
    let prior_root = manifests.parent().ok_or_else(|| {
        PocError::Integrity("incremental prior manifest has no canonical directory".to_owned())
    })?;
    let destination = std::fs::canonicalize(canonical_object_dir).map_err(|error| {
        PocError::io(
            "canonicalize incremental canonical object directory",
            canonical_object_dir,
            error,
        )
    })?;
    let prior_root = canonical_source_root(prior_root)?;
    let mut sources = Vec::with_capacity(MAX_IMMUTABLE_SOURCE_ROOTS);
    if prior_root != destination {
        sources.push(prior_root.clone());
    }
    sources.extend(load_immutable_source_chain(&prior_root)?);
    normalize_source_roots(sources, &destination)
}

fn materialization_source_roots(canonical_object_dir: &Path) -> PocResult<Vec<PathBuf>> {
    let destination = std::fs::canonicalize(canonical_object_dir).map_err(|error| {
        PocError::io(
            "canonicalize materialization canonical object directory",
            canonical_object_dir,
            error,
        )
    })?;
    normalize_source_roots(load_immutable_source_chain(&destination)?, &destination)
}

fn canonical_source_root(path: &Path) -> PocResult<PathBuf> {
    let canonical = std::fs::canonicalize(path)
        .map_err(|error| PocError::io("canonicalize immutable semantic source", path, error))?;
    let metadata = std::fs::metadata(&canonical)
        .map_err(|error| PocError::io("stat immutable semantic source", &canonical, error))?;
    if !metadata.is_dir() {
        return Err(PocError::Integrity(
            "immutable semantic source is not a directory".to_owned(),
        ));
    }
    Ok(canonical)
}

fn load_immutable_source_chain(canonical_object_dir: &Path) -> PocResult<Vec<PathBuf>> {
    let path = canonical_object_dir.join(IMMUTABLE_SOURCE_CHAIN_FILE);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(PocError::io(
                "read immutable semantic source chain",
                &path,
                error,
            ));
        }
    };
    if bytes.len() > 16 * 1024 {
        return Err(PocError::Integrity(
            "immutable semantic source chain exceeds fixed bound".to_owned(),
        ));
    }
    let chain: ImmutableSourceChain = serde_json::from_slice(&bytes)?;
    if chain.version != IMMUTABLE_SOURCE_CHAIN_VERSION
        || chain.source_roots.len() > MAX_IMMUTABLE_SOURCE_ROOTS
    {
        return Err(PocError::Integrity(
            "immutable semantic source chain has an unsupported shape".to_owned(),
        ));
    }
    chain
        .source_roots
        .iter()
        .map(|root| {
            if !root.is_absolute() {
                return Err(PocError::Integrity(
                    "immutable semantic source chain has a relative root".to_owned(),
                ));
            }
            canonical_source_root(root)
        })
        .collect()
}

fn normalize_source_roots(roots: Vec<PathBuf>, destination: &Path) -> PocResult<Vec<PathBuf>> {
    let mut normalized = Vec::with_capacity(roots.len());
    for root in roots {
        if root == destination || normalized.iter().any(|existing| existing == &root) {
            continue;
        }
        normalized.push(root);
        if normalized.len() > MAX_IMMUTABLE_SOURCE_ROOTS {
            return Err(PocError::Integrity(
                "immutable semantic source chain exceeds fixed depth".to_owned(),
            ));
        }
    }
    Ok(normalized)
}

fn install_immutable_source_chain(
    canonical_object_dir: &Path,
    source_roots: &[PathBuf],
) -> PocResult<()> {
    if source_roots.is_empty() {
        return Ok(());
    }
    let chain = ImmutableSourceChain {
        version: IMMUTABLE_SOURCE_CHAIN_VERSION,
        source_roots: source_roots.to_vec(),
    };
    let path = canonical_object_dir.join(IMMUTABLE_SOURCE_CHAIN_FILE);
    install_immutable_bytes(&serde_json::to_vec(&chain)?, &path)
}

fn build_receipt(
    operation_id: OperationId,
    roots: &TrieRoots,
    manifest: &RootManifest,
    scan: &ScanStats,
    spool: &SpoolStats,
    durability: CanonicalDurabilityReceipt,
    phase_spans: Vec<SemanticPhaseSpan>,
) -> PocResult<SemanticBuildReceipt> {
    let peak_open_data_fds = scan
        .peak_open_data_fds
        .max(spool.peak_open_files)
        .max(SEMANTIC_SPOOL_PEAK_DATA_FDS);
    if peak_open_data_fds > SEMANTIC_MAX_DATA_FDS {
        return Err(PocError::Integrity(format!(
            "semantic FD maximum {peak_open_data_fds} exceeds fixed limit {SEMANTIC_MAX_DATA_FDS}"
        )));
    }
    Ok(SemanticBuildReceipt {
        schema_version: SCHEMA_VERSION,
        semantic_format: SEMANTIC_FORMAT_VERSION.to_owned(),
        operation_id,
        roots: roots.to_root_pair()?,
        record_stream_sha256: manifest.record_stream_sha256.clone(),
        entry_count: manifest.entry_count,
        bytes_read: scan.bytes_read,
        spool_runs: spool.initial_runs,
        spool_bytes: spool.bytes_written,
        peak_open_data_fds: peak_open_data_fds
            .try_into()
            .map_err(|_| PocError::Integrity("semantic FD maximum overflow".to_owned()))?,
        peak_data_workers: scan.peak_data_workers,
        phase_spans,
        durability,
    })
}

fn durability_receipt(
    manifest_path: &Path,
    store: &ImmutableObjectStore,
    object_set_sha256: String,
    semantic_attribution: AttributionInput,
) -> PocResult<CanonicalDurabilityReceipt> {
    Ok(CanonicalDurabilityReceipt {
        root_manifest: manifest_path.to_path_buf(),
        semantic_attribution,
        immutable_object_count: store.objects_written(),
        immutable_object_bytes: store.bytes_written(),
        object_set_sha256,
        files_fsynced: true,
        object_directory_fsynced: true,
        manifest_fsynced: true,
        manifest_directory_fsynced: true,
    })
}

fn resource_maxima(
    spool: &SpoolStats,
    scan_peak_fds: usize,
    peak_data_workers: u16,
) -> SemanticResourceMaxima {
    let scan_managed_bytes = MAIN_SPOOL_MEMORY_BYTES
        + HARDLINK_SPOOL_MEMORY_BYTES
        + SEMANTIC_SCAN_TRANSFER_BYTES.saturating_mul(usize::from(peak_data_workers))
        + scan::MAX_XATTR_TRANSIENT_BYTES;
    let incremental_apply_managed_bytes = INCREMENTAL_MUTATION_BATCH_MANAGED_BYTES;
    SemanticResourceMaxima {
        application_pool_bytes: RESIDENT_POOL_BYTES,
        peak_managed_bytes: u64::try_from(scan_managed_bytes)
            .unwrap_or(u64::MAX)
            .max(u64::try_from(incremental_apply_managed_bytes).unwrap_or(u64::MAX))
            .max(u64::try_from(trie::EXISTING_OBJECT_CACHE_BYTES).unwrap_or(u64::MAX)),
        scan_window_bytes: SEMANTIC_SCAN_WINDOW_BYTES,
        spool_run_bytes: SEMANTIC_SPOOL_RUN_BYTES,
        merge_fan_in: SEMANTIC_MERGE_FAN_IN,
        peak_open_data_fds: u16::try_from(
            scan_peak_fds
                .max(spool.peak_open_files)
                .max(SEMANTIC_SPOOL_PEAK_DATA_FDS),
        )
        .unwrap_or(u16::MAX),
        peak_data_workers: MAX_DATA_WORKERS.min(peak_data_workers),
        trie_fan_out: SEMANTIC_TRIE_FAN_OUT,
    }
}

fn phase(name: &str, elapsed_ns: u64) -> SemanticPhaseSpan {
    SemanticPhaseSpan {
        phase: name.to_owned(),
        elapsed_ns,
    }
}

fn elapsed_ns(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

fn sha256_file(path: &Path) -> PocResult<String> {
    let file =
        File::open(path).map_err(|error| PocError::io("open file for SHA-256", path, error))?;
    let mut reader = BufReader::with_capacity(32 * 1024, file);
    let mut buffer = [0_u8; 32 * 1024];
    let mut digest = Sha256::new();
    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|error| PocError::io("read file for SHA-256", path, error))?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(hex_digest(digest.finalize().into()))
}

fn sync_directory(path: &Path) -> PocResult<()> {
    let directory =
        File::open(path).map_err(|error| PocError::io("open directory for fsync", path, error))?;
    directory
        .sync_all()
        .map_err(|error| PocError::io("fsync directory", path, error))
}

fn hex_digest(bytes: [u8; 32]) -> String {
    use std::fmt::Write as _;
    let mut output = String::with_capacity(64);
    for byte in bytes {
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

#[cfg(unix)]
fn path_from_semantic_bytes(bytes: Vec<u8>) -> PocResult<PathBuf> {
    Ok(PathBuf::from(OsString::from_vec(bytes)))
}

#[cfg(not(unix))]
fn path_from_semantic_bytes(bytes: Vec<u8>) -> PocResult<PathBuf> {
    String::from_utf8(bytes).map(PathBuf::from).map_err(|_| {
        PocError::Integrity("semantic path is not valid UTF-8 on this host".to_owned())
    })
}

struct DeltaStreamReader {
    path: PathBuf,
    reader: BufReader<File>,
    bytes_read: u64,
}

impl DeltaStreamReader {
    fn open(path: &Path) -> PocResult<Self> {
        let file = File::open(path)
            .map_err(|error| PocError::io("open affected semantic stream", path, error))?;
        let mut reader = BufReader::new(file);
        let mut magic = [0_u8; 8];
        reader
            .read_exact(&mut magic)
            .map_err(|error| PocError::io("read affected semantic stream", path, error))?;
        if &magic != DELTA_MAGIC {
            return Err(PocError::Integrity(
                "affected semantic stream has wrong magic".to_owned(),
            ));
        }
        Ok(Self {
            path: path.to_path_buf(),
            reader,
            bytes_read: 8,
        })
    }

    fn next_mutation(&mut self) -> PocResult<Option<RecordMutation>> {
        let mut length = [0_u8; 4];
        if !read_exact_or_eof(&mut self.reader, &mut length)
            .map_err(|error| PocError::io("read affected semantic frame", &self.path, error))?
        {
            return Ok(None);
        }
        let length = usize::try_from(u32::from_be_bytes(length))
            .map_err(|_| PocError::Integrity("affected frame length overflow".to_owned()))?;
        if length > record::MAX_RECORD_BYTES + record::MAX_KEY_BYTES + 16 {
            return Err(PocError::Integrity(
                "affected semantic frame exceeds bound".to_owned(),
            ));
        }
        let mut bytes = vec![0_u8; length];
        self.reader
            .read_exact(&mut bytes)
            .map_err(|error| PocError::io("read affected semantic frame", &self.path, error))?;
        self.bytes_read = self
            .bytes_read
            .saturating_add(u64::try_from(length + 4).unwrap_or(u64::MAX));
        RecordMutation::decode(&bytes).map(Some)
    }

    const fn bytes_read(&self) -> u64 {
        self.bytes_read
    }
}

fn read_exact_or_eof(reader: &mut impl Read, bytes: &mut [u8]) -> std::io::Result<bool> {
    let mut filled = 0;
    while filled < bytes.len() {
        let count = reader.read(&mut bytes[filled..])?;
        if count == 0 {
            if filled == 0 {
                return Ok(false);
            }
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "partial affected semantic frame",
            ));
        }
        filled += count;
    }
    Ok(true)
}
