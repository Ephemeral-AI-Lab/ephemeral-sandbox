use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{evidence, PocError, PocResult, RunId, SCHEMA_VERSION};

pub const MANIFEST_FILE: &str = "manifest.sha256";
const HASH_BUFFER_BYTES: usize = 32 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaseOutcome {
    Passed,
    Failed,
    Cancelled,
    Incomplete,
    Unqualified,
    NotRunBudget,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceClass {
    MatchedSpeedup,
    HistoricalEquivalent,
    AbsoluteGateOnly,
    PhysicalFloor,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AssertionReceipt {
    pub name: String,
    pub passed: bool,
    pub observed: String,
    pub expected: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CaseReceipt {
    pub schema_version: u32,
    pub run_id: RunId,
    pub case_id: String,
    pub outcome: CaseOutcome,
    pub evidence_class: EvidenceClass,
    pub started_unix_ms: u64,
    pub finished_unix_ms: u64,
    pub duration_ns: u64,
    pub assertions: Vec<AssertionReceipt>,
    pub failures_and_unknowns: Vec<String>,
    pub artifact_path: PathBuf,
}

impl CaseReceipt {
    #[must_use]
    pub fn passes(&self) -> bool {
        self.outcome == CaseOutcome::Passed
            && self.assertions.iter().all(|assertion| assertion.passed)
            && self.failures_and_unknowns.is_empty()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ManifestEntry {
    pub relative_path: PathBuf,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ManifestReceipt {
    pub schema_version: u32,
    pub evidence_root: PathBuf,
    pub manifest_path: PathBuf,
    pub entries: Vec<ManifestEntry>,
    pub manifest_sha256: String,
    pub verified: bool,
}

pub fn seal_manifest(evidence_root: &Path) -> PocResult<ManifestReceipt> {
    require_real_directory(evidence_root)?;
    let manifest_path = evidence_root.join(MANIFEST_FILE);
    let entries = collect_entries(evidence_root)?;
    let mut body = Vec::new();
    for entry in &entries {
        let path = manifest_path_text(&entry.relative_path)?;
        body.extend_from_slice(entry.sha256.as_bytes());
        body.extend_from_slice(b"  ");
        body.extend_from_slice(path.as_bytes());
        body.push(b'\n');
    }
    evidence::write_atomic_bytes(&manifest_path, &body)?;
    let manifest_sha256 = digest_file(&manifest_path)?.0;
    let verified = verify_manifest(evidence_root)?.verified;
    Ok(ManifestReceipt {
        schema_version: SCHEMA_VERSION,
        evidence_root: evidence_root.to_path_buf(),
        manifest_path,
        entries,
        manifest_sha256,
        verified,
    })
}

pub fn verify_manifest(evidence_root: &Path) -> PocResult<ManifestReceipt> {
    require_real_directory(evidence_root)?;
    let manifest_path = evidence_root.join(MANIFEST_FILE);
    let file = File::open(&manifest_path)
        .map_err(|source| PocError::io("open evidence manifest", &manifest_path, source))?;
    let mut expected = BTreeMap::new();
    for line in BufReader::new(file).lines() {
        let line =
            line.map_err(|source| PocError::io("read evidence manifest", &manifest_path, source))?;
        let (digest, path) = line.split_once("  ").ok_or_else(|| {
            PocError::Integrity(format!("malformed evidence manifest line: {line}"))
        })?;
        validate_digest(digest)?;
        let relative_path = PathBuf::from(path);
        validate_relative_path(&relative_path)?;
        if expected
            .insert(relative_path.clone(), digest.to_owned())
            .is_some()
        {
            return Err(PocError::Integrity(format!(
                "duplicate evidence manifest path: {}",
                relative_path.display()
            )));
        }
    }
    let actual = collect_entries(evidence_root)?;
    let actual_paths: BTreeSet<_> = actual
        .iter()
        .map(|entry| entry.relative_path.clone())
        .collect();
    let expected_paths: BTreeSet<_> = expected.keys().cloned().collect();
    if actual_paths != expected_paths {
        return Err(PocError::Integrity(format!(
            "evidence manifest path set differs: missing={:?}, unexpected={:?}",
            expected_paths.difference(&actual_paths).collect::<Vec<_>>(),
            actual_paths.difference(&expected_paths).collect::<Vec<_>>()
        )));
    }
    for entry in &actual {
        if expected.get(&entry.relative_path) != Some(&entry.sha256) {
            return Err(PocError::Integrity(format!(
                "evidence digest mismatch for {}",
                entry.relative_path.display()
            )));
        }
    }
    let manifest_sha256 = digest_file(&manifest_path)?.0;
    Ok(ManifestReceipt {
        schema_version: SCHEMA_VERSION,
        evidence_root: evidence_root.to_path_buf(),
        manifest_path,
        entries: actual,
        manifest_sha256,
        verified: true,
    })
}

pub fn verify_case_set(
    evidence_root: &Path,
    required_case_ids: &[&str],
) -> PocResult<Vec<CaseReceipt>> {
    let mut receipts = Vec::with_capacity(required_case_ids.len());
    for case_id in required_case_ids {
        validate_case_id(case_id)?;
        let path = evidence_root
            .join("cases")
            .join(case_id)
            .join("result.json");
        let receipt: CaseReceipt = evidence::read_json(&path)?;
        if receipt.case_id != *case_id || receipt.artifact_path != path || !receipt.passes() {
            return Err(PocError::Integrity(format!(
                "required case {case_id} is not a complete passing receipt"
            )));
        }
        receipts.push(receipt);
    }
    Ok(receipts)
}

fn collect_entries(evidence_root: &Path) -> PocResult<Vec<ManifestEntry>> {
    let mut pending = vec![evidence_root.to_path_buf()];
    let mut entries = Vec::new();
    while let Some(path) = pending.pop() {
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|source| PocError::io("stat evidence artifact", &path, source))?;
        if metadata.file_type().is_symlink() {
            return Err(PocError::Integrity(format!(
                "evidence tree contains symlink: {}",
                path.display()
            )));
        }
        if metadata.is_dir() {
            for entry in std::fs::read_dir(&path)
                .map_err(|source| PocError::io("read evidence directory", &path, source))?
            {
                let entry =
                    entry.map_err(|source| PocError::io("read evidence entry", &path, source))?;
                pending.push(entry.path());
            }
        } else if metadata.is_file() && path != evidence_root.join(MANIFEST_FILE) {
            let relative_path = path
                .strip_prefix(evidence_root)
                .map_err(|error| PocError::Integrity(error.to_string()))?
                .to_path_buf();
            validate_relative_path(&relative_path)?;
            let (sha256, bytes) = digest_file(&path)?;
            entries.push(ManifestEntry {
                relative_path,
                sha256,
                bytes,
            });
        } else if !metadata.is_file() {
            return Err(PocError::Integrity(format!(
                "evidence artifact is not a regular file: {}",
                path.display()
            )));
        }
    }
    entries.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(entries)
}

fn digest_file(path: &Path) -> PocResult<(String, u64)> {
    let mut file =
        File::open(path).map_err(|source| PocError::io("open evidence artifact", path, source))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; HASH_BUFFER_BYTES];
    let mut bytes = 0_u64;
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|source| PocError::io("hash evidence artifact", path, source))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
        bytes = bytes
            .checked_add(u64::try_from(read).expect("read length fits u64"))
            .ok_or_else(|| PocError::Integrity("evidence byte count overflow".to_owned()))?;
    }
    Ok((format!("{:x}", digest.finalize()), bytes))
}

fn manifest_path_text(path: &Path) -> PocResult<String> {
    validate_relative_path(path)?;
    path.to_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| PocError::Integrity("evidence path is not UTF-8".to_owned()))
}

fn validate_relative_path(path: &Path) -> PocResult<()> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(PocError::Integrity(format!(
            "invalid relative evidence path: {}",
            path.display()
        )));
    }
    Ok(())
}

fn validate_digest(digest: &str) -> PocResult<()> {
    if digest.len() != 64
        || !digest
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(PocError::Integrity(format!(
            "invalid manifest SHA-256: {digest}"
        )));
    }
    Ok(())
}

fn validate_case_id(case_id: &str) -> PocResult<()> {
    let valid = !case_id.is_empty()
        && case_id.len() <= 32
        && case_id
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-');
    if !valid {
        return Err(PocError::Integrity(format!(
            "invalid evidence case id: {case_id}"
        )));
    }
    Ok(())
}

fn require_real_directory(path: &Path) -> PocResult<()> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|source| PocError::io("stat evidence root", path, source))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(PocError::Integrity(format!(
            "evidence root is not a real directory: {}",
            path.display()
        )));
    }
    Ok(())
}
