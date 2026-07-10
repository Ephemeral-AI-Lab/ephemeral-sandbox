//! Emit the winner map as one whiteout-preserving, zstd-compressed tar
//! spool. Entries carry the source's mode and second-granular mtime;
//! deletions ride the logical OCI encoding (`.wh.<name>`) and opaque cuts as
//! `.wh..wh..opq` — never kernel whiteouts, which need privileges to
//! extract. uid/gid, xattrs, and cross-winner hardlinks are not carried.

use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

use crate::error::LayerStackError;
use crate::model::LayerPath;
use crate::whiteout::{LOGICAL_WHITEOUT_PREFIX, OPAQUE_MARKER};

use super::delta::DeltaWinner;

const MARKER_MODE: u32 = 0o644;

/// Entry counts of one emitted spool, mirrored into the daemon start result.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DeltaStreamStats {
    pub files: u64,
    pub symlinks: u64,
    pub whiteouts: u64,
    pub opaques: u64,
}

/// Stream the winner map into a `tar.zst` spool at `spool_path`, in
/// deterministic winner-map order, compressed at `spool_zstd_level` (the
/// caller injects the level; this crate stays config-free).
///
/// # Errors
///
/// Returns [`LayerStackError`] when a winner source vanishes mid-read or the
/// spool write fails; the caller owns removing a partial spool.
pub fn emit_delta_stream(
    winners: &BTreeMap<LayerPath, DeltaWinner>,
    spool_path: &Path,
    spool_zstd_level: i32,
) -> Result<DeltaStreamStats, LayerStackError> {
    if let Some(parent) = spool_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let spool = std::fs::File::create(spool_path)?;
    let encoder = zstd::stream::write::Encoder::new(spool, spool_zstd_level)?;
    let mut builder = tar::Builder::new(encoder);
    let mut stats = DeltaStreamStats::default();
    for (path, winner) in winners {
        append_winner(&mut builder, path, winner, &mut stats)?;
    }
    let encoder = builder.into_inner()?;
    let mut spool = encoder.finish()?;
    spool.flush()?;
    Ok(stats)
}

fn append_winner(
    builder: &mut tar::Builder<zstd::stream::write::Encoder<'static, std::fs::File>>,
    path: &LayerPath,
    winner: &DeltaWinner,
    stats: &mut DeltaStreamStats,
) -> Result<(), LayerStackError> {
    match winner {
        DeltaWinner::Directory { source } => append_directory(builder, path, source),
        DeltaWinner::OpaqueDir { source } => {
            append_directory(builder, path, source)?;
            stats.opaques += 1;
            append_marker(builder, &format!("{}/{OPAQUE_MARKER}", path.as_str()))
        }
        DeltaWinner::File { source } => {
            let file = std::fs::File::open(source)?;
            let meta = file.metadata()?;
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Regular);
            header.set_size(meta.len());
            header.set_mode(meta.permissions().mode() & 0o7777);
            header.set_mtime(clamp_mtime(meta.mtime()));
            builder.append_data(&mut header, path.as_str(), file)?;
            stats.files += 1;
            Ok(())
        }
        DeltaWinner::Symlink { source } => {
            let meta = std::fs::symlink_metadata(source)?;
            let target = std::fs::read_link(source)?;
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            header.set_mode(meta.permissions().mode() & 0o7777);
            header.set_mtime(clamp_mtime(meta.mtime()));
            builder.append_link(&mut header, path.as_str(), target)?;
            stats.symlinks += 1;
            Ok(())
        }
        DeltaWinner::Delete => {
            stats.whiteouts += 1;
            append_marker(builder, &whiteout_entry_name(path))
        }
    }
}

fn append_directory(
    builder: &mut tar::Builder<zstd::stream::write::Encoder<'static, std::fs::File>>,
    path: &LayerPath,
    source: &Path,
) -> Result<(), LayerStackError> {
    let meta = std::fs::symlink_metadata(source)?;
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Directory);
    header.set_size(0);
    header.set_mode(meta.permissions().mode() & 0o7777);
    header.set_mtime(clamp_mtime(meta.mtime()));
    builder.append_data(&mut header, format!("{}/", path.as_str()), std::io::empty())?;
    Ok(())
}

fn append_marker(
    builder: &mut tar::Builder<zstd::stream::write::Encoder<'static, std::fs::File>>,
    name: &str,
) -> Result<(), LayerStackError> {
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_size(0);
    header.set_mode(MARKER_MODE);
    header.set_mtime(0);
    builder.append_data(&mut header, name, std::io::empty())?;
    Ok(())
}

fn whiteout_entry_name(target: &LayerPath) -> String {
    match target.as_str().rsplit_once('/') {
        Some((parent, name)) => format!("{parent}/{LOGICAL_WHITEOUT_PREFIX}{name}"),
        None => format!("{LOGICAL_WHITEOUT_PREFIX}{}", target.as_str()),
    }
}

fn clamp_mtime(mtime: i64) -> u64 {
    u64::try_from(mtime).unwrap_or(0)
}
