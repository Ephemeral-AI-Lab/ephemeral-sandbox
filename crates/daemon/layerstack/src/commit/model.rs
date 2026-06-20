use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::model::LayerPath;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommitStatus {
    #[serde(rename = "accepted")]
    Accepted,
    #[serde(rename = "committed")]
    Committed,
    #[serde(rename = "aborted_version")]
    AbortedVersion,
    #[serde(rename = "dropped")]
    Dropped,
    #[serde(rename = "failed")]
    Failed,
}

impl CommitStatus {
    #[must_use]
    pub const fn status_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Committed => "committed",
            Self::AbortedVersion => "aborted_version",
            Self::Dropped => "dropped",
            Self::Failed => "failed",
        }
    }

    #[must_use]
    pub const fn is_published(self) -> bool {
        matches!(self, Self::Accepted | Self::Committed)
    }

    #[must_use]
    pub const fn is_non_conflicting(self) -> bool {
        matches!(self, Self::Accepted | Self::Committed | Self::Dropped)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileResult {
    pub path: LayerPath,
    pub status: CommitStatus,
    pub message: String,
    pub observed_version: Option<u64>,
    pub observed_state: Option<String>,
}

impl FileResult {
    #[must_use]
    pub fn conflict_message<'a>(&'a self, fallback: &'a str) -> &'a str {
        if self.message.is_empty() {
            fallback
        } else {
            self.message.as_str()
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChangesetResult {
    pub files: Vec<FileResult>,
    pub published_manifest_version: Option<u64>,
    pub timings: BTreeMap<String, f64>,
}

impl ChangesetResult {
    #[must_use]
    pub fn success(&self) -> bool {
        self.files.iter().all(|f| f.status.is_non_conflicting())
    }

    #[must_use]
    pub fn first_conflict(&self) -> Option<&FileResult> {
        self.files
            .iter()
            .find(|file| !file.status.is_non_conflicting())
    }

    #[must_use]
    pub fn published_paths(&self) -> Vec<String> {
        self.files
            .iter()
            .filter(|file| file.status.is_published())
            .map(|file| file.path.as_str().to_owned())
            .collect()
    }

    #[must_use]
    pub fn published_file_count(&self) -> usize {
        self.files
            .iter()
            .filter(|file| file.status.is_published())
            .count()
    }
}
