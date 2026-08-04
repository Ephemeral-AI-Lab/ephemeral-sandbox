use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::error::LayerStackError;
use crate::fs::{
    fsync_dir, next_unique, read_manifest, remove_path, write_layer_digest, write_manifest,
};
use crate::model::{Manifest, MANIFEST_SCHEMA_VERSION};
use crate::stack::LayerStack;
use crate::{ACTIVE_MANIFEST_FILE, LAYERS_DIR, STAGING_DIR};

use super::binding::{
    read_workspace_binding, validate_manifest_for_root, validate_workspace_binding_paths,
    write_workspace_binding_at, WorkspaceBinding,
};
use super::layer::{
    build_base_layer, build_shared_base_layer, hash_shared_base_layer, SHARED_BASE_DIR,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedWorkspaceBase {
    pub root_hash: String,
    pub cache_entry_root: PathBuf,
    pub base_mount_source: PathBuf,
    pub bytes: u64,
    pub built: bool,
}

pub fn ensure_workspace_base(
    layer_stack_root: impl AsRef<Path>,
    workspace_root: impl AsRef<Path>,
) -> Result<(WorkspaceBinding, bool), LayerStackError> {
    let stack = layer_stack_root.as_ref();
    let workspace = workspace_root.as_ref();
    if let Some(binding) = read_workspace_binding(stack)? {
        if Path::new(&binding.workspace_root) != workspace {
            return Err(LayerStackError::WorkspaceBinding(format!(
                "workspace binding points at a different workspace: {} != {}",
                binding.workspace_root,
                workspace.display()
            )));
        }
        if Path::new(&binding.layer_stack_root) != stack {
            return Err(LayerStackError::WorkspaceBinding(format!(
                "workspace binding points at a different layer stack: {} != {}",
                binding.layer_stack_root,
                stack.display()
            )));
        }
        validate_manifest_for_root(stack, &binding)?;
        return Ok((binding, false));
    }
    let built = build_workspace_base(stack, workspace, false)?;
    Ok((built, true))
}

pub fn build_workspace_base(
    layer_stack_root: impl AsRef<Path>,
    workspace_root: impl AsRef<Path>,
    reset: bool,
) -> Result<WorkspaceBinding, LayerStackError> {
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
) -> Result<WorkspaceBinding, LayerStackError> {
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

    let _stack_guard = LayerStack::open(stack.to_path_buf())?;
    reject_existing_base_state(stack)?;

    let base_layer = build_base_layer(stack, snapshot)?;
    write_layer_digest(stack, &base_layer.layer_ref.layer_id, &base_layer.root_hash)?;

    let manifest = Manifest::new(1, vec![base_layer.layer_ref], MANIFEST_SCHEMA_VERSION)
        .map_err(LayerStackError::from)?;
    write_manifest(stack.join(ACTIVE_MANIFEST_FILE), &manifest)?;

    let binding = WorkspaceBinding {
        workspace_root: binding_workspace.to_string_lossy().into_owned(),
        layer_stack_root: binding_stack.to_string_lossy().into_owned(),
        base_root_hash: base_layer.root_hash,
    };
    write_workspace_binding_at(stack, &binding)?;
    Ok(binding)
}

pub fn build_shared_workspace_base(
    cache_root: impl AsRef<Path>,
    workspace_root: impl AsRef<Path>,
) -> Result<SharedWorkspaceBase, LayerStackError> {
    let cache = cache_root.as_ref();
    let workspace = workspace_root.as_ref();
    if !cache.is_absolute() {
        return Err(LayerStackError::WorkspaceBinding(format!(
            "shared base cache root must be absolute: {}",
            cache.display()
        )));
    }
    if !workspace.is_dir() {
        return Err(LayerStackError::WorkspaceBinding(format!(
            "workspace_root does not exist: {}",
            workspace.display()
        )));
    }
    std::fs::create_dir_all(cache)?;
    let snapshot = hash_shared_base_layer(workspace)?;
    let final_root = cache.join(&snapshot.root_hash);
    if final_root.exists() {
        return validated_shared_base_cache_hit(
            final_root,
            &snapshot.root_hash,
            &snapshot.layer_ref.layer_id,
        );
    }
    let tmp = cache.join(format!(
        ".building-{}-{}",
        std::process::id(),
        next_unique()
    ));
    if tmp.exists() {
        remove_path(&tmp)?;
    }
    std::fs::create_dir_all(&tmp)?;
    let result = (|| {
        let base_layer = build_shared_base_layer(&tmp, workspace)?;
        let root_hash = base_layer.root_hash;
        let final_root = cache.join(&root_hash);
        if final_root.exists() {
            remove_path(&tmp)?;
            return validated_shared_base_cache_hit(
                final_root,
                &root_hash,
                &base_layer.layer_ref.layer_id,
            );
        }
        let _ = std::fs::remove_dir(tmp.join(STAGING_DIR));
        match std::fs::rename(&tmp, &final_root) {
            Ok(()) => {
                fsync_dir(cache)?;
                Ok(SharedWorkspaceBase {
                    root_hash,
                    cache_entry_root: final_root.clone(),
                    base_mount_source: final_root.join(SHARED_BASE_DIR),
                    bytes: base_layer.bytes,
                    built: true,
                })
            }
            Err(err) if err.kind() == ErrorKind::AlreadyExists => {
                remove_path(&tmp)?;
                validated_shared_base_cache_hit(
                    final_root,
                    &root_hash,
                    &base_layer.layer_ref.layer_id,
                )
            }
            Err(err) => Err(err.into()),
        }
    })();
    if result.is_err() {
        let _ = remove_path(&tmp);
    }
    result
}

fn validated_shared_base_cache_hit(
    cache_entry_root: PathBuf,
    expected_root_hash: &str,
    layer_id: &str,
) -> Result<SharedWorkspaceBase, LayerStackError> {
    let base_mount_source = cache_entry_root.join(SHARED_BASE_DIR);
    let layer = base_mount_source.join(layer_id);
    if !layer.is_dir() {
        return Err(LayerStackError::Storage(format!(
            "shared base cache entry is missing its layer directory: {}",
            layer.display()
        )));
    }
    let cached = hash_shared_base_layer(&layer).map_err(|error| {
        LayerStackError::Storage(format!(
            "shared base cache entry could not be validated without mutation: {}: {error}",
            layer.display()
        ))
    })?;
    if cached.root_hash != expected_root_hash {
        return Err(LayerStackError::Storage(format!(
            "shared base cache entry root hash mismatch: expected {expected_root_hash}, observed {} at {}",
            cached.root_hash,
            layer.display()
        )));
    }
    Ok(SharedWorkspaceBase {
        root_hash: expected_root_hash.to_owned(),
        cache_entry_root,
        base_mount_source,
        bytes: 0,
        built: false,
    })
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
