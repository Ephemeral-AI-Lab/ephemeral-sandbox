use std::fmt;
use std::path::{Path, PathBuf};

use sandbox_runtime_layerstack_core::{Digest32, RootId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::generation::{CapabilityProfile, MaterializationId, MaterializationKey};
use super::operation::{read_common_state, reap_common_work, replace_common_state};

const OPERATION_ID_DOMAIN: &[u8] = b"EOS-LS3-MATERIALIZATION-OPERATION\0";
const STATE_CHECKSUM_DOMAIN: &[u8] = b"EOS-LS3-MATERIALIZATION-OPERATION-STATE\0";

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MaterializationPhase {
    Owned,
    Building,
    CarrierSynced,
    GenerationAllocated,
    CarrierInstalled,
    ManifestDurable,
    CurrentDurable,
    TerminalBuilt,
    Failed,
    Cancelled,
}

impl MaterializationPhase {
    pub(crate) const fn is_terminal(self) -> bool {
        matches!(self, Self::TerminalBuilt | Self::Failed | Self::Cancelled)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MaterializationOperationState {
    pub(crate) schema: String,
    pub(crate) schema_version: u16,
    pub(crate) operation_id: String,
    pub(crate) materialization_id: String,
    pub(crate) root_id: String,
    pub(crate) backend_kind: String,
    pub(crate) backend_format_version: u16,
    pub(crate) target_profile: String,
    pub(crate) phase: MaterializationPhase,
    pub(crate) generation: Option<u64>,
    pub(crate) fence: Option<u64>,
    pub(crate) native_tree_sha256: Option<String>,
    pub(crate) entry_count: Option<u64>,
    pub(crate) logical_bytes: Option<u64>,
    pub(crate) allocated_bytes: Option<u64>,
    pub(crate) maximum_buffer_bytes: Option<u64>,
    pub(crate) required_capabilities: Option<CapabilityProfile>,
    pub(crate) provided_capabilities: Option<CapabilityProfile>,
    pub(crate) error_code: Option<String>,
    pub(crate) created_unix_seconds: u64,
    pub(crate) updated_unix_seconds: u64,
    pub(crate) checksum_sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MaterializationOperationBuild {
    pub(crate) native_tree_sha256: String,
    pub(crate) entry_count: u64,
    pub(crate) logical_bytes: u64,
    pub(crate) allocated_bytes: u64,
    pub(crate) maximum_buffer_bytes: u64,
    pub(crate) required_capabilities: CapabilityProfile,
    pub(crate) provided_capabilities: CapabilityProfile,
}

#[derive(Clone, Debug)]
pub(crate) struct MaterializationOperation {
    storage_root: PathBuf,
    state: MaterializationOperationState,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum MaterializationOperationError {
    Io(String),
    Invalid(String),
    Corrupt(String),
    Transition(String),
}

impl fmt::Display for MaterializationOperationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(message) => write!(formatter, "materialization operation I/O: {message}"),
            Self::Invalid(message) => {
                write!(formatter, "invalid materialization operation: {message}")
            }
            Self::Corrupt(message) => {
                write!(formatter, "corrupt materialization operation: {message}")
            }
            Self::Transition(message) => write!(
                formatter,
                "invalid materialization operation transition: {message}"
            ),
        }
    }
}

impl std::error::Error for MaterializationOperationError {}

pub(crate) fn recognizes_materialization_state(operation_path: &Path, bytes: &[u8]) -> bool {
    let Some(expected_operation_id) = operation_path.file_name().and_then(|name| name.to_str())
    else {
        return false;
    };
    let Ok(state) = serde_json::from_slice::<MaterializationOperationState>(bytes) else {
        return false;
    };
    let Some(root_digest) = parse_digest(&state.root_id) else {
        return false;
    };
    let key = MaterializationKey {
        root: RootId::new(Digest32::new(root_digest)),
        backend_kind: state.backend_kind.clone(),
        backend_format_version: state.backend_format_version,
        target_profile: state.target_profile.clone(),
    };
    let Ok(materialization_id) = key.id() else {
        return false;
    };
    verify_state(&state, &key, materialization_id, expected_operation_id).is_ok()
}

impl MaterializationOperation {
    pub(crate) fn load(
        storage_root: PathBuf,
        key: &MaterializationKey,
    ) -> Result<Option<Self>, MaterializationOperationError> {
        let materialization_id = key
            .id()
            .map_err(|error| MaterializationOperationError::Invalid(error.to_string()))?;
        let operation_id = operation_id(materialization_id);
        let state_path = state_path(&storage_root, &operation_id);
        let Some(bytes) = read_common_state(&state_path)
            .map_err(|error| MaterializationOperationError::Io(error.to_string()))?
        else {
            return Ok(None);
        };
        let state: MaterializationOperationState = serde_json::from_slice(&bytes)
            .map_err(|error| MaterializationOperationError::Corrupt(error.to_string()))?;
        verify_state(&state, key, materialization_id, &operation_id)?;
        Ok(Some(Self {
            storage_root,
            state,
        }))
    }

    pub(crate) fn open(
        storage_root: PathBuf,
        key: &MaterializationKey,
        now_unix_seconds: u64,
    ) -> Result<Self, MaterializationOperationError> {
        if let Some(operation) = Self::load(storage_root.clone(), key)? {
            return Ok(operation);
        }
        let materialization_id = key
            .id()
            .map_err(|error| MaterializationOperationError::Invalid(error.to_string()))?;
        let operation_id = operation_id(materialization_id);
        let state_path = state_path(&storage_root, &operation_id);
        let state = seal_state(MaterializationOperationState {
            schema: "layerstack-materialization-operation-v1".to_owned(),
            schema_version: 1,
            operation_id: operation_id.clone(),
            materialization_id: materialization_id.hex(),
            root_id: format!("sha256:{}", hex(key.root.digest().as_bytes())),
            backend_kind: key.backend_kind.clone(),
            backend_format_version: key.backend_format_version,
            target_profile: key.target_profile.clone(),
            phase: MaterializationPhase::Owned,
            generation: None,
            fence: None,
            native_tree_sha256: None,
            entry_count: None,
            logical_bytes: None,
            allocated_bytes: None,
            maximum_buffer_bytes: None,
            required_capabilities: None,
            provided_capabilities: None,
            error_code: None,
            created_unix_seconds: now_unix_seconds,
            updated_unix_seconds: now_unix_seconds,
            checksum_sha256: String::new(),
        })?;
        write_state(&state_path, &state)?;
        Ok(Self {
            storage_root,
            state,
        })
    }

    pub(crate) fn state(&self) -> &MaterializationOperationState {
        &self.state
    }

    pub(crate) fn operation_id(&self) -> &str {
        &self.state.operation_id
    }

    pub(crate) fn work_carrier(&self) -> PathBuf {
        operation_path(&self.storage_root, &self.state.operation_id)
            .join("work")
            .join("carrier")
    }

    pub(crate) fn transition(
        &mut self,
        phase: MaterializationPhase,
        generation: Option<(u64, u64)>,
        build: Option<MaterializationOperationBuild>,
        error_code: Option<String>,
        now_unix_seconds: u64,
    ) -> Result<(), MaterializationOperationError> {
        validate_transition(self.state.phase, phase)?;
        if let Some((generation, fence)) = generation {
            if generation == 0 || fence == 0 {
                return Err(MaterializationOperationError::Invalid(
                    "generation and fence must be nonzero".to_owned(),
                ));
            }
            if self
                .state
                .generation
                .zip(self.state.fence)
                .is_some_and(|existing| existing != (generation, fence))
            {
                return Err(MaterializationOperationError::Transition(
                    "generation tuple changed".to_owned(),
                ));
            }
            self.state.generation = Some(generation);
            self.state.fence = Some(fence);
        }
        if let Some(build) = build {
            let digest = build.native_tree_sha256;
            if !is_hex(&digest, 32) {
                return Err(MaterializationOperationError::Invalid(
                    "native tree digest is not lowercase SHA-256 hex".to_owned(),
                ));
            }
            if self
                .state
                .native_tree_sha256
                .as_ref()
                .is_some_and(|existing| existing != &digest)
            {
                return Err(MaterializationOperationError::Transition(
                    "native tree digest changed".to_owned(),
                ));
            }
            self.state.native_tree_sha256 = Some(digest);
            self.state.entry_count = Some(build.entry_count);
            self.state.logical_bytes = Some(build.logical_bytes);
            self.state.allocated_bytes = Some(build.allocated_bytes);
            self.state.maximum_buffer_bytes = Some(build.maximum_buffer_bytes);
            self.state.required_capabilities = Some(build.required_capabilities);
            self.state.provided_capabilities = Some(build.provided_capabilities);
        }
        if error_code
            .as_ref()
            .is_some_and(|value| value.is_empty() || value.len() > 128 || !value.is_ascii())
        {
            return Err(MaterializationOperationError::Invalid(
                "invalid operation error code".to_owned(),
            ));
        }
        self.state.phase = phase;
        self.state.error_code = error_code;
        self.state.updated_unix_seconds = now_unix_seconds;
        self.state = seal_state(self.state.clone())?;
        write_state(&self.state_path(), &self.state)
    }

    pub(crate) fn restart(
        &mut self,
        now_unix_seconds: u64,
    ) -> Result<(), MaterializationOperationError> {
        if !matches!(
            self.state.phase,
            MaterializationPhase::Failed | MaterializationPhase::Cancelled
        ) {
            return Err(MaterializationOperationError::Transition(
                "only failed or cancelled operations can restart".to_owned(),
            ));
        }
        self.reap_work()?;
        self.state.phase = MaterializationPhase::Owned;
        self.state.generation = None;
        self.state.fence = None;
        self.state.native_tree_sha256 = None;
        self.state.entry_count = None;
        self.state.logical_bytes = None;
        self.state.allocated_bytes = None;
        self.state.maximum_buffer_bytes = None;
        self.state.required_capabilities = None;
        self.state.provided_capabilities = None;
        self.state.error_code = None;
        self.state.updated_unix_seconds = now_unix_seconds;
        self.state = seal_state(self.state.clone())?;
        write_state(&self.state_path(), &self.state)
    }

    pub(crate) fn reap_work(&self) -> Result<bool, MaterializationOperationError> {
        reap_common_work(&operation_path(&self.storage_root, &self.state.operation_id).join("work"))
            .map_err(|error| MaterializationOperationError::Io(error.to_string()))
    }

    fn state_path(&self) -> PathBuf {
        state_path(&self.storage_root, &self.state.operation_id)
    }
}

fn validate_transition(
    current: MaterializationPhase,
    next: MaterializationPhase,
) -> Result<(), MaterializationOperationError> {
    let valid = current == next
        || matches!(
            (current, next),
            (MaterializationPhase::Owned, MaterializationPhase::Building)
                | (
                    MaterializationPhase::Building,
                    MaterializationPhase::CarrierSynced
                )
                | (
                    MaterializationPhase::CarrierSynced,
                    MaterializationPhase::GenerationAllocated
                )
                | (
                    MaterializationPhase::GenerationAllocated,
                    MaterializationPhase::CarrierInstalled
                )
                | (
                    MaterializationPhase::CarrierInstalled,
                    MaterializationPhase::ManifestDurable
                )
                | (
                    MaterializationPhase::ManifestDurable,
                    MaterializationPhase::CurrentDurable
                )
                | (
                    MaterializationPhase::CurrentDurable,
                    MaterializationPhase::TerminalBuilt
                )
                | (
                    MaterializationPhase::CarrierSynced,
                    MaterializationPhase::CurrentDurable
                )
        )
        || (!current.is_terminal()
            && matches!(
                next,
                MaterializationPhase::Failed | MaterializationPhase::Cancelled
            ));
    if valid {
        Ok(())
    } else {
        Err(MaterializationOperationError::Transition(format!(
            "{current:?} -> {next:?}"
        )))
    }
}

fn verify_state(
    state: &MaterializationOperationState,
    key: &MaterializationKey,
    materialization_id: MaterializationId,
    expected_operation_id: &str,
) -> Result<(), MaterializationOperationError> {
    if state.schema != "layerstack-materialization-operation-v1"
        || state.schema_version != 1
        || state.operation_id != expected_operation_id
        || state.materialization_id != materialization_id.hex()
        || state.root_id != format!("sha256:{}", hex(key.root.digest().as_bytes()))
        || state.backend_kind != key.backend_kind
        || state.backend_format_version != key.backend_format_version
        || state.target_profile != key.target_profile
        || state.generation.is_some() != state.fence.is_some()
        || state.generation == Some(0)
        || state.fence == Some(0)
        || state
            .native_tree_sha256
            .as_ref()
            .is_some_and(|value| !is_hex(value, 32))
        || [
            state.native_tree_sha256.is_some(),
            state.entry_count.is_some(),
            state.logical_bytes.is_some(),
            state.allocated_bytes.is_some(),
            state.maximum_buffer_bytes.is_some(),
            state.required_capabilities.is_some(),
            state.provided_capabilities.is_some(),
        ]
        .windows(2)
        .any(|values| values[0] != values[1])
    {
        return Err(MaterializationOperationError::Corrupt(
            "operation identity or framing".to_owned(),
        ));
    }
    let expected = checksum_state(state)?;
    if state.checksum_sha256 != expected {
        return Err(MaterializationOperationError::Corrupt(
            "operation state checksum".to_owned(),
        ));
    }
    Ok(())
}

fn seal_state(
    mut state: MaterializationOperationState,
) -> Result<MaterializationOperationState, MaterializationOperationError> {
    state.checksum_sha256 = checksum_state(&state)?;
    Ok(state)
}

fn checksum_state(
    state: &MaterializationOperationState,
) -> Result<String, MaterializationOperationError> {
    let mut value = serde_json::to_value(state)
        .map_err(|error| MaterializationOperationError::Invalid(error.to_string()))?;
    value
        .as_object_mut()
        .ok_or_else(|| {
            MaterializationOperationError::Invalid("operation state is not an object".to_owned())
        })?
        .insert(
            "checksum_sha256".to_owned(),
            serde_json::Value::String(String::new()),
        );
    let bytes = serde_json::to_vec(&value)
        .map_err(|error| MaterializationOperationError::Invalid(error.to_string()))?;
    let mut hasher = Sha256::new();
    hasher.update(STATE_CHECKSUM_DOMAIN);
    hasher.update(bytes);
    Ok(hex(&hasher.finalize()))
}

fn write_state(
    path: &Path,
    state: &MaterializationOperationState,
) -> Result<(), MaterializationOperationError> {
    let bytes = serde_json::to_vec(state)
        .map_err(|error| MaterializationOperationError::Invalid(error.to_string()))?;
    replace_common_state(path, &bytes)
        .map_err(|error| MaterializationOperationError::Io(error.to_string()))
}

fn operation_id(materialization_id: MaterializationId) -> String {
    let mut hasher = Sha256::new();
    hasher.update(OPERATION_ID_DOMAIN);
    hasher.update(materialization_id.as_bytes());
    hex(&hasher.finalize())
}

fn operation_path(storage_root: &Path, operation_id: &str) -> PathBuf {
    storage_root.join("operations").join(operation_id)
}

fn state_path(storage_root: &Path, operation_id: &str) -> PathBuf {
    operation_path(storage_root, operation_id).join("STATE")
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

fn parse_digest(value: &str) -> Option<[u8; 32]> {
    let value = value.strip_prefix("sha256:")?;
    if !is_hex(value, 32) {
        return None;
    }
    let mut digest = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        digest[index] = (hex_nibble(pair[0])? << 4) | hex_nibble(pair[1])?;
    }
    Some(digest)
}

const fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}
