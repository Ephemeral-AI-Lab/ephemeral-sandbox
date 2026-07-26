//! Candidate materialization squash producer.
//!
//! Squash owns only private reconstruction and Ready validation. It deliberately
//! has no selector or writer-lock dependency; `MaterializationPublisher` is the
//! sole authority that can make the resulting generation visible.

mod flatten;

use super::generation::{
    CurrentGenerationSubject, GenerationSelection, GenerationStore, MaterializationKey,
};
use super::materialization::MaterializationError;
use super::materialization_operation::{
    MaterializationOperation, MaterializationOperationBuild, MaterializationPublicationSubject,
};
use super::materialization_publication::publication_subject;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub(crate) struct CandidateSquashProducer {
    prior: GenerationSelection,
}

impl CandidateSquashProducer {
    pub(crate) fn new(prior: GenerationSelection) -> Self {
        Self { prior }
    }

    pub(crate) fn prior(&self) -> &GenerationSelection {
        &self.prior
    }

    pub(crate) fn prior_subject(&self) -> MaterializationPublicationSubject {
        publication_subject(&self.prior)
    }

    pub(crate) fn open_operation(
        &self,
        storage_root: PathBuf,
        key: &MaterializationKey,
        generations: &GenerationStore,
        now_unix_seconds: u64,
    ) -> Result<MaterializationOperation, MaterializationError> {
        let generation = self
            .prior
            .manifest
            .generation
            .checked_add(1)
            .ok_or_else(|| {
                MaterializationError::Generation("generation counter exhausted".to_owned())
            })?;
        let fence = self.prior.manifest.fence.checked_add(1).ok_or_else(|| {
            MaterializationError::Generation("generation fence exhausted".to_owned())
        })?;
        let expected_build = MaterializationOperationBuild {
            native_tree_sha256: self.prior.manifest.native_tree_sha256.clone(),
            entry_count: self.prior.manifest.entry_count,
            logical_bytes: self.prior.manifest.logical_bytes,
            allocated_bytes: self.prior.manifest.allocated_bytes,
            maximum_buffer_bytes: 0,
            required_capabilities: self.prior.manifest.required_capabilities.clone(),
            provided_capabilities: self.prior.manifest.provided_capabilities.clone(),
        };
        Ok(MaterializationOperation::open_squash_with_holds(
            storage_root,
            key,
            generations,
            self.prior_subject(),
            (generation, fence),
            expected_build,
            now_unix_seconds,
        )?)
    }

    pub(crate) fn validate_current(
        &self,
        current: &CurrentGenerationSubject,
    ) -> Result<(), MaterializationError> {
        if current.selects(&self.prior) {
            Ok(())
        } else {
            Err(MaterializationError::Coordination(
                "squash source CURRENT changed before private build".to_owned(),
            ))
        }
    }

    pub(crate) fn validate_ready(
        &self,
        key: &MaterializationKey,
        ready: &GenerationSelection,
    ) -> Result<(), MaterializationError> {
        flatten::validate_identity_preservation(key, &self.prior, ready)
    }

    pub(crate) fn selected_by(
        &self,
        operation: &MaterializationOperation,
        current: &GenerationSelection,
    ) -> bool {
        operation
            .state()
            .publication_new_subject
            .as_ref()
            .is_some_and(|subject| *subject == publication_subject(current))
    }
}
