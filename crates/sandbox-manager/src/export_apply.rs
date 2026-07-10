//! Host-side renderer for the export delta stream. The applier is a host
//! process holding operator privileges while consuming sandbox-authored tar,
//! so a compromised daemon is in scope (spec invariant 9): every entry name
//! is validated before any filesystem mutation, every path is reached
//! through a dest-rooted `O_NOFOLLOW` fd walk, hardlink entries are
//! rejected, and decompressed bytes and entry counts are capped against
//! bombs. Dir mode reproduces `apply_layer`'s three-pass order — opaque
//! clears, then whiteout deletions, then content — so an opaque clear can
//! never destroy a just-written winner (spec invariant 2).

use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rustix::fs::{AtFlags, Mode, OFlags};
use rustix::io::Errno;

const LOGICAL_WHITEOUT_PREFIX: &str = ".wh.";
const OPAQUE_MARKER: &str = ".wh..wh..opq";

const DECOMPRESSED_CAP_SENTINEL: &str = "export decompressed-byte cap exceeded";

/// Bomb caps for the host-side apply, injected by the gateway from
/// `manager.export`; `Default` preserves the shipped policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportApplyCaps {
    pub max_stream_bytes: u64,
    pub max_decompressed_bytes: u64,
    pub max_apply_entries: u64,
}

impl Default for ExportApplyCaps {
    fn default() -> Self {
        Self {
            max_stream_bytes: 2 * 1024 * 1024 * 1024,
            max_decompressed_bytes: 8 * 1024 * 1024 * 1024,
            max_apply_entries: 1_000_000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArchiveFormat {
    Tar,
    TarZst,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DirApplyStats {
    pub(crate) files_written: u64,
    pub(crate) symlinks_written: u64,
    pub(crate) deletes_applied: u64,
    pub(crate) opaque_clears: u64,
    pub(crate) skipped_unchanged: u64,
    pub(crate) bytes_written: u64,
}

/// Apply the complete compressed delta from `delivery` onto `dest` (an
/// existing directory, already canonicalized by the dest guard). The
/// validation pass tees the compressed bytes into memory for the apply pass.
/// No filesystem mutation happens until the whole archive has been
/// validated.
pub(crate) fn apply_dir_delta(
    delivery: &mut impl Read,
    dest: &Path,
    caps: ExportApplyCaps,
) -> Result<DirApplyStats, String> {
    let mut compressed: Vec<u8> = Vec::new();
    let plan = plan_stream(
        TeeReader {
            inner: &mut *delivery,
            sink: &mut compressed,
        },
        caps,
    )?;
    drain_delivery(delivery, &mut compressed)?;
    let root = open_dest_root(dest)?;
    let mut stats = DirApplyStats::default();
    for dir in &plan.opaque_dirs {
        clear_directory(&root, dir)?;
        stats.opaque_clears += 1;
    }
    for target in &plan.whiteout_targets {
        remove_validated_target(&root, target)?;
        stats.deletes_applied += 1;
    }
    apply_content(&compressed, &plan, &root, &mut stats, caps)?;
    Ok(stats)
}

/// Render the complete delivery as an archive file: `TarZst` writes the bytes
/// as received; `Tar` decompresses them (capped). Complete-or-absent via a
/// nonce-named sibling temp file and one rename; a rendering error removes the
/// temporary file before the destination is replaced.
pub(crate) fn write_archive(
    delivery: &mut impl Read,
    dest: &Path,
    format: ArchiveFormat,
    caps: ExportApplyCaps,
) -> Result<u64, String> {
    let temp = archive_temp_path(dest)?;
    let result = (|| -> Result<u64, String> {
        let mut file = std::fs::File::create(&temp)
            .map_err(|error| format!("create archive temp {}: {error}", temp.display()))?;
        let bytes = match format {
            ArchiveFormat::TarZst => std::io::copy(delivery, &mut file)
                .map_err(|error| format!("write archive: {error}"))?,
            ArchiveFormat::Tar => {
                let mut decoder = capped_decoder(delivery, caps.max_decompressed_bytes)?;
                std::io::copy(&mut decoder, &mut file)
                    .map_err(|error| map_cap_error(error, caps.max_decompressed_bytes))?
            }
        };
        file.flush()
            .map_err(|error| format!("flush archive: {error}"))?;
        std::fs::rename(&temp, dest)
            .map_err(|error| format!("rename archive into place: {error}"))?;
        Ok(bytes)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// Tee every byte the validation pass consumes into the apply-pass buffer.
struct TeeReader<'a, R> {
    inner: R,
    sink: &'a mut Vec<u8>,
}

impl<R: Read> Read for TeeReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buf)?;
        self.sink.extend_from_slice(&buf[..read]);
        Ok(read)
    }
}

/// The tar reader stops at the end-of-archive marker, which can sit before
/// the compressed stream's final bytes; drain the rest so the apply pass
/// sees the identical byte sequence and transport completeness (truncation,
/// overrun) is still verified end-to-end.
fn drain_delivery(delivery: &mut impl Read, sink: &mut Vec<u8>) -> Result<(), String> {
    let mut buf = [0u8; 64 * 1024];
    loop {
        let read = delivery
            .read(&mut buf)
            .map_err(|error| format!("export stream read failed: {error}"))?;
        if read == 0 {
            return Ok(());
        }
        sink.extend_from_slice(&buf[..read]);
    }
}

#[derive(Debug)]
enum PlannedEntry {
    Directory {
        comps: Vec<String>,
        mode: u32,
    },
    File {
        comps: Vec<String>,
        mode: u32,
        mtime: u64,
        size: u64,
    },
    Symlink {
        comps: Vec<String>,
    },
    Marker,
}

#[derive(Debug)]
struct ApplyPlan {
    entries: Vec<PlannedEntry>,
    opaque_dirs: Vec<Vec<String>>,
    whiteout_targets: Vec<Vec<String>>,
}

/// Validation pass over the whole stream before any mutation: reject
/// absolute names, `..` components, reserved `.wh.` components outside the
/// marker encodings, out-of-range whiteout targets (validated after the
/// prefix strip), hardlinks, and unsupported entry types; enforce the entry
/// and decompressed-byte caps. A hostile stream is rejected with zero
/// filesystem writes.
fn plan_stream(compressed: impl Read, caps: ExportApplyCaps) -> Result<ApplyPlan, String> {
    let decoder = capped_decoder(compressed, caps.max_decompressed_bytes)?;
    let mut archive = tar::Archive::new(decoder);
    let mut plan = ApplyPlan {
        entries: Vec::new(),
        opaque_dirs: Vec::new(),
        whiteout_targets: Vec::new(),
    };
    let entry_cap = caps.max_apply_entries;
    let entries = archive
        .entries()
        .map_err(|error| format!("export stream is not a tar archive: {error}"))?;
    for entry in entries {
        let entry = entry.map_err(|error| map_cap_error(error, caps.max_decompressed_bytes))?;
        if plan.entries.len() as u64 >= entry_cap {
            return Err(format!(
                "export entry-count cap exceeded ({entry_cap} entries)"
            ));
        }
        let raw_name = entry.path_bytes().into_owned();
        let name = String::from_utf8(raw_name)
            .map_err(|_| "rejected entry: name is not UTF-8".to_owned())?;
        let comps = validated_components(&name)?;
        let planned = classify_entry(&entry, &name, comps, &mut plan)?;
        plan.entries.push(planned);
    }
    plan.opaque_dirs.sort();
    plan.whiteout_targets.sort();
    Ok(plan)
}

fn classify_entry(
    entry: &tar::Entry<'_, impl Read>,
    name: &str,
    comps: Vec<String>,
    plan: &mut ApplyPlan,
) -> Result<PlannedEntry, String> {
    let header = entry.header();
    let kind = header.entry_type();
    if kind == tar::EntryType::Link {
        return Err(format!("rejected hardlink entry: {name}"));
    }
    let last = comps.last().map(String::as_str).unwrap_or_default();
    if last == OPAQUE_MARKER {
        if comps.len() < 2 {
            return Err(format!(
                "rejected opaque marker at the archive root: {name}"
            ));
        }
        plan.opaque_dirs.push(comps[..comps.len() - 1].to_vec());
        return Ok(PlannedEntry::Marker);
    }
    if let Some(stripped) = last.strip_prefix(LOGICAL_WHITEOUT_PREFIX) {
        if stripped.is_empty()
            || stripped == "."
            || stripped == ".."
            || stripped.contains('/')
            || stripped.starts_with(LOGICAL_WHITEOUT_PREFIX)
        {
            return Err(format!(
                "rejected whiteout entry with invalid target: {name}"
            ));
        }
        let mut target = comps[..comps.len() - 1].to_vec();
        target.push(stripped.to_owned());
        plan.whiteout_targets.push(target);
        return Ok(PlannedEntry::Marker);
    }
    let mode = header
        .mode()
        .map_err(|error| format!("rejected entry {name}: unreadable mode: {error}"))?
        & 0o7777;
    match kind {
        tar::EntryType::Directory => Ok(PlannedEntry::Directory { comps, mode }),
        tar::EntryType::Regular | tar::EntryType::Continuous => {
            let mtime = header
                .mtime()
                .map_err(|error| format!("rejected entry {name}: unreadable mtime: {error}"))?;
            let size = header
                .size()
                .map_err(|error| format!("rejected entry {name}: unreadable size: {error}"))?;
            Ok(PlannedEntry::File {
                comps,
                mode,
                mtime,
                size,
            })
        }
        tar::EntryType::Symlink => {
            let target = header
                .link_name()
                .map_err(|error| format!("rejected symlink entry {name}: {error}"))?;
            if target.is_none() {
                return Err(format!("rejected symlink entry without a target: {name}"));
            }
            Ok(PlannedEntry::Symlink { comps })
        }
        other => Err(format!("rejected unsupported entry type {other:?}: {name}")),
    }
}

/// Entry-name law (invariant 9): relative, no `..`, no NUL, and no reserved
/// `.wh.` component anywhere except a final marker component.
fn validated_components(name: &str) -> Result<Vec<String>, String> {
    if name.starts_with('/') {
        return Err(format!("rejected absolute entry name: {name}"));
    }
    if name.contains('\0') {
        return Err(format!("rejected entry name with NUL: {name}"));
    }
    let mut comps: Vec<String> = Vec::new();
    for part in name.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            return Err(format!("rejected entry name with '..': {name}"));
        }
        comps.push(part.to_owned());
    }
    if comps.is_empty() {
        return Err(format!("rejected empty entry name: {name:?}"));
    }
    for part in &comps[..comps.len() - 1] {
        if part.starts_with(LOGICAL_WHITEOUT_PREFIX) {
            return Err(format!(
                "rejected reserved .wh. path component in entry name: {name}"
            ));
        }
    }
    Ok(comps)
}

fn apply_content(
    compressed: &[u8],
    plan: &ApplyPlan,
    root: &OwnedFd,
    stats: &mut DirApplyStats,
    caps: ExportApplyCaps,
) -> Result<(), String> {
    let decoder = capped_decoder(compressed, caps.max_decompressed_bytes)?;
    let mut archive = tar::Archive::new(decoder);
    let entries = archive
        .entries()
        .map_err(|error| format!("export stream is not a tar archive: {error}"))?;
    let mut planned = plan.entries.iter();
    for entry in entries {
        let mut entry = entry.map_err(|error| map_cap_error(error, caps.max_decompressed_bytes))?;
        let planned_entry = planned
            .next()
            .ok_or_else(|| "export stream changed between passes".to_owned())?;
        match planned_entry {
            PlannedEntry::Marker => {}
            PlannedEntry::Directory { comps, mode } => {
                let parent = walk_to_parent(root, comps, true)?
                    .ok_or_else(|| format!("unreachable directory {}", comps.join("/")))?;
                ensure_directory(&parent, last(comps), *mode)?;
            }
            PlannedEntry::File {
                comps,
                mode,
                mtime,
                size,
            } => {
                let parent = walk_to_parent(root, comps, true)?
                    .ok_or_else(|| format!("unreachable file {}", comps.join("/")))?;
                let name = last(comps);
                if unchanged(&parent, name, *size, *mtime) {
                    // A (size, second-mtime) match is only a *candidate* skip: it
                    // holds for a genuine prior-export watermark, but equally for a
                    // pre-existing dest file that collides on size and shares the
                    // winner's mtime second (the documented skip-unchanged
                    // sub-second hole). Confirm by content so a real winner is
                    // never skipped; an identical re-run still skips with zero
                    // bytes written (invariant 4).
                    let mut winner_bytes = Vec::new();
                    entry
                        .read_to_end(&mut winner_bytes)
                        .map_err(|error| map_cap_error(error, caps.max_decompressed_bytes))?;
                    if dest_file_content_equals(&parent, name, &winner_bytes)? {
                        stats.skipped_unchanged += 1;
                        continue;
                    }
                    remove_recursive_at(&parent, name)?;
                    let mut reader = winner_bytes.as_slice();
                    let written = write_file(
                        &parent,
                        name,
                        *mode,
                        *mtime,
                        &mut reader,
                        caps.max_decompressed_bytes,
                    )?;
                    stats.bytes_written += written;
                    stats.files_written += 1;
                } else {
                    remove_recursive_at(&parent, name)?;
                    let written = write_file(
                        &parent,
                        name,
                        *mode,
                        *mtime,
                        &mut entry,
                        caps.max_decompressed_bytes,
                    )?;
                    stats.bytes_written += written;
                    stats.files_written += 1;
                }
            }
            PlannedEntry::Symlink { comps } => {
                let target = entry
                    .header()
                    .link_name()
                    .map_err(|error| format!("symlink target unreadable: {error}"))?
                    .ok_or_else(|| "symlink target vanished between passes".to_owned())?
                    .into_owned();
                let parent = walk_to_parent(root, comps, true)?
                    .ok_or_else(|| format!("unreachable symlink {}", comps.join("/")))?;
                let name = last(comps);
                remove_recursive_at(&parent, name)?;
                rustix::fs::symlinkat(&*target, &parent, name)
                    .map_err(|errno| format!("symlink {}: {errno}", comps.join("/")))?;
                stats.symlinks_written += 1;
            }
        }
    }
    Ok(())
}

fn last(comps: &[String]) -> &str {
    comps.last().map(String::as_str).unwrap_or_default()
}

fn open_dest_root(dest: &Path) -> Result<OwnedFd, String> {
    rustix::fs::open(
        dest,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|errno| format!("open export destination {}: {errno}", dest.display()))
}

fn open_child_dir(parent: &OwnedFd, name: &str) -> Result<OwnedFd, Errno> {
    rustix::fs::openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
}

fn is_non_directory(errno: Errno) -> bool {
    matches!(errno, Errno::LOOP | Errno::NOTDIR | Errno::MLINK)
}

/// Open the parent directory of `comps` by stepping one component at a time
/// with `O_NOFOLLOW | O_DIRECTORY`, so no symlink component — pre-existing
/// or planted by an earlier entry — is ever followed out of dest. With
/// `create`, missing components are created and non-directories at a
/// directory position are replaced (the ensure-dir law); without it, a
/// missing or blocked component reports `None`.
fn walk_to_parent(
    root: &OwnedFd,
    comps: &[String],
    create: bool,
) -> Result<Option<OwnedFd>, String> {
    let mut fd = root
        .try_clone()
        .map_err(|error| format!("clone dest root fd: {error}"))?;
    for comp in &comps[..comps.len() - 1] {
        fd = match open_child_dir(&fd, comp) {
            Ok(next) => next,
            Err(Errno::NOENT) if create => {
                match rustix::fs::mkdirat(&fd, comp.as_str(), Mode::from_raw_mode(0o755)) {
                    Ok(()) | Err(Errno::EXIST) => {}
                    Err(errno) => return Err(format!("mkdir {comp}: {errno}")),
                }
                open_child_dir(&fd, comp)
                    .map_err(|errno| format!("open created directory {comp}: {errno}"))?
            }
            Err(Errno::NOENT) => return Ok(None),
            Err(errno) if is_non_directory(errno) && create => {
                rustix::fs::unlinkat(&fd, comp.as_str(), AtFlags::empty())
                    .map_err(|errno| format!("replace non-directory {comp}: {errno}"))?;
                rustix::fs::mkdirat(&fd, comp.as_str(), Mode::from_raw_mode(0o755))
                    .map_err(|errno| format!("mkdir {comp}: {errno}"))?;
                open_child_dir(&fd, comp)
                    .map_err(|errno| format!("open created directory {comp}: {errno}"))?
            }
            Err(errno) if is_non_directory(errno) => return Ok(None),
            Err(errno) => return Err(format!("walk component {comp}: {errno}")),
        };
    }
    Ok(Some(fd))
}

fn ensure_directory(parent: &OwnedFd, name: &str, mode: u32) -> Result<(), String> {
    let dir = match open_child_dir(parent, name) {
        Ok(dir) => dir,
        Err(Errno::NOENT) => {
            rustix::fs::mkdirat(parent, name, Mode::from_raw_mode(0o755))
                .map_err(|errno| format!("mkdir {name}: {errno}"))?;
            open_child_dir(parent, name)
                .map_err(|errno| format!("open created directory {name}: {errno}"))?
        }
        Err(errno) if is_non_directory(errno) => {
            rustix::fs::unlinkat(parent, name, AtFlags::empty())
                .map_err(|errno| format!("replace non-directory {name}: {errno}"))?;
            rustix::fs::mkdirat(parent, name, Mode::from_raw_mode(0o755))
                .map_err(|errno| format!("mkdir {name}: {errno}"))?;
            open_child_dir(parent, name)
                .map_err(|errno| format!("open created directory {name}: {errno}"))?
        }
        Err(errno) => return Err(format!("open directory {name}: {errno}")),
    };
    rustix::fs::fchmod(&dir, Mode::from_raw_mode(mode as rustix::fs::RawMode))
        .map_err(|errno| format!("chmod directory {name}: {errno}"))?;
    Ok(())
}

/// Opaque clear: ensure the directory exists (replacing a non-directory at
/// its position), then remove every child through the fd walk — this is
/// what removes base-origin files the sandbox masked with an opaque cut.
fn clear_directory(root: &OwnedFd, comps: &[String]) -> Result<(), String> {
    let Some(parent) = walk_to_parent(root, comps, true)? else {
        return Ok(());
    };
    ensure_directory(&parent, last(comps), 0o755)?;
    let dir = open_child_dir(&parent, last(comps))
        .map_err(|errno| format!("open opaque directory {}: {errno}", comps.join("/")))?;
    for name in dir_child_names(&dir)? {
        remove_recursive_at(&dir, &name)?;
    }
    Ok(())
}

fn remove_validated_target(root: &OwnedFd, comps: &[String]) -> Result<(), String> {
    let Some(parent) = walk_to_parent(root, comps, false)? else {
        return Ok(());
    };
    remove_recursive_at(&parent, last(comps))
}

fn remove_recursive_at(parent: &OwnedFd, name: &str) -> Result<(), String> {
    match open_child_dir(parent, name) {
        Ok(dir) => {
            for child in dir_child_names(&dir)? {
                remove_recursive_at(&dir, &child)?;
            }
            match rustix::fs::unlinkat(parent, name, AtFlags::REMOVEDIR) {
                Ok(()) | Err(Errno::NOENT) => Ok(()),
                Err(errno) => Err(format!("remove directory {name}: {errno}")),
            }
        }
        Err(Errno::NOENT) => Ok(()),
        Err(errno) if is_non_directory(errno) => {
            match rustix::fs::unlinkat(parent, name, AtFlags::empty()) {
                Ok(()) | Err(Errno::NOENT) => Ok(()),
                Err(errno) => Err(format!("remove {name}: {errno}")),
            }
        }
        Err(errno) => Err(format!("open {name} for removal: {errno}")),
    }
}

fn dir_child_names(dir: &OwnedFd) -> Result<Vec<String>, String> {
    use std::os::fd::AsFd as _;
    let reader = rustix::fs::Dir::read_from(dir.as_fd())
        .map_err(|errno| format!("read directory: {errno}"))?;
    let mut names = Vec::new();
    for entry in reader {
        let entry = entry.map_err(|errno| format!("read directory entry: {errno}"))?;
        let bytes = entry.file_name().to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        names.push(String::from_utf8_lossy(bytes).into_owned());
    }
    Ok(names)
}

/// skip-unchanged: (size, second-truncated mtime) equality between the tar
/// entry and the existing destination regular file. Sound because every
/// write stamps the entry's second-granular mtime.
fn unchanged(parent: &OwnedFd, name: &str, size: u64, mtime: u64) -> bool {
    let Ok(stat) = rustix::fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW) else {
        return false;
    };
    let file_type = rustix::fs::FileType::from_raw_mode(stat.st_mode as rustix::fs::RawMode);
    if file_type != rustix::fs::FileType::RegularFile {
        return false;
    }
    let dest_size = u64::try_from(stat.st_size).unwrap_or(u64::MAX);
    let Ok(dest_mtime) = u64::try_from(stat.st_mtime) else {
        return false;
    };
    dest_size == size && dest_mtime == mtime
}

/// Confirm a candidate skip by content: read the existing dest regular file
/// (reached no-follow) and compare it byte-for-byte to the winner. Returns
/// `false` when the file cannot be read as a plain file, so the winner is
/// written rather than skipped.
fn dest_file_content_equals(parent: &OwnedFd, name: &str, winner: &[u8]) -> Result<bool, String> {
    let fd = match rustix::fs::openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(Errno::NOENT) => return Ok(false),
        Err(errno) if is_non_directory(errno) => return Ok(false),
        Err(errno) => return Err(format!("open {name} for content compare: {errno}")),
    };
    let mut file = std::fs::File::from(fd);
    let mut current = Vec::new();
    file.read_to_end(&mut current)
        .map_err(|error| format!("read {name} for content compare: {error}"))?;
    Ok(current == winner)
}

fn write_file(
    parent: &OwnedFd,
    name: &str,
    mode: u32,
    mtime: u64,
    content: &mut impl Read,
    max_decompressed_bytes: u64,
) -> Result<u64, String> {
    let fd = rustix::fs::openat(
        parent,
        name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )
    .map_err(|errno| format!("create file {name}: {errno}"))?;
    let mut file = std::fs::File::from(fd);
    let written = std::io::copy(content, &mut file)
        .map_err(|error| map_cap_error(error, max_decompressed_bytes))?;
    file.flush()
        .map_err(|error| format!("flush {name}: {error}"))?;
    rustix::fs::fchmod(&file, Mode::from_raw_mode(mode as rustix::fs::RawMode))
        .map_err(|errno| format!("chmod {name}: {errno}"))?;
    let stamp = rustix::fs::Timespec {
        tv_sec: i64::try_from(mtime).unwrap_or(0),
        tv_nsec: 0,
    };
    rustix::fs::futimens(
        &file,
        &rustix::fs::Timestamps {
            last_access: stamp,
            last_modification: stamp,
        },
    )
    .map_err(|errno| format!("stamp mtime on {name}: {errno}"))?;
    Ok(written)
}

fn archive_temp_path(dest: &Path) -> Result<PathBuf, String> {
    static NONCE: AtomicU64 = AtomicU64::new(0);
    let name = dest
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("archive destination has no file name: {}", dest.display()))?;
    Ok(dest.with_file_name(format!(
        ".{name}.{}.{:04x}.tmp",
        std::process::id(),
        NONCE.fetch_add(1, Ordering::Relaxed)
    )))
}

struct CappedReader<R> {
    inner: R,
    remaining: u64,
}

impl<R: Read> Read for CappedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buf)?;
        let read_len = read as u64;
        if read_len > self.remaining {
            return Err(std::io::Error::other(DECOMPRESSED_CAP_SENTINEL));
        }
        self.remaining -= read_len;
        Ok(read)
    }
}

fn capped_decoder<R: Read>(
    compressed: R,
    max_decompressed_bytes: u64,
) -> Result<CappedReader<zstd::stream::read::Decoder<'static, std::io::BufReader<R>>>, String> {
    let decoder = zstd::stream::read::Decoder::new(compressed)
        .map_err(|error| format!("export stream is not zstd-framed: {error}"))?;
    Ok(CappedReader {
        inner: decoder,
        remaining: max_decompressed_bytes,
    })
}

fn map_cap_error(error: std::io::Error, max_decompressed_bytes: u64) -> String {
    if error.to_string().contains(DECOMPRESSED_CAP_SENTINEL) {
        format!("{DECOMPRESSED_CAP_SENTINEL} ({max_decompressed_bytes} bytes)")
    } else {
        format!("export stream read failed: {error}")
    }
}
