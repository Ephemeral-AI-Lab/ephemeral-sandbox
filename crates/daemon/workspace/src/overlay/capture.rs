use std::path::{Path, PathBuf};

use layerstack::service::{self, BoundedCaptureOptions, Snapshot};
use layerstack::{CaptureRouteStats, LayerChange, ProtectedPathDrop};

use super::tree::TreeResourceStats;

/// Captured upperdir changes and resource stats.
#[derive(Debug, Clone, PartialEq)]
pub struct CapturedChanges {
    pub changes: Vec<LayerChange>,
    pub protected_drops: Vec<ProtectedPathDrop>,
    pub stats: TreeResourceStats,
    pub capture_s: f64,
}

/// Captured ephemeral command changes with command-snapshot routing metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct RoutedCapturedChanges {
    pub captured: CapturedChanges,
    pub route_stats: CaptureRouteStats,
    pub metadata_path_count: usize,
    pub spool_dir: Option<PathBuf>,
}

/// Error raised while capturing an overlay upperdir.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureError {
    pub reason: String,
    pub failing_path: Option<String>,
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.failing_path.as_deref() {
            Some(path) => write!(formatter, "capture failed at {path}: {}", self.reason),
            None => write!(formatter, "capture failed: {}", self.reason),
        }
    }
}

impl std::error::Error for CaptureError {}

/// Capture an upperdir delta and resource stats.
///
/// # Errors
///
/// Returns [`CaptureError`] when the upperdir walk fails.
pub fn capture_upperdir(upperdir: &Path) -> Result<CapturedChanges, CaptureError> {
    capture_upperdir_with_payloads(upperdir, true)
}

/// Capture an upperdir delta and resource stats, optionally avoiding regular
/// file payload materialization.
///
/// # Errors
///
/// Returns [`CaptureError`] when the upperdir walk fails.
pub fn capture_upperdir_with_payloads(
    upperdir: &Path,
    materialize_payloads: bool,
) -> Result<CapturedChanges, CaptureError> {
    let start = std::time::Instant::now();
    let captured = if materialize_payloads {
        layerstack::capture_upperdir_with_stats(upperdir)
    } else {
        layerstack::capture_upperdir_metadata_with_stats(upperdir)
    }
    .map_err(|error| CaptureError {
        failing_path: error.failing_path().map(|path| path.display().to_string()),
        reason: error.to_string(),
    })?;
    Ok(CapturedChanges {
        changes: captured.changes,
        protected_drops: captured.protected_drops,
        stats: TreeResourceStats::from(captured.stats),
        capture_s: start.elapsed().as_secs_f64(),
    })
}

/// Capture an ephemeral command upperdir using explicit bounded-capture options.
///
/// # Errors
///
/// Returns [`CaptureError`] when metadata capture, routing, or selected payload
/// materialization fails.
pub fn capture_upperdir_for_snapshot_with_options(
    root: &Path,
    snapshot: &Snapshot,
    upperdir: &Path,
    spool_dir: &Path,
    options: BoundedCaptureOptions,
) -> Result<RoutedCapturedChanges, CaptureError> {
    let start = std::time::Instant::now();
    let captured = service::capture_upperdir_for_snapshot_with_options(
        root,
        snapshot.manifest_version,
        &snapshot.layer_paths,
        upperdir,
        spool_dir,
        options,
    )
    .map_err(|error| CaptureError {
        failing_path: None,
        reason: error.to_string(),
    })?;
    Ok(RoutedCapturedChanges {
        captured: CapturedChanges {
            changes: captured.changes,
            protected_drops: captured.protected_drops,
            stats: TreeResourceStats::from(captured.stats),
            capture_s: start.elapsed().as_secs_f64(),
        },
        route_stats: captured.route_stats,
        metadata_path_count: captured.metadata_path_count,
        spool_dir: captured.spool_dir,
    })
}
