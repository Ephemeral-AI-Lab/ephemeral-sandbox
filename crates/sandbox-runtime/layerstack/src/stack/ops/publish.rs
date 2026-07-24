use std::io::ErrorKind;

use crate::error::LayerStackError;
use crate::fs::{
    allocate_layer_dirs, fsync_dir, fsync_tree_files, layer_digest_path, remove_path,
    write_layer_bytes, write_layer_digest, write_manifest,
};
use crate::model::{published_layer_bytes, try_layer_digest, LayerChange, LayerRef, Manifest};
use crate::stack::layer::write_layer_changes;
use crate::stack::publish::model::{PublishValidatedChangesRequest, PublishValidatedChangesResult};
use crate::stack::publish::{plan_publish, resolve_publish_changes};
use crate::stack::LayerStack;
use crate::{ACTIVE_MANIFEST_FILE, LAYERS_DIR, LAYER_METADATA_DIR};

const FAIL_NEXT_PUBLISH_MARKER_FILE: &str = "fail-next-publish";
const ENABLE_TEST_FAILPOINTS_ENV: &str = "SANDBOX_LAYERSTACK_ENABLE_TEST_FAILPOINTS";
const TEST_FAILPOINT_STAGE_ENV: &str = "SANDBOX_LAYERSTACK_TEST_FAILPOINT_STAGE";

impl LayerStack {
    pub fn publish_layer(&mut self, changes: &[LayerChange]) -> Result<Manifest, LayerStackError> {
        let mut observation = self.observation.begin_operation(true, true);
        let _guard = self.writer_lock.exclusive()?;
        let active = self.read_active_manifest_unlocked()?;
        Ok(self
            .publish_layer_unlocked(&active, changes, &mut observation)?
            .manifest)
    }

    pub fn publish_validated_changes(
        &mut self,
        request: PublishValidatedChangesRequest,
    ) -> Result<PublishValidatedChangesResult, LayerStackError> {
        let mut observation = self.observation.begin_operation(true, true);
        let plan = plan_publish(&self.view, &request)?;
        let _guard = self.writer_lock.exclusive()?;
        let active = self.read_active_manifest_unlocked()?;
        let resolved = resolve_publish_changes(&self.view, &active, &request, &plan)?;
        if resolved.changes.is_empty() {
            return Ok(PublishValidatedChangesResult {
                manifest: active,
                route_summary: plan.route_summary(),
                no_op: true,
                origin: Vec::new(),
            });
        }
        let outcome = self.publish_layer_unlocked(&active, &resolved.changes, &mut observation)?;
        let origin = if outcome.created {
            resolved.origin
        } else {
            Vec::new()
        };
        Ok(PublishValidatedChangesResult {
            manifest: outcome.manifest,
            route_summary: plan.route_summary(),
            no_op: !outcome.created,
            origin,
        })
    }

    pub(in crate::stack) fn publish_layer_unlocked(
        &self,
        active: &Manifest,
        changes: &[LayerChange],
        observation: &mut crate::stack::observation::StorageOperationGuard,
    ) -> Result<PublishLayerOutcome, LayerStackError> {
        let published_bytes = published_layer_bytes(changes);
        let digest = try_layer_digest(changes)?;
        observation.state().record_hashed(published_bytes);
        if self.head_layer_digest(active)? == Some(digest.clone()) {
            observation.state().record_reused(published_bytes);
            return Ok(PublishLayerOutcome {
                manifest: active.clone(),
                created: false,
            });
        }

        self.take_publish_failpoint_marker()?;
        observation.mark_staging();

        let next_version = active.version + 1;
        let (layer_id, staging_dir, layer_dir) =
            allocate_layer_dirs(&self.storage_root, 'L', next_version)?;
        std::fs::create_dir_all(&staging_dir)?;
        if let Err(err) = write_layer_changes(&staging_dir, changes)
            .and_then(|()| self.take_publish_failpoint_stage("staging_fsync"))
            .and_then(|()| fsync_tree_files(&staging_dir))
            .and_then(|()| fsync_dir(&staging_dir))
        {
            let _ = std::fs::remove_dir_all(&staging_dir);
            return Err(err);
        }

        if let Err(err) = self
            .take_publish_failpoint_stage("layer_rename")
            .and_then(|()| std::fs::rename(&staging_dir, &layer_dir).map_err(Into::into))
        {
            let _ = std::fs::remove_dir_all(&staging_dir);
            return Err(err.into());
        }
        if let Some(parent) = layer_dir.parent() {
            if let Err(err) = fsync_dir(parent) {
                let _ = remove_path(&layer_dir);
                return Err(err);
            }
        }

        if let Err(err) = self
            .take_publish_failpoint_stage("metadata")
            .and_then(|()| write_layer_digest(&self.storage_root, &layer_id, &digest))
        {
            let _ = remove_path(&layer_dir);
            return Err(err);
        }

        let latest = match self
            .take_publish_failpoint_stage("occ_reread")
            .and_then(|()| self.read_active_manifest_unlocked())
        {
            Ok(latest) => latest,
            Err(err) => {
                let _ = remove_path(&layer_dir);
                let _ = std::fs::remove_file(layer_digest_path(&self.storage_root, &layer_id));
                return Err(err);
            }
        };
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
        if let Err(err) = self
            .take_publish_failpoint_stage("manifest_replace")
            .and_then(|()| write_manifest(self.storage_root.join(ACTIVE_MANIFEST_FILE), &manifest))
        {
            let _ = remove_path(&layer_dir);
            let _ = std::fs::remove_file(layer_digest_path(&self.storage_root, &layer_id));
            return Err(err);
        }
        let _ = write_layer_bytes(&self.storage_root, &layer_id, published_bytes);
        observation.state().record_committed(published_bytes);
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

    fn take_publish_failpoint_stage(&self, stage: &str) -> Result<(), LayerStackError> {
        if std::env::var(ENABLE_TEST_FAILPOINTS_ENV).ok().as_deref() != Some("1") {
            return Ok(());
        }
        if std::env::var(TEST_FAILPOINT_STAGE_ENV).ok().as_deref() != Some(stage) {
            return Ok(());
        }
        Err(LayerStackError::Storage(format!(
            "injected layerstack publish failure at {stage}"
        )))
    }
}

pub(in crate::stack) struct PublishLayerOutcome {
    pub(in crate::stack) manifest: Manifest,
    pub(in crate::stack) created: bool,
}
