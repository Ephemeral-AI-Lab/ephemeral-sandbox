use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::checks::CheckResult;
use crate::definitions::{definition, PhaseReference};
use crate::model::{OperationId, PhaseId, PhaseSource};
use crate::scheduler::{
    ObservationRecord, PhaseObservation, RequestObservation, ResourceObservation,
    SequencedObservation, TrialSample, OBSERVATION_SCHEMA_NAME as CURRENT_OBSERVATION_SCHEMA_NAME,
    OBSERVATION_SCHEMA_VERSION as CURRENT_OBSERVATION_SCHEMA_VERSION,
};

const MAX_ARTIFACT_DOWNLOAD_BYTES: u64 = 16 * 1024 * 1024;
const RECOVERY_QUARANTINE_DIRECTORY: &str = ".recovery-quarantine";
const RECOVERY_SCAN_BUFFER_BYTES: usize = 64 * 1024;
const OBSERVATION_SCHEMA_V1: u32 = 1;
const OBSERVATION_SCHEMA_V2: u32 = 2;
const MAX_BOUNDED_EVIDENCE_BYTES: usize = 1024 * 1024;
pub const BOUNDED_EVIDENCE_SCHEMA_NAME: &str = "eos_benchmark_operation_evidence";
pub const BOUNDED_EVIDENCE_SCHEMA_VERSION: u32 = 1;
const BOUNDED_EVIDENCE_MEDIA_TYPE: &str = "application/json";
const BOUNDED_EVIDENCE_PREFIX: &str = "operation-evidence-";
const DYNAMIC_BOUNDED_EVIDENCE_PREFIX: &str = "bounded_evidence_";
const CELLS_DIRECTORY: &str = "cells";
const TRIALS_DIRECTORY: &str = "trials";
const BOUNDED_EVIDENCE_DIRECTORY: &str = "bounded-evidence";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchemaEnvelope<T> {
    pub schema_name: String,
    pub schema_version: u32,
    pub data: T,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SequencedObservationV1 {
    sequence: u64,
    record: ObservationRecordV1,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SequencedObservationV2 {
    sequence: u64,
    record: ObservationRecordV2,
}

#[derive(Debug, Deserialize)]
#[serde(
    tag = "record",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum ObservationRecordV2 {
    Trial(TrialSampleV2),
    Request(RequestObservation),
    Resource(ResourceObservation),
    Phase(PhaseObservation),
    Check(CheckResult),
    Operation(crate::scheduler::OperationObservation),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TrialSampleV2 {
    operation_id: OperationId,
    cell_id: String,
    trial_id: String,
    kind: crate::scheduler::TrialKind,
    sequence_in_cell: u32,
    lifecycle: crate::scheduler::LifecycleDurations,
    product_succeeded: bool,
    infrastructure_failed: bool,
    cleanup_baseline_restored: bool,
    correctness: crate::checks::CorrectnessFold,
    primary_operation_latency_ns: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(
    tag = "record",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum ObservationRecordV1 {
    Trial(TrialSampleV2),
    Request(RequestObservation),
    Resource(ResourceObservation),
    Phase(PhaseObservationV1),
    Check(CheckResult),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PhaseObservationV1 {
    id: PhaseId,
    semantic_revision: u32,
    cell_id: String,
    trial_id: String,
    request_id: Option<String>,
    source: PhaseSource,
    start_offset_ns: u64,
    duration_ns: u64,
    status: crate::scheduler::PhaseStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactId {
    RunManifest,
    IntentPlan,
    ExpandedPlan,
    DefinitionSnapshot,
    EnvironmentMetadata,
    Events,
    Observations,
    Summary,
    Report,
    JsonExport,
    CsvExport,
    BoundedEvidence,
}

#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactIndexEntry {
    pub id: ArtifactId,
    pub artifact_id: String,
    pub file_name: String,
    pub media_type: &'static str,
    pub bytes: u64,
    pub sha256: String,
    pub downloadable: bool,
}

#[derive(Debug, Clone)]
pub struct ArtifactContent {
    pub id: ArtifactId,
    pub artifact_id: String,
    pub media_type: &'static str,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRef {
    pub artifact_id: String,
    pub media_type: String,
    pub size_bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PartialRecordWarning {
    pub line: usize,
}

#[derive(Debug, Clone)]
pub struct RecoveredRecords<T> {
    pub records: Vec<T>,
    pub partial_tail: Option<PartialRecordWarning>,
}

/// Metadata for an incomplete final NDJSON record moved out of the
/// authoritative append log during restart recovery. Quarantine files are
/// intentionally absent from the artifact download allowlist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuarantinedTail {
    pub artifact: ArtifactId,
    pub line: usize,
    pub bytes: u64,
    pub sha256: String,
    pub file_name: String,
}

#[derive(Debug, Clone)]
pub struct ArtifactStore {
    results_root: PathBuf,
}

#[derive(Debug, Error)]
pub enum ArtifactError {
    #[error("invalid run id {0}")]
    InvalidRunId(String),
    #[error("run artifacts already exist for {0}")]
    RunAlreadyExists(String),
    #[error("run artifacts do not exist for {0}")]
    RunNotFound(String),
    #[error("artifact id is not allowlisted: {0}")]
    UnknownArtifact(String),
    #[error("invalid artifact path component {0}")]
    InvalidArtifactComponent(String),
    #[error("artifact {id:?} exceeds the {limit} byte download cap")]
    ArtifactTooLarge { id: ArtifactId, limit: u64 },
    #[error("immutable artifact already exists with different content at {0}")]
    ImmutableArtifactConflict(PathBuf),
    #[error("artifact schema mismatch: expected {expected}, received {actual}")]
    SchemaMismatch { expected: String, actual: String },
    #[error("unsupported artifact schema version {version} for {schema}")]
    UnsupportedSchema { schema: String, version: u32 },
    #[error("partial trailing NDJSON record at line {line} in {path}")]
    PartialNdjsonTail { path: PathBuf, line: usize },
    #[error("artifact JSON failed at {path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("artifact filesystem operation failed for {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl ArtifactId {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RunManifest => "run_manifest",
            Self::IntentPlan => "intent_plan",
            Self::ExpandedPlan => "expanded_plan",
            Self::DefinitionSnapshot => "definition_snapshot",
            Self::EnvironmentMetadata => "environment_metadata",
            Self::Events => "events",
            Self::Observations => "observations",
            Self::Summary => "summary",
            Self::Report => "report",
            Self::JsonExport => "json_export",
            Self::CsvExport => "csv_export",
            Self::BoundedEvidence => "bounded_evidence",
        }
    }

    #[must_use]
    pub fn file_name(self) -> &'static str {
        match self {
            Self::RunManifest => "run-manifest.json",
            Self::IntentPlan => "intent-plan.json",
            Self::ExpandedPlan => "expanded-plan.json",
            Self::DefinitionSnapshot => "definition-snapshot.json",
            Self::EnvironmentMetadata => "environment-metadata.json",
            Self::Events => "events.ndjson",
            Self::Observations => "observations.ndjson",
            Self::Summary => "summary.json",
            Self::Report => "report.json",
            Self::JsonExport => "export.json",
            Self::CsvExport => "export.csv",
            Self::BoundedEvidence => "bounded-evidence",
        }
    }

    #[must_use]
    pub fn media_type(self) -> &'static str {
        match self {
            Self::Events | Self::Observations => "application/x-ndjson",
            Self::CsvExport => "text/csv; charset=utf-8",
            Self::RunManifest
            | Self::IntentPlan
            | Self::ExpandedPlan
            | Self::DefinitionSnapshot
            | Self::EnvironmentMetadata
            | Self::Summary
            | Self::Report
            | Self::JsonExport
            | Self::BoundedEvidence => "application/json",
        }
    }

    pub fn parse(value: &str) -> Result<Self, ArtifactError> {
        match value {
            "run_manifest" => Ok(Self::RunManifest),
            "intent_plan" => Ok(Self::IntentPlan),
            "expanded_plan" => Ok(Self::ExpandedPlan),
            "definition_snapshot" => Ok(Self::DefinitionSnapshot),
            "environment_metadata" => Ok(Self::EnvironmentMetadata),
            "events" => Ok(Self::Events),
            "observations" => Ok(Self::Observations),
            "summary" => Ok(Self::Summary),
            "report" => Ok(Self::Report),
            "json_export" => Ok(Self::JsonExport),
            "csv_export" => Ok(Self::CsvExport),
            "bounded_evidence" => Ok(Self::BoundedEvidence),
            other => Err(ArtifactError::UnknownArtifact(other.to_owned())),
        }
    }
}

impl ArtifactStore {
    pub fn new(results_root: &Path) -> Result<Self, ArtifactError> {
        fs::create_dir_all(results_root).map_err(|source| io_error(results_root, source))?;
        let results_root = results_root
            .canonicalize()
            .map_err(|source| io_error(results_root, source))?;
        Ok(Self { results_root })
    }

    pub fn create_run(&self, run_id: &str) -> Result<PathBuf, ArtifactError> {
        validate_run_id(run_id)?;
        let path = self.results_root.join(run_id);
        match fs::create_dir(&path) {
            Ok(()) => {
                sync_directory(&self.results_root)?;
                Ok(path)
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                Err(ArtifactError::RunAlreadyExists(run_id.to_owned()))
            }
            Err(source) => Err(io_error(&path, source)),
        }
    }

    /// Removes a run directory only during failed initial artifact creation.
    /// The directory must contain regular files from the closed artifact
    /// allowlist and no directories, symlinks, or unknown names.
    pub fn remove_incomplete_run(&self, run_id: &str) -> Result<(), ArtifactError> {
        let run = self.run_path(run_id)?;
        let allowed = [
            ArtifactId::RunManifest,
            ArtifactId::IntentPlan,
            ArtifactId::ExpandedPlan,
            ArtifactId::DefinitionSnapshot,
            ArtifactId::EnvironmentMetadata,
            ArtifactId::Events,
            ArtifactId::Observations,
            ArtifactId::Summary,
            ArtifactId::Report,
            ArtifactId::JsonExport,
            ArtifactId::CsvExport,
        ];
        for entry in fs::read_dir(&run).map_err(|source| io_error(&run, source))? {
            let entry = entry.map_err(|source| io_error(&run, source))?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).map_err(|source| io_error(&path, source))?;
            let known = entry
                .file_name()
                .to_str()
                .is_some_and(|name| allowed.iter().any(|artifact| artifact.file_name() == name));
            if !known || !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err(ArtifactError::UnknownArtifact(
                    entry.file_name().to_string_lossy().into_owned(),
                ));
            }
        }
        fs::remove_dir_all(&run).map_err(|source| io_error(&run, source))?;
        sync_directory(&self.results_root)
    }

    pub fn list_run_ids(&self) -> Result<Vec<String>, ArtifactError> {
        let mut run_ids = Vec::new();
        for entry in fs::read_dir(&self.results_root)
            .map_err(|source| io_error(&self.results_root, source))?
        {
            let entry = entry.map_err(|source| io_error(&self.results_root, source))?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).map_err(|source| io_error(&path, source))?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                continue;
            }
            let Some(run_id) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if validate_run_id(&run_id).is_ok() {
                run_ids.push(run_id);
            }
        }
        run_ids.sort_unstable_by(|left, right| right.cmp(left));
        Ok(run_ids)
    }

    pub fn run_path(&self, run_id: &str) -> Result<PathBuf, ArtifactError> {
        validate_run_id(run_id)?;
        let path = self.results_root.join(run_id);
        let metadata = fs::symlink_metadata(&path).map_err(|source| {
            if source.kind() == io::ErrorKind::NotFound {
                ArtifactError::RunNotFound(run_id.to_owned())
            } else {
                io_error(&path, source)
            }
        })?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(ArtifactError::RunNotFound(run_id.to_owned()));
        }
        Ok(path)
    }

    pub fn write_immutable<T: Serialize>(
        &self,
        run_id: &str,
        id: ArtifactId,
        schema_name: &str,
        schema_version: u32,
        value: &T,
    ) -> Result<(), ArtifactError> {
        let run = self.run_path(run_id)?;
        let path = run.join(id.file_name());
        let envelope = SchemaEnvelope {
            schema_name: schema_name.to_owned(),
            schema_version,
            data: value,
        };
        write_json_create_new(&path, &envelope)
    }

    pub fn replace_snapshot<T: Serialize>(
        &self,
        run_id: &str,
        id: ArtifactId,
        schema_name: &str,
        schema_version: u32,
        value: &T,
    ) -> Result<(), ArtifactError> {
        let run = self.run_path(run_id)?;
        let path = run.join(id.file_name());
        let envelope = SchemaEnvelope {
            schema_name: schema_name.to_owned(),
            schema_version,
            data: value,
        };
        write_json_replace(&path, &envelope)
    }

    /// Atomically replaces a regenerable, non-JSON derived export. The closed
    /// artifact id prevents callers from turning this into an arbitrary path
    /// writer; authoritative evidence is never accepted here.
    pub fn replace_derived_export(
        &self,
        run_id: &str,
        id: ArtifactId,
        bytes: &[u8],
    ) -> Result<(), ArtifactError> {
        if !matches!(id, ArtifactId::CsvExport) {
            return Err(ArtifactError::UnknownArtifact(id.as_str().to_owned()));
        }
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_ARTIFACT_DOWNLOAD_BYTES {
            return Err(ArtifactError::ArtifactTooLarge {
                id,
                limit: MAX_ARTIFACT_DOWNLOAD_BYTES,
            });
        }
        let run = self.run_path(run_id)?;
        write_bytes_replace(&run.join(id.file_name()), bytes)
    }

    pub fn read_envelope<T: DeserializeOwned>(
        &self,
        run_id: &str,
        id: ArtifactId,
        expected_schema: &str,
        supported_version: u32,
    ) -> Result<T, ArtifactError> {
        let run = self.run_path(run_id)?;
        let path = run.join(id.file_name());
        let bytes = fs::read(&path).map_err(|source| io_error(&path, source))?;
        decode_envelope(&bytes, &path, expected_schema, supported_version)
    }

    pub fn append_record<T: Serialize>(
        &self,
        run_id: &str,
        id: ArtifactId,
        schema_name: &str,
        schema_version: u32,
        value: &T,
    ) -> Result<(), ArtifactError> {
        if !matches!(id, ArtifactId::Events | ArtifactId::Observations) {
            return Err(ArtifactError::UnknownArtifact(id.as_str().to_owned()));
        }
        let run = self.run_path(run_id)?;
        let path = run.join(id.file_name());
        let envelope = SchemaEnvelope {
            schema_name: schema_name.to_owned(),
            schema_version,
            data: value,
        };
        let bytes = serde_json::to_vec(&envelope).map_err(|source| ArtifactError::Json {
            path: path.clone(),
            source,
        })?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|source| io_error(&path, source))?;
        file.write_all(&bytes)
            .and_then(|()| file.write_all(b"\n"))
            .and_then(|()| file.sync_data())
            .map_err(|source| io_error(&path, source))
    }

    pub fn read_records<T: DeserializeOwned>(
        &self,
        run_id: &str,
        id: ArtifactId,
        expected_schema: &str,
        supported_version: u32,
    ) -> Result<Vec<T>, ArtifactError> {
        let run = self.run_path(run_id)?;
        let path = run.join(id.file_name());
        if !path.exists() {
            return Ok(Vec::new());
        }
        reject_partial_tail(&path)?;
        let reader = BufReader::new(File::open(&path).map_err(|source| io_error(&path, source))?);
        let mut records = Vec::new();
        for line in reader.lines() {
            let line = line.map_err(|source| io_error(&path, source))?;
            if line.is_empty() {
                continue;
            }
            records.push(decode_envelope(
                line.as_bytes(),
                &path,
                expected_schema,
                supported_version,
            )?);
        }
        Ok(records)
    }

    pub fn read_records_recovering<T: DeserializeOwned>(
        &self,
        run_id: &str,
        id: ArtifactId,
        expected_schema: &str,
        supported_version: u32,
    ) -> Result<RecoveredRecords<T>, ArtifactError> {
        let run = self.run_path(run_id)?;
        let path = run.join(id.file_name());
        if !path.exists() {
            return Ok(RecoveredRecords {
                records: Vec::new(),
                partial_tail: None,
            });
        }
        let bytes = fs::read(&path).map_err(|source| io_error(&path, source))?;
        let partial_tail =
            (!bytes.is_empty() && bytes.last() != Some(&b'\n')).then(|| PartialRecordWarning {
                line: bytes.iter().filter(|byte| **byte == b'\n').count() + 1,
            });
        let complete_length = if partial_tail.is_some() {
            bytes
                .iter()
                .rposition(|byte| *byte == b'\n')
                .map_or(0, |index| index + 1)
        } else {
            bytes.len()
        };
        let mut records = Vec::new();
        for line in bytes[..complete_length].split(|byte| *byte == b'\n') {
            if line.is_empty() {
                continue;
            }
            records.push(decode_envelope(
                line,
                &path,
                expected_schema,
                supported_version,
            )?);
        }
        Ok(RecoveredRecords {
            records,
            partial_tail,
        })
    }

    /// Writes one immutable, content-addressed evidence object below the
    /// authoritative cell/trial hierarchy. The returned opaque id is the only
    /// identifier accepted by the download API; caller-controlled path
    /// components are never exposed as request paths.
    pub fn write_trial_evidence<T: Serialize>(
        &self,
        run_id: &str,
        cell_id: &str,
        trial_id: &str,
        value: &T,
    ) -> Result<ArtifactRef, ArtifactError> {
        validate_artifact_component(cell_id)?;
        validate_artifact_component(trial_id)?;
        let run = self.run_path(run_id)?;
        let cells = ensure_private_subdirectory(&run, CELLS_DIRECTORY)?;
        let cell = ensure_private_subdirectory(&cells, cell_id)?;
        let trials = ensure_private_subdirectory(&cell, TRIALS_DIRECTORY)?;
        let trial = ensure_private_subdirectory(&trials, trial_id)?;
        let evidence = ensure_private_subdirectory(&trial, BOUNDED_EVIDENCE_DIRECTORY)?;

        let envelope = SchemaEnvelope {
            schema_name: BOUNDED_EVIDENCE_SCHEMA_NAME.to_owned(),
            schema_version: BOUNDED_EVIDENCE_SCHEMA_VERSION,
            data: value,
        };
        let mut bytes =
            serde_json::to_vec_pretty(&envelope).map_err(|source| ArtifactError::Json {
                path: evidence.clone(),
                source,
            })?;
        bytes.push(b'\n');
        if bytes.len() > MAX_BOUNDED_EVIDENCE_BYTES {
            return Err(ArtifactError::ArtifactTooLarge {
                id: ArtifactId::BoundedEvidence,
                limit: MAX_BOUNDED_EVIDENCE_BYTES as u64,
            });
        }

        let content_hex = format!("{:x}", Sha256::digest(&bytes));
        let file_name = format!("{BOUNDED_EVIDENCE_PREFIX}{content_hex}.json");
        let path = evidence.join(&file_name);
        write_immutable_bytes(&path, &bytes)?;

        let relative = bounded_evidence_relative_path(cell_id, trial_id, &file_name);
        Ok(ArtifactRef {
            artifact_id: bounded_evidence_artifact_id(&relative),
            media_type: BOUNDED_EVIDENCE_MEDIA_TYPE.to_owned(),
            size_bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            sha256: format!("sha256:{content_hex}"),
        })
    }

    /// Preserves an incomplete final NDJSON record in a closed, non-served
    /// quarantine and truncates the source log to its last complete record.
    /// Complete logs are left byte-for-byte unchanged.
    pub fn quarantine_partial_tail(
        &self,
        run_id: &str,
        id: ArtifactId,
    ) -> Result<Option<QuarantinedTail>, ArtifactError> {
        if !matches!(id, ArtifactId::Events | ArtifactId::Observations) {
            return Err(ArtifactError::UnknownArtifact(id.as_str().to_owned()));
        }
        let run = self.run_path(run_id)?;
        let path = run.join(id.file_name());
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(io_error(&path, source)),
        };
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(ArtifactError::UnknownArtifact(id.as_str().to_owned()));
        }
        if metadata.len() == 0 {
            return Ok(None);
        }

        let mut source = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|error| io_error(&path, error))?;
        if source_byte_at_end(&mut source, &path)? == b'\n' {
            return Ok(None);
        }

        let complete_length = last_newline_end(&mut source, metadata.len(), &path)?;
        let line = count_newlines(&mut source, complete_length, &path)?.saturating_add(1);
        let quarantine = ensure_quarantine_directory(&run)?;
        let temporary = quarantine.join(format!(
            ".{}.partial-{}-{}.tmp",
            id.as_str(),
            std::process::id(),
            Uuid::new_v4().simple()
        ));
        let mut destination = create_private_file(&temporary)?;
        source
            .seek(SeekFrom::Start(complete_length))
            .map_err(|error| io_error(&path, error))?;
        let mut digest = Sha256::new();
        let mut copied = 0_u64;
        let mut buffer = [0_u8; RECOVERY_SCAN_BUFFER_BYTES];
        loop {
            let read = source
                .read(&mut buffer)
                .map_err(|error| io_error(&path, error))?;
            if read == 0 {
                break;
            }
            digest.update(&buffer[..read]);
            destination
                .write_all(&buffer[..read])
                .map_err(|error| io_error(&temporary, error))?;
            copied = copied.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
        }
        destination
            .sync_all()
            .map_err(|error| io_error(&temporary, error))?;
        let sha256 = format!("sha256:{:x}", digest.finalize());
        let digest_name = sha256.strip_prefix("sha256:").unwrap_or(&sha256);
        let file_name = format!("{}.partial-tail-{digest_name}.bin", id.as_str());
        let final_path = quarantine.join(&file_name);
        match fs::hard_link(&temporary, &final_path) {
            Ok(()) => {
                fs::remove_file(&temporary).map_err(|source| io_error(&temporary, source))?;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                fs::remove_file(&temporary).map_err(|source| io_error(&temporary, source))?;
            }
            Err(source) => return Err(io_error(&final_path, source)),
        }
        sync_directory(&quarantine)?;

        source
            .set_len(complete_length)
            .and_then(|()| source.sync_all())
            .map_err(|error| io_error(&path, error))?;
        sync_directory(&run)?;
        Ok(Some(QuarantinedTail {
            artifact: id,
            line,
            bytes: copied,
            sha256,
            file_name,
        }))
    }

    pub fn index(&self, run_id: &str) -> Result<Vec<ArtifactIndexEntry>, ArtifactError> {
        let run = self.run_path(run_id)?;
        let ids = [
            ArtifactId::RunManifest,
            ArtifactId::IntentPlan,
            ArtifactId::ExpandedPlan,
            ArtifactId::DefinitionSnapshot,
            ArtifactId::EnvironmentMetadata,
            ArtifactId::Events,
            ArtifactId::Observations,
            ArtifactId::Summary,
            ArtifactId::Report,
            ArtifactId::JsonExport,
            ArtifactId::CsvExport,
        ];
        let mut entries = Vec::new();
        for id in ids {
            let path = run.join(id.file_name());
            let Ok(metadata) = fs::symlink_metadata(&path) else {
                continue;
            };
            if metadata.is_file() && !metadata.file_type().is_symlink() {
                entries.push(ArtifactIndexEntry {
                    id,
                    artifact_id: id.as_str().to_owned(),
                    file_name: id.file_name().to_owned(),
                    media_type: id.media_type(),
                    bytes: metadata.len(),
                    sha256: sha256_file(&path)?,
                    downloadable: metadata.len() <= MAX_ARTIFACT_DOWNLOAD_BYTES,
                });
            }
        }
        entries.extend(
            bounded_evidence_entries(&run)?
                .into_iter()
                .map(|indexed| indexed.entry),
        );
        Ok(entries)
    }

    pub fn content(&self, run_id: &str, id_text: &str) -> Result<ArtifactContent, ArtifactError> {
        let run = self.run_path(run_id)?;
        if is_dynamic_bounded_evidence_id(id_text) {
            let indexed = bounded_evidence_entries(&run)?
                .into_iter()
                .find(|indexed| indexed.entry.artifact_id == id_text)
                .ok_or_else(|| ArtifactError::UnknownArtifact(id_text.to_owned()))?;
            if !indexed.entry.downloadable {
                return Err(ArtifactError::ArtifactTooLarge {
                    id: ArtifactId::BoundedEvidence,
                    limit: MAX_ARTIFACT_DOWNLOAD_BYTES,
                });
            }
            let bytes =
                fs::read(&indexed.path).map_err(|source| io_error(&indexed.path, source))?;
            return Ok(ArtifactContent {
                id: ArtifactId::BoundedEvidence,
                artifact_id: indexed.entry.artifact_id,
                media_type: BOUNDED_EVIDENCE_MEDIA_TYPE,
                bytes,
            });
        }

        let id = ArtifactId::parse(id_text)?;
        if id == ArtifactId::BoundedEvidence {
            return Err(ArtifactError::UnknownArtifact(id_text.to_owned()));
        }
        let path = run.join(id.file_name());
        let metadata = fs::symlink_metadata(&path).map_err(|source| io_error(&path, source))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(ArtifactError::UnknownArtifact(id_text.to_owned()));
        }
        if metadata.len() > MAX_ARTIFACT_DOWNLOAD_BYTES {
            return Err(ArtifactError::ArtifactTooLarge {
                id,
                limit: MAX_ARTIFACT_DOWNLOAD_BYTES,
            });
        }
        let bytes = fs::read(&path).map_err(|source| io_error(&path, source))?;
        Ok(ArtifactContent {
            id,
            artifact_id: id.as_str().to_owned(),
            media_type: id.media_type(),
            bytes,
        })
    }
}

#[derive(Debug)]
struct IndexedArtifactPath {
    entry: ArtifactIndexEntry,
    path: PathBuf,
}

fn bounded_evidence_entries(run: &Path) -> Result<Vec<IndexedArtifactPath>, ArtifactError> {
    let cells = run.join(CELLS_DIRECTORY);
    let cells_metadata = match fs::symlink_metadata(&cells) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(io_error(&cells, source)),
    };
    if !cells_metadata.is_dir() || cells_metadata.file_type().is_symlink() {
        return Err(ArtifactError::UnknownArtifact(CELLS_DIRECTORY.to_owned()));
    }

    let mut indexed = Vec::new();
    for cell in strict_subdirectories(&cells)? {
        let cell_id = path_component(&cell)?;
        validate_artifact_component(&cell_id)?;
        let trials = cell.join(TRIALS_DIRECTORY);
        let Some(trials) = existing_plain_directory(&trials)? else {
            continue;
        };
        for trial in strict_subdirectories(&trials)? {
            let trial_id = path_component(&trial)?;
            validate_artifact_component(&trial_id)?;
            let evidence = trial.join(BOUNDED_EVIDENCE_DIRECTORY);
            let Some(evidence) = existing_plain_directory(&evidence)? else {
                continue;
            };
            for item in fs::read_dir(&evidence).map_err(|source| io_error(&evidence, source))? {
                let item = item.map_err(|source| io_error(&evidence, source))?;
                let path = item.path();
                let Some(file_name) = item.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                if !is_bounded_evidence_file_name(&file_name) {
                    continue;
                }
                let metadata =
                    fs::symlink_metadata(&path).map_err(|source| io_error(&path, source))?;
                if !is_owned_regular_file(&metadata) {
                    return Err(ArtifactError::UnknownArtifact(file_name));
                }
                let relative = bounded_evidence_relative_path(&cell_id, &trial_id, &file_name);
                indexed.push(IndexedArtifactPath {
                    entry: ArtifactIndexEntry {
                        id: ArtifactId::BoundedEvidence,
                        artifact_id: bounded_evidence_artifact_id(&relative),
                        file_name: relative,
                        media_type: BOUNDED_EVIDENCE_MEDIA_TYPE,
                        bytes: metadata.len(),
                        sha256: sha256_file(&path)?,
                        downloadable: metadata.len() <= MAX_ARTIFACT_DOWNLOAD_BYTES,
                    },
                    path,
                });
            }
        }
    }
    indexed.sort_unstable_by(|left, right| {
        left.entry
            .artifact_id
            .cmp(&right.entry.artifact_id)
            .then_with(|| left.entry.file_name.cmp(&right.entry.file_name))
    });
    Ok(indexed)
}

fn strict_subdirectories(parent: &Path) -> Result<Vec<PathBuf>, ArtifactError> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(parent).map_err(|source| io_error(parent, source))? {
        let entry = entry.map_err(|source| io_error(parent, source))?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|source| io_error(&path, source))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(ArtifactError::UnknownArtifact(
                entry.file_name().to_string_lossy().into_owned(),
            ));
        }
        paths.push(path);
    }
    paths.sort_unstable();
    Ok(paths)
}

fn existing_plain_directory(path: &Path) -> Result<Option<PathBuf>, ArtifactError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            Ok(Some(path.to_path_buf()))
        }
        Ok(_) => Err(ArtifactError::UnknownArtifact(
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("artifact-directory")
                .to_owned(),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(io_error(path, source)),
    }
}

fn ensure_private_subdirectory(parent: &Path, name: &str) -> Result<PathBuf, ArtifactError> {
    let path = parent.join(name);
    match existing_plain_directory(&path)? {
        Some(path) => Ok(path),
        None => {
            create_private_directory(&path)?;
            sync_directory(parent)?;
            Ok(path)
        }
    }
}

fn write_immutable_bytes(path: &Path, bytes: &[u8]) -> Result<(), ArtifactError> {
    let parent = path.parent().unwrap_or(Path::new("."));
    let temporary = parent.join(format!(
        ".{}.tmp-{}-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("bounded-evidence"),
        std::process::id(),
        Uuid::new_v4().simple(),
    ));
    let mut file = create_private_file(&temporary)?;
    if let Err(source) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        let _ = fs::remove_file(&temporary);
        return Err(io_error(&temporary, source));
    }
    match fs::hard_link(&temporary, path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let metadata = match fs::symlink_metadata(path) {
                Ok(metadata) => metadata,
                Err(source) => {
                    let _ = fs::remove_file(&temporary);
                    return Err(io_error(path, source));
                }
            };
            if !is_owned_regular_file(&metadata) {
                let _ = fs::remove_file(&temporary);
                return Err(ArtifactError::UnknownArtifact(
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("bounded-evidence")
                        .to_owned(),
                ));
            }
            let existing = match fs::read(path) {
                Ok(existing) => existing,
                Err(source) => {
                    let _ = fs::remove_file(&temporary);
                    return Err(io_error(path, source));
                }
            };
            if existing != bytes {
                let _ = fs::remove_file(&temporary);
                return Err(ArtifactError::ImmutableArtifactConflict(path.to_path_buf()));
            }
        }
        Err(source) => {
            let _ = fs::remove_file(&temporary);
            return Err(io_error(path, source));
        }
    }
    fs::remove_file(&temporary).map_err(|source| io_error(&temporary, source))?;
    sync_directory(parent)
}

#[cfg(unix)]
fn is_owned_regular_file(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;

    metadata.is_file() && !metadata.file_type().is_symlink() && metadata.nlink() == 1
}

#[cfg(not(unix))]
fn is_owned_regular_file(metadata: &fs::Metadata) -> bool {
    metadata.is_file() && !metadata.file_type().is_symlink()
}

fn bounded_evidence_relative_path(cell_id: &str, trial_id: &str, file_name: &str) -> String {
    format!(
        "{CELLS_DIRECTORY}/{cell_id}/{TRIALS_DIRECTORY}/{trial_id}/{BOUNDED_EVIDENCE_DIRECTORY}/{file_name}"
    )
}

fn bounded_evidence_artifact_id(relative_path: &str) -> String {
    format!(
        "{DYNAMIC_BOUNDED_EVIDENCE_PREFIX}{:x}",
        Sha256::digest(relative_path.as_bytes())
    )
}

fn is_dynamic_bounded_evidence_id(value: &str) -> bool {
    value
        .strip_prefix(DYNAMIC_BOUNDED_EVIDENCE_PREFIX)
        .is_some_and(|digest| {
            digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

fn is_bounded_evidence_file_name(value: &str) -> bool {
    value
        .strip_prefix(BOUNDED_EVIDENCE_PREFIX)
        .and_then(|rest| rest.strip_suffix(".json"))
        .is_some_and(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
}

fn path_component(path: &Path) -> Result<String, ArtifactError> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .ok_or_else(|| ArtifactError::InvalidArtifactComponent(path.display().to_string()))
}

fn decode_envelope<T: DeserializeOwned>(
    bytes: &[u8],
    path: &Path,
    expected_schema: &str,
    current_version: u32,
) -> Result<T, ArtifactError> {
    let envelope: SchemaEnvelope<serde_json::Value> =
        serde_json::from_slice(bytes).map_err(|source| json_error(path, source))?;
    if envelope.schema_name != expected_schema {
        return Err(ArtifactError::SchemaMismatch {
            expected: expected_schema.to_owned(),
            actual: envelope.schema_name,
        });
    }

    match envelope.schema_version {
        version if version == current_version => decode_current(envelope.data, path),
        OBSERVATION_SCHEMA_V2
            if expected_schema == CURRENT_OBSERVATION_SCHEMA_NAME
                && current_version == CURRENT_OBSERVATION_SCHEMA_VERSION =>
        {
            decode_observation_v2(envelope.data, path)
        }
        OBSERVATION_SCHEMA_V1
            if expected_schema == CURRENT_OBSERVATION_SCHEMA_NAME
                && current_version == CURRENT_OBSERVATION_SCHEMA_VERSION =>
        {
            decode_observation_v1(envelope.data, path)
        }
        version => Err(ArtifactError::UnsupportedSchema {
            schema: expected_schema.to_owned(),
            version,
        }),
    }
}

fn decode_current<T: DeserializeOwned>(
    data: serde_json::Value,
    path: &Path,
) -> Result<T, ArtifactError> {
    serde_json::from_value(data).map_err(|source| json_error(path, source))
}

fn decode_observation_v2<T: DeserializeOwned>(
    data: serde_json::Value,
    path: &Path,
) -> Result<T, ArtifactError> {
    let old: SequencedObservationV2 =
        serde_json::from_value(data).map_err(|source| json_error(path, source))?;
    let current = migrate_observation_v2(old);
    let current = serde_json::to_value(current).map_err(|source| json_error(path, source))?;
    serde_json::from_value(current).map_err(|source| json_error(path, source))
}

fn decode_observation_v1<T: DeserializeOwned>(
    data: serde_json::Value,
    path: &Path,
) -> Result<T, ArtifactError> {
    let old: SequencedObservationV1 =
        serde_json::from_value(data).map_err(|source| json_error(path, source))?;
    let current = migrate_observation_v1(old).map_err(|message| {
        json_error(
            path,
            <serde_json::Error as serde::de::Error>::custom(message),
        )
    })?;
    let current = serde_json::to_value(current).map_err(|source| json_error(path, source))?;
    serde_json::from_value(current).map_err(|source| json_error(path, source))
}

fn migrate_observation_v1(old: SequencedObservationV1) -> Result<SequencedObservation, String> {
    let record = match old.record {
        ObservationRecordV1::Trial(record) => ObservationRecord::Trial(migrate_trial_v2(record)),
        ObservationRecordV1::Request(record) => ObservationRecord::Request(record),
        ObservationRecordV1::Resource(record) => ObservationRecord::Resource(record),
        ObservationRecordV1::Phase(record) => {
            ObservationRecord::Phase(migrate_phase_observation_v1(record)?)
        }
        ObservationRecordV1::Check(record) => ObservationRecord::Check(record),
    };
    Ok(SequencedObservation {
        sequence: old.sequence,
        record,
    })
}

fn migrate_observation_v2(old: SequencedObservationV2) -> SequencedObservation {
    let record = match old.record {
        ObservationRecordV2::Trial(record) => ObservationRecord::Trial(migrate_trial_v2(record)),
        ObservationRecordV2::Request(record) => ObservationRecord::Request(record),
        ObservationRecordV2::Resource(record) => ObservationRecord::Resource(record),
        ObservationRecordV2::Phase(record) => ObservationRecord::Phase(record),
        ObservationRecordV2::Check(record) => ObservationRecord::Check(record),
        ObservationRecordV2::Operation(record) => ObservationRecord::Operation(record),
    };
    SequencedObservation {
        sequence: old.sequence,
        record,
    }
}

fn migrate_trial_v2(old: TrialSampleV2) -> TrialSample {
    TrialSample {
        operation_id: old.operation_id,
        cell_id: old.cell_id,
        trial_id: old.trial_id,
        kind: old.kind,
        sequence_in_cell: old.sequence_in_cell,
        lifecycle: old.lifecycle,
        product_succeeded: old.product_succeeded,
        infrastructure_failed: old.infrastructure_failed,
        cleanup_baseline_restored: old.cleanup_baseline_restored,
        correctness: old.correctness,
        primary_operation_latency_ns: old.primary_operation_latency_ns,
        artifacts: Vec::new(),
    }
}

fn migrate_phase_observation_v1(old: PhaseObservationV1) -> Result<PhaseObservation, String> {
    let (operation, phase) = unique_phase_definition(&old)?;
    if operation != OperationId::SquashLayerstack {
        return Err(format!(
            "v1 phase {:?} resolved to unsupported operation context {operation:?}",
            old.id
        ));
    }
    Ok(PhaseObservation {
        id: old.id,
        semantic_revision: old.semantic_revision,
        unit: phase.unit,
        cell_id: old.cell_id,
        trial_id: old.trial_id,
        request_id: old.request_id,
        source: old.source,
        correlation: phase.correlation,
        trace_span_name: phase.trace_span_name.to_owned(),
        start_offset_ns: old.start_offset_ns,
        duration_ns: old.duration_ns,
        status: old.status,
    })
}

fn unique_phase_definition(
    old: &PhaseObservationV1,
) -> Result<(OperationId, &'static PhaseReference), String> {
    let id_matches = OperationId::ALL
        .into_iter()
        .flat_map(|operation| {
            definition(operation)
                .phases
                .iter()
                .map(move |phase| (operation, phase))
        })
        .filter(|(_, phase)| phase.id == old.id)
        .collect::<Vec<_>>();
    let exact_matches = id_matches
        .iter()
        .copied()
        .filter(|(_, phase)| {
            phase.semantic_revision == old.semantic_revision && phase.source == old.source
        })
        .collect::<Vec<_>>();
    match exact_matches.as_slice() {
        [matched] => Ok(*matched),
        [] if id_matches.is_empty() => Err(format!(
            "v1 phase {:?} is not registered by any operation definition",
            old.id
        )),
        [] => Err(format!(
            "v1 phase {:?} revision {} and source {:?} do not match its registered definition",
            old.id, old.semantic_revision, old.source
        )),
        _ => Err(format!(
            "v1 phase {:?} revision {} and source {:?} is ambiguous across operation definitions",
            old.id, old.semantic_revision, old.source
        )),
    }
}

fn json_error(path: &Path, source: serde_json::Error) -> ArtifactError {
    ArtifactError::Json {
        path: path.to_path_buf(),
        source,
    }
}

fn validate_run_id(run_id: &str) -> Result<(), ArtifactError> {
    let valid = !run_id.is_empty()
        && run_id.len() <= 64
        && run_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-');
    if valid {
        Ok(())
    } else {
        Err(ArtifactError::InvalidRunId(run_id.to_owned()))
    }
}

fn validate_artifact_component(value: &str) -> Result<(), ArtifactError> {
    let valid = !value.is_empty()
        && value.len() <= 128
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':'));
    if valid {
        Ok(())
    } else {
        Err(ArtifactError::InvalidArtifactComponent(value.to_owned()))
    }
}

fn write_json_create_new<T: Serialize>(path: &Path, value: &T) -> Result<(), ArtifactError> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|source| ArtifactError::Json {
        path: path.to_path_buf(),
        source,
    })?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|source| io_error(path, source))?;
    file.write_all(&bytes)
        .and_then(|()| file.write_all(b"\n"))
        .and_then(|()| file.sync_all())
        .map_err(|source| io_error(path, source))?;
    sync_directory(path.parent().unwrap_or(Path::new(".")))
}

fn write_json_replace<T: Serialize>(path: &Path, value: &T) -> Result<(), ArtifactError> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|source| ArtifactError::Json {
        path: path.to_path_buf(),
        source,
    })?;
    bytes.push(b'\n');
    write_bytes_replace(path, &bytes)
}

fn write_bytes_replace(path: &Path, bytes: &[u8]) -> Result<(), ArtifactError> {
    let parent = path.parent().unwrap_or(Path::new("."));
    let temporary = parent.join(format!(
        ".{}.tmp-{}-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("artifact"),
        std::process::id(),
        Uuid::new_v4().simple(),
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|source| io_error(&temporary, source))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|source| io_error(&temporary, source))?;
    fs::rename(&temporary, path).map_err(|source| io_error(path, source))?;
    sync_directory(parent)
}

fn sha256_file(path: &Path) -> Result<String, ArtifactError> {
    let mut file = File::open(path).map_err(|source| io_error(path, source))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|source| io_error(path, source))?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

fn reject_partial_tail(path: &Path) -> Result<(), ArtifactError> {
    let mut file = File::open(path).map_err(|source| io_error(path, source))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|source| io_error(path, source))?;
    if bytes.is_empty() || bytes.last() == Some(&b'\n') {
        return Ok(());
    }
    let line = bytes.iter().filter(|byte| **byte == b'\n').count() + 1;
    Err(ArtifactError::PartialNdjsonTail {
        path: path.to_path_buf(),
        line,
    })
}

fn source_byte_at_end(source: &mut File, path: &Path) -> Result<u8, ArtifactError> {
    source
        .seek(SeekFrom::End(-1))
        .map_err(|error| io_error(path, error))?;
    let mut byte = [0_u8; 1];
    source
        .read_exact(&mut byte)
        .map_err(|error| io_error(path, error))?;
    Ok(byte[0])
}

fn last_newline_end(source: &mut File, length: u64, path: &Path) -> Result<u64, ArtifactError> {
    let mut end = length;
    let mut buffer = [0_u8; RECOVERY_SCAN_BUFFER_BYTES];
    while end > 0 {
        let start = end.saturating_sub(RECOVERY_SCAN_BUFFER_BYTES as u64);
        let chunk_length = usize::try_from(end - start).unwrap_or(RECOVERY_SCAN_BUFFER_BYTES);
        source
            .seek(SeekFrom::Start(start))
            .and_then(|_| source.read_exact(&mut buffer[..chunk_length]))
            .map_err(|error| io_error(path, error))?;
        if let Some(index) = buffer[..chunk_length]
            .iter()
            .rposition(|byte| *byte == b'\n')
        {
            return Ok(start.saturating_add(index as u64).saturating_add(1));
        }
        end = start;
    }
    Ok(0)
}

fn count_newlines(source: &mut File, length: u64, path: &Path) -> Result<usize, ArtifactError> {
    source
        .seek(SeekFrom::Start(0))
        .map_err(|error| io_error(path, error))?;
    let mut remaining = length;
    let mut count = 0_usize;
    let mut buffer = [0_u8; RECOVERY_SCAN_BUFFER_BYTES];
    while remaining > 0 {
        let requested = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
        source
            .read_exact(&mut buffer[..requested])
            .map_err(|error| io_error(path, error))?;
        count = count.saturating_add(
            buffer[..requested]
                .iter()
                .filter(|byte| **byte == b'\n')
                .count(),
        );
        remaining -= requested as u64;
    }
    Ok(count)
}

fn ensure_quarantine_directory(run: &Path) -> Result<PathBuf, ArtifactError> {
    let path = run.join(RECOVERY_QUARANTINE_DIRECTORY);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => return Ok(path),
        Ok(_) => {
            return Err(ArtifactError::UnknownArtifact(
                RECOVERY_QUARANTINE_DIRECTORY.to_owned(),
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(source) => return Err(io_error(&path, source)),
    }
    create_private_directory(&path)?;
    sync_directory(run)?;
    Ok(path)
}

#[cfg(unix)]
fn create_private_directory(path: &Path) -> Result<(), ArtifactError> {
    use std::os::unix::fs::DirBuilderExt;

    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700);
    builder
        .create(path)
        .map_err(|source| io_error(path, source))
}

#[cfg(not(unix))]
fn create_private_directory(path: &Path) -> Result<(), ArtifactError> {
    fs::create_dir(path).map_err(|source| io_error(path, source))
}

#[cfg(unix)]
fn create_private_file(path: &Path) -> Result<File, ArtifactError> {
    use std::os::unix::fs::OpenOptionsExt;

    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|source| io_error(path, source))
}

#[cfg(not(unix))]
fn create_private_file(path: &Path) -> Result<File, ArtifactError> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|source| io_error(path, source))
}

fn sync_directory(path: &Path) -> Result<(), ArtifactError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io_error(path, source))
}

fn io_error(path: &Path, source: io::Error) -> ArtifactError {
    ArtifactError::Io {
        path: path.to_path_buf(),
        source,
    }
}
