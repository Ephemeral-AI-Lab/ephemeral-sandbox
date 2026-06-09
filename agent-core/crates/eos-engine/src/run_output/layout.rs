use std::path::{Path, PathBuf};

use eos_types::AgentRunRecordDir;

use super::error::{AgentRunRecordError, Result};

pub(crate) fn record_dir(root: &Path, record_dir: &AgentRunRecordDir) -> Result<PathBuf> {
    record_dir_path(root, record_dir.as_str())
}

fn record_dir_path(root: &Path, record_dir: &str) -> Result<PathBuf> {
    let mut path = root.to_path_buf();
    for segment in record_dir.split('/') {
        path.push(safe_segment("record_dir", segment)?);
    }
    Ok(path)
}

fn safe_segment<'a>(field: &'static str, value: &'a str) -> Result<&'a str> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.contains('/')
        || value.contains('\\')
        || value.contains(std::path::MAIN_SEPARATOR)
    {
        return Err(AgentRunRecordError::unsafe_segment(field, value));
    }
    Ok(value)
}
