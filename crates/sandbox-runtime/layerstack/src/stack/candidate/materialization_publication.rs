use std::sync::Arc;

use super::generation::{
    CurrentGenerationSubject, GenerationSelection, GenerationStore, MaterializationKey,
};
use super::materialization::MaterializationError;
use super::materialization_operation::{
    MaterializationOperation, MaterializationPhase, MaterializationPublicationSubject,
    PreparedMaterializationPublication, PreparedMaterializationTerminal,
};
use crate::lock::StorageWriterLockLease;

pub(crate) trait MaterializationGcBridge: Send + Sync {
    /// Validate replacement admission before the selector writer lock is taken.
    fn preflight_replacement(
        &self,
        old: Option<&GenerationSelection>,
        new: &GenerationSelection,
    ) -> Result<(), MaterializationError>;

    /// Durably admit the new logical root while the selector writer lock is held.
    ///
    /// Implementations must perform only bounded root-table work and must not
    /// wait on another lock, worker, traversal, verification, or network I/O.
    fn admit_new_root(
        &self,
        key: &MaterializationKey,
        new: &GenerationSelection,
    ) -> Result<(), MaterializationError>;

    /// Hand the exact old published generation to Stage 05 after selector commit.
    fn handoff_old_generation(
        &self,
        key: &MaterializationKey,
        old: &GenerationSelection,
        new: &GenerationSelection,
    ) -> Result<(), MaterializationError>;
}

#[derive(Debug, Default)]
pub(crate) struct DisabledMaterializationGcBridge;

impl MaterializationGcBridge for DisabledMaterializationGcBridge {
    fn preflight_replacement(
        &self,
        old: Option<&GenerationSelection>,
        new: &GenerationSelection,
    ) -> Result<(), MaterializationError> {
        if old.is_some_and(|old| !same_selection(old, new)) {
            Err(MaterializationError::BridgeUnavailable(
                "replacement publication requires the Stage 05 GC bridge".to_owned(),
            ))
        } else {
            Ok(())
        }
    }

    fn admit_new_root(
        &self,
        _key: &MaterializationKey,
        _new: &GenerationSelection,
    ) -> Result<(), MaterializationError> {
        Ok(())
    }

    fn handoff_old_generation(
        &self,
        _key: &MaterializationKey,
        old: &GenerationSelection,
        new: &GenerationSelection,
    ) -> Result<(), MaterializationError> {
        if same_selection(old, new) {
            Ok(())
        } else {
            Err(MaterializationError::BridgeUnavailable(
                "old-generation handoff requires the Stage 05 GC bridge".to_owned(),
            ))
        }
    }
}

pub(crate) struct MaterializationPublisher {
    generations: GenerationStore,
    bridge: Arc<dyn MaterializationGcBridge>,
}

impl MaterializationPublisher {
    pub(crate) fn new(
        generations: GenerationStore,
        bridge: Arc<dyn MaterializationGcBridge>,
    ) -> Self {
        Self {
            generations,
            bridge,
        }
    }

    pub(crate) fn publish(
        &self,
        key: &MaterializationKey,
        ready: &GenerationSelection,
        operation: &mut MaterializationOperation,
        writer_lock: &StorageWriterLockLease,
        now_unix_seconds: u64,
    ) -> Result<GenerationSelection, MaterializationError> {
        let (old, prepared_publication) =
            if operation.state().phase == MaterializationPhase::Published {
                (self.prepared_old(key, operation)?, None)
            } else {
                let (old, prepared) = self.prepare(key, ready, operation, now_unix_seconds)?;
                (old, Some(prepared))
            };
        self.publish_prepared(
            key,
            ready,
            old.as_ref(),
            prepared_publication,
            operation,
            writer_lock,
        )
        .map(|(selection, _)| selection)
    }

    pub(crate) fn prepare(
        &self,
        key: &MaterializationKey,
        ready: &GenerationSelection,
        operation: &mut MaterializationOperation,
        now_unix_seconds: u64,
    ) -> Result<
        (
            Option<GenerationSelection>,
            PreparedMaterializationPublication,
        ),
        MaterializationError,
    > {
        let old = match operation
            .state()
            .publication_old_subject
            .as_ref()
            .or(operation.state().prior_generation_hold.as_ref())
        {
            Some(subject) => {
                let selection = self.generations.read_generation(key, subject.generation)?;
                if publication_subject(&selection) != *subject {
                    return Err(MaterializationError::Operation(
                        "prepared publication old subject is corrupt".to_owned(),
                    ));
                }
                Some(selection)
            }
            None => None,
        };
        self.prepare_with_verified_old(ready, operation, old, now_unix_seconds)
    }

    pub(crate) fn prepare_with_verified_old(
        &self,
        ready: &GenerationSelection,
        operation: &mut MaterializationOperation,
        old: Option<GenerationSelection>,
        now_unix_seconds: u64,
    ) -> Result<
        (
            Option<GenerationSelection>,
            PreparedMaterializationPublication,
        ),
        MaterializationError,
    > {
        let new_subject = publication_subject(ready);
        if let Some(existing) = operation.state().publication_new_subject.as_ref() {
            if existing != &new_subject {
                return Err(MaterializationError::Operation(
                    "prepared publication new subject differs from ready generation".to_owned(),
                ));
            }
        }
        let expected_old_subject = operation
            .state()
            .publication_old_subject
            .as_ref()
            .or(operation.state().prior_generation_hold.as_ref());
        if expected_old_subject != old.as_ref().map(publication_subject).as_ref() {
            return Err(MaterializationError::Operation(
                "verified publication old subject differs from operation hold".to_owned(),
            ));
        }
        self.bridge.preflight_replacement(old.as_ref(), ready)?;
        let prepared_publication = operation.prepare_publication(
            old.as_ref().map(publication_subject),
            new_subject,
            now_unix_seconds,
        )?;
        Ok((old, prepared_publication))
    }

    pub(crate) fn publish_prepared(
        &self,
        key: &MaterializationKey,
        ready: &GenerationSelection,
        old: Option<&GenerationSelection>,
        prepared_publication: Option<PreparedMaterializationPublication>,
        operation: &mut MaterializationOperation,
        writer_lock: &StorageWriterLockLease,
    ) -> Result<(GenerationSelection, Option<PreparedMaterializationTerminal>), MaterializationError>
    {
        let new_subject = publication_subject(ready);
        if operation.state().publication_new_subject.as_ref() != Some(&new_subject) {
            return Err(MaterializationError::Operation(
                "prepared publication new subject differs from ready generation".to_owned(),
            ));
        }
        if operation.state().publication_old_subject.as_ref()
            != old.map(publication_subject).as_ref()
        {
            return Err(MaterializationError::Operation(
                "prepared publication old subject differs from its verified generation".to_owned(),
            ));
        }
        if operation.state().phase == MaterializationPhase::Published {
            let current = self.generations.lookup_current(key)?.ok_or_else(|| {
                MaterializationError::Coordination(
                    "published materialization has no CURRENT selector".to_owned(),
                )
            })?;
            if !same_selection(&current, ready) {
                return Err(MaterializationError::Coordination(
                    "published materialization CURRENT differs from prepared generation".to_owned(),
                ));
            }
            if let Some(old) = old {
                self.bridge.handoff_old_generation(key, old, &current)?;
            }
            return Ok((current, None));
        }
        if operation.state().phase != MaterializationPhase::Ready {
            return Err(MaterializationError::Operation(
                "materialization publication is not durably Ready".to_owned(),
            ));
        }
        let prepared_publication = prepared_publication.ok_or_else(|| {
            MaterializationError::Operation(
                "durably Ready publication omitted its prepared lifecycle witnesses".to_owned(),
            )
        })?;

        let (published, prepared_terminal) = {
            let _guard = writer_lock
                .exclusive()
                .map_err(|error| MaterializationError::Lock(error.to_string()))?;
            let current = self.generations.lookup_current_subject(key)?;
            let published = if current
                .as_ref()
                .is_some_and(|current| current.selects(ready))
            {
                ready.clone()
            } else {
                if !same_optional_current_subject(current.as_ref(), old) {
                    return Err(MaterializationError::Coordination(
                        "CURRENT changed after publication preflight".to_owned(),
                    ));
                }
                self.bridge.admit_new_root(key, ready)?;
                self.generations.promote_preverified_selection(key, ready)?;
                ready.clone()
            };
            // Published is the durable witness for the selector
            // linearization and must be recorded before releasing the writer
            // lock. Recovery repairs the exact Ready/CURRENT gap if this write
            // fails after the selector replacement.
            let prepared_terminal = operation.commit_prepared_publication(prepared_publication)?;
            (published, prepared_terminal)
        };

        if let Some(old) = old {
            self.bridge.handoff_old_generation(key, old, &published)?;
        }
        Ok((published, Some(prepared_terminal)))
    }

    fn prepared_old(
        &self,
        key: &MaterializationKey,
        operation: &MaterializationOperation,
    ) -> Result<Option<GenerationSelection>, MaterializationError> {
        match operation.state().publication_old_subject.as_ref() {
            Some(subject) => {
                let selection = self.generations.read_generation(key, subject.generation)?;
                if publication_subject(&selection) != *subject {
                    return Err(MaterializationError::Operation(
                        "prepared publication old subject is corrupt".to_owned(),
                    ));
                }
                Ok(Some(selection))
            }
            None => Ok(None),
        }
    }
}

pub(crate) fn publication_subject(
    selection: &GenerationSelection,
) -> MaterializationPublicationSubject {
    MaterializationPublicationSubject {
        generation: selection.manifest.generation,
        fence: selection.manifest.fence,
        manifest_sha256: selection.manifest_sha256.clone(),
    }
}

fn same_optional_current_subject(
    left: Option<&CurrentGenerationSubject>,
    right: Option<&GenerationSelection>,
) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => left.selects(right),
        (None, Some(_)) | (Some(_), None) => false,
    }
}

fn same_selection(left: &GenerationSelection, right: &GenerationSelection) -> bool {
    left.manifest.generation == right.manifest.generation
        && left.manifest.fence == right.manifest.fence
        && left.manifest_sha256 == right.manifest_sha256
}
