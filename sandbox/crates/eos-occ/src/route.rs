//! Route classification and per-path publish outcomes.
//!
//! A changeset is split into disjoint normalized paths; each path is routed to
//! exactly one of four destinations and, after the publish transaction, lands a
//! per-path [`OccStatus`]. These mirror the Rust `RouteDecision` /
//! `FileStatus` enums byte-for-byte (the `str` values are part of the wire
//! contract, so the `serde` rename strings below are load-bearing).

use std::collections::BTreeMap;

use eos_protocol::LayerPath;
use serde::{Deserialize, Serialize};

/// Where a single normalized path is routed during preparation.
///
/// The wire strings are exact: `gated`/`direct`/`drop`/`reject`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Route {
    /// Tracked-by-git path: publish through CAS with a base-hash check.
    #[serde(rename = "gated")]
    Gated,
    /// Gitignored path: publish directly without a base-hash gate.
    #[serde(rename = "direct")]
    Direct,
    /// `.git` internal path: dropped (never published).
    #[serde(rename = "drop")]
    Drop,
    /// Disallowed path (absolute / escaping): rejected.
    #[serde(rename = "reject")]
    Reject,
}

/// Terminal per-path status after the publish transaction resolves.
///
/// Wire strings are exact and include `aborted_version` — the stale-base
/// outcome surfaced when the CAS retry budget is exhausted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum OccStatus {
    /// Validated and staged (atomic batch not yet finalized).
    #[serde(rename = "accepted")]
    Accepted,
    /// Published into a new manifest version.
    #[serde(rename = "committed")]
    Committed,
    /// Stale base hash; CAS retry budget exhausted.
    #[serde(rename = "aborted_version")]
    AbortedVersion,
    /// Overlapping in-flight publish to the same path aborted this one.
    #[serde(rename = "aborted_overlap")]
    AbortedOverlap,
    /// Routed [`Route::Drop`]: intentionally not published.
    #[serde(rename = "dropped")]
    Dropped,
    /// Routed [`Route::Reject`]: disallowed path.
    #[serde(rename = "rejected")]
    Rejected,
    /// Publish failed for a non-version reason.
    #[serde(rename = "failed")]
    Failed,
}

impl OccStatus {
    /// Stable wire string for this path status.
    #[must_use]
    pub const fn wire_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Committed => "committed",
            Self::AbortedVersion => "aborted_version",
            Self::AbortedOverlap => "aborted_overlap",
            Self::Dropped => "dropped",
            Self::Rejected => "rejected",
            Self::Failed => "failed",
        }
    }

    /// Did this path actually land in a published manifest?
    #[must_use]
    pub const fn is_published(self) -> bool {
        matches!(self, Self::Accepted | Self::Committed)
    }

    /// Is this a success outcome (published, or a deliberate drop)?
    #[must_use]
    pub const fn is_success(self) -> bool {
        matches!(self, Self::Accepted | Self::Committed | Self::Dropped)
    }
}

/// The route + reason a preparer assigned to one normalized path.
///
/// This is the per-path half of a `PublishDecision`: the input contract the
/// commit queue consumes (one entry per disjoint path).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishDecision {
    /// Normalized, validated path (unrepresentable when invalid).
    pub path: LayerPath,
    /// Destination this path was routed to.
    pub route: Route,
    /// Expected content hash for gated writes/deletes.
    ///
    /// `None` means the path did not exist at the operation snapshot. Direct,
    /// dropped, and rejected routes do not use this field.
    pub base_hash: Option<String>,
    /// Optional human-readable reason (e.g. why dropped/rejected).
    pub message: Option<String>,
}

/// Terminal outcome for one path after the publish transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileResult {
    /// The normalized path this result describes.
    pub path: LayerPath,
    /// Terminal status.
    pub status: OccStatus,
    /// Diagnostic message (empty by default, mirrors Rust).
    pub message: String,
}

impl FileResult {
    /// Return the diagnostic message, falling back when the result has none.
    #[must_use]
    pub fn conflict_message<'a>(&'a self, fallback: &'a str) -> &'a str {
        if self.message.is_empty() {
            fallback
        } else {
            self.message.as_str()
        }
    }
}

/// Aggregate result of a published (or aborted) changeset.
#[derive(Debug, Clone, PartialEq)]
pub struct ChangesetResult {
    /// Per-path outcomes, one entry per input path.
    pub files: Vec<FileResult>,
    /// Manifest version produced, or `None` if nothing landed.
    pub published_manifest_version: Option<u64>,
    /// Per-commit phase timings keyed with the Rust-compatible `occ.commit.*`
    /// names.
    pub timings: BTreeMap<String, f64>,
}

impl ChangesetResult {
    /// True iff every path reached a success status.
    #[must_use]
    pub fn success(&self) -> bool {
        self.files.iter().all(|f| f.status.is_success())
    }

    /// First path that failed to reach a success status.
    #[must_use]
    pub fn first_conflict(&self) -> Option<&FileResult> {
        self.files.iter().find(|file| !file.status.is_success())
    }

    /// Paths that landed in the published manifest.
    #[must_use]
    pub fn published_paths(&self) -> Vec<String> {
        self.files
            .iter()
            .filter(|file| file.status.is_published())
            .map(|file| file.path.as_str().to_owned())
            .collect()
    }

    /// Count paths that landed in the published manifest.
    #[must_use]
    pub fn published_file_count(&self) -> usize {
        self.files
            .iter()
            .filter(|file| file.status.is_published())
            .count()
    }
}
