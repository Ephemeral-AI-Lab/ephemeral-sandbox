//! Checkpoint-based depth control for sandbox layer stacks.
//!
//! Squash is NON-DESTRUCTIVE until the retaining lease releases: it segments
//! the active manifest around the [`crate::lease::LeaseRegistry::lease_head_layers`]
//! barrier set, projects each foldable run into a single checkpoint layer, and
//! pointer-swaps a shorter manifest. Layers below a lease head stay on disk for
//! that lease's frozen reads (see the DUAL-SET note in [`crate::lease`]).
//! `// PORT backend/src/sandbox/layer_stack/squash.py`

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use eos_protocol::{LayerRef, Manifest, MANIFEST_SCHEMA_VERSION};

use crate::error::LayerStackError;
use crate::{MergedView, LAYERS_DIR, STAGING_DIR};

/// Leading discriminator for a built checkpoint layer id, distinguishing a
/// squash checkpoint (`B…`) from a normal published layer (`L…`).
///
/// The full id is `B{version:06}-{unique:08x}` (see `allocate_checkpoint_paths`),
/// mirroring Python's `B{next_version:06d}-{uuid4().hex[:8]}` SHAPE. The 8-hex
/// suffix is a process-unique counter here rather than a random UUID: the suffix
/// is non-deterministic on the Python side too, so cross-runtime byte-identity of
/// a squash id is impossible by construction — only the `B`-prefix + version +
/// uniqueness contract is load-bearing.
/// `// PORT backend/src/sandbox/layer_stack/squash.py:179-180 — _default_checkpoint_id`
pub const CHECKPOINT_ID_PREFIX: char = 'B';

/// A foldable run of >=2 contiguous layers that collapse into one checkpoint.
/// `// PORT backend/src/sandbox/layer_stack/squash.py:20-26 — CheckpointSegment`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointSegment {
    pub layers: Vec<LayerRef>,
}

impl CheckpointSegment {
    /// Construct a segment, enforcing the >=2-layer invariant.
    ///
    /// # Errors
    ///
    /// Returns [`LayerStackError::InvalidSquashPlan`] when fewer than two
    /// layers are provided.
    ///
    /// `// PORT backend/src/sandbox/layer_stack/squash.py:24-26 — __post_init__`
    pub fn new(layers: Vec<LayerRef>) -> Result<Self, LayerStackError> {
        if layers.len() <= 1 {
            return Err(LayerStackError::InvalidSquashPlan(
                "checkpoint segments must contain at least two layers".to_owned(),
            ));
        }
        Ok(Self { layers })
    }
}

/// One entry of a squash plan: either a kept single layer or a foldable segment.
/// `// PORT backend/src/sandbox/layer_stack/squash.py:29 — _SquashPlanEntry`
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SquashPlanEntry {
    /// A layer kept as-is (a lease-head barrier or a singleton run).
    Keep(LayerRef),
    /// A run of layers that fold into one checkpoint.
    Segment(CheckpointSegment),
}

/// A computed squash plan: the active manifest snapshot + the per-run entries.
///
/// Requires >=1 checkpoint segment (else there is nothing to fold).
/// `// PORT backend/src/sandbox/layer_stack/squash.py:32-48 — SquashPlan`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SquashPlan {
    pub active_version: i64,
    pub active_layers: Vec<LayerRef>,
    pub entries: Vec<SquashPlanEntry>,
}

impl SquashPlan {
    /// Construct + validate (non-empty active layers, non-empty entries, >=1
    /// checkpoint segment).
    ///
    /// # Errors
    ///
    /// Returns [`LayerStackError::InvalidSquashPlan`] when the plan has no
    /// active layers, no entries, or no foldable checkpoint segment.
    ///
    /// `// PORT backend/src/sandbox/layer_stack/squash.py:38-44 — __post_init__`
    pub fn new(
        active_version: i64,
        active_layers: Vec<LayerRef>,
        entries: Vec<SquashPlanEntry>,
    ) -> Result<Self, LayerStackError> {
        if active_layers.is_empty() {
            return Err(LayerStackError::InvalidSquashPlan(
                "active_layers must not be empty".to_owned(),
            ));
        }
        if entries.is_empty() {
            return Err(LayerStackError::InvalidSquashPlan(
                "entries must not be empty".to_owned(),
            ));
        }
        if !entries
            .iter()
            .any(|e| matches!(e, SquashPlanEntry::Segment(_)))
        {
            return Err(LayerStackError::InvalidSquashPlan(
                "squash plans must include at least one checkpoint segment".to_owned(),
            ));
        }
        Ok(Self {
            active_version,
            active_layers,
            entries,
        })
    }

    /// The checkpoint segments of this plan, in order.
    /// `// PORT backend/src/sandbox/layer_stack/squash.py:46-48 — checkpoint_segments`
    #[must_use]
    pub fn checkpoint_segments(&self) -> Vec<&CheckpointSegment> {
        self.entries
            .iter()
            .filter_map(|e| match e {
                SquashPlanEntry::Segment(s) => Some(s),
                SquashPlanEntry::Keep(_) => None,
            })
            .collect()
    }
}

/// Plans runs between lease heads and projects each run into a checkpoint layer.
/// `// PORT backend/src/sandbox/layer_stack/squash.py:51-59 — LayerCheckpointSquasher`
#[derive(Debug)]
pub struct LayerCheckpointSquasher {
    storage_root: PathBuf,
    view: MergedView,
}

impl LayerCheckpointSquasher {
    /// Bind a squasher to a storage root (owns its own [`crate::MergedView`]).
    /// `// PORT backend/src/sandbox/layer_stack/squash.py:54-59 — __init__`
    #[must_use]
    pub fn new(storage_root: PathBuf) -> Self {
        Self {
            view: MergedView::new(storage_root.clone()),
            storage_root,
        }
    }

    /// Compute a squash plan, or `None` when the manifest is already within
    /// `max_depth` or no run yields >= `min_reduction` folds. Segments around
    /// the `lease_head_layers` barrier set (those layers stay visible).
    ///
    /// # Errors
    ///
    /// Returns [`LayerStackError`] when the depth/reduction inputs are invalid
    /// or a candidate segment violates squash-plan invariants.
    ///
    /// `// PORT backend/src/sandbox/layer_stack/squash.py:61-93 — plan`
    pub fn plan(
        &self,
        active_manifest: &Manifest,
        max_depth: usize,
        lease_head_layers: &[LayerRef],
        min_reduction: usize,
    ) -> Result<Option<SquashPlan>, LayerStackError> {
        if max_depth == 0 {
            return Err(LayerStackError::InvalidSquashPlan(
                "max_depth must be positive".to_owned(),
            ));
        }
        if min_reduction == 0 {
            return Err(LayerStackError::InvalidSquashPlan(
                "min_reduction must be positive".to_owned(),
            ));
        }
        if active_manifest.layers.len() <= max_depth {
            return Ok(None);
        }

        let entries = segment_around_lease_heads(&active_manifest.layers, lease_head_layers)?;
        if entries.len() >= active_manifest.layers.len() {
            return Ok(None);
        }
        if active_manifest.layers.len() - entries.len() < min_reduction {
            return Ok(None);
        }
        let checkpoint_segments: Vec<&CheckpointSegment> = entries
            .iter()
            .filter_map(|entry| match entry {
                SquashPlanEntry::Segment(segment) => Some(segment),
                SquashPlanEntry::Keep(_) => None,
            })
            .collect();
        if entries.len() > max_depth
            && checkpoint_segments
                .iter()
                .all(|segment| segment.layers.len() <= max_depth)
        {
            return Ok(None);
        }

        SquashPlan::new(
            active_manifest.version,
            active_manifest.layers.clone(),
            entries,
        )
        .map(Some)
    }

    /// Project a segment's layers into a fresh checkpoint layer directory and
    /// return its `LayerRef` (id format `B{active_version+1:06}-{uuid8}`).
    ///
    /// # Errors
    ///
    /// Returns [`LayerStackError`] when checkpoint paths cannot be allocated,
    /// the segment cannot be projected, or the staging directory cannot be
    /// persisted as a layer.
    ///
    /// `// PORT backend/src/sandbox/layer_stack/squash.py:95-113 — build_checkpoint`
    pub fn build_checkpoint(
        &self,
        segment: &CheckpointSegment,
        active_version: i64,
    ) -> Result<LayerRef, LayerStackError> {
        let (layer_id, staging_dir, layer_dir) =
            self.allocate_checkpoint_paths(active_version + 1)?;
        let segment_manifest = Manifest::new(
            active_version,
            segment.layers.clone(),
            MANIFEST_SCHEMA_VERSION,
        )
        .map_err(LayerStackError::from)?;
        if let Err(err) = self.view.project(&staging_dir, &segment_manifest) {
            let _ = std::fs::remove_dir_all(&staging_dir);
            return Err(err);
        }
        if let Some(parent) = layer_dir.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if let Err(err) = std::fs::rename(&staging_dir, &layer_dir) {
            let _ = std::fs::remove_dir_all(&staging_dir);
            return Err(err.into());
        }
        Ok(LayerRef {
            layer_id: layer_id.clone(),
            path: format!("{LAYERS_DIR}/{layer_id}"),
        })
    }

    /// Rename a prebuilt checkpoint so its id matches the publishing manifest
    /// version (the `B{manifest_version:06}-…` prefix invariant).
    ///
    /// # Errors
    ///
    /// Returns [`LayerStackError`] when the checkpoint path is invalid, missing,
    /// or cannot be renamed into its final layer id.
    ///
    /// `// PORT backend/src/sandbox/layer_stack/squash.py:115-126 — relabel_checkpoint`
    pub fn relabel_checkpoint(
        &self,
        checkpoint: &LayerRef,
        manifest_version: i64,
    ) -> Result<LayerRef, LayerStackError> {
        let current = self.layer_path(checkpoint)?;
        if !current.exists() {
            return Err(LayerStackError::Storage(format!(
                "checkpoint layer is missing: {}",
                checkpoint.layer_id
            )));
        }
        let (layer_id, _staging_dir, layer_dir) =
            self.allocate_checkpoint_paths(manifest_version)?;
        if let Some(parent) = layer_dir.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(current, &layer_dir)?;
        // Persist the renamed checkpoint dir entry before the manifest that
        // publishes it is written, matching Python squash.py:125
        // `fsync_path(layer_dir.parent)` — else a crash can leave the fsynced
        // manifest pointing at a non-durable layer dir entry.
        if let Some(parent) = layer_dir.parent() {
            crate::stack::fsync_dir(parent)?;
        }
        Ok(LayerRef {
            layer_id: layer_id.clone(),
            path: format!("{LAYERS_DIR}/{layer_id}"),
        })
    }

    /// Best-effort removal of an uncommitted checkpoint (rollback path).
    ///
    /// # Errors
    ///
    /// Returns [`LayerStackError`] when the checkpoint path is invalid or
    /// removal fails for a reason other than the path already being absent.
    ///
    /// `// PORT backend/src/sandbox/layer_stack/squash.py:128-130 — discard_checkpoint`
    pub fn discard_checkpoint(&self, checkpoint: &LayerRef) -> Result<(), LayerStackError> {
        let path = self.layer_path(checkpoint)?;
        match std::fs::remove_dir_all(path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err.into()),
        }
    }

    fn allocate_checkpoint_paths(
        &self,
        next_version: i64,
    ) -> Result<(String, PathBuf, PathBuf), LayerStackError> {
        std::fs::create_dir_all(self.storage_root.join(LAYERS_DIR))?;
        std::fs::create_dir_all(self.storage_root.join(STAGING_DIR))?;
        for _ in 0..100 {
            let unique = NEXT_CHECKPOINT.fetch_add(1, Ordering::Relaxed);
            let layer_id = format!("{CHECKPOINT_ID_PREFIX}{next_version:06}-{unique:08x}");
            let staging_dir = self
                .storage_root
                .join(STAGING_DIR)
                .join(format!("{layer_id}.staging"));
            let layer_dir = self.storage_root.join(LAYERS_DIR).join(&layer_id);
            if !staging_dir.exists() && !layer_dir.exists() {
                return Ok((layer_id, staging_dir, layer_dir));
            }
        }
        Err(LayerStackError::LayerIdAllocation)
    }

    fn layer_path(&self, layer: &LayerRef) -> Result<PathBuf, LayerStackError> {
        if layer.path.is_empty() || layer.path.contains('\0') {
            return Err(LayerStackError::Manifest(
                "invalid checkpoint layer path".to_owned(),
            ));
        }
        let path = Path::new(&layer.path);
        if path.is_absolute() || path.components().any(|part| part.as_os_str() == "..") {
            return Err(LayerStackError::Manifest(format!(
                "invalid checkpoint layer path: {}",
                layer.path
            )));
        }
        Ok(self.storage_root.join(path))
    }
}

/// If the active manifest's tail still equals the plan's snapshotted active
/// layers, return the live prefix above them; else `None` (CAS lost).
///
/// `// PORT backend/src/sandbox/layer_stack/squash.py:167-176 — manifest_prefix_before_plan`
#[must_use]
pub fn manifest_prefix_before_plan<'m>(
    manifest: &'m Manifest,
    plan: &SquashPlan,
) -> Option<&'m [LayerRef]> {
    let planned_depth = plan.active_layers.len();
    if planned_depth > manifest.layers.len() {
        return None;
    }
    let split = manifest.layers.len() - planned_depth;
    if manifest.layers[split..] != plan.active_layers {
        return None;
    }
    Some(&manifest.layers[..split])
}

fn segment_around_lease_heads(
    layers: &[LayerRef],
    lease_head_layers: &[LayerRef],
) -> Result<Vec<SquashPlanEntry>, LayerStackError> {
    let mut entries = Vec::new();
    let mut run = Vec::new();
    for layer in layers {
        if lease_head_layers.contains(layer) {
            flush_run(&mut entries, &mut run)?;
            entries.push(SquashPlanEntry::Keep(layer.clone()));
        } else {
            run.push(layer.clone());
        }
    }
    flush_run(&mut entries, &mut run)?;
    Ok(entries)
}

fn flush_run(
    entries: &mut Vec<SquashPlanEntry>,
    run: &mut Vec<LayerRef>,
) -> Result<(), LayerStackError> {
    match run.len() {
        0 => {}
        1 => entries.push(SquashPlanEntry::Keep(run[0].clone())),
        _ => entries.push(SquashPlanEntry::Segment(CheckpointSegment::new(
            std::mem::take(run),
        )?)),
    }
    run.clear();
    Ok(())
}

static NEXT_CHECKPOINT: AtomicU64 = AtomicU64::new(0);
