use std::io::ErrorKind;

use sandbox_observability::record::names;
use sandbox_observability::Observer;

use crate::error::LayerStackError;
use crate::fs::{
    allocate_layer_dirs, fsync_dir, fsync_tree_files, layer_digest_path, remove_path,
    write_layer_bytes, write_layer_digest, write_manifest,
};
use crate::model::{try_layer_digest, LayerChange, LayerRef, Manifest};
use crate::stack::layer::write_layer_changes;
use crate::stack::publish::model::{PublishValidatedChangesRequest, PublishValidatedChangesResult};
use crate::stack::publish::{plan_publish, validate_source_paths};
use crate::stack::LayerStack;
use crate::{ACTIVE_MANIFEST_FILE, LAYERS_DIR, LAYER_METADATA_DIR};

const FAIL_NEXT_PUBLISH_MARKER_FILE: &str = "fail-next-publish";
const ENABLE_TEST_FAILPOINTS_ENV: &str = "SANDBOX_LAYERSTACK_ENABLE_TEST_FAILPOINTS";

impl LayerStack {
    pub fn publish_layer(&mut self, changes: &[LayerChange]) -> Result<Manifest, LayerStackError> {
        let _guard = self.writer_lock.exclusive()?;
        let active = self.read_active_manifest_unlocked()?;
        Ok(self.publish_layer_unlocked(&active, changes)?.manifest)
    }

    pub fn publish_validated_changes(
        &mut self,
        request: PublishValidatedChangesRequest,
    ) -> Result<PublishValidatedChangesResult, LayerStackError> {
        let plan = plan_publish(&self.view, &request)?;
        let _guard = self.writer_lock.exclusive()?;
        let active = self.read_active_manifest_unlocked()?;
        validate_source_paths(&self.view, &active, &plan)?;
        let changes = plan.accepted_changes();
        if changes.is_empty() {
            return Ok(PublishValidatedChangesResult {
                manifest: active,
                route_summary: plan.route_summary(),
                no_op: true,
            });
        }
        let outcome = self.publish_layer_unlocked(&active, changes)?;
        Ok(PublishValidatedChangesResult {
            manifest: outcome.manifest,
            route_summary: plan.route_summary(),
            no_op: !outcome.created,
        })
    }

    /// `publish_validated_changes` wrapped in a `layerstack.publish` span. The
    /// span records the publish facts (`base`/`revision`/`layers_added`/`bytes`/
    /// `no_op`) and flips to `error` with `reason="manifest_conflict"` when the
    /// active manifest moved underneath the publish. The `Observer` is threaded in
    /// at this boundary; the lock-held domain internals stay obs-free.
    pub fn publish_validated_changes_traced(
        &mut self,
        request: PublishValidatedChangesRequest,
        obs: &Observer,
    ) -> Result<PublishValidatedChangesResult, LayerStackError> {
        let base = request.base.revision.manifest_version;
        let bytes = published_layer_bytes(&request.changes);
        obs.scope(names::LAYERSTACK_PUBLISH, |span| {
            span.attr("base", base).attr("bytes", bytes);
            let result = self.publish_validated_changes(request);
            match &result {
                Ok(published) => {
                    span.attr("revision", published.manifest.version)
                        .attr("no_op", published.no_op)
                        .attr("layers_added", if published.no_op { 0 } else { 1 });
                }
                Err(LayerStackError::ManifestConflict { .. }) => {
                    span.attr("reason", "manifest_conflict");
                }
                Err(_) => {}
            }
            result
        })
    }

    pub(in crate::stack) fn publish_layer_unlocked(
        &self,
        active: &Manifest,
        changes: &[LayerChange],
    ) -> Result<PublishLayerOutcome, LayerStackError> {
        let digest = try_layer_digest(changes)?;
        if self.head_layer_digest(active)? == Some(digest.clone()) {
            return Ok(PublishLayerOutcome {
                manifest: active.clone(),
                created: false,
            });
        }

        self.take_publish_failpoint_marker()?;

        let next_version = active.version + 1;
        let (layer_id, staging_dir, layer_dir) =
            allocate_layer_dirs(&self.storage_root, 'L', next_version)?;
        std::fs::create_dir_all(&staging_dir)?;
        if let Err(err) = write_layer_changes(&staging_dir, changes)
            .and_then(|()| fsync_tree_files(&staging_dir))
            .and_then(|()| fsync_dir(&staging_dir))
        {
            let _ = std::fs::remove_dir_all(&staging_dir);
            return Err(err);
        }

        if let Err(err) = std::fs::rename(&staging_dir, &layer_dir) {
            let _ = std::fs::remove_dir_all(&staging_dir);
            return Err(err.into());
        }
        if let Some(parent) = layer_dir.parent() {
            fsync_dir(parent)?;
        }

        if let Err(err) = write_layer_digest(&self.storage_root, &layer_id, &digest) {
            let _ = remove_path(&layer_dir);
            return Err(err);
        }

        let latest = self.read_active_manifest_unlocked()?;
        if latest != *active {
            let _ = remove_path(&layer_dir);
            let _ = std::fs::remove_file(layer_digest_path(&self.storage_root, &layer_id));
            return Err(LayerStackError::ManifestConflict {
                expected: active.version,
                found: latest.version,
            });
        }

        let mut layers = Vec::with_capacity(active.layers.len() + 1);
        layers.push(LayerRef {
            layer_id: layer_id.clone(),
            path: format!("{LAYERS_DIR}/{layer_id}"),
        });
        layers.extend(active.layers.clone());
        let manifest = Manifest::new(next_version, layers, active.schema_version)
            .map_err(LayerStackError::from)?;
        if let Err(err) = write_manifest(self.storage_root.join(ACTIVE_MANIFEST_FILE), &manifest) {
            let _ = remove_path(&layer_dir);
            let _ = std::fs::remove_file(layer_digest_path(&self.storage_root, &layer_id));
            return Err(err);
        }
        let _ = write_layer_bytes(
            &self.storage_root,
            &layer_id,
            published_layer_bytes(changes),
        );
        Ok(PublishLayerOutcome {
            manifest,
            created: true,
        })
    }

    fn head_layer_digest(&self, manifest: &Manifest) -> Result<Option<String>, LayerStackError> {
        let Some(head) = manifest.layers.first() else {
            return Ok(None);
        };
        let path = layer_digest_path(&self.storage_root, &head.layer_id);
        match std::fs::read_to_string(path) {
            Ok(value) => Ok(Some(value)),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    fn take_publish_failpoint_marker(&self) -> Result<(), LayerStackError> {
        if std::env::var(ENABLE_TEST_FAILPOINTS_ENV).ok().as_deref() != Some("1") {
            return Ok(());
        }
        let marker = self
            .storage_root
            .join(LAYER_METADATA_DIR)
            .join(FAIL_NEXT_PUBLISH_MARKER_FILE);
        match std::fs::remove_file(&marker) {
            Ok(()) => Err(LayerStackError::Storage(
                "injected layerstack publish failure".to_owned(),
            )),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err.into()),
        }
    }
}

fn published_layer_bytes(changes: &[LayerChange]) -> u64 {
    changes
        .iter()
        .map(|change| match change {
            LayerChange::Write { content, .. } => u64::try_from(content.len()).unwrap_or(u64::MAX),
            LayerChange::WriteFile { size, .. } => *size,
            LayerChange::Delete { .. }
            | LayerChange::Symlink { .. }
            | LayerChange::OpaqueDir { .. } => 0,
        })
        .fold(0_u64, u64::saturating_add)
}

pub(in crate::stack) struct PublishLayerOutcome {
    pub(in crate::stack) manifest: Manifest,
    pub(in crate::stack) created: bool,
}
