//! Sessionless write/edit: `amend_path` runs the caller's transform against the
//! active head under the layerstack exclusive writer lock, publishes the
//! resulting layer, and records blame for `owner` — all atomic, no retry.

use std::sync::PoisonError;

use sandbox_runtime_layerstack::{LayerPath, LayerStack, ManifestFileRead};

use crate::layerstack::service::model::{AmendError, AmendOutcome};
use crate::layerstack::{LayerStackService, LayerStackServiceError};

impl LayerStackService {
    /// Atomic read-modify-write of `rel` on the active head. `transform` receives
    /// the classified current read (bounded by `max_bytes`) and returns the bytes
    /// to publish; the commit and its blame attribution to `owner` happen under
    /// the same writer lock as the read, so there is no conflict and no retry.
    ///
    /// # Errors
    /// [`AmendError::Transform`] when `transform` rejects the read;
    /// [`AmendError::LayerStack`] when the read or commit fails.
    pub fn amend_path<E>(
        &self,
        rel: &LayerPath,
        owner: &str,
        max_bytes: usize,
        transform: impl FnOnce(ManifestFileRead) -> Result<Vec<u8>, E>,
    ) -> Result<AmendOutcome, AmendError<E>> {
        let stack = LayerStack::open(self.layer_stack_root.clone()).map_err(|error| {
            AmendError::LayerStack(LayerStackServiceError::LayerStack {
                operation: "open",
                error,
            })
        })?;
        // Serialize the commit with the audit append so amend and publish commits
        // to one path append their audit events in commit order (§13).
        let _audit_gate = self
            .audit_gate
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let commit = stack
            .amend_path(rel, max_bytes, transform)
            .map_err(map_amend_error)?;
        if !commit.origin.is_empty() {
            self.file
                .record_layer_publish(owner, &commit.origin, &commit.changes);
        }
        Ok(AmendOutcome {
            existed_before: commit.existed_before,
            bytes_written: commit.bytes_written,
        })
    }
}

fn map_amend_error<E>(error: sandbox_runtime_layerstack::AmendError<E>) -> AmendError<E> {
    match error {
        sandbox_runtime_layerstack::AmendError::Transform(inner) => AmendError::Transform(inner),
        sandbox_runtime_layerstack::AmendError::LayerStack(error) => {
            AmendError::LayerStack(map_layerstack_error(error))
        }
    }
}

fn map_layerstack_error(
    error: sandbox_runtime_layerstack::LayerStackError,
) -> LayerStackServiceError {
    match error {
        sandbox_runtime_layerstack::LayerStackError::PublishRejected(rejection) => {
            LayerStackServiceError::PublishRejected { rejection }
        }
        error => LayerStackServiceError::LayerStack {
            operation: "amend",
            error,
        },
    }
}
