use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use crate::error::LayerStackError;
use crate::lock::STORAGE_WRITER_LOCK_FILE;
use crate::model::{LayerRef, Manifest, MANIFEST_SCHEMA_VERSION};
use crate::{LAYERS_DIR, LAYER_METADATA_DIR, STAGING_DIR};
use serde_json::{json, Value};

pub(crate) fn canonical_key(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

pub(crate) fn next_unique() -> u64 {
    NEXT_UNIQUE.fetch_add(1, Ordering::Relaxed)
}

pub(crate) fn allocate_layer_dirs(
    storage_root: &Path,
    prefix: char,
    next_version: i64,
) -> Result<(String, PathBuf, PathBuf), LayerStackError> {
    std::fs::create_dir_all(storage_root.join(LAYERS_DIR))?;
    std::fs::create_dir_all(storage_root.join(STAGING_DIR))?;
    for _ in 0..100 {
        let layer_id = format!("{prefix}{next_version:06}-{:08x}", next_unique());
        let staging_dir = storage_root
            .join(STAGING_DIR)
            .join(format!("{layer_id}.staging"));
        let layer_dir = storage_root.join(LAYERS_DIR).join(&layer_id);
        if !staging_dir.exists() && !layer_dir.exists() {
            return Ok((layer_id, staging_dir, layer_dir));
        }
    }
    Err(LayerStackError::LayerIdAllocation)
}

pub(crate) fn remove_path(path: &Path) -> Result<(), LayerStackError> {
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

pub(crate) fn join_layer_path(root: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(root.to_path_buf(), |path, part| {
        if part.is_empty() {
            path
        } else {
            path.join(part)
        }
    })
}

pub(crate) fn record_elapsed(timings: &mut BTreeMap<String, f64>, key: &str, start: Instant) {
    timings.insert(key.to_owned(), start.elapsed().as_secs_f64());
}

pub(crate) fn check_layer_path(path: &str) -> Result<(), LayerStackError> {
    if path.is_empty() {
        return Err(LayerStackError::Manifest(
            "layer path must not be empty".to_owned(),
        ));
    }
    if path.contains('\0') {
        return Err(LayerStackError::Manifest(format!(
            "layer path must not contain NUL bytes: {path:?}"
        )));
    }
    let path_ref = Path::new(path);
    if path_ref.is_absolute() {
        return Err(LayerStackError::Manifest(format!(
            "layer path must be relative: {path}"
        )));
    }
    if path_ref.components().any(|part| part.as_os_str() == "..") {
        return Err(LayerStackError::Manifest(format!(
            "layer path must not contain '..': {path}"
        )));
    }
    Ok(())
}

pub(crate) fn resolve_layer_path(storage_root: &Path, path: &str) -> PathBuf {
    let path = Path::new(path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        storage_root.join(path)
    }
}

pub(crate) fn replace_workspace_contents(
    destination: &Path,
    source: &Path,
) -> Result<(), LayerStackError> {
    std::fs::create_dir_all(destination)?;
    for child in std::fs::read_dir(destination)? {
        remove_path(&child?.path())?;
    }
    for child in std::fs::read_dir(source)? {
        let child = child?;
        move_path(&child.path(), &destination.join(child.file_name()))?;
    }
    Ok(())
}

fn move_path(source: &Path, destination: &Path) -> Result<(), LayerStackError> {
    match std::fs::rename(source, destination) {
        Ok(()) => Ok(()),
        Err(err) if err.raw_os_error() == Some(18) => {
            copy_path(source, destination)?;
            remove_path(source)
        }
        Err(err) => Err(err.into()),
    }
}

fn copy_path(source: &Path, destination: &Path) -> Result<(), LayerStackError> {
    let meta = std::fs::symlink_metadata(source)?;
    if meta.file_type().is_symlink() {
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let target = std::fs::read_link(source)?;
        remove_path(destination)?;
        std::os::unix::fs::symlink(target, destination)?;
    } else if meta.is_dir() {
        std::fs::create_dir_all(destination)?;
        for child in std::fs::read_dir(source)? {
            let child = child?;
            copy_path(&child.path(), &destination.join(child.file_name()))?;
        }
    } else if meta.is_file() {
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        remove_path(destination)?;
        std::fs::copy(source, destination)?;
    }
    Ok(())
}

pub(crate) fn clear_storage_root_preserving_lock(
    storage_root: &Path,
) -> Result<(), LayerStackError> {
    std::fs::create_dir_all(storage_root)?;
    for child in std::fs::read_dir(storage_root)? {
        let child = child?;
        if child.file_name() == STORAGE_WRITER_LOCK_FILE {
            continue;
        }
        remove_path(&child.path())?;
    }
    Ok(())
}

pub(crate) fn write_atomic(path: impl AsRef<Path>, bytes: &[u8]) -> Result<(), LayerStackError> {
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
        next_unique()
    ));
    let result = (|| -> Result<(), LayerStackError> {
        write_bytes_fsynced(&tmp, bytes)?;
        std::fs::rename(&tmp, path)?;
        if let Some(parent) = path.parent() {
            fsync_dir(parent)?;
        }
        Ok(())
    })();
    if let Err(err) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(err);
    }
    Ok(())
}

fn write_bytes_fsynced(path: &Path, bytes: &[u8]) -> Result<(), LayerStackError> {
    use std::io::Write as _;

    let mut file = std::fs::File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

pub(crate) fn fsync_dir(path: &Path) -> Result<(), LayerStackError> {
    std::fs::File::open(path)?.sync_all()?;
    Ok(())
}

pub(crate) fn fsync_tree_files(root: &Path) -> Result<(), LayerStackError> {
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = std::fs::symlink_metadata(&path)?.file_type();
        if file_type.is_dir() {
            fsync_tree_files(&path)?;
        } else if file_type.is_file() {
            std::fs::File::open(&path)?.sync_all()?;
        }
    }
    Ok(())
}

pub(crate) fn read_manifest(path: impl AsRef<Path>) -> Result<Manifest, LayerStackError> {
    let path = path.as_ref();
    if !path.exists() {
        return Manifest::new(0, vec![], MANIFEST_SCHEMA_VERSION).map_err(LayerStackError::from);
    }
    let payload = std::fs::read_to_string(path)?;
    let value: Value =
        serde_json::from_str(&payload).map_err(|err| LayerStackError::Manifest(err.to_string()))?;
    let obj = value.as_object().ok_or_else(|| {
        LayerStackError::Manifest("manifest payload must be an object".to_owned())
    })?;
    let version = obj.get("version").and_then(Value::as_i64).ok_or_else(|| {
        LayerStackError::Manifest("manifest payload missing required field: version".to_owned())
    })?;
    let schema_version = obj
        .get("schema_version")
        .and_then(Value::as_i64)
        .unwrap_or(MANIFEST_SCHEMA_VERSION);
    if schema_version > MANIFEST_SCHEMA_VERSION {
        return Err(LayerStackError::Manifest(format!(
            "manifest schema_version is newer than this runtime supports: {schema_version}"
        )));
    }
    let raw_layers = obj.get("layers").and_then(Value::as_array).ok_or_else(|| {
        LayerStackError::Manifest("manifest payload missing required field: layers".to_owned())
    })?;
    let mut layers = Vec::with_capacity(raw_layers.len());
    for item in raw_layers {
        let item = item.as_object().ok_or_else(|| {
            LayerStackError::Manifest("manifest layer entries must be objects".to_owned())
        })?;
        let layer = LayerRef {
            layer_id: item
                .get("layer_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            path: item
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        };
        validate_layer_ref(&layer)?;
        layers.push(layer);
    }
    Manifest::new(version, layers, schema_version).map_err(LayerStackError::from)
}

pub(crate) fn write_manifest(
    path: impl AsRef<Path>,
    manifest: &Manifest,
) -> Result<(), LayerStackError> {
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

pub(crate) fn layer_digest_path(storage_root: &Path, layer_id: &str) -> PathBuf {
    storage_root
        .join(LAYER_METADATA_DIR)
        .join(format!("{layer_id}.digest"))
}

pub(crate) fn write_layer_digest(
    storage_root: &Path,
    layer_id: &str,
    digest: &str,
) -> Result<(), LayerStackError> {
    write_atomic(layer_digest_path(storage_root, layer_id), digest.as_bytes())
}

pub(crate) fn validate_layer_ref(layer: &LayerRef) -> Result<(), LayerStackError> {
    if layer.layer_id.is_empty() {
        return Err(LayerStackError::Manifest(
            "layer_id must not be empty".to_owned(),
        ));
    }
    check_layer_path(&layer.path)
}

static NEXT_UNIQUE: AtomicU64 = AtomicU64::new(0);
