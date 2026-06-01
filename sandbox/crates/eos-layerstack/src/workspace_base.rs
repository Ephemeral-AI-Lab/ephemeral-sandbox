//! Workspace-base construction for an empty layer stack.
//!
//! `// PORT backend/src/sandbox/layer_stack/workspace_base.py`

use std::collections::BTreeMap;
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use eos_protocol::{LayerRef, Manifest, MANIFEST_SCHEMA_VERSION};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::error::LayerStackError;
use crate::stack::LayerStack;
use crate::workspace_binding::{read_workspace_binding, WorkspaceBinding, WORKSPACE_BINDING_FILE};
use crate::{ACTIVE_MANIFEST_FILE, LAYERS_DIR, LAYER_METADATA_DIR, STAGING_DIR};

/// The immutable base-layer id used by the Python implementation.
/// `// PORT backend/src/sandbox/layer_stack/workspace_base.py:31`
pub const WORKSPACE_BASE_LAYER_ID: &str = "B000001-base";

/// Build result: binding plus phase timings.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceBaseBuild {
    pub binding: WorkspaceBinding,
    pub timings: BTreeMap<String, f64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BaseEntry {
    Directory {
        path: String,
    },
    File {
        path: String,
        source_path: PathBuf,
        size: u64,
        content_hash: String,
    },
    Symlink {
        path: String,
        link_target: String,
    },
}

impl BaseEntry {
    fn path(&self) -> &str {
        match self {
            BaseEntry::Directory { path }
            | BaseEntry::File { path, .. }
            | BaseEntry::Symlink { path, .. } => path,
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            BaseEntry::Directory { .. } => "directory",
            BaseEntry::File { .. } => "file",
            BaseEntry::Symlink { .. } => "symlink",
        }
    }
}

/// Return the existing workspace base, or build it if the stack is unbound.
/// `// PORT backend/src/sandbox/daemon/layer_stack_runtime.py:119-134`
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

/// Build or rebuild the workspace base for one layer-stack root.
/// `// PORT backend/src/sandbox/layer_stack/workspace_base.py:82-141`
pub fn build_workspace_base(
    layer_stack_root: impl AsRef<Path>,
    workspace_root: impl AsRef<Path>,
    reset: bool,
) -> Result<WorkspaceBaseBuild, LayerStackError> {
    let workspace = workspace_root.as_ref();
    let stack = layer_stack_root.as_ref();
    validate_workspace_binding_paths(workspace, stack)?;
    if !workspace.is_dir() {
        return Err(LayerStackError::WorkspaceBinding(format!(
            "workspace_root does not exist: {}",
            workspace.display()
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
    let (entries, root_hash) = collect_base_entries(workspace)?;
    record_elapsed(&mut timings, "workspace_base.collect_s", collect_start);
    record_inventory(&mut timings, &entries);

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
        workspace_root: workspace.to_string_lossy().into_owned(),
        layer_stack_root: stack.to_string_lossy().into_owned(),
        active_manifest_version: manifest.version,
        active_root_hash: root_hash.clone(),
        base_manifest_version: manifest.version,
        base_root_hash: root_hash,
    };
    let write_binding_start = Instant::now();
    write_workspace_binding(&binding)?;
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

fn collect_base_entries(workspace: &Path) -> Result<(Vec<BaseEntry>, String), LayerStackError> {
    let mut entries = Vec::new();
    let mut special = Vec::new();
    let mut unstable = Vec::new();
    collect_dir(
        workspace,
        workspace,
        &mut entries,
        &mut special,
        &mut unstable,
    )?;
    if !special.is_empty() || !unstable.is_empty() {
        special.sort();
        unstable.sort();
        return Err(LayerStackError::Storage(format!(
            "workspace base must be a full copy; special={} [{}], unstable={} [{}]",
            special.len(),
            format_path_sample(&special),
            unstable.len(),
            format_path_sample(&unstable)
        )));
    }
    entries.sort_by(|left, right| left.path().cmp(right.path()));
    let mut digest = Sha256::new();
    for entry in &entries {
        update_root_hash(&mut digest, entry);
    }
    Ok((entries, hex_digest(digest.finalize())))
}

fn collect_dir(
    workspace: &Path,
    current: &Path,
    entries: &mut Vec<BaseEntry>,
    special: &mut Vec<String>,
    unstable: &mut Vec<String>,
) -> Result<(), LayerStackError> {
    let mut children = match std::fs::read_dir(current) {
        Ok(read_dir) => read_dir.collect::<Result<Vec<_>, _>>()?,
        Err(err) if err.kind() == ErrorKind::NotFound => {
            unstable.push(relative_path(workspace, current));
            return Ok(());
        }
        Err(err) => return Err(err.into()),
    };
    children.sort_by_key(|entry| entry.file_name());
    for child in children {
        let path = child.path();
        let rel = relative_path(workspace, &path);
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(err) if err.kind() == ErrorKind::NotFound => {
                unstable.push(rel);
                continue;
            }
            Err(err) => return Err(err.into()),
        };
        let file_type = meta.file_type();
        if file_type.is_symlink() {
            match symlink_entry(&path, rel) {
                Ok(entry) => entries.push(entry),
                Err(rejected) => special.push(rejected),
            }
        } else if meta.is_dir() {
            entries.push(BaseEntry::Directory { path: rel });
            collect_dir(workspace, &path, entries, special, unstable)?;
        } else if meta.is_file() {
            let content_hash = match file_hash(&path) {
                Ok(hash) => hash,
                Err(err) if err.kind() == ErrorKind::NotFound => {
                    unstable.push(rel);
                    continue;
                }
                Err(_) => {
                    special.push(rel);
                    continue;
                }
            };
            entries.push(BaseEntry::File {
                path: rel,
                source_path: path,
                size: meta.len(),
                content_hash,
            });
        } else {
            special.push(rel);
        }
    }
    Ok(())
}

fn symlink_entry(path: &Path, rel: String) -> Result<BaseEntry, String> {
    let target = std::fs::read_link(path).map_err(|_| rel.clone())?;
    let link_target = target.to_string_lossy().into_owned();
    Ok(BaseEntry::Symlink {
        path: rel,
        link_target,
    })
}

fn write_base_layer(stack: &Path, entries: &[BaseEntry]) -> Result<LayerRef, LayerStackError> {
    let layer_id = WORKSPACE_BASE_LAYER_ID;
    let layer_dir = stack.join(LAYERS_DIR).join(layer_id);
    let staging_dir = stack.join(STAGING_DIR).join(format!("{layer_id}.staging"));
    if layer_dir.exists() || staging_dir.exists() {
        return Err(LayerStackError::Storage(format!(
            "base layer already exists: {}",
            layer_dir.display()
        )));
    }
    std::fs::create_dir_all(&staging_dir)?;
    let result = (|| {
        for entry in entries {
            let target = join_layer_path(&staging_dir, entry.path());
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            match entry {
                BaseEntry::Directory { .. } => {
                    std::fs::create_dir_all(&target)?;
                }
                BaseEntry::File {
                    source_path,
                    content_hash,
                    path,
                    ..
                } => {
                    let current_hash = file_hash(source_path).map_err(|err| {
                        if err.kind() == ErrorKind::NotFound {
                            LayerStackError::Storage(format!(
                                "workspace base path changed while copying: {path}"
                            ))
                        } else {
                            err.into()
                        }
                    })?;
                    if &current_hash != content_hash {
                        return Err(LayerStackError::Storage(format!(
                            "workspace base path changed while copying: {path}"
                        )));
                    }
                    remove_path(&target)?;
                    std::fs::copy(source_path, &target)?;
                }
                BaseEntry::Symlink { link_target, .. } => {
                    remove_path(&target)?;
                    std::os::unix::fs::symlink(link_target, &target)?;
                }
            }
        }
        if let Some(parent) = layer_dir.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(&staging_dir, &layer_dir)?;
        Ok::<(), LayerStackError>(())
    })();
    if let Err(err) = result {
        let _ = remove_path(&staging_dir);
        let _ = remove_path(&layer_dir);
        return Err(err);
    }
    Ok(LayerRef {
        layer_id: layer_id.to_owned(),
        path: format!("{LAYERS_DIR}/{layer_id}"),
    })
}

fn validate_manifest_for_root(stack: &Path) -> Result<(), LayerStackError> {
    let manifest_file = stack.join(ACTIVE_MANIFEST_FILE);
    if !manifest_file.exists() {
        return Err(LayerStackError::WorkspaceBinding(format!(
            "active manifest is missing for workspace binding: {}",
            manifest_file.display()
        )));
    }
    let manifest = read_manifest(manifest_file)?;
    if manifest.version <= 0 || manifest.layers.is_empty() {
        return Err(LayerStackError::WorkspaceBinding(format!(
            "active manifest is empty for workspace binding: {}",
            stack.join(ACTIVE_MANIFEST_FILE).display()
        )));
    }
    Ok(())
}

fn validate_workspace_binding_paths(workspace: &Path, stack: &Path) -> Result<(), LayerStackError> {
    if !workspace.is_absolute() {
        return Err(LayerStackError::WorkspaceBinding(format!(
            "workspace_root must be absolute: {}",
            workspace.display()
        )));
    }
    if !stack.is_absolute() {
        return Err(LayerStackError::WorkspaceBinding(format!(
            "layer_stack_root must be absolute: {}",
            stack.display()
        )));
    }
    if stack == workspace || stack.starts_with(workspace) {
        return Err(LayerStackError::WorkspaceBinding(format!(
            "layer_stack_root must be outside workspace_root: {} is inside {}",
            stack.display(),
            workspace.display()
        )));
    }
    Ok(())
}

fn read_manifest(path: impl AsRef<Path>) -> Result<Manifest, LayerStackError> {
    let path = path.as_ref();
    if !path.exists() {
        return Manifest::new(0, Vec::new(), MANIFEST_SCHEMA_VERSION)
            .map_err(LayerStackError::from);
    }
    let payload = std::fs::read_to_string(path)?;
    let value: serde_json::Value =
        serde_json::from_str(&payload).map_err(|err| LayerStackError::Manifest(err.to_string()))?;
    let obj = value.as_object().ok_or_else(|| {
        LayerStackError::Manifest("manifest payload must be an object".to_owned())
    })?;
    let version = obj
        .get("version")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| {
            LayerStackError::Manifest("manifest payload missing required field: version".to_owned())
        })?;
    let schema_version = obj
        .get("schema_version")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(MANIFEST_SCHEMA_VERSION);
    let raw_layers = obj
        .get("layers")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            LayerStackError::Manifest("manifest payload missing required field: layers".to_owned())
        })?;
    let mut layers = Vec::with_capacity(raw_layers.len());
    for item in raw_layers {
        let item = item.as_object().ok_or_else(|| {
            LayerStackError::Manifest("manifest layer entries must be objects".to_owned())
        })?;
        layers.push(LayerRef {
            layer_id: item
                .get("layer_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            path: item
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        });
    }
    Manifest::new(version, layers, schema_version).map_err(LayerStackError::from)
}

fn write_manifest(path: impl AsRef<Path>, manifest: &Manifest) -> Result<(), LayerStackError> {
    let value = json!({
        "schema_version": manifest.schema_version,
        "version": manifest.version,
        "layers": manifest
            .layers
            .iter()
            .map(|layer| json!({"layer_id": &layer.layer_id, "path": &layer.path}))
            .collect::<Vec<_>>(),
    });
    let encoded = serde_json::to_vec_pretty(&value)
        .map_err(|err| LayerStackError::Manifest(err.to_string()))?;
    write_atomic(path, &encoded)
}

fn write_workspace_binding(binding: &WorkspaceBinding) -> Result<(), LayerStackError> {
    validate_workspace_binding_paths(
        Path::new(&binding.workspace_root),
        Path::new(&binding.layer_stack_root),
    )?;
    let encoded = serde_json::to_vec_pretty(binding)
        .map_err(|err| LayerStackError::WorkspaceBinding(err.to_string()))?;
    write_atomic(
        Path::new(&binding.layer_stack_root).join(WORKSPACE_BINDING_FILE),
        &encoded,
    )
}

fn write_layer_digest(stack: &Path, layer_id: &str, digest: &str) -> Result<(), LayerStackError> {
    write_atomic(
        stack
            .join(LAYER_METADATA_DIR)
            .join(format!("{layer_id}.digest")),
        digest.as_bytes(),
    )
}

fn file_hash(path: &Path) -> Result<String, std::io::Error> {
    let mut file = std::fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(hex_digest(digest.finalize()))
}

fn update_root_hash(digest: &mut Sha256, entry: &BaseEntry) {
    digest.update(entry.kind().as_bytes());
    digest.update(b"\0");
    digest.update(entry.path().as_bytes());
    digest.update(b"\0");
    match entry {
        BaseEntry::File {
            size, content_hash, ..
        } => {
            digest.update(size.to_string().as_bytes());
            digest.update(b"\0");
            digest.update(content_hash.as_bytes());
        }
        BaseEntry::Symlink { link_target, .. } => {
            digest.update(link_target.as_bytes());
        }
        BaseEntry::Directory { .. } => {}
    }
    digest.update(b"\0");
}

fn record_inventory(timings: &mut BTreeMap<String, f64>, entries: &[BaseEntry]) {
    timings.insert(
        "workspace_base.inventory.files".to_owned(),
        entries
            .iter()
            .filter(|entry| matches!(entry, BaseEntry::File { .. }))
            .count() as f64,
    );
    timings.insert(
        "workspace_base.inventory.dirs".to_owned(),
        entries
            .iter()
            .filter(|entry| matches!(entry, BaseEntry::Directory { .. }))
            .count() as f64,
    );
    timings.insert(
        "workspace_base.inventory.symlinks".to_owned(),
        entries
            .iter()
            .filter(|entry| matches!(entry, BaseEntry::Symlink { .. }))
            .count() as f64,
    );
    timings.insert(
        "workspace_base.inventory.bytes".to_owned(),
        entries
            .iter()
            .map(|entry| match entry {
                BaseEntry::File { size, .. } => *size,
                _ => 0,
            })
            .sum::<u64>() as f64,
    );
}

fn record_elapsed(timings: &mut BTreeMap<String, f64>, key: &str, start: Instant) {
    timings.insert(key.to_owned(), start.elapsed().as_secs_f64());
}

fn relative_path(workspace: &Path, path: &Path) -> String {
    path.strip_prefix(workspace)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn join_layer_path(root: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(root.to_path_buf(), |path, part| {
        if part.is_empty() {
            path
        } else {
            path.join(part)
        }
    })
}

fn dir_has_entries(path: &Path) -> Result<bool, LayerStackError> {
    match std::fs::read_dir(path) {
        Ok(mut entries) => Ok(entries.next().is_some()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err.into()),
    }
}

fn format_path_sample(paths: &[String]) -> String {
    const LIMIT: usize = 5;
    let mut sample = paths.iter().take(LIMIT).cloned().collect::<Vec<_>>();
    if paths.len() > LIMIT {
        sample.push(format!("+{} more", paths.len() - LIMIT));
    }
    sample.join(", ")
}

fn remove_path(path: &Path) -> Result<(), LayerStackError> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() || meta.is_file() => {
            std::fs::remove_file(path)?;
        }
        Ok(meta) if meta.is_dir() => {
            std::fs::remove_dir_all(path)?;
        }
        Ok(_) => {}
        Err(err) if err.kind() == ErrorKind::NotFound => {}
        Err(err) => return Err(err.into()),
    }
    Ok(())
}

fn write_atomic(path: impl AsRef<Path>, bytes: &[u8]) -> Result<(), LayerStackError> {
    static NEXT_TMP_WRITE: AtomicU64 = AtomicU64::new(0);
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_file_name(format!(
        ".{}.{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("layerstack"),
        std::process::id(),
        NEXT_TMP_WRITE.fetch_add(1, Ordering::Relaxed)
    ));
    if let Err(err) = std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, path)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(err.into());
    }
    Ok(())
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let bytes = bytes.as_ref();
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}
