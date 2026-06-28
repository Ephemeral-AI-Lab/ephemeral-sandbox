use std::path::Path;

use sha2::{Digest, Sha256};

use crate::model::hex_lower;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum BaseEntry {
    Directory {
        path: String,
    },
    File {
        path: String,
        size: u64,
        content_hash: String,
    },
    Symlink {
        path: String,
        link_target: String,
    },
}

impl BaseEntry {
    pub(super) fn path(&self) -> &str {
        match self {
            Self::Directory { path } | Self::File { path, .. } | Self::Symlink { path, .. } => path,
        }
    }

    const fn kind(&self) -> &'static str {
        match self {
            Self::Directory { .. } => "directory",
            Self::File { .. } => "file",
            Self::Symlink { .. } => "symlink",
        }
    }
}

pub(super) fn base_root_hash(entries: &mut [BaseEntry]) -> String {
    entries.sort_by(|left, right| left.path().cmp(right.path()));
    let mut digest = Sha256::new();
    for entry in entries {
        update_root_hash(&mut digest, entry);
    }
    hex_lower(digest.finalize())
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

pub(super) fn relative_path(workspace: &Path, path: &Path) -> String {
    path.strip_prefix(workspace)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

pub(super) fn format_path_sample(paths: &[String]) -> String {
    const LIMIT: usize = 5;
    let mut sample = paths.iter().take(LIMIT).cloned().collect::<Vec<_>>();
    if paths.len() > LIMIT {
        sample.push(format!("+{} more", paths.len() - LIMIT));
    }
    sample.join(", ")
}
