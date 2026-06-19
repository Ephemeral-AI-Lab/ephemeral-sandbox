use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::Path;
use std::time::Instant;

use crate::error::LayerStackError;
use crate::fs::{read_manifest, record_elapsed, remove_path, write_layer_digest, write_manifest};
use crate::model::{Manifest, MANIFEST_SCHEMA_VERSION};
use crate::stack::LayerStack;
use crate::{ACTIVE_MANIFEST_FILE, LAYERS_DIR, STAGING_DIR};

use super::binding::{
    read_workspace_binding, validate_manifest_for_root, validate_workspace_binding_paths,
    write_workspace_binding_at, WorkspaceBinding,
};
use super::collect::collect_base_entries;
use super::layer::write_base_layer;

#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceBaseBuild {
    pub binding: WorkspaceBinding,
    pub timings: BTreeMap<String, f64>,
}

pub fn ensure_workspace_base(
    layer_stack_root: impl AsRef<Path>,
    workspace_root: impl AsRef<Path>,
) -> Result<(WorkspaceBinding, bool), LayerStackError> {
    let stack = layer_stack_root.as_ref();
    let workspace = workspace_root.as_ref();
    if let Some(binding) = read_workspace_binding(stack)? {
        validate_manifest_for_root(stack)?;
        if Path::new(&binding.workspace_root) != workspace {
            return Err(LayerStackError::WorkspaceBinding(format!(
                "workspace binding points at a different workspace: {} != {}",
                binding.workspace_root,
                workspace.display()
            )));
        }
        return Ok((binding, false));
    }
    let built = build_workspace_base(stack, workspace, false)?;
    Ok((built.binding, true))
}

pub fn build_workspace_base(
    layer_stack_root: impl AsRef<Path>,
    workspace_root: impl AsRef<Path>,
    reset: bool,
) -> Result<WorkspaceBaseBuild, LayerStackError> {
    build_workspace_base_from_snapshot(
        layer_stack_root.as_ref(),
        layer_stack_root.as_ref(),
        workspace_root.as_ref(),
        workspace_root.as_ref(),
        reset,
    )
}

fn build_workspace_base_from_snapshot(
    layer_stack_root: impl AsRef<Path>,
    binding_layer_stack_root: impl AsRef<Path>,
    binding_workspace_root: impl AsRef<Path>,
    snapshot_root: impl AsRef<Path>,
    reset: bool,
) -> Result<WorkspaceBaseBuild, LayerStackError> {
    let stack = layer_stack_root.as_ref();
    let binding_stack = binding_layer_stack_root.as_ref();
    let binding_workspace = binding_workspace_root.as_ref();
    let snapshot = snapshot_root.as_ref();
    validate_workspace_binding_paths(binding_workspace, binding_stack)?;
    if !stack.is_absolute() {
        return Err(LayerStackError::WorkspaceBinding(format!(
            "layer_stack_root must be absolute: {}",
            stack.display()
        )));
    }
    if !snapshot.is_dir() {
        return Err(LayerStackError::WorkspaceBinding(format!(
            "workspace_root does not exist: {}",
            snapshot.display()
        )));
    }

    if reset {
        remove_path(stack)?;
    }

    let mut timings = BTreeMap::new();
    let prepare_start = Instant::now();
    let _stack_guard = LayerStack::open(stack.to_path_buf())?;
    reject_existing_base_state(stack)?;
    record_elapsed(
        &mut timings,
        "workspace_base.prepare_stack_s",
        prepare_start,
    );

    let collect_start = Instant::now();
    let (entries, root_hash) = collect_base_entries(snapshot)?;
    record_elapsed(&mut timings, "workspace_base.collect_s", collect_start);

    let write_layer_start = Instant::now();
    let layer_ref = write_base_layer(stack, &entries)?;
    write_layer_digest(stack, &layer_ref.layer_id, &root_hash)?;
    record_elapsed(
        &mut timings,
        "workspace_base.write_layer_s",
        write_layer_start,
    );

    let manifest = Manifest::new(1, vec![layer_ref], MANIFEST_SCHEMA_VERSION)
        .map_err(LayerStackError::from)?;
    let write_manifest_start = Instant::now();
    write_manifest(stack.join(ACTIVE_MANIFEST_FILE), &manifest)?;
    record_elapsed(
        &mut timings,
        "workspace_base.write_manifest_s",
        write_manifest_start,
    );

    let binding = WorkspaceBinding {
        workspace_root: binding_workspace.to_string_lossy().into_owned(),
        layer_stack_root: binding_stack.to_string_lossy().into_owned(),
        active_manifest_version: manifest.version,
        active_root_hash: root_hash.clone(),
        base_manifest_version: manifest.version,
        base_root_hash: root_hash,
    };
    let write_binding_start = Instant::now();
    write_workspace_binding_at(stack, &binding)?;
    record_elapsed(
        &mut timings,
        "workspace_base.write_binding_s",
        write_binding_start,
    );
    Ok(WorkspaceBaseBuild { binding, timings })
}

fn reject_existing_base_state(stack: &Path) -> Result<(), LayerStackError> {
    if read_workspace_binding(stack)?.is_some() {
        return Err(LayerStackError::WorkspaceBinding(format!(
            "workspace base already exists at {}",
            stack.display()
        )));
    }
    let active = read_manifest(stack.join(ACTIVE_MANIFEST_FILE))?;
    if active.version != 0 || !active.layers.is_empty() {
        return Err(LayerStackError::Manifest(format!(
            "layer stack is not empty: manifest version {}",
            active.version
        )));
    }
    if dir_has_entries(&stack.join(LAYERS_DIR))? || dir_has_entries(&stack.join(STAGING_DIR))? {
        return Err(LayerStackError::Storage(format!(
            "layer stack has existing layer or staging state: {}",
            stack.display()
        )));
    }
    Ok(())
}

fn dir_has_entries(path: &Path) -> Result<bool, LayerStackError> {
    match std::fs::read_dir(path) {
        Ok(mut entries) => Ok(entries.next().is_some()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err.into()),
    }
}
