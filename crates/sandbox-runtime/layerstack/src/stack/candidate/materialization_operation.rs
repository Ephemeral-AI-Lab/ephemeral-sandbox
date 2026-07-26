use std::fmt;
use std::path::{Path, PathBuf};

use crate::lock::{assert_writer_lock_allows, WriterLockForbiddenWork};
use sandbox_runtime_layerstack_core::{AttributionRootId, Digest32, RootId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::generation::{
    CapabilityProfile, GenerationStore, MaterializationId, MaterializationKey,
};
use super::operation::{
    prepare_common_states, read_common_state, reap_common_work,
    reap_common_work_before_state_replace, replace_common_state, PreparedCommonStateFile,
};

const OPERATION_ID_DOMAIN: &[u8] = b"EOS-LS3-MATERIALIZATION-OPERATION\0";
const STATE_CHECKSUM_DOMAIN: &[u8] = b"EOS-LS3-MATERIALIZATION-OPERATION-STATE\0";
const DEFAULT_OPERATION_SCOPE: &str = "materialize";
const MAX_OPERATION_STATE_BYTES: usize = 256 * 1024;
pub(crate) const MAX_ACTIVE_TYPED_HOLDS: usize = 4_096;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MaterializationPhase {
    Building,
    Ready,
    Published,
    Terminal,
}

impl MaterializationPhase {
    pub(crate) const fn is_terminal(self) -> bool {
        matches!(self, Self::Terminal)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MaterializationCheckpoint {
    Admitted,
    CarrierSynced,
    GenerationAllocated,
    CarrierInstalled,
    ManifestDurable,
    CurrentDurable,
    Complete,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MaterializationTerminalOutcome {
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MaterializationPublicationSubject {
    pub(crate) generation: u64,
    pub(crate) fence: u64,
    pub(crate) manifest_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MaterializationSourceHold {
    pub(crate) locator_id: String,
    pub(crate) carrier_id: String,
    pub(crate) locator_generation: u64,
    pub(crate) carrier_generation: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MaterializationOperationState {
    pub(crate) schema: String,
    pub(crate) schema_version: u16,
    pub(crate) operation_id: String,
    pub(crate) operation_scope: String,
    pub(crate) materialization_id: String,
    pub(crate) root_id: String,
    pub(crate) attribution_root_id: String,
    pub(crate) root_hold: String,
    pub(crate) source_holds: Vec<MaterializationSourceHold>,
    pub(crate) prior_generation_hold: Option<MaterializationPublicationSubject>,
    pub(crate) backend_kind: String,
    pub(crate) backend_format_version: u16,
    pub(crate) target_profile: String,
    pub(crate) phase: MaterializationPhase,
    pub(crate) checkpoint: MaterializationCheckpoint,
    pub(crate) terminal_outcome: Option<MaterializationTerminalOutcome>,
    pub(crate) generation: Option<u64>,
    pub(crate) fence: Option<u64>,
    pub(crate) publication_old_subject: Option<MaterializationPublicationSubject>,
    pub(crate) publication_new_subject: Option<MaterializationPublicationSubject>,
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

pub(crate) struct PreparedMaterializationPublication {
    source_checksum_sha256: String,
    state: MaterializationOperationState,
    prepared_state: PreparedCommonStateFile,
    prepared_terminal: PreparedMaterializationTerminal,
}

pub(crate) struct PreparedMaterializationTerminal {
    source_checksum_sha256: String,
    state: MaterializationOperationState,
    prepared_state: PreparedCommonStateFile,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum MaterializationOperationError {
    Io(String),
    Generation(String),
    Invalid(String),
    Corrupt(String),
    Transition(String),
}

impl fmt::Display for MaterializationOperationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(message) => write!(formatter, "materialization operation I/O: {message}"),
            Self::Generation(message) => {
                write!(formatter, "materialization generation failed: {message}")
            }
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
    let Ok(key) = key_from_state(&state) else {
        return false;
    };
    let Ok(materialization_id) = key.id() else {
        return false;
    };
    verify_state(&state, &key, materialization_id, expected_operation_id).is_ok()
}

impl MaterializationOperation {
    pub(crate) fn load_path(
        storage_root: PathBuf,
        path: &Path,
    ) -> Result<Self, MaterializationOperationError> {
        if path.parent() != Some(storage_root.join("operations").as_path()) {
            return Err(MaterializationOperationError::Invalid(
                "materialization operation path escaped the registry".to_owned(),
            ));
        }
        let operation_id = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                MaterializationOperationError::Invalid(
                    "materialization operation path is not UTF-8".to_owned(),
                )
            })?;
        let bytes = read_common_state(&path.join("STATE"))
            .map_err(|error| MaterializationOperationError::Io(error.to_string()))?
            .ok_or_else(|| {
                MaterializationOperationError::Invalid(
                    "materialization operation STATE is absent".to_owned(),
                )
            })?;
        let state: MaterializationOperationState = serde_json::from_slice(&bytes)
            .map_err(|error| MaterializationOperationError::Corrupt(error.to_string()))?;
        let key = key_from_state(&state)?;
        let materialization_id = key
            .id()
            .map_err(|error| MaterializationOperationError::Invalid(error.to_string()))?;
        verify_state(&state, &key, materialization_id, operation_id)?;
        Ok(Self {
            storage_root,
            state,
        })
    }

    pub(crate) fn load(
        storage_root: PathBuf,
        key: &MaterializationKey,
    ) -> Result<Option<Self>, MaterializationOperationError> {
        Self::load_scoped(storage_root, key, DEFAULT_OPERATION_SCOPE)
    }

    fn load_scoped(
        storage_root: PathBuf,
        key: &MaterializationKey,
        operation_scope: &str,
    ) -> Result<Option<Self>, MaterializationOperationError> {
        validate_operation_scope(operation_scope)?;
        let materialization_id = key
            .id()
            .map_err(|error| MaterializationOperationError::Invalid(error.to_string()))?;
        let operation_id = operation_id(materialization_id, operation_scope);
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
        Self::open_with_holds(storage_root, key, Vec::new(), None, now_unix_seconds)
    }

    pub(crate) fn open_with_holds(
        storage_root: PathBuf,
        key: &MaterializationKey,
        source_holds: Vec<MaterializationSourceHold>,
        prior_generation_hold: Option<MaterializationPublicationSubject>,
        now_unix_seconds: u64,
    ) -> Result<Self, MaterializationOperationError> {
        Self::open_scoped_with_holds(
            storage_root,
            key,
            DEFAULT_OPERATION_SCOPE,
            source_holds,
            prior_generation_hold,
            None,
            None,
            now_unix_seconds,
        )
    }

    pub(crate) fn open_squash_with_holds(
        storage_root: PathBuf,
        key: &MaterializationKey,
        generations: &GenerationStore,
        prior_generation_hold: MaterializationPublicationSubject,
        generation: (u64, u64),
        expected_build: MaterializationOperationBuild,
        now_unix_seconds: u64,
    ) -> Result<Self, MaterializationOperationError> {
        let operation_scope = format!("squash:{}", prior_generation_hold.manifest_sha256);
        Self::open_scoped_with_holds(
            storage_root,
            key,
            &operation_scope,
            Vec::new(),
            Some(prior_generation_hold),
            Some((generation, expected_build)),
            Some(generations),
            now_unix_seconds,
        )
    }

    fn open_scoped_with_holds(
        storage_root: PathBuf,
        key: &MaterializationKey,
        operation_scope: &str,
        source_holds: Vec<MaterializationSourceHold>,
        prior_generation_hold: Option<MaterializationPublicationSubject>,
        preallocated: Option<((u64, u64), MaterializationOperationBuild)>,
        generations: Option<&GenerationStore>,
        now_unix_seconds: u64,
    ) -> Result<Self, MaterializationOperationError> {
        validate_operation_scope(operation_scope)?;
        let root_hold = format!("sha256:{}", hex(key.root.digest().as_bytes()));
        validate_holds(&root_hold, &source_holds, prior_generation_hold.as_ref())?;
        if let Some(operation) = Self::load_scoped(storage_root.clone(), key, operation_scope)? {
            if operation.state.root_hold != root_hold
                || operation.state.source_holds != source_holds
                || operation.state.prior_generation_hold != prior_generation_hold
            {
                return Err(MaterializationOperationError::Transition(
                    "materialization hold set changed".to_owned(),
                ));
            }
            if let Some((generation, expected_build)) = preallocated.as_ref() {
                let has_build = operation.state.native_tree_sha256.is_some();
                if has_build
                    && (operation.state.generation.zip(operation.state.fence) != Some(*generation)
                        || !state_matches_expected_build(&operation.state, expected_build))
                {
                    return Err(MaterializationOperationError::Transition(
                        "preallocated squash generation or build expectation changed".to_owned(),
                    ));
                }
                if !has_build
                    && !(operation.state.phase == MaterializationPhase::Building
                        && operation.state.checkpoint == MaterializationCheckpoint::Admitted
                        && operation.state.generation.is_none())
                {
                    return Err(MaterializationOperationError::Transition(
                        "existing squash operation omitted its preallocated build truth".to_owned(),
                    ));
                }
            }
            return Ok(operation);
        }
        let materialization_id = key
            .id()
            .map_err(|error| MaterializationOperationError::Invalid(error.to_string()))?;
        if let Some(generations) = generations {
            let generation = preallocated
                .as_ref()
                .map(|((generation, _), _)| *generation)
                .ok_or_else(|| {
                    MaterializationOperationError::Invalid(
                        "generation guard requires a preallocated generation".to_owned(),
                    )
                })?;
            generations
                .ensure_generation_absent(materialization_id, generation)
                .map_err(|error| MaterializationOperationError::Generation(error.to_string()))?;
        }
        let operation_id = operation_id(materialization_id, operation_scope);
        let state_path = state_path(&storage_root, &operation_id);
        let (generation, fence, expected_build) = match preallocated {
            Some(((generation, fence), build)) => (Some(generation), Some(fence), Some(build)),
            None => (None, None, None),
        };
        let state = seal_state(MaterializationOperationState {
            schema: "layerstack-materialization-operation-v3".to_owned(),
            schema_version: 3,
            operation_id: operation_id.clone(),
            operation_scope: operation_scope.to_owned(),
            materialization_id: materialization_id.hex(),
            root_id: format!("sha256:{}", hex(key.root.digest().as_bytes())),
            attribution_root_id: format!(
                "sha256:{}",
                hex(key.attribution_root.digest().as_bytes())
            ),
            root_hold,
            source_holds,
            prior_generation_hold,
            backend_kind: key.backend_kind.clone(),
            backend_format_version: key.backend_format_version,
            target_profile: key.target_profile.clone(),
            phase: MaterializationPhase::Building,
            checkpoint: MaterializationCheckpoint::Admitted,
            terminal_outcome: None,
            generation,
            fence,
            publication_old_subject: None,
            publication_new_subject: None,
            native_tree_sha256: expected_build
                .as_ref()
                .map(|build| build.native_tree_sha256.clone()),
            entry_count: expected_build.as_ref().map(|build| build.entry_count),
            logical_bytes: expected_build.as_ref().map(|build| build.logical_bytes),
            allocated_bytes: expected_build.as_ref().map(|build| build.allocated_bytes),
            maximum_buffer_bytes: expected_build
                .as_ref()
                .map(|build| build.maximum_buffer_bytes),
            required_capabilities: expected_build
                .as_ref()
                .map(|build| build.required_capabilities.clone()),
            provided_capabilities: expected_build
                .as_ref()
                .map(|build| build.provided_capabilities.clone()),
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

    pub(crate) fn key(&self) -> Result<MaterializationKey, MaterializationOperationError> {
        key_from_state(&self.state)
    }

    pub(crate) fn operation_id(&self) -> &str {
        &self.state.operation_id
    }

    pub(crate) fn active_typed_hold_count(&self) -> usize {
        if self.state.phase.is_terminal() {
            0
        } else {
            hold_count(
                &self.state.source_holds,
                self.state.prior_generation_hold.as_ref(),
            )
        }
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
        self.apply_transition(phase, generation, build, error_code, now_unix_seconds)?;
        write_state(&self.state_path(), &self.state)
    }

    fn apply_transition(
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
        self.state.checkpoint = match phase {
            MaterializationPhase::Building => self.state.checkpoint,
            MaterializationPhase::Ready => MaterializationCheckpoint::ManifestDurable,
            MaterializationPhase::Published => MaterializationCheckpoint::CurrentDurable,
            MaterializationPhase::Terminal => MaterializationCheckpoint::Complete,
        };
        self.state.terminal_outcome = if phase == MaterializationPhase::Terminal {
            Some(match error_code.as_deref() {
                Some("cancelled") => MaterializationTerminalOutcome::Cancelled,
                Some(_) => MaterializationTerminalOutcome::Failed,
                None => MaterializationTerminalOutcome::Succeeded,
            })
        } else {
            None
        };
        self.state.error_code = error_code;
        self.state.updated_unix_seconds = now_unix_seconds;
        self.state = seal_state(self.state.clone())?;
        Ok(())
    }

    pub(crate) fn advance(
        &mut self,
        checkpoint: MaterializationCheckpoint,
        generation: Option<(u64, u64)>,
        build: Option<MaterializationOperationBuild>,
        now_unix_seconds: u64,
    ) -> Result<(), MaterializationOperationError> {
        if self.state.phase != MaterializationPhase::Building
            || checkpoint < self.state.checkpoint
            || checkpoint > MaterializationCheckpoint::ManifestDurable
        {
            return Err(MaterializationOperationError::Transition(format!(
                "{:?}/{:?} -> Building/{checkpoint:?}",
                self.state.phase, self.state.checkpoint
            )));
        }
        self.apply_transition(
            MaterializationPhase::Building,
            generation,
            build,
            None,
            now_unix_seconds,
        )?;
        self.state.checkpoint = checkpoint;
        self.state = seal_state(self.state.clone())?;
        write_state(&self.state_path(), &self.state)
    }

    pub(crate) fn has_preallocated_build(&self) -> bool {
        self.state.operation_scope.starts_with("squash:")
            && self.state.phase == MaterializationPhase::Building
            && self.state.checkpoint == MaterializationCheckpoint::Admitted
            && self.state.generation.is_some()
            && self.state.native_tree_sha256.is_some()
    }

    /// Accept a verified squash build against the expectation already stored
    /// in the initial durable STATE.
    ///
    /// This advances only the in-memory checkpoint. A crash before Ready
    /// reloads the durable Admitted expectation and either reconstructs again
    /// or verifies the already-installed immutable carrier.
    pub(crate) fn accept_preallocated_build(
        &mut self,
        build: MaterializationOperationBuild,
        now_unix_seconds: u64,
    ) -> Result<(), MaterializationOperationError> {
        if !self.has_preallocated_build() || !state_matches_expected_build(&self.state, &build) {
            return Err(MaterializationOperationError::Transition(
                "verified squash build differs from its durable expectation".to_owned(),
            ));
        }
        self.state.maximum_buffer_bytes = Some(build.maximum_buffer_bytes);
        self.state.checkpoint = MaterializationCheckpoint::GenerationAllocated;
        self.state.updated_unix_seconds = now_unix_seconds;
        self.state = seal_state(self.state.clone())?;
        Ok(())
    }

    pub(crate) fn prepare_publication(
        &mut self,
        old_subject: Option<MaterializationPublicationSubject>,
        new_subject: MaterializationPublicationSubject,
        now_unix_seconds: u64,
    ) -> Result<PreparedMaterializationPublication, MaterializationOperationError> {
        if !matches!(
            self.state.phase,
            MaterializationPhase::Building | MaterializationPhase::Ready
        ) {
            return Err(MaterializationOperationError::Transition(format!(
                "{:?} -> Ready/publication-prepared",
                self.state.phase
            )));
        }
        if old_subject != self.state.prior_generation_hold {
            return Err(MaterializationOperationError::Transition(
                "publication old subject differs from the durable prior-generation hold".to_owned(),
            ));
        }
        let tuple = (new_subject.generation, new_subject.fence);
        if self.state.generation.zip(self.state.fence) != Some(tuple)
            || !valid_publication_subject(&new_subject)
            || old_subject
                .as_ref()
                .is_some_and(|old| !valid_publication_subject(old) || old == &new_subject)
        {
            return Err(MaterializationOperationError::Invalid(
                "invalid materialization publication subjects".to_owned(),
            ));
        }
        match self.state.publication_new_subject.as_ref() {
            Some(existing)
                if existing == &new_subject
                    && self.state.publication_old_subject == old_subject
                    && self.state.phase == MaterializationPhase::Ready => {}
            Some(_) => {
                return Err(MaterializationOperationError::Transition(
                    "materialization publication subjects changed".to_owned(),
                ));
            }
            None => {}
        }
        let mut ready = self.clone();
        if ready.state.phase == MaterializationPhase::Building {
            if ready.state.checkpoint < MaterializationCheckpoint::GenerationAllocated
                || ready.state.native_tree_sha256.is_none()
            {
                return Err(MaterializationOperationError::Transition(
                    "publication cannot prepare before the verified build and generation tuple"
                        .to_owned(),
                ));
            }
            ready.apply_transition(
                MaterializationPhase::Ready,
                Some((new_subject.generation, new_subject.fence)),
                None,
                None,
                now_unix_seconds,
            )?;
        }
        ready.state.publication_old_subject = old_subject;
        ready.state.publication_new_subject = Some(new_subject);
        ready.state.updated_unix_seconds = now_unix_seconds;
        ready.state = seal_state(ready.state)?;

        let mut published = ready.clone();
        published.apply_transition(
            MaterializationPhase::Published,
            Some(tuple),
            None,
            None,
            now_unix_seconds,
        )?;
        let terminal_source_checksum = published.state.checksum_sha256.clone();
        let mut terminal = published.clone();
        terminal.apply_transition(
            MaterializationPhase::Terminal,
            Some(tuple),
            None,
            None,
            now_unix_seconds,
        )?;

        let ready_changed = ready.state.checksum_sha256 != self.state.checksum_sha256;
        let mut states = Vec::with_capacity(if ready_changed { 3 } else { 2 });
        if ready_changed {
            states.push((ready.state_path(), encode_state(&ready.state)?));
        }
        states.push((published.state_path(), encode_state(&published.state)?));
        states.push((terminal.state_path(), encode_state(&terminal.state)?));
        let mut prepared = prepare_common_states(&states)
            .map_err(|error| MaterializationOperationError::Io(error.to_string()))?;

        if ready_changed {
            prepared
                .remove(0)
                .replace_and_sync()
                .map_err(|error| MaterializationOperationError::Io(error.to_string()))?;
            self.state = ready.state;
        }
        let prepared_state = prepared.remove(0);
        let prepared_terminal_state = prepared.remove(0);
        Ok(PreparedMaterializationPublication {
            source_checksum_sha256: self.state.checksum_sha256.clone(),
            state: published.state,
            prepared_state,
            prepared_terminal: PreparedMaterializationTerminal {
                source_checksum_sha256: terminal_source_checksum,
                state: terminal.state,
                prepared_state: prepared_terminal_state,
            },
        })
    }

    pub(crate) fn commit_prepared_publication(
        &mut self,
        prepared: PreparedMaterializationPublication,
    ) -> Result<PreparedMaterializationTerminal, MaterializationOperationError> {
        if self.state.checksum_sha256 != prepared.source_checksum_sha256
            || self.state.phase != MaterializationPhase::Ready
        {
            return Err(MaterializationOperationError::Transition(
                "materialization operation changed after publication was prepared".to_owned(),
            ));
        }
        prepared
            .prepared_state
            .replace_and_sync()
            .map_err(|error| MaterializationOperationError::Io(error.to_string()))?;
        self.state = prepared.state;
        Ok(prepared.prepared_terminal)
    }

    pub(crate) fn commit_prepared_terminal(
        &mut self,
        prepared: PreparedMaterializationTerminal,
    ) -> Result<(), MaterializationOperationError> {
        if self.state.checksum_sha256 != prepared.source_checksum_sha256
            || self.state.phase != MaterializationPhase::Published
            || prepared.state.phase != MaterializationPhase::Terminal
            || prepared.state.terminal_outcome != Some(MaterializationTerminalOutcome::Succeeded)
        {
            return Err(MaterializationOperationError::Transition(
                "materialization operation changed after terminal state was prepared".to_owned(),
            ));
        }
        prepared
            .prepared_state
            .replace_and_sync()
            .map_err(|error| MaterializationOperationError::Io(error.to_string()))?;
        self.state = prepared.state;
        Ok(())
    }

    pub(crate) fn restart(
        &mut self,
        now_unix_seconds: u64,
    ) -> Result<(), MaterializationOperationError> {
        if self.state.phase != MaterializationPhase::Terminal
            || !matches!(
                self.state.terminal_outcome,
                Some(
                    MaterializationTerminalOutcome::Failed
                        | MaterializationTerminalOutcome::Cancelled
                )
            )
        {
            return Err(MaterializationOperationError::Transition(
                "only terminal failed or cancelled operations can restart".to_owned(),
            ));
        }
        self.reap_work()?;
        self.state.phase = MaterializationPhase::Building;
        self.state.checkpoint = MaterializationCheckpoint::Admitted;
        self.state.terminal_outcome = None;
        self.state.generation = None;
        self.state.fence = None;
        self.state.publication_old_subject = None;
        self.state.publication_new_subject = None;
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
        assert_writer_lock_allows(WriterLockForbiddenWork::Cleanup);
        reap_common_work(&operation_path(&self.storage_root, &self.state.operation_id).join("work"))
            .map_err(|error| MaterializationOperationError::Io(error.to_string()))
    }

    pub(crate) fn reap_work_before_terminal(&self) -> Result<bool, MaterializationOperationError> {
        assert_writer_lock_allows(WriterLockForbiddenWork::Cleanup);
        if self.state.phase != MaterializationPhase::Published {
            return Err(MaterializationOperationError::Transition(
                "work cleanup may be coalesced only with Published -> Terminal".to_owned(),
            ));
        }
        reap_common_work_before_state_replace(
            &operation_path(&self.storage_root, &self.state.operation_id).join("work"),
        )
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
            (MaterializationPhase::Building, MaterializationPhase::Ready)
                | (MaterializationPhase::Ready, MaterializationPhase::Published)
                | (
                    MaterializationPhase::Published,
                    MaterializationPhase::Terminal
                )
                | (
                    MaterializationPhase::Building,
                    MaterializationPhase::Terminal
                )
                | (MaterializationPhase::Ready, MaterializationPhase::Terminal)
        );
    if valid {
        Ok(())
    } else {
        Err(MaterializationOperationError::Transition(format!(
            "{current:?} -> {next:?}"
        )))
    }
}

fn state_matches_expected_build(
    state: &MaterializationOperationState,
    expected: &MaterializationOperationBuild,
) -> bool {
    state.native_tree_sha256.as_ref() == Some(&expected.native_tree_sha256)
        && state.entry_count == Some(expected.entry_count)
        && state.logical_bytes == Some(expected.logical_bytes)
        && state.allocated_bytes == Some(expected.allocated_bytes)
        && state.required_capabilities.as_ref() == Some(&expected.required_capabilities)
        && state.provided_capabilities.as_ref() == Some(&expected.provided_capabilities)
}

fn verify_state(
    state: &MaterializationOperationState,
    key: &MaterializationKey,
    materialization_id: MaterializationId,
    expected_operation_id: &str,
) -> Result<(), MaterializationOperationError> {
    if state.schema != "layerstack-materialization-operation-v3"
        || state.schema_version != 3
        || state.operation_id != expected_operation_id
        || validate_operation_scope(&state.operation_scope).is_err()
        || state.operation_id != operation_id(materialization_id, &state.operation_scope)
        || state.materialization_id != materialization_id.hex()
        || state.root_id != format!("sha256:{}", hex(key.root.digest().as_bytes()))
        || state.attribution_root_id
            != format!("sha256:{}", hex(key.attribution_root.digest().as_bytes()))
        || state.root_hold != state.root_id
        || state.backend_kind != key.backend_kind
        || state.backend_format_version != key.backend_format_version
        || state.target_profile != key.target_profile
        || state.generation.is_some() != state.fence.is_some()
        || state.generation == Some(0)
        || state.fence == Some(0)
        || state
            .publication_old_subject
            .as_ref()
            .is_some_and(|subject| !valid_publication_subject(subject))
        || state
            .publication_new_subject
            .as_ref()
            .is_some_and(|subject| {
                !valid_publication_subject(subject)
                    || Some((subject.generation, subject.fence))
                        != state.generation.zip(state.fence)
            })
        || state
            .publication_old_subject
            .as_ref()
            .zip(state.publication_new_subject.as_ref())
            .is_some_and(|(old, new)| old == new)
        || (state.phase == MaterializationPhase::Building
            && (state.publication_old_subject.is_some() || state.publication_new_subject.is_some()))
        || ((state.phase == MaterializationPhase::Published
            || (state.phase == MaterializationPhase::Terminal
                && state.terminal_outcome == Some(MaterializationTerminalOutcome::Succeeded)))
            && state.publication_new_subject.is_none())
        || state.phase.is_terminal() != state.terminal_outcome.is_some()
        || (!state.phase.is_terminal() && state.error_code.is_some())
        || (state.phase == MaterializationPhase::Ready
            && state.checkpoint < MaterializationCheckpoint::ManifestDurable)
        || (state.phase == MaterializationPhase::Published
            && state.checkpoint < MaterializationCheckpoint::CurrentDurable)
        || (state.phase == MaterializationPhase::Terminal
            && state.checkpoint != MaterializationCheckpoint::Complete)
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
    validate_holds(
        &state.root_hold,
        &state.source_holds,
        state.prior_generation_hold.as_ref(),
    )
    .map_err(|_| {
        MaterializationOperationError::Corrupt("operation hold set is invalid".to_owned())
    })?;
    if state.publication_old_subject.is_some()
        && state.publication_old_subject != state.prior_generation_hold
    {
        return Err(MaterializationOperationError::Corrupt(
            "publication old subject differs from prior-generation hold".to_owned(),
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

fn key_from_state(
    state: &MaterializationOperationState,
) -> Result<MaterializationKey, MaterializationOperationError> {
    let root_digest = parse_digest(&state.root_id).ok_or_else(|| {
        MaterializationOperationError::Corrupt(
            "materialization operation root ID is invalid".to_owned(),
        )
    })?;
    let attribution_root_digest = parse_digest(&state.attribution_root_id).ok_or_else(|| {
        MaterializationOperationError::Corrupt(
            "materialization operation attribution root ID is invalid".to_owned(),
        )
    })?;
    Ok(MaterializationKey {
        root: RootId::new(Digest32::new(root_digest)),
        attribution_root: AttributionRootId::new(Digest32::new(attribution_root_digest)),
        backend_kind: state.backend_kind.clone(),
        backend_format_version: state.backend_format_version,
        target_profile: state.target_profile.clone(),
    })
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
    let bytes = encode_state(state)?;
    replace_common_state(path, &bytes)
        .map_err(|error| MaterializationOperationError::Io(error.to_string()))
}

fn encode_state(
    state: &MaterializationOperationState,
) -> Result<Vec<u8>, MaterializationOperationError> {
    let bytes = serde_json::to_vec(state)
        .map_err(|error| MaterializationOperationError::Invalid(error.to_string()))?;
    if bytes.len() > MAX_OPERATION_STATE_BYTES {
        return Err(MaterializationOperationError::Invalid(
            "operation STATE exceeds 256 KiB".to_owned(),
        ));
    }
    Ok(bytes)
}

fn operation_id(materialization_id: MaterializationId, operation_scope: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(OPERATION_ID_DOMAIN);
    hasher.update(materialization_id.as_bytes());
    hasher.update((operation_scope.len() as u64).to_be_bytes());
    hasher.update(operation_scope.as_bytes());
    hex(&hasher.finalize())
}

fn validate_operation_scope(scope: &str) -> Result<(), MaterializationOperationError> {
    if scope == DEFAULT_OPERATION_SCOPE
        || scope
            .strip_prefix("squash:")
            .is_some_and(|digest| is_hex(digest, 32))
    {
        Ok(())
    } else {
        Err(MaterializationOperationError::Invalid(
            "operation scope is not a bounded materialization producer identity".to_owned(),
        ))
    }
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

fn valid_publication_subject(subject: &MaterializationPublicationSubject) -> bool {
    subject.generation != 0 && subject.fence != 0 && is_hex(&subject.manifest_sha256, 32)
}

fn hold_count(
    source_holds: &[MaterializationSourceHold],
    prior_generation_hold: Option<&MaterializationPublicationSubject>,
) -> usize {
    1_usize
        .saturating_add(source_holds.len())
        .saturating_add(usize::from(prior_generation_hold.is_some()))
}

fn validate_holds(
    root_hold: &str,
    source_holds: &[MaterializationSourceHold],
    prior_generation_hold: Option<&MaterializationPublicationSubject>,
) -> Result<(), MaterializationOperationError> {
    if parse_digest(root_hold).is_none() {
        return Err(MaterializationOperationError::Invalid(
            "root hold is not an exact RootId".to_owned(),
        ));
    }
    if hold_count(source_holds, prior_generation_hold) > MAX_ACTIVE_TYPED_HOLDS {
        return Err(MaterializationOperationError::Invalid(
            "active typed hold cap exceeded".to_owned(),
        ));
    }
    if source_holds.windows(2).any(|pair| pair[0] >= pair[1])
        || source_holds.iter().any(|hold| {
            parse_digest(&hold.locator_id).is_none()
                || parse_digest(&hold.carrier_id).is_none()
                || hold.locator_generation == 0
                || hold.carrier_generation == 0
        })
        || prior_generation_hold.is_some_and(|subject| !valid_publication_subject(subject))
    {
        return Err(MaterializationOperationError::Invalid(
            "source or prior-generation hold is invalid or non-canonical".to_owned(),
        ));
    }
    Ok(())
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
