//! Classified one-level directory listings of the active manifest: the merged
//! view of a directory across the layer chain, honoring whiteouts and opaque
//! markers, for the runtime `file_list` operation. Read-only; never projects
//! or mutates storage.

use crate::error::LayerStackError;
use crate::model::LayerPath;
use crate::stack::LayerStack;

/// One visible entry of a listed directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestDirEntry {
    pub name: String,
    pub kind: ManifestDirEntryKind,
    pub size: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestDirEntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

impl ManifestDirEntryKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Directory => "directory",
            Self::Symlink => "symlink",
            Self::Other => "other",
        }
    }
}

/// Outcome of a merged directory listing. `rel = None` lists the workspace
/// root, which always exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestDirList {
    Absent,
    NotDirectory,
    Entries {
        entries: Vec<ManifestDirEntry>,
        truncated: bool,
    },
}

impl LayerStack {
    /// List one directory level of the active head under a shared lock,
    /// merged across the layer chain: upper layers win per name, whiteouts
    /// hide lower entries, and an opaque marker cuts lower layers off.
    ///
    /// # Errors
    /// Returns [`LayerStackError`] when the stack cannot be opened or read.
    pub fn list_dir(
        &self,
        rel: Option<&LayerPath>,
        limit: usize,
    ) -> Result<ManifestDirList, LayerStackError> {
        let _observation = self.observation.begin_operation(false, false);
        let _guard = self.writer_lock.shared()?;
        let manifest = self.read_active_manifest_unlocked()?;
        self.view.list_dir(rel, &manifest, limit)
    }
}
