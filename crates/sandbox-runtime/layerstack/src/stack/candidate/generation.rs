use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use sandbox_runtime_layerstack_core::{Digest32, RootId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::operation::{read_common_state, replace_common_state, sync_common_parent};

const MATERIALIZATION_KEY_DOMAIN: &[u8] = b"EOS-LS3-MATERIALIZATION-KEY\0";
const CURRENT_CHECKSUM_DOMAIN: &[u8] = b"EOS-LS3-MATERIALIZATION-CURRENT\0";
const LEASE_CHECKSUM_DOMAIN: &[u8] = b"EOS-LS3-MATERIALIZATION-LEASE\0";
const LEASE_ID_DOMAIN: &[u8] = b"EOS-LS3-MATERIALIZATION-LEASE-ID\0";
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const MAX_LEASE_BYTES: u64 = 4096;
const BACKEND_KIND: &str = "linux-overlayfs-native";
const TARGET_PROFILE: &str = "linux-overlayfs-v1";
const CARRIER_RELATIVE_PATH: &str = "carriers/native";

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct MaterializationKey {
    pub(crate) root: RootId,
    pub(crate) backend_kind: String,
    pub(crate) backend_format_version: u16,
    pub(crate) target_profile: String,
}

impl MaterializationKey {
    pub(crate) fn linux_overlayfs(root: RootId) -> Self {
        Self {
            root,
            backend_kind: BACKEND_KIND.to_owned(),
            backend_format_version: 1,
            target_profile: TARGET_PROFILE.to_owned(),
        }
    }

    pub(crate) fn validate(&self) -> Result<(), GenerationError> {
        if self.backend_kind != BACKEND_KIND
            || self.backend_format_version != 1
            || self.target_profile != TARGET_PROFILE
        {
            return Err(GenerationError::Unsupported(
                "unsupported native materialization profile".to_owned(),
            ));
        }
        Ok(())
    }

    pub(crate) fn id(&self) -> Result<MaterializationId, GenerationError> {
        self.validate()?;
        let mut preimage = Vec::with_capacity(
            MATERIALIZATION_KEY_DOMAIN.len()
                + 32
                + self.backend_kind.len()
                + self.target_profile.len()
                + 6,
        );
        preimage.extend_from_slice(MATERIALIZATION_KEY_DOMAIN);
        preimage.extend_from_slice(self.root.digest().as_bytes());
        push_bounded_string(&mut preimage, &self.backend_kind)?;
        preimage.extend_from_slice(&self.backend_format_version.to_be_bytes());
        push_bounded_string(&mut preimage, &self.target_profile)?;
        Ok(MaterializationId(sha256(&preimage)))
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct MaterializationId([u8; 32]);

impl MaterializationId {
    pub(crate) const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub(crate) const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub(crate) fn hex(self) -> String {
        hex(&self.0)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CapabilityProfile {
    pub(crate) feature_bits: u64,
    pub(crate) raw_byte_names: bool,
    pub(crate) exact_metadata: bool,
    pub(crate) sparse_files: bool,
    pub(crate) hardlinks: bool,
    pub(crate) symlinks: bool,
    pub(crate) devices: bool,
    pub(crate) fifos: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CarrierDescriptor {
    pub(crate) carrier_id: String,
    pub(crate) relative_path: String,
    pub(crate) native_tree_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GenerationManifest {
    pub(crate) schema: String,
    pub(crate) schema_version: u16,
    pub(crate) materialization_id: String,
    pub(crate) root_id: String,
    pub(crate) backend_kind: String,
    pub(crate) backend_format_version: u16,
    pub(crate) target_profile: String,
    pub(crate) generation: u64,
    pub(crate) fence: u64,
    pub(crate) carriers: Vec<CarrierDescriptor>,
    pub(crate) required_capabilities: CapabilityProfile,
    pub(crate) provided_capabilities: CapabilityProfile,
    pub(crate) logical_verification_root: String,
    pub(crate) native_tree_sha256: String,
    pub(crate) entry_count: u64,
    pub(crate) logical_bytes: u64,
    pub(crate) allocated_bytes: u64,
    pub(crate) allocation_method: String,
    pub(crate) build_operation_id: String,
    pub(crate) completed_unix_seconds: u64,
}

impl GenerationManifest {
    pub(crate) fn validate_for(
        &self,
        key: &MaterializationKey,
        id: MaterializationId,
        generation: u64,
    ) -> Result<(), GenerationError> {
        if self.schema != "layerstack-materialization-generation-v1"
            || self.schema_version != 1
            || self.materialization_id != id.hex()
            || self.root_id != digest_string(key.root.digest())
            || self.backend_kind != key.backend_kind
            || self.backend_format_version != key.backend_format_version
            || self.target_profile != key.target_profile
            || self.generation != generation
            || self.fence == 0
            || self.logical_verification_root != digest_string(key.root.digest())
            || self.carriers.len() != 1
            || self.carriers[0].carrier_id != "native"
            || self.carriers[0].relative_path != CARRIER_RELATIVE_PATH
            || self.carriers[0].native_tree_sha256 != self.native_tree_sha256
            || self.allocation_method != "stat.st_blocks*512"
            || !is_hex(&self.native_tree_sha256, 32)
            || !is_hex(&self.build_operation_id, 32)
        {
            return Err(GenerationError::Corrupt(
                "generation manifest identity or framing".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GenerationSelection {
    pub(crate) manifest: GenerationManifest,
    pub(crate) manifest_sha256: String,
    pub(crate) carrier_path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GenerationSnapshot {
    pub(crate) manifest_sha256: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct CurrentGeneration {
    schema: String,
    schema_version: u16,
    materialization_id: String,
    generation: u64,
    fence: u64,
    manifest_sha256: String,
    checksum_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GenerationLease {
    pub(crate) schema: String,
    pub(crate) schema_version: u16,
    pub(crate) lease_id: String,
    pub(crate) materialization_id: String,
    pub(crate) generation: u64,
    pub(crate) fence: u64,
    pub(crate) owner: String,
    pub(crate) session_id: String,
    pub(crate) acquired_unix_seconds: u64,
    pub(crate) renewed_unix_seconds: u64,
    pub(crate) expires_unix_seconds: u64,
    pub(crate) checksum_sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum GenerationError {
    Io(String),
    Invalid(String),
    Corrupt(String),
    Unsupported(String),
    NotFound,
    Collision(String),
}

impl fmt::Display for GenerationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(message) => write!(formatter, "materialization generation I/O: {message}"),
            Self::Invalid(message) => {
                write!(formatter, "invalid materialization generation: {message}")
            }
            Self::Corrupt(message) => {
                write!(formatter, "corrupt materialization generation: {message}")
            }
            Self::Unsupported(message) => write!(
                formatter,
                "unsupported materialization generation: {message}"
            ),
            Self::NotFound => write!(formatter, "materialization generation was not found"),
            Self::Collision(message) => {
                write!(formatter, "materialization generation collision: {message}")
            }
        }
    }
}

impl std::error::Error for GenerationError {}

impl From<std::io::Error> for GenerationError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

#[derive(Clone, Debug)]
pub(crate) struct GenerationStore {
    storage_root: PathBuf,
}

impl GenerationStore {
    pub(crate) fn new(storage_root: PathBuf) -> Result<Self, GenerationError> {
        let metadata = std::fs::symlink_metadata(&storage_root)?;
        if !metadata.file_type().is_dir() {
            return Err(GenerationError::Invalid(
                "storage root is not a directory".to_owned(),
            ));
        }
        Ok(Self { storage_root })
    }

    pub(crate) fn lookup_current(
        &self,
        key: &MaterializationKey,
    ) -> Result<Option<GenerationSelection>, GenerationError> {
        let id = key.id()?;
        let Some(bytes) = read_common_state(&self.current_path(id))
            .map_err(|error| GenerationError::Io(error.to_string()))?
        else {
            return Ok(None);
        };
        let current: CurrentGeneration = serde_json::from_slice(&bytes)
            .map_err(|error| GenerationError::Corrupt(error.to_string()))?;
        verify_current(&current, id)?;
        let manifest_path = self.manifest_path(id, current.generation);
        let manifest_bytes =
            read_bounded(&manifest_path, MAX_MANIFEST_BYTES)?.ok_or(GenerationError::NotFound)?;
        let manifest_sha256 = hex(&sha256(&manifest_bytes));
        if manifest_sha256 != current.manifest_sha256 {
            return Err(GenerationError::Corrupt(
                "CURRENT manifest digest mismatch".to_owned(),
            ));
        }
        let manifest: GenerationManifest = serde_json::from_slice(&manifest_bytes)
            .map_err(|error| GenerationError::Corrupt(error.to_string()))?;
        manifest.validate_for(key, id, current.generation)?;
        if manifest.fence != current.fence {
            return Err(GenerationError::Corrupt(
                "CURRENT fence does not match generation manifest".to_owned(),
            ));
        }
        let carrier_path = self
            .generation_path(id, current.generation)
            .join(CARRIER_RELATIVE_PATH);
        let metadata =
            std::fs::symlink_metadata(&carrier_path).map_err(|_| GenerationError::NotFound)?;
        if !metadata.file_type().is_dir() {
            return Err(GenerationError::Corrupt(
                "native carrier is not a directory".to_owned(),
            ));
        }
        Ok(Some(GenerationSelection {
            manifest,
            manifest_sha256,
            carrier_path,
        }))
    }

    pub(crate) fn next_generation(
        &self,
        id: MaterializationId,
    ) -> Result<(u64, u64), GenerationError> {
        let generations = self.materialization_path(id).join("generations");
        let mut maximum = 0_u64;
        match std::fs::read_dir(&generations) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry?;
                    if !entry.file_type()?.is_dir() {
                        return Err(GenerationError::Corrupt(
                            "generation entry is not a directory".to_owned(),
                        ));
                    }
                    let name = entry.file_name();
                    let name = name.to_str().ok_or_else(|| {
                        GenerationError::Corrupt("non-UTF-8 generation directory".to_owned())
                    })?;
                    let value = name.parse::<u64>().map_err(|_| {
                        GenerationError::Corrupt("invalid generation directory".to_owned())
                    })?;
                    maximum = maximum.max(value);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let generation = maximum
            .checked_add(1)
            .ok_or_else(|| GenerationError::Invalid("generation counter exhausted".to_owned()))?;
        Ok((generation, generation))
    }

    pub(crate) fn install_carrier(
        &self,
        id: MaterializationId,
        generation: u64,
        work_carrier: &Path,
    ) -> Result<PathBuf, GenerationError> {
        let carrier_parent = self.generation_path(id, generation).join("carriers");
        ensure_directory(&self.storage_root, &carrier_parent)?;
        let carrier = carrier_parent.join("native");
        match std::fs::symlink_metadata(&carrier) {
            Ok(metadata) if metadata.file_type().is_dir() => {
                match std::fs::symlink_metadata(work_carrier) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        return Ok(carrier);
                    }
                    Ok(_) => {}
                    Err(error) => return Err(error.into()),
                }
                return Err(GenerationError::Collision(
                    "work and installed native carriers both exist".to_owned(),
                ));
            }
            Ok(_) => {
                return Err(GenerationError::Collision(
                    "native carrier already exists before install".to_owned(),
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        match std::fs::rename(work_carrier, &carrier) {
            Ok(()) => sync_dir(&carrier_parent)?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(GenerationError::Collision(
                    "native carrier already exists".to_owned(),
                ));
            }
            Err(error) => return Err(error.into()),
        }
        Ok(carrier)
    }

    pub(crate) fn publish(
        &self,
        key: &MaterializationKey,
        manifest: &GenerationManifest,
    ) -> Result<GenerationSelection, GenerationError> {
        self.publish_manifest(key, manifest)?;
        self.promote_generation(key, manifest.generation)
    }

    pub(crate) fn publish_manifest(
        &self,
        key: &MaterializationKey,
        manifest: &GenerationManifest,
    ) -> Result<GenerationSelection, GenerationError> {
        let id = key.id()?;
        manifest.validate_for(key, id, manifest.generation)?;
        let manifest_bytes = serde_json::to_vec(manifest)
            .map_err(|error| GenerationError::Invalid(error.to_string()))?;
        if manifest_bytes.len() as u64 > MAX_MANIFEST_BYTES {
            return Err(GenerationError::Invalid(
                "generation manifest exceeds bound".to_owned(),
            ));
        }
        let manifest_path = self.manifest_path(id, manifest.generation);
        ensure_directory(
            &self.storage_root,
            manifest_path.parent().expect("manifest parent"),
        )?;
        write_immutable(&manifest_path, &manifest_bytes)?;
        let manifest_sha256 = hex(&sha256(&manifest_bytes));
        let carrier_path = self
            .generation_path(id, manifest.generation)
            .join(CARRIER_RELATIVE_PATH);
        let metadata =
            std::fs::symlink_metadata(&carrier_path).map_err(|_| GenerationError::NotFound)?;
        if !metadata.file_type().is_dir() {
            return Err(GenerationError::Corrupt(
                "published carrier is not a directory".to_owned(),
            ));
        }
        Ok(GenerationSelection {
            manifest: manifest.clone(),
            manifest_sha256,
            carrier_path,
        })
    }

    pub(crate) fn promote_generation(
        &self,
        key: &MaterializationKey,
        generation: u64,
    ) -> Result<GenerationSelection, GenerationError> {
        let id = key.id()?;
        let selection = self.read_generation(key, generation)?;
        let current = seal_current(CurrentGeneration {
            schema: "layerstack-materialization-current-v1".to_owned(),
            schema_version: 1,
            materialization_id: id.hex(),
            generation,
            fence: selection.manifest.fence,
            manifest_sha256: selection.manifest_sha256.clone(),
            checksum_sha256: String::new(),
        })?;
        let current_bytes = serde_json::to_vec(&current)
            .map_err(|error| GenerationError::Invalid(error.to_string()))?;
        replace_common_state(&self.current_path(id), &current_bytes)
            .map_err(|error| GenerationError::Io(error.to_string()))?;
        self.lookup_current(key)?
            .ok_or_else(|| GenerationError::Corrupt("published CURRENT disappeared".to_owned()))
    }

    pub(crate) fn read_generation(
        &self,
        key: &MaterializationKey,
        generation: u64,
    ) -> Result<GenerationSelection, GenerationError> {
        let id = key.id()?;
        let manifest_path = self.manifest_path(id, generation);
        let manifest_bytes =
            read_bounded(&manifest_path, MAX_MANIFEST_BYTES)?.ok_or(GenerationError::NotFound)?;
        let manifest_sha256 = hex(&sha256(&manifest_bytes));
        let manifest: GenerationManifest = serde_json::from_slice(&manifest_bytes)
            .map_err(|error| GenerationError::Corrupt(error.to_string()))?;
        manifest.validate_for(key, id, generation)?;
        let carrier_path = self
            .generation_path(id, generation)
            .join(CARRIER_RELATIVE_PATH);
        let metadata =
            std::fs::symlink_metadata(&carrier_path).map_err(|_| GenerationError::NotFound)?;
        if !metadata.file_type().is_dir() {
            return Err(GenerationError::Corrupt(
                "generation carrier is not a directory".to_owned(),
            ));
        }
        Ok(GenerationSelection {
            manifest,
            manifest_sha256,
            carrier_path,
        })
    }

    pub(crate) fn generation_numbers(
        &self,
        id: MaterializationId,
    ) -> Result<Vec<u64>, GenerationError> {
        let path = self.materialization_path(id).join("generations");
        let entries = match std::fs::read_dir(&path) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut generations = Vec::new();
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                return Err(GenerationError::Corrupt(
                    "generation entry is not a directory".to_owned(),
                ));
            }
            let name = entry.file_name();
            let name = name.to_str().ok_or_else(|| {
                GenerationError::Corrupt("non-UTF-8 generation directory".to_owned())
            })?;
            if name.len() != 20 || !name.as_bytes().iter().all(u8::is_ascii_digit) {
                return Err(GenerationError::Corrupt(
                    "generation directory is not canonical".to_owned(),
                ));
            }
            let generation = name
                .parse::<u64>()
                .map_err(|_| GenerationError::Corrupt("invalid generation directory".to_owned()))?;
            if generation == 0 || format!("{generation:020}") != name {
                return Err(GenerationError::Corrupt(
                    "generation directory is not canonical".to_owned(),
                ));
            }
            generations.push(generation);
        }
        generations.sort_unstable();
        Ok(generations)
    }

    pub(crate) fn generation_snapshot(
        &self,
        id: MaterializationId,
        generation: u64,
    ) -> Result<GenerationSnapshot, GenerationError> {
        let path = self.generation_path(id, generation);
        let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                GenerationError::NotFound
            } else {
                GenerationError::from(error)
            }
        })?;
        if !metadata.file_type().is_dir() {
            return Err(GenerationError::Corrupt(
                "generation owner is not a directory".to_owned(),
            ));
        }
        let manifest_sha256 =
            read_bounded(&self.manifest_path(id, generation), MAX_MANIFEST_BYTES)?
                .map(|bytes| hex(&sha256(&bytes)));
        Ok(GenerationSnapshot { manifest_sha256 })
    }

    pub(crate) fn active_generation_lease_exists(
        &self,
        id: MaterializationId,
        generation: u64,
        now_unix_seconds: u64,
    ) -> Result<bool, GenerationError> {
        let leases = self.storage_root.join("refs").join("leases");
        let entries = match std::fs::read_dir(&leases) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name();
            let name = name
                .to_str()
                .ok_or_else(|| GenerationError::Corrupt("non-UTF-8 lease path".to_owned()))?;
            let Some(lease_id) = name.strip_prefix("materialization-") else {
                continue;
            };
            if !is_hex(lease_id, 32) {
                return Err(GenerationError::Corrupt(
                    "materialization lease path is not canonical".to_owned(),
                ));
            }
            let bytes =
                read_bounded(&entry.path(), MAX_LEASE_BYTES)?.ok_or(GenerationError::NotFound)?;
            let lease: GenerationLease = serde_json::from_slice(&bytes)
                .map_err(|error| GenerationError::Corrupt(error.to_string()))?;
            verify_lease(&lease)?;
            if lease.lease_id != lease_id {
                return Err(GenerationError::Corrupt(
                    "materialization lease path ID mismatch".to_owned(),
                ));
            }
            if lease.materialization_id == id.hex()
                && lease.generation == generation
                && lease.expires_unix_seconds > now_unix_seconds
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(crate) fn remove_generation(
        &self,
        id: MaterializationId,
        generation: u64,
        expected: &GenerationSnapshot,
    ) -> Result<(), GenerationError> {
        let observed = self.generation_snapshot(id, generation)?;
        if &observed != expected {
            return Err(GenerationError::Collision(
                "generation changed during retirement grace period".to_owned(),
            ));
        }
        let path = self.generation_path(id, generation);
        remove_owned_tree(&path)?;
        let parent = path.parent().ok_or_else(|| {
            GenerationError::Invalid("generation directory has no owner".to_owned())
        })?;
        sync_dir(parent)
    }

    pub(crate) fn acquire_lease(
        &self,
        key: &MaterializationKey,
        selection: &GenerationSelection,
        owner: &str,
        session_id: &str,
        now_unix_seconds: u64,
        expires_unix_seconds: u64,
    ) -> Result<GenerationLease, GenerationError> {
        validate_lease_text(owner)?;
        validate_lease_text(session_id)?;
        if expires_unix_seconds <= now_unix_seconds {
            return Err(GenerationError::Invalid(
                "lease expiry must follow acquisition".to_owned(),
            ));
        }
        let id = key.id()?;
        let mut lease_preimage = Vec::new();
        lease_preimage.extend_from_slice(LEASE_ID_DOMAIN);
        lease_preimage.extend_from_slice(id.as_bytes());
        lease_preimage.extend_from_slice(&selection.manifest.generation.to_be_bytes());
        lease_preimage.extend_from_slice(&selection.manifest.fence.to_be_bytes());
        push_bounded_string(&mut lease_preimage, owner)?;
        push_bounded_string(&mut lease_preimage, session_id)?;
        let lease_id = hex(&sha256(&lease_preimage));
        let lease = seal_lease(GenerationLease {
            schema: "layerstack-materialization-lease-v1".to_owned(),
            schema_version: 1,
            lease_id,
            materialization_id: id.hex(),
            generation: selection.manifest.generation,
            fence: selection.manifest.fence,
            owner: owner.to_owned(),
            session_id: session_id.to_owned(),
            acquired_unix_seconds: now_unix_seconds,
            renewed_unix_seconds: now_unix_seconds,
            expires_unix_seconds,
            checksum_sha256: String::new(),
        })?;
        let bytes = serde_json::to_vec(&lease)
            .map_err(|error| GenerationError::Invalid(error.to_string()))?;
        if bytes.len() as u64 > MAX_LEASE_BYTES {
            return Err(GenerationError::Invalid("lease exceeds bound".to_owned()));
        }
        let path = self.lease_path(&lease.lease_id);
        ensure_directory(&self.storage_root, path.parent().expect("lease parent"))?;
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                file.write_all(&bytes)?;
                file.sync_all()?;
                sync_common_parent(&path)
                    .map_err(|error| GenerationError::Io(error.to_string()))?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let existing =
                    read_bounded(&path, MAX_LEASE_BYTES)?.ok_or(GenerationError::NotFound)?;
                let existing: GenerationLease = serde_json::from_slice(&existing)
                    .map_err(|error| GenerationError::Corrupt(error.to_string()))?;
                verify_lease(&existing)?;
                if !same_lease_identity(&existing, &lease) {
                    return Err(GenerationError::Collision(
                        "lease ID maps to a different exact tuple".to_owned(),
                    ));
                }
                if now_unix_seconds < existing.renewed_unix_seconds {
                    return Err(GenerationError::Invalid(
                        "lease acquisition time moved backwards".to_owned(),
                    ));
                }
                let reacquired = now_unix_seconds >= existing.expires_unix_seconds;
                let renewed = seal_lease(GenerationLease {
                    acquired_unix_seconds: if reacquired {
                        now_unix_seconds
                    } else {
                        existing.acquired_unix_seconds
                    },
                    renewed_unix_seconds: now_unix_seconds,
                    expires_unix_seconds,
                    checksum_sha256: String::new(),
                    ..existing
                })?;
                replace_lease(&path, &renewed)?;
                return Ok(renewed);
            }
            Err(error) => return Err(error.into()),
        }
        Ok(lease)
    }

    pub(crate) fn renew_lease(
        &self,
        lease: &GenerationLease,
        now_unix_seconds: u64,
        expires_unix_seconds: u64,
    ) -> Result<GenerationLease, GenerationError> {
        verify_lease(lease)?;
        if expires_unix_seconds <= now_unix_seconds {
            return Err(GenerationError::Invalid(
                "lease expiry must follow renewal".to_owned(),
            ));
        }
        let path = self.lease_path(&lease.lease_id);
        let bytes = read_bounded(&path, MAX_LEASE_BYTES)?.ok_or(GenerationError::NotFound)?;
        let found: GenerationLease = serde_json::from_slice(&bytes)
            .map_err(|error| GenerationError::Corrupt(error.to_string()))?;
        verify_lease(&found)?;
        if !same_lease_identity(&found, lease)
            || found.acquired_unix_seconds != lease.acquired_unix_seconds
        {
            return Err(GenerationError::Collision(
                "lease identity changed before renewal".to_owned(),
            ));
        }
        if now_unix_seconds < found.renewed_unix_seconds
            || now_unix_seconds >= found.expires_unix_seconds
        {
            return Err(GenerationError::Invalid(
                "expired lease cannot be renewed".to_owned(),
            ));
        }
        let renewed = seal_lease(GenerationLease {
            renewed_unix_seconds: now_unix_seconds,
            expires_unix_seconds,
            checksum_sha256: String::new(),
            ..found
        })?;
        replace_lease(&path, &renewed)?;
        Ok(renewed)
    }

    pub(crate) fn release_lease(&self, lease: &GenerationLease) -> Result<bool, GenerationError> {
        verify_lease(lease)?;
        let path = self.lease_path(&lease.lease_id);
        let Some(bytes) = read_bounded(&path, MAX_LEASE_BYTES)? else {
            return Ok(false);
        };
        let found: GenerationLease = serde_json::from_slice(&bytes)
            .map_err(|error| GenerationError::Corrupt(error.to_string()))?;
        verify_lease(&found)?;
        if !same_lease_identity(&found, lease)
            || found.acquired_unix_seconds != lease.acquired_unix_seconds
        {
            return Err(GenerationError::Collision(
                "lease identity changed before release".to_owned(),
            ));
        }
        std::fs::remove_file(&path)?;
        sync_common_parent(&path).map_err(|error| GenerationError::Io(error.to_string()))?;
        Ok(true)
    }

    pub(crate) fn operation_work_carrier(
        &self,
        operation_id: &str,
    ) -> Result<PathBuf, GenerationError> {
        if !is_hex(operation_id, 32) {
            return Err(GenerationError::Invalid(
                "operation ID is not a SHA-256 hex value".to_owned(),
            ));
        }
        Ok(self
            .storage_root
            .join("operations")
            .join(operation_id)
            .join("work")
            .join("carrier"))
    }

    fn materialization_path(&self, id: MaterializationId) -> PathBuf {
        self.storage_root.join("materializations").join(id.hex())
    }

    fn generation_path(&self, id: MaterializationId, generation: u64) -> PathBuf {
        self.materialization_path(id)
            .join("generations")
            .join(format!("{generation:020}"))
    }

    fn manifest_path(&self, id: MaterializationId, generation: u64) -> PathBuf {
        self.generation_path(id, generation).join("manifest.json")
    }

    fn current_path(&self, id: MaterializationId) -> PathBuf {
        self.materialization_path(id).join("CURRENT")
    }

    fn lease_path(&self, lease_id: &str) -> PathBuf {
        self.storage_root
            .join("refs")
            .join("leases")
            .join(format!("materialization-{lease_id}"))
    }
}

fn replace_lease(path: &Path, lease: &GenerationLease) -> Result<(), GenerationError> {
    let bytes =
        serde_json::to_vec(lease).map_err(|error| GenerationError::Invalid(error.to_string()))?;
    if bytes.len() as u64 > MAX_LEASE_BYTES {
        return Err(GenerationError::Invalid("lease exceeds bound".to_owned()));
    }
    replace_common_state(path, &bytes).map_err(|error| GenerationError::Io(error.to_string()))
}

fn remove_owned_tree(path: &Path) -> Result<(), GenerationError> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir() {
        std::fs::remove_file(path)?;
        return Ok(());
    }
    make_directory_removable(path, &metadata)?;
    for entry in std::fs::read_dir(path)? {
        remove_owned_tree(&entry?.path())?;
    }
    std::fs::remove_dir(path)?;
    Ok(())
}

#[cfg(unix)]
fn make_directory_removable(
    path: &Path,
    metadata: &std::fs::Metadata,
) -> Result<(), GenerationError> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = metadata.permissions();
    let mode = permissions.mode();
    if mode & 0o700 != 0o700 {
        permissions.set_mode(mode | 0o700);
        std::fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn make_directory_removable(
    path: &Path,
    metadata: &std::fs::Metadata,
) -> Result<(), GenerationError> {
    let mut permissions = metadata.permissions();
    if permissions.readonly() {
        permissions.set_readonly(false);
        std::fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

fn same_lease_identity(left: &GenerationLease, right: &GenerationLease) -> bool {
    left.lease_id == right.lease_id
        && left.materialization_id == right.materialization_id
        && left.generation == right.generation
        && left.fence == right.fence
        && left.owner == right.owner
        && left.session_id == right.session_id
}

fn seal_current(mut current: CurrentGeneration) -> Result<CurrentGeneration, GenerationError> {
    current.checksum_sha256 = checksum_json(CURRENT_CHECKSUM_DOMAIN, &current)?;
    Ok(current)
}

fn verify_current(
    current: &CurrentGeneration,
    id: MaterializationId,
) -> Result<(), GenerationError> {
    if current.schema != "layerstack-materialization-current-v1"
        || current.schema_version != 1
        || current.materialization_id != id.hex()
        || current.generation == 0
        || current.fence == 0
        || !is_hex(&current.manifest_sha256, 32)
    {
        return Err(GenerationError::Corrupt("CURRENT framing".to_owned()));
    }
    let expected = checksum_json(CURRENT_CHECKSUM_DOMAIN, current)?;
    if current.checksum_sha256 != expected {
        return Err(GenerationError::Corrupt("CURRENT checksum".to_owned()));
    }
    Ok(())
}

fn seal_lease(mut lease: GenerationLease) -> Result<GenerationLease, GenerationError> {
    lease.checksum_sha256 = checksum_json(LEASE_CHECKSUM_DOMAIN, &lease)?;
    Ok(lease)
}

fn verify_lease(lease: &GenerationLease) -> Result<(), GenerationError> {
    if lease.schema != "layerstack-materialization-lease-v1"
        || lease.schema_version != 1
        || !is_hex(&lease.lease_id, 32)
        || !is_hex(&lease.materialization_id, 32)
        || lease.generation == 0
        || lease.fence == 0
        || lease.expires_unix_seconds <= lease.acquired_unix_seconds
        || lease.renewed_unix_seconds < lease.acquired_unix_seconds
        || lease.renewed_unix_seconds >= lease.expires_unix_seconds
    {
        return Err(GenerationError::Corrupt("lease framing".to_owned()));
    }
    validate_lease_text(&lease.owner)?;
    validate_lease_text(&lease.session_id)?;
    let expected = checksum_json(LEASE_CHECKSUM_DOMAIN, lease)?;
    if lease.checksum_sha256 != expected {
        return Err(GenerationError::Corrupt("lease checksum".to_owned()));
    }
    Ok(())
}

fn checksum_json<T: Serialize>(domain: &[u8], value: &T) -> Result<String, GenerationError> {
    let mut object =
        serde_json::to_value(value).map_err(|error| GenerationError::Invalid(error.to_string()))?;
    let map = object
        .as_object_mut()
        .ok_or_else(|| GenerationError::Invalid("checksummed value is not an object".to_owned()))?;
    map.insert(
        "checksum_sha256".to_owned(),
        serde_json::Value::String(String::new()),
    );
    let bytes =
        serde_json::to_vec(&object).map_err(|error| GenerationError::Invalid(error.to_string()))?;
    let mut preimage = Vec::with_capacity(domain.len() + bytes.len());
    preimage.extend_from_slice(domain);
    preimage.extend_from_slice(&bytes);
    Ok(hex(&sha256(&preimage)))
}

fn write_immutable(path: &Path, bytes: &[u8]) -> Result<(), GenerationError> {
    let parent = path
        .parent()
        .ok_or_else(|| GenerationError::Invalid("immutable file has no parent".to_owned()))?;
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => {
            file.write_all(bytes)?;
            file.sync_all()?;
            sync_dir(parent)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing =
                read_bounded(path, MAX_MANIFEST_BYTES)?.ok_or(GenerationError::NotFound)?;
            if existing != bytes {
                return Err(GenerationError::Collision(
                    "immutable manifest has different bytes".to_owned(),
                ));
            }
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn read_bounded(path: &Path, maximum: u64) -> Result<Option<Vec<u8>>, GenerationError> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !metadata.file_type().is_file() || metadata.len() > maximum {
        return Err(GenerationError::Corrupt(
            "bounded record is not a regular file".to_owned(),
        ));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    File::open(path)?
        .take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 != metadata.len() {
        return Err(GenerationError::Corrupt(
            "bounded record changed while reading".to_owned(),
        ));
    }
    Ok(Some(bytes))
}

fn ensure_directory(storage_root: &Path, target: &Path) -> Result<(), GenerationError> {
    if !target.starts_with(storage_root) {
        return Err(GenerationError::Invalid(
            "directory escapes storage root".to_owned(),
        ));
    }
    let relative = target
        .strip_prefix(storage_root)
        .map_err(|_| GenerationError::Invalid("directory escapes storage root".to_owned()))?;
    let mut current = storage_root.to_path_buf();
    for component in relative.components() {
        current.push(component);
        match std::fs::create_dir(&current) {
            Ok(()) => {
                let parent = current.parent().ok_or_else(|| {
                    GenerationError::Invalid("created directory has no parent".to_owned())
                })?;
                sync_dir(parent)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if !std::fs::symlink_metadata(&current)?.file_type().is_dir() {
                    return Err(GenerationError::Corrupt(
                        "storage component is not a directory".to_owned(),
                    ));
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn sync_dir(path: &Path) -> Result<(), GenerationError> {
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(windows)]
fn sync_dir(_path: &Path) -> Result<(), GenerationError> {
    Ok(())
}

fn push_bounded_string(output: &mut Vec<u8>, value: &str) -> Result<(), GenerationError> {
    if value.is_empty() || value.len() > 255 || value.as_bytes().contains(&0) {
        return Err(GenerationError::Invalid(
            "materialization key string is invalid".to_owned(),
        ));
    }
    let length = u16::try_from(value.len())
        .map_err(|_| GenerationError::Invalid("materialization key string".to_owned()))?;
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(value.as_bytes());
    Ok(())
}

fn validate_lease_text(value: &str) -> Result<(), GenerationError> {
    if value.is_empty() || value.len() > 256 || value.as_bytes().contains(&0) {
        return Err(GenerationError::Invalid("lease text field".to_owned()));
    }
    Ok(())
}

pub(crate) fn digest_string(value: Digest32) -> String {
    format!("sha256:{}", hex(value.as_bytes()))
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

fn is_hex(value: &str, bytes: usize) -> bool {
    value.len() == bytes.saturating_mul(2)
        && value
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}
