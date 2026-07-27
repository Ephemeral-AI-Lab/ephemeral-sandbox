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
    SEMANTIC_SCAN_WINDOW_BYTES, SEMANTIC_SPOOL_RUN_BYTES, SEMANTIC_TRIE_FAN_OUT,
};
use crate::m1_contract::SEMANTIC_FORMAT_VERSION;
use crate::{
    AttributionInput, CanonicalDurabilityReceipt, CanonicalRootPair, OperationId, PocError,
    PocResult, SemanticBuildReceipt, SemanticBuildRequest, SemanticPhaseSpan, SCHEMA_VERSION,
};

use self::record::RecordMutation;
use self::scan::ScanStats;
use self::spool::{BoundedSpool, SpoolStats};
use self::trie::{ImmutableObjectStore, TrieRoots};

const MAIN_SPOOL_MEMORY_BYTES: usize = 3 * 1024 * 1024;
const HARDLINK_SPOOL_MEMORY_BYTES: usize = 1024 * 1024;
const DELTA_MAGIC: &[u8; 8] = b"MPLADLT1";
const MANIFEST_VERSION: u32 = 1;
const MAX_AFFECTED_STREAM_BYTES: u64 = 16 * 1024 * 1024;
const MAX_AFFECTED_RECORDS: u64 = 4_096;

#[derive(Clone, Debug, Eq, PartialEq)]
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

pub fn build(request: &SemanticBuildRequest) -> PocResult<SemanticBuildReceipt> {
    build_with_output(request).map(|output| output.receipt)
}

pub fn build_with_output(request: &SemanticBuildRequest) -> PocResult<SemanticBuildOutput> {
    validate_full_request(request)?;
    prepare_empty_directory(&request.spool_dir)?;
    std::fs::create_dir_all(&request.canonical_object_dir).map_err(|error| {
        PocError::io(
            "create canonical object directory",
            &request.canonical_object_dir,
            error,
        )
    })?;
    validate_full_path_isolation(request)?;

    let started = Instant::now();
    let scan_started = Instant::now();
    let record_spool_dir = request.spool_dir.join("records");
    let hardlink_spool_dir = request.spool_dir.join("hardlinks");
    let mut records = BoundedSpool::new(record_spool_dir, MAIN_SPOOL_MEMORY_BYTES)?;
    let mut hardlinks = BoundedSpool::new(hardlink_spool_dir, HARDLINK_SPOOL_MEMORY_BYTES)?;
    let scan = scan::scan_tree(&request.sealed_tree, &mut records, &mut hardlinks)?;
    scan::append_hardlink_records(&mut records, hardlinks.finish()?, &request.spool_dir)?;
    let scan_elapsed = elapsed_ns(scan_started);

    let sort_started = Instant::now();
    let sorted = records.finish()?;
    let sort_elapsed = elapsed_ns(sort_started);
    let spool_stats = sorted.stats();

    let hash_started = Instant::now();
    let mut store = ImmutableObjectStore::new(&request.canonical_object_dir)?;
    let roots = trie::build_from_sorted_records(&sorted, &request.attribution, &mut store)?;
    let hash_elapsed = elapsed_ns(hash_started);

    let install_started = Instant::now();
    let manifest = manifest_for(&roots, spool_stats.records_out, &request.attribution);
    store.sync_directory()?;
    let root_manifest_path = install_manifest(&request.canonical_object_dir, &manifest)?;
    let record_stream_path =
        materialize_record_stream(&root_manifest_path, &request.canonical_object_dir)?;
    let install_elapsed = elapsed_ns(install_started);
    let durability = durability_receipt(&root_manifest_path, &store)?;
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
    let resource_maxima = resource_maxima(&spool_stats, scan.peak_open_data_fds);
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
    std::fs::create_dir_all(&request.canonical_object_dir).map_err(|error| {
        PocError::io(
            "create incremental canonical object directory",
            &request.canonical_object_dir,
            error,
        )
    })?;

    let started = Instant::now();
    let mut reader = DeltaStreamReader::open(&request.affected_stream)?;
    let mut store = ImmutableObjectStore::new(&request.canonical_object_dir)?;
    let mut roots = TrieRoots::from_hex(&prior.content_root, &prior.attribution_root)?;
    trie::validate_roots(&roots, &mut store)?;
    let mut entry_count = prior.entry_count;
    let mut affected_record_count = 0_u64;
    let mut previous_key = None;
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
        let outcome = trie::apply_mutation(&roots, &mutation, &request.attribution, &mut store)?;
        roots = outcome.roots;
        match (outcome.existed, &mutation) {
            (false, RecordMutation::Replace(_)) => entry_count = entry_count.saturating_add(1),
            (true, RecordMutation::Delete { .. }) => entry_count = entry_count.saturating_sub(1),
            (false, RecordMutation::Delete { .. }) => {
                return Err(PocError::Integrity(
                    "incremental delete names a missing canonical key".to_owned(),
                ));
            }
            _ => {}
        }
        affected_record_count = affected_record_count.saturating_add(1);
    }
    let update_elapsed = elapsed_ns(started);
    let manifest = manifest_for(&roots, entry_count, &request.attribution);
    let install_started = Instant::now();
    store.sync_directory()?;
    let root_manifest_path = install_manifest(&request.canonical_object_dir, &manifest)?;
    let install_elapsed = elapsed_ns(install_started);
    let durability = durability_receipt(&root_manifest_path, &store)?;
    let spool_stats = SpoolStats::default();
    let receipt = SemanticBuildReceipt {
        schema_version: SCHEMA_VERSION,
        semantic_format: SEMANTIC_FORMAT_VERSION.to_owned(),
        operation_id: request.operation_id.clone(),
        roots: roots.to_root_pair()?,
        record_stream_sha256: manifest.record_stream_sha256,
        entry_count,
        bytes_read: reader.bytes_read(),
        spool_runs: 0,
        spool_bytes: 0,
        peak_open_data_fds: 2,
        peak_data_workers: 1,
        phase_spans: vec![
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
        affected_input_bytes: reader.bytes_read(),
        prior_node_bytes_read: store.bytes_read(),
        immutable_payload_bytes_read: 0,
        resource_maxima: resource_maxima(&spool_stats, 2),
    })
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
    let roots = TrieRoots::from_hex(&manifest.content_root, &manifest.attribution_root)?;
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
    let mut store = ImmutableObjectStore::new(canonical_object_dir)?;
    trie::visit_records(&roots, &mut store, |record| {
        let frame = record.encode_frame()?;
        writer
            .write_all(&frame)
            .map_err(|error| PocError::io("write semantic record stream", &temporary, error))
    })?;
    writer
        .flush()
        .map_err(|error| PocError::io("flush semantic record stream", &temporary, error))?;
    writer
        .get_ref()
        .sync_all()
        .map_err(|error| PocError::io("fsync semantic record stream", &temporary, error))?;
    match std::fs::hard_link(&temporary, &path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(PocError::io("install semantic record stream", &path, error));
        }
    }
    std::fs::remove_file(&temporary).map_err(|error| {
        PocError::io(
            "remove installed semantic record stream temporary",
            &temporary,
            error,
        )
    })?;
    sync_directory(&streams)?;
    Ok(path)
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

fn install_manifest(object_dir: &Path, manifest: &RootManifest) -> PocResult<PathBuf> {
    let manifests = object_dir.join("manifests");
    std::fs::create_dir_all(&manifests)
        .map_err(|error| PocError::io("create root manifest directory", &manifests, error))?;
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

fn build_receipt(
    operation_id: OperationId,
    roots: &TrieRoots,
    manifest: &RootManifest,
    scan: &ScanStats,
    spool: &SpoolStats,
    durability: CanonicalDurabilityReceipt,
    phase_spans: Vec<SemanticPhaseSpan>,
) -> PocResult<SemanticBuildReceipt> {
    let peak_open_data_fds = scan.peak_open_data_fds.max(spool.peak_open_files);
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
        peak_data_workers: 1,
        phase_spans,
        durability,
    })
}

fn durability_receipt(
    manifest_path: &Path,
    store: &ImmutableObjectStore,
) -> PocResult<CanonicalDurabilityReceipt> {
    Ok(CanonicalDurabilityReceipt {
        root_manifest: manifest_path.to_path_buf(),
        immutable_object_count: store.objects_written(),
        immutable_object_bytes: store.bytes_written(),
        object_set_sha256: store.object_set_sha256(),
        files_fsynced: true,
        object_directory_fsynced: true,
        manifest_fsynced: true,
        manifest_directory_fsynced: true,
    })
}

fn resource_maxima(spool: &SpoolStats, scan_peak_fds: usize) -> SemanticResourceMaxima {
    SemanticResourceMaxima {
        application_pool_bytes: RESIDENT_POOL_BYTES,
        peak_managed_bytes: u64::try_from(
            MAIN_SPOOL_MEMORY_BYTES
                + HARDLINK_SPOOL_MEMORY_BYTES
                + SEMANTIC_SCAN_WINDOW_BYTES
                + scan::MAX_XATTR_TRANSIENT_BYTES,
        )
        .unwrap_or(u64::MAX),
        scan_window_bytes: SEMANTIC_SCAN_WINDOW_BYTES,
        spool_run_bytes: SEMANTIC_SPOOL_RUN_BYTES,
        merge_fan_in: SEMANTIC_MERGE_FAN_IN,
        peak_open_data_fds: u16::try_from(scan_peak_fds.max(spool.peak_open_files))
            .unwrap_or(u16::MAX),
        peak_data_workers: MAX_DATA_WORKERS.min(1),
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
