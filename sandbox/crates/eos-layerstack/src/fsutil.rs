use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::error::LayerStackError;

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
