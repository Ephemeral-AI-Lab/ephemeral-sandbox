#![cfg_attr(test, allow(dead_code))]

use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use sandbox_runtime_layerstack_core::{
    digest_preimage_header_len, encode_digest_preimage_header, encode_tree_record,
    tree_entry_record_len, CanonicalSink, CapabilitySet, ChunkProfileId, Digest32, DigestDomain,
    Error, ErrorKind, FieldClass, FormatVersion, ObjectId, ObjectKind, RootId, RootRecordV2,
    TreeEntry, TreeManifestId, TypedDigest, ValidatedTree, MAX_PATH_BYTES, MAX_RECORD_BYTES,
    MAX_TINY_ENTRIES, ROOT_FORMAT_V2,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Default)]
pub struct Sha256Digest;

struct HashSink<'a> {
    hasher: &'a mut Sha256,
    remaining: u64,
    version: FormatVersion,
}

impl CanonicalSink for HashSink<'_> {
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let length = u64::try_from(bytes.len()).map_err(|_| {
            Error::new(
                ErrorKind::LimitExceeded,
                self.version,
                FieldClass::Digest,
                0,
            )
        })?;
        if length > self.remaining {
            return Err(Error::new(
                ErrorKind::DigestFailure,
                self.version,
                FieldClass::Digest,
                u32::try_from(length).unwrap_or(u32::MAX),
            ));
        }
        self.hasher.update(bytes);
        self.remaining -= length;
        Ok(())
    }
}

impl TypedDigest for Sha256Digest {
    fn digest(
        &mut self,
        domain: DigestDomain,
        version: FormatVersion,
        payload_len: u64,
        encode_payload: &mut dyn FnMut(&mut dyn CanonicalSink) -> Result<(), Error>,
    ) -> Result<Digest32, Error> {
        let mut hasher = Sha256::new();
        {
            let mut prefix_sink = HashSink {
                hasher: &mut hasher,
                remaining: digest_preimage_header_len(domain),
                version,
            };
            encode_digest_preimage_header(domain, version, payload_len, &mut prefix_sink)?;
            if prefix_sink.remaining != 0 {
                return Err(Error::new(
                    ErrorKind::DigestFailure,
                    version,
                    FieldClass::Digest,
                    u32::try_from(prefix_sink.remaining).unwrap_or(u32::MAX),
                ));
            }
        }
        let mut payload_sink = HashSink {
            hasher: &mut hasher,
            remaining: payload_len,
            version,
        };
        encode_payload(&mut payload_sink)?;
        if payload_sink.remaining != 0 {
            return Err(Error::new(
                ErrorKind::DigestFailure,
                version,
                FieldClass::Digest,
                u32::try_from(payload_sink.remaining).unwrap_or(u32::MAX),
            ));
        }
        let bytes: [u8; 32] = hasher.finalize().into();
        Ok(Digest32::new(bytes))
    }
}

pub fn chunk_payload_id(first: &[u8], second: &[u8]) -> Result<ObjectId, Error> {
    let payload_len = u64::try_from(first.len())
        .ok()
        .and_then(|left| {
            u64::try_from(second.len())
                .ok()
                .and_then(|right| left.checked_add(right))
        })
        .ok_or_else(|| Error::new(ErrorKind::Overflow, ROOT_FORMAT_V2, FieldClass::Length, 0))?;
    let maximum_payload = u64::from(MAX_RECORD_BYTES)
        .checked_sub(digest_preimage_header_len(DigestDomain::Object(
            ObjectKind::ChunkPayload,
        )))
        .ok_or_else(|| Error::new(ErrorKind::Overflow, ROOT_FORMAT_V2, FieldClass::Length, 0))?;
    if payload_len > maximum_payload {
        return Err(Error::new(
            ErrorKind::LimitExceeded,
            ROOT_FORMAT_V2,
            FieldClass::Length,
            u32::MAX,
        ));
    }
    let mut digest = Sha256Digest;
    let mut invocations = 0_u8;
    let value = {
        let mut encode_payload = |sink: &mut dyn CanonicalSink| {
            invocations = invocations.checked_add(1).ok_or_else(|| {
                Error::new(ErrorKind::Overflow, ROOT_FORMAT_V2, FieldClass::Digest, 0)
            })?;
            sink.write_all(first)?;
            sink.write_all(second)
        };
        digest.digest(
            DigestDomain::Object(ObjectKind::ChunkPayload),
            ROOT_FORMAT_V2,
            payload_len,
            &mut encode_payload,
        )?
    };
    if invocations != 1 {
        return Err(Error::new(
            ErrorKind::DigestFailure,
            ROOT_FORMAT_V2,
            FieldClass::Digest,
            u32::from(invocations),
        ));
    }
    Ok(ObjectId::new(ObjectKind::ChunkPayload, value))
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationJson {
    pub generation: u64,
    pub id: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RootRecordJson {
    pub schema: String,
    pub version: u16,
    pub root_id: String,
    pub required_capabilities: u64,
    pub chunk_profile: u16,
    pub tree_manifest: String,
    pub parent: Option<String>,
    pub base: Option<String>,
    pub publication: PublicationJson,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TreeManifestJson {
    pub schema: String,
    pub version: u16,
    pub tree_manifest_id: String,
    pub entry_count: u64,
    pub required_capabilities: u64,
}

#[derive(Debug, Error)]
pub enum DiagnosticError {
    #[error("diagnostic JSON serialization failed")]
    Serialize(#[source] serde_json::Error),
    #[error("diagnostic JSON is not canonical")]
    NonCanonical,
    #[error("diagnostic identity is invalid")]
    InvalidIdentity,
}

fn digest_string(digest: Digest32) -> String {
    let mut output = String::with_capacity(71);
    output.push_str("sha256:");
    for byte in digest.as_bytes() {
        let _ = write!(&mut output, "{byte:02x}");
    }
    output
}

fn root_string(value: RootId) -> String {
    digest_string(value.digest())
}

fn tree_string(value: TreeManifestId) -> String {
    digest_string(value.digest())
}

fn publication_string(bytes: &[u8; 16]) -> String {
    let mut output = String::with_capacity(32);
    for byte in bytes {
        let _ = write!(&mut output, "{byte:02x}");
    }
    output
}

fn validate_hex(value: &str, prefix: &str, byte_count: usize) -> bool {
    let Some(expected_hex_len) = byte_count.checked_mul(2) else {
        return false;
    };
    value.strip_prefix(prefix).is_some_and(|hex| {
        hex.len() == expected_hex_len
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

pub fn canonical_root_json(record: &RootRecordV2, id: RootId) -> Result<String, DiagnosticError> {
    serde_json::to_string(&RootRecordJson {
        schema: "root-record-v2".to_owned(),
        version: record.format().get(),
        root_id: root_string(id),
        required_capabilities: record.required_capabilities().bits(),
        chunk_profile: record.chunk_profile().get(),
        tree_manifest: tree_string(record.tree_manifest()),
        parent: record.parent().map(root_string),
        base: record.base().map(root_string),
        publication: PublicationJson {
            generation: record.publication().generation(),
            id: publication_string(record.publication().id().as_bytes()),
        },
    })
    .map_err(DiagnosticError::Serialize)
}

pub fn canonical_tree_json(tree: ValidatedTree) -> Result<String, DiagnosticError> {
    serde_json::to_string(&TreeManifestJson {
        schema: "tree-manifest-v2".to_owned(),
        version: ROOT_FORMAT_V2.get(),
        tree_manifest_id: tree_string(tree.id()),
        entry_count: tree.entry_count(),
        required_capabilities: tree.required_capabilities().bits(),
    })
    .map_err(DiagnosticError::Serialize)
}

pub fn parse_canonical_root_json(input: &str) -> Result<RootRecordJson, DiagnosticError> {
    let value: RootRecordJson = serde_json::from_str(input).map_err(DiagnosticError::Serialize)?;
    let encoded = serde_json::to_string(&value).map_err(DiagnosticError::Serialize)?;
    if encoded != input {
        return Err(DiagnosticError::NonCanonical);
    }
    if value.schema != "root-record-v2"
        || value.version != ROOT_FORMAT_V2.get()
        || value.chunk_profile != ChunkProfileId::SEQ_CDC_V1.get()
        || CapabilitySet::from_bits(value.required_capabilities).is_err()
        || !validate_hex(&value.root_id, "sha256:", 32)
        || !validate_hex(&value.tree_manifest, "sha256:", 32)
        || value
            .parent
            .as_deref()
            .is_some_and(|id| !validate_hex(id, "sha256:", 32))
        || value
            .base
            .as_deref()
            .is_some_and(|id| !validate_hex(id, "sha256:", 32))
        || !validate_hex(&value.publication.id, "", 16)
        || value.publication.id.bytes().all(|byte| byte == b'0')
    {
        return Err(DiagnosticError::InvalidIdentity);
    }
    Ok(value)
}

pub fn parse_canonical_tree_json(input: &str) -> Result<TreeManifestJson, DiagnosticError> {
    let value: TreeManifestJson =
        serde_json::from_str(input).map_err(DiagnosticError::Serialize)?;
    let encoded = serde_json::to_string(&value).map_err(DiagnosticError::Serialize)?;
    if encoded != input {
        return Err(DiagnosticError::NonCanonical);
    }
    if value.schema != "tree-manifest-v2"
        || value.version != ROOT_FORMAT_V2.get()
        || CapabilitySet::from_bits(value.required_capabilities).is_err()
        || !validate_hex(&value.tree_manifest_id, "sha256:", 32)
    {
        return Err(DiagnosticError::InvalidIdentity);
    }
    Ok(value)
}

const PORTABLE_PREPARATION_MERGE_FAN_IN: usize = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PortableBackendMarker {
    LinuxWhiteout,
    OpaqueDirectory,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PortablePreparationInput {
    Entry(Box<TreeEntry>),
    BackendMarker(PortableBackendMarker),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PortablePreparationStats {
    pub input_items: u64,
    pub linux_whiteouts_filtered: u64,
    pub opaque_directories_filtered: u64,
    pub initial_runs: u64,
    pub merge_passes: u64,
    pub max_merge_fan_in: u64,
    pub coalesced_duplicates: u64,
    pub output_entries: u64,
}

#[derive(Debug, Error)]
pub enum PortablePreparationError {
    #[error("portable preparation accepts at most 256 input items")]
    InputLimit,
    #[error("portable preparation spool path already exists")]
    SpoolPathExists,
    #[error("portable preparation spool is malformed")]
    MalformedSpool,
    #[error("portable preparation found conflicting entries for one path")]
    ConflictingDuplicate,
    #[error("portable preparation I/O failed")]
    Io(#[from] io::Error),
    #[error("portable root contract rejected prepared input")]
    Contract(#[from] Error),
}

#[derive(Debug)]
pub struct PreparedPortableTree<'a> {
    entries: Vec<&'a TreeEntry>,
    entries_bytes: u64,
    stats: PortablePreparationStats,
}

impl PreparedPortableTree<'_> {
    #[must_use]
    pub const fn stats(&self) -> PortablePreparationStats {
        self.stats
    }

    #[must_use]
    pub const fn entry_count(&self) -> u64 {
        self.stats.output_entries
    }

    #[must_use]
    pub const fn entries_bytes(&self) -> u64 {
        self.entries_bytes
    }

    pub fn encode(
        &self,
        sink: &mut dyn CanonicalSink,
    ) -> Result<CapabilitySet, PortablePreparationError> {
        let mut entries = self.entries.iter().copied();
        encode_tree_record(
            self.stats.output_entries,
            self.entries_bytes,
            &mut entries,
            sink,
        )
        .map_err(PortablePreparationError::Contract)
    }
}

struct OwnedPortableSpool {
    root: PathBuf,
    files: Vec<PathBuf>,
    next_run: u64,
    armed: bool,
}

impl OwnedPortableSpool {
    fn create(root: &Path) -> Result<Self, PortablePreparationError> {
        match fs::create_dir(root) {
            Ok(()) => Ok(Self {
                root: root.to_path_buf(),
                files: Vec::new(),
                next_run: 0,
                armed: true,
            }),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                Err(PortablePreparationError::SpoolPathExists)
            }
            Err(error) => Err(PortablePreparationError::Io(error)),
        }
    }

    fn create_run(&mut self) -> Result<(PathBuf, BufWriter<File>), PortablePreparationError> {
        let path = self.root.join(format!("run-{:08}.spool", self.next_run));
        self.next_run = self
            .next_run
            .checked_add(1)
            .ok_or(PortablePreparationError::InputLimit)?;
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        self.files.push(path.clone());
        Ok((path, BufWriter::new(file)))
    }

    fn cleanup(&mut self) -> Result<(), PortablePreparationError> {
        let mut first_error = None;
        for path in self.files.iter().rev() {
            if let Err(error) = fs::remove_file(path) {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
        if let Err(error) = fs::remove_dir(&self.root) {
            if first_error.is_none() {
                first_error = Some(error);
            }
        }
        if let Some(error) = first_error {
            return Err(PortablePreparationError::Io(error));
        }
        self.armed = false;
        Ok(())
    }
}

impl Drop for OwnedPortableSpool {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        for path in self.files.iter().rev() {
            let _ = fs::remove_file(path);
        }
        let _ = fs::remove_dir(&self.root);
    }
}

#[derive(Clone, Debug)]
struct PortableRunHead {
    path: Vec<u8>,
    ordinal: u32,
}

struct PortableRunCursor<'a> {
    reader: BufReader<File>,
    head: Option<PortableRunHead>,
    inputs: &'a [PortablePreparationInput],
}

impl<'a> PortableRunCursor<'a> {
    fn open(
        path: &Path,
        inputs: &'a [PortablePreparationInput],
    ) -> Result<Self, PortablePreparationError> {
        let mut cursor = Self {
            reader: BufReader::new(File::open(path)?),
            head: None,
            inputs,
        };
        cursor.advance()?;
        Ok(cursor)
    }

    fn advance(&mut self) -> Result<(), PortablePreparationError> {
        let mut path_len_bytes = [0_u8; 4];
        let bytes_read = self.reader.read(&mut path_len_bytes[..1])?;
        if bytes_read == 0 {
            self.head = None;
            return Ok(());
        }
        self.reader.read_exact(&mut path_len_bytes[1..])?;
        let path_len = usize::try_from(u32::from_be_bytes(path_len_bytes))
            .map_err(|_| PortablePreparationError::MalformedSpool)?;
        if path_len == 0 || path_len > MAX_PATH_BYTES {
            return Err(PortablePreparationError::MalformedSpool);
        }
        let mut path = Vec::new();
        path.try_reserve_exact(path_len)
            .map_err(|_| PortablePreparationError::InputLimit)?;
        path.resize(path_len, 0);
        self.reader.read_exact(&mut path)?;
        let mut ordinal_bytes = [0_u8; 4];
        self.reader.read_exact(&mut ordinal_bytes)?;
        let ordinal = u32::from_be_bytes(ordinal_bytes);
        let entry = entry_for_ordinal(self.inputs, ordinal)?;
        if entry.path().as_bytes() != path {
            return Err(PortablePreparationError::MalformedSpool);
        }
        self.head = Some(PortableRunHead { path, ordinal });
        Ok(())
    }
}

fn entry_for_ordinal(
    inputs: &[PortablePreparationInput],
    ordinal: u32,
) -> Result<&TreeEntry, PortablePreparationError> {
    let ordinal = usize::try_from(ordinal).map_err(|_| PortablePreparationError::MalformedSpool)?;
    match inputs.get(ordinal) {
        Some(PortablePreparationInput::Entry(entry)) => Ok(entry),
        Some(PortablePreparationInput::BackendMarker(_)) | None => {
            Err(PortablePreparationError::MalformedSpool)
        }
    }
}

fn write_portable_run_record(
    writer: &mut dyn Write,
    path: &[u8],
    ordinal: u32,
) -> Result<(), PortablePreparationError> {
    let path_len = u32::try_from(path.len()).map_err(|_| PortablePreparationError::InputLimit)?;
    writer.write_all(&path_len.to_be_bytes())?;
    writer.write_all(path)?;
    writer.write_all(&ordinal.to_be_bytes())?;
    Ok(())
}

fn merge_portable_runs(
    inputs: &[PortablePreparationInput],
    input_runs: &[PathBuf],
    output: &mut BufWriter<File>,
    stats: &mut PortablePreparationStats,
) -> Result<(), PortablePreparationError> {
    let mut cursors = input_runs
        .iter()
        .map(|path| PortableRunCursor::open(path, inputs))
        .collect::<Result<Vec<_>, _>>()?;
    loop {
        let minimum_path = cursors
            .iter()
            .filter_map(|cursor| cursor.head.as_ref().map(|head| head.path.as_slice()))
            .min()
            .map(<[u8]>::to_vec);
        let Some(minimum_path) = minimum_path else {
            break;
        };
        let matching: Vec<usize> = cursors
            .iter()
            .enumerate()
            .filter_map(|(index, cursor)| {
                cursor
                    .head
                    .as_ref()
                    .filter(|head| head.path == minimum_path)
                    .map(|_| index)
            })
            .collect();
        let Some(first_index) = matching.first().copied() else {
            return Err(PortablePreparationError::MalformedSpool);
        };
        let first_ordinal = cursors[first_index]
            .head
            .as_ref()
            .map(|head| head.ordinal)
            .ok_or(PortablePreparationError::MalformedSpool)?;
        let first_entry = entry_for_ordinal(inputs, first_ordinal)?;
        for index in matching.iter().copied().skip(1) {
            let candidate_ordinal = cursors[index]
                .head
                .as_ref()
                .map(|head| head.ordinal)
                .ok_or(PortablePreparationError::MalformedSpool)?;
            if first_entry != entry_for_ordinal(inputs, candidate_ordinal)? {
                return Err(PortablePreparationError::ConflictingDuplicate);
            }
            stats.coalesced_duplicates = stats
                .coalesced_duplicates
                .checked_add(1)
                .ok_or(PortablePreparationError::InputLimit)?;
        }
        write_portable_run_record(output, &minimum_path, first_ordinal)?;
        for index in matching {
            cursors[index].advance()?;
        }
    }
    output.flush()?;
    Ok(())
}

fn read_prepared_ordinals(
    inputs: &[PortablePreparationInput],
    final_run: Option<&Path>,
) -> Result<(Vec<u32>, u64), PortablePreparationError> {
    let Some(final_run) = final_run else {
        return Ok((Vec::new(), 0));
    };
    let mut cursor = PortableRunCursor::open(final_run, inputs)?;
    let mut ordinals = Vec::new();
    let mut entries_bytes = 0_u64;
    let mut previous_path: Option<Vec<u8>> = None;
    while let Some(head) = cursor.head.clone() {
        if previous_path
            .as_deref()
            .is_some_and(|previous| previous >= head.path.as_slice())
        {
            return Err(PortablePreparationError::MalformedSpool);
        }
        let entry = entry_for_ordinal(inputs, head.ordinal)?;
        entries_bytes = entries_bytes
            .checked_add(u64::from(tree_entry_record_len(entry)?))
            .ok_or(PortablePreparationError::InputLimit)?;
        ordinals.push(head.ordinal);
        if u64::try_from(ordinals.len())
            .ok()
            .is_none_or(|length| length > MAX_TINY_ENTRIES)
        {
            return Err(PortablePreparationError::InputLimit);
        }
        previous_path = Some(head.path);
        cursor.advance()?;
    }
    Ok((ordinals, entries_bytes))
}

fn prepare_tiny_portable_tree_inner<'a>(
    inputs: &'a [PortablePreparationInput],
    spool: &mut OwnedPortableSpool,
) -> Result<PreparedPortableTree<'a>, PortablePreparationError> {
    if u64::try_from(inputs.len())
        .ok()
        .is_none_or(|length| length > MAX_TINY_ENTRIES)
    {
        return Err(PortablePreparationError::InputLimit);
    }
    let mut stats = PortablePreparationStats {
        input_items: u64::try_from(inputs.len())
            .map_err(|_| PortablePreparationError::InputLimit)?,
        ..PortablePreparationStats::default()
    };
    let mut runs = Vec::new();
    for (ordinal, input) in inputs.iter().enumerate() {
        match input {
            PortablePreparationInput::Entry(entry) => {
                let (path, mut writer) = spool.create_run()?;
                write_portable_run_record(
                    &mut writer,
                    entry.path().as_bytes(),
                    u32::try_from(ordinal).map_err(|_| PortablePreparationError::InputLimit)?,
                )?;
                writer.flush()?;
                drop(writer);
                runs.push(path);
                stats.initial_runs = stats
                    .initial_runs
                    .checked_add(1)
                    .ok_or(PortablePreparationError::InputLimit)?;
            }
            PortablePreparationInput::BackendMarker(PortableBackendMarker::LinuxWhiteout) => {
                stats.linux_whiteouts_filtered = stats
                    .linux_whiteouts_filtered
                    .checked_add(1)
                    .ok_or(PortablePreparationError::InputLimit)?;
            }
            PortablePreparationInput::BackendMarker(PortableBackendMarker::OpaqueDirectory) => {
                stats.opaque_directories_filtered = stats
                    .opaque_directories_filtered
                    .checked_add(1)
                    .ok_or(PortablePreparationError::InputLimit)?;
            }
        }
    }
    while runs.len() > 1 {
        stats.merge_passes = stats
            .merge_passes
            .checked_add(1)
            .ok_or(PortablePreparationError::InputLimit)?;
        let mut next_runs = Vec::new();
        for group in runs.chunks(PORTABLE_PREPARATION_MERGE_FAN_IN) {
            stats.max_merge_fan_in = stats
                .max_merge_fan_in
                .max(u64::try_from(group.len()).map_err(|_| PortablePreparationError::InputLimit)?);
            if group.len() == 1 {
                next_runs.push(group[0].clone());
                continue;
            }
            let (output_path, mut output) = spool.create_run()?;
            merge_portable_runs(inputs, group, &mut output, &mut stats)?;
            drop(output);
            next_runs.push(output_path);
        }
        runs = next_runs;
    }
    let (ordinals, entries_bytes) =
        read_prepared_ordinals(inputs, runs.first().map(PathBuf::as_path))?;
    stats.output_entries =
        u64::try_from(ordinals.len()).map_err(|_| PortablePreparationError::InputLimit)?;
    let entries = ordinals
        .into_iter()
        .map(|ordinal| entry_for_ordinal(inputs, ordinal))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(PreparedPortableTree {
        entries,
        entries_bytes,
        stats,
    })
}

pub fn prepare_tiny_portable_tree<'a>(
    inputs: &'a [PortablePreparationInput],
    nonexisting_spool_dir: &Path,
) -> Result<PreparedPortableTree<'a>, PortablePreparationError> {
    let mut spool = OwnedPortableSpool::create(nonexisting_spool_dir)?;
    let result = prepare_tiny_portable_tree_inner(inputs, &mut spool);
    let cleanup = spool.cleanup();
    match (result, cleanup) {
        (Ok(prepared), Ok(())) => Ok(prepared),
        (Err(error), Ok(())) => Err(error),
        (_, Err(cleanup_error)) => Err(cleanup_error),
    }
}
