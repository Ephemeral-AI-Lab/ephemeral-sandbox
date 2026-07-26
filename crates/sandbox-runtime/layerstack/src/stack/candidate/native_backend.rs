use std::fmt;
use std::path::Path;

use sandbox_runtime_layerstack_core::RootId;

use super::generation::CapabilityProfile;
use super::tree::PersistentPages;

pub(crate) const MAX_HYDRATION_STREAM_BYTES: usize = 256 * 1024;
pub(crate) const MIN_HYDRATION_STREAM_BYTES: usize = 32 * 1024 + 15 + 256;
pub(crate) const CAP_XATTR: u64 = 1 << 0;
pub(crate) const CAP_SPARSE: u64 = 1 << 1;
pub(crate) const CAP_HARDLINK: u64 = 1 << 2;
pub(crate) const CAP_SYMLINK: u64 = 1 << 3;
pub(crate) const CAP_DEVICE: u64 = 1 << 4;
pub(crate) const CAP_FIFO: u64 = 1 << 5;
const BUILD_METADATA_RESERVATION_BYTES: u64 = 1024 * 1024;
const KNOWN_CAPABILITIES: u64 =
    CAP_XATTR | CAP_SPARSE | CAP_HARDLINK | CAP_SYMLINK | CAP_DEVICE | CAP_FIFO;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NativeBuildResult {
    pub(crate) native_tree_sha256: String,
    pub(crate) entry_count: u64,
    pub(crate) logical_bytes: u64,
    pub(crate) allocated_bytes: u64,
    pub(crate) maximum_buffer_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NativePreflight {
    pub(crate) required_capabilities: CapabilityProfile,
    pub(crate) predicted_allocated_bytes: u64,
    pub(crate) build_reservation_bytes: u64,
}

pub(crate) struct NativeReconstructionResources<'a> {
    pub(crate) hydration_byte_permit_bytes: usize,
    pub(crate) metadata_queue_depth: usize,
    pub(crate) target: &'a crate::supervisor::MaterializationTarget,
    pub(crate) observation: Option<&'a crate::stack::HiddenValidationObservation>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum NativeBackendError {
    Unsupported(String),
    Capability(String),
    Invalid(String),
    Limit(String),
    Io(String),
    Tree(String),
    Cancelled(String),
}

impl fmt::Display for NativeBackendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(message) => {
                write!(formatter, "native backend unsupported: {message}")
            }
            Self::Capability(message) => {
                write!(formatter, "native backend capability mismatch: {message}")
            }
            Self::Invalid(message) => {
                write!(formatter, "invalid native materialization: {message}")
            }
            Self::Limit(message) => write!(formatter, "native materialization limit: {message}"),
            Self::Io(message) => write!(formatter, "native materialization I/O: {message}"),
            Self::Tree(message) => write!(formatter, "native materialization tree: {message}"),
            Self::Cancelled(message) => {
                write!(formatter, "native materialization cancelled: {message}")
            }
        }
    }
}

impl std::error::Error for NativeBackendError {}

impl From<std::io::Error> for NativeBackendError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct NativeBackend;

impl NativeBackend {
    pub(crate) const fn new() -> Self {
        Self
    }

    pub(crate) fn build_reservation_from_verified_target(
        &self,
        allocated_bytes: u64,
    ) -> Result<u64, NativeBackendError> {
        let staging_bytes = allocated_bytes
            .checked_add(19)
            .ok_or_else(|| NativeBackendError::Limit("workspace staging reservation".to_owned()))?
            / 20;
        allocated_bytes
            .checked_add(staging_bytes)
            .and_then(|value| value.checked_add(BUILD_METADATA_RESERVATION_BYTES))
            .ok_or_else(|| NativeBackendError::Limit("workspace build reservation".to_owned()))
    }

    /// Validate only the capability plan recorded in a selected generation.
    ///
    /// Warm activation may authenticate bounded selector/manifest metadata and
    /// the native provider profile, but it must not reopen the logical object
    /// graph or hash carrier contents.
    pub(crate) fn validate_warm_capabilities(
        &self,
        required: &CapabilityProfile,
        provided: &CapabilityProfile,
    ) -> Result<(), NativeBackendError> {
        let runtime = self.provided_capabilities();
        let required_profile_is_canonical = required.raw_byte_names
            && required.exact_metadata
            && required.feature_bits & !KNOWN_CAPABILITIES == 0
            && required.sparse_files == (required.feature_bits & CAP_SPARSE != 0)
            && required.hardlinks == (required.feature_bits & CAP_HARDLINK != 0)
            && required.symlinks == (required.feature_bits & CAP_SYMLINK != 0)
            && required.devices == (required.feature_bits & CAP_DEVICE != 0)
            && required.fifos == (required.feature_bits & CAP_FIFO != 0);
        if !required_profile_is_canonical
            || provided != &runtime
            || required.feature_bits & !provided.feature_bits != 0
            || (required.raw_byte_names && !provided.raw_byte_names)
            || (required.exact_metadata && !provided.exact_metadata)
            || (required.sparse_files && !provided.sparse_files)
            || (required.hardlinks && !provided.hardlinks)
            || (required.symlinks && !provided.symlinks)
            || (required.devices && !provided.devices)
            || (required.fifos && !provided.fifos)
        {
            return Err(NativeBackendError::Capability(
                "selected native capability plan is unsupported or non-canonical".to_owned(),
            ));
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use std::collections::{BTreeMap, BTreeSet};
    use std::ffi::OsStr;
    use std::fs::File;
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::os::fd::{AsFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{fchown, lchown, FileExt, PermissionsExt};

    use rayon::prelude::*;
    use rustix::fs::{
        AtFlags, FileType, Mode, OFlags, Stat, StatExt, Timespec, Timestamps, XattrFlags,
    };
    use sandbox_runtime_layerstack_core::{FileNodeId, HardlinkGroupIdV3};
    use sha2::{Digest, Sha256};

    use super::{
        CapabilityProfile, NativeBackend, NativeBackendError, NativeBuildResult, NativePreflight,
        NativeReconstructionResources, Path, PersistentPages, RootId, CAP_DEVICE, CAP_FIFO,
        CAP_HARDLINK, CAP_SPARSE, CAP_SYMLINK, CAP_XATTR, KNOWN_CAPABILITIES,
        MAX_HYDRATION_STREAM_BYTES,
    };
    use crate::lock::{assert_writer_lock_allows, WriterLockForbiddenWork};
    use crate::stack::candidate::object_store::{
        LooseObjectStore, MAX_CHUNK_BYTES, RECORD_HEADER_BYTES,
    };
    use crate::stack::candidate::tree::{
        FileKindV3, MaterializationNodeV3, MetadataV3, SegmentDescriptor, SegmentKind, TreeError,
    };
    use crate::stack::{HiddenQueuedWork, HiddenTaskWork, HiddenValidationObservation};
    use crate::supervisor::{MaterializationTarget, MetadataQueue};
    use crate::Sha256Digest;

    const MAX_DEPTH: u8 = 64;
    const MAX_DIRECTORY_ENTRIES: usize = 4096;
    const MAX_FILE_SEGMENTS: usize = 4096;
    const MAX_HARDLINK_GROUPS: usize = 4096;
    const MAX_XATTRS: usize = 256;
    const MAX_XATTR_LIST_BYTES: usize = 256 * 1024;
    const MAX_XATTR_VALUE_BYTES: usize = 256 * 1024;
    const ZERO_BUFFER_BYTES: usize = 32 * 1024;
    const HYDRATION_ITEM_BOOKKEEPING_BYTES: usize = 256;
    impl NativeBackend {
        pub(crate) fn provided_capabilities(&self) -> CapabilityProfile {
            CapabilityProfile {
                feature_bits: KNOWN_CAPABILITIES,
                raw_byte_names: true,
                exact_metadata: true,
                sparse_files: true,
                hardlinks: true,
                symlinks: true,
                devices: true,
                fifos: true,
            }
        }

        pub(crate) fn preflight(
            &self,
            pages: &mut PersistentPages<'_>,
            root: RootId,
            allocation_unit: u64,
        ) -> Result<NativePreflight, NativeBackendError> {
            assert_writer_lock_allows(WriterLockForbiddenWork::TreeWalk);
            if allocation_unit == 0 {
                return Err(NativeBackendError::Limit(
                    "filesystem allocation unit".to_owned(),
                ));
            }
            let descriptor = pages
                .root_descriptor(root)
                .map_err(|error| NativeBackendError::Tree(error.to_string()))?;
            if descriptor.chunk_profile != 1 {
                return Err(NativeBackendError::Capability(
                    "unsupported chunk profile".to_owned(),
                ));
            }
            if descriptor.required_capabilities & !KNOWN_CAPABILITIES != 0 {
                return Err(NativeBackendError::Capability(
                    "root requires unknown feature bits".to_owned(),
                ));
            }
            let effective_uid = rustix::process::geteuid();
            let effective_gid = rustix::process::getegid();
            let mut hardlink_counts = BTreeMap::new();
            let mut accounting = PreflightAccounting {
                allocation_unit,
                predicted_allocated_bytes: 0,
            };
            self.preflight_node(
                pages,
                descriptor.root_file,
                &[],
                0,
                effective_uid.as_raw(),
                effective_gid.as_raw(),
                effective_uid.is_root(),
                descriptor.required_capabilities,
                &mut hardlink_counts,
                &mut accounting,
            )?;
            if hardlink_counts
                .values()
                .any(|(expected, observed)| expected != observed)
            {
                return Err(NativeBackendError::Invalid(
                    "hardlink group paths differ from logical tree".to_owned(),
                ));
            }
            let build_reservation_bytes =
                self.build_reservation_from_verified_target(accounting.predicted_allocated_bytes)?;
            Ok(NativePreflight {
                required_capabilities: CapabilityProfile {
                    feature_bits: descriptor.required_capabilities,
                    raw_byte_names: true,
                    exact_metadata: true,
                    sparse_files: descriptor.required_capabilities & CAP_SPARSE != 0,
                    hardlinks: descriptor.required_capabilities & CAP_HARDLINK != 0,
                    symlinks: descriptor.required_capabilities & CAP_SYMLINK != 0,
                    devices: descriptor.required_capabilities & CAP_DEVICE != 0,
                    fifos: descriptor.required_capabilities & CAP_FIFO != 0,
                },
                predicted_allocated_bytes: accounting.predicted_allocated_bytes,
                build_reservation_bytes,
            })
        }

        pub(crate) fn reconstruct_bounded<C>(
            &self,
            pages: &mut PersistentPages<'_>,
            root: RootId,
            carrier: &Path,
            resources: NativeReconstructionResources<'_>,
            check: C,
        ) -> Result<NativeBuildResult, NativeBackendError>
        where
            C: FnMut() -> Result<(), NativeBackendError>,
        {
            let NativeReconstructionResources {
                hydration_byte_permit_bytes,
                metadata_queue_depth,
                target,
                observation,
            } = resources;
            assert_writer_lock_allows(WriterLockForbiddenWork::TreeWalk);
            if target.reserved_permits().0 < hydration_byte_permit_bytes {
                return Err(NativeBackendError::Limit(
                    "hydration bytes exceed admitted target permits".to_owned(),
                ));
            }
            let mut check = check;
            if hydration_byte_permit_bytes == 0
                || hydration_byte_permit_bytes > MAX_HYDRATION_STREAM_BYTES
            {
                return Err(NativeBackendError::Limit(
                    "hydration byte permit".to_owned(),
                ));
            }
            check()?;
            if std::fs::symlink_metadata(carrier).is_ok() {
                return Err(NativeBackendError::Invalid(
                    "work carrier already exists".to_owned(),
                ));
            }
            let parent = carrier.parent().ok_or_else(|| {
                NativeBackendError::Invalid("work carrier has no parent".to_owned())
            })?;
            std::fs::create_dir_all(parent)?;
            std::fs::create_dir(carrier)?;
            let root_fd = open_dir(carrier)?;
            let descriptor = pages
                .root_descriptor(root)
                .map_err(|error| NativeBackendError::Tree(error.to_string()))?;
            let root_node = pages
                .materialization_node(descriptor.root_file)
                .map_err(|error| NativeBackendError::Tree(error.to_string()))?;
            if root_node.kind != FileKindV3::Directory {
                return Err(NativeBackendError::Invalid(
                    "root file node is not a directory".to_owned(),
                ));
            }
            let mut context = BuildContext {
                root_fd: &root_fd,
                carrier,
                hardlinks: BTreeMap::new(),
                hasher: Sha256::new(),
                entry_count: 0,
                logical_bytes: 0,
                allocated_bytes: 0,
                maximum_buffer_bytes: 0,
                hydration_byte_permit_bytes,
                metadata_queue_depth,
                target,
                observation: observation.cloned(),
            };
            context.observe_node(&[], descriptor.root_file);
            self.populate_directory(pages, &[], &root_node, 0, &mut context, &mut check)?;
            apply_fd_metadata(&root_fd, &root_node.metadata)?;
            context.observe_allocated(&rustix::fs::fstat(&root_fd).map_err(io_error)?)?;
            // This is the one durability boundary for the complete carrier.
            // `syncfs` covers every file and directory populated above, so
            // flushing each inode first only duplicates the same I/O.
            rustix::fs::syncfs(&root_fd).map_err(io_error)?;
            let BuildContext {
                hasher,
                entry_count,
                logical_bytes,
                allocated_bytes,
                maximum_buffer_bytes,
                ..
            } = context;
            Ok(NativeBuildResult {
                native_tree_sha256: hex(&hasher.finalize()),
                entry_count,
                logical_bytes,
                allocated_bytes,
                maximum_buffer_bytes,
            })
        }

        pub(crate) fn verify(
            &self,
            pages: &mut PersistentPages<'_>,
            root: RootId,
            carrier: &Path,
        ) -> Result<NativeBuildResult, NativeBackendError> {
            assert_writer_lock_allows(WriterLockForbiddenWork::PayloadVerification);
            let descriptor = pages
                .root_descriptor(root)
                .map_err(|error| NativeBackendError::Tree(error.to_string()))?;
            let root_node = pages
                .materialization_node(descriptor.root_file)
                .map_err(|error| NativeBackendError::Tree(error.to_string()))?;
            if root_node.kind != FileKindV3::Directory {
                return Err(NativeBackendError::Invalid(
                    "root file node is not a directory".to_owned(),
                ));
            }
            let root_fd = open_dir(carrier)?;
            let mut context = VerifyContext {
                root_fd: &root_fd,
                carrier,
                hardlinks: BTreeMap::new(),
                inodes: BTreeMap::new(),
                hasher: Sha256::new(),
                entry_count: 0,
                logical_bytes: 0,
                allocated_bytes: 0,
                maximum_buffer_bytes: 0,
            };
            context.observe_node(&[], descriptor.root_file);
            verify_open_metadata(&root_fd, carrier, &root_node, &mut context)?;
            self.verify_directory(pages, &[], &root_node, 0, &mut context)?;
            context.finish_hardlinks()?;
            Ok(NativeBuildResult {
                native_tree_sha256: hex(&context.hasher.finalize()),
                entry_count: context.entry_count,
                logical_bytes: context.logical_bytes,
                allocated_bytes: context.allocated_bytes,
                maximum_buffer_bytes: context.maximum_buffer_bytes,
            })
        }

        #[allow(clippy::too_many_arguments)]
        fn preflight_node(
            &self,
            pages: &mut PersistentPages<'_>,
            file: FileNodeId,
            relative: &[u8],
            depth: u8,
            effective_uid: u32,
            effective_gid: u32,
            privileged: bool,
            required_capabilities: u64,
            hardlink_counts: &mut BTreeMap<HardlinkGroupIdV3, (usize, usize)>,
            accounting: &mut PreflightAccounting,
        ) -> Result<(), NativeBackendError> {
            if depth > MAX_DEPTH {
                return Err(NativeBackendError::Limit("path depth".to_owned()));
            }
            let node = pages
                .materialization_node(file)
                .map_err(|error| NativeBackendError::Tree(error.to_string()))?;
            accounting.observe_node(&node, relative)?;
            require_declared_capability(
                !node.metadata.xattrs.is_empty(),
                required_capabilities,
                CAP_XATTR,
                "xattr",
            )?;
            require_declared_capability(
                node.hardlink.is_some(),
                required_capabilities,
                CAP_HARDLINK,
                "hardlink",
            )?;
            if let Some(group) = node.hardlink {
                if hardlink_counts.len() >= MAX_HARDLINK_GROUPS
                    && !hardlink_counts.contains_key(&group)
                {
                    return Err(NativeBackendError::Limit("hardlink group count".to_owned()));
                }
                let expected = pages
                    .hardlink_group(group)
                    .map_err(|error| NativeBackendError::Tree(error.to_string()))?;
                if !expected.iter().any(|path| path == relative) {
                    return Err(NativeBackendError::Invalid(
                        "hardlink node path is absent from its group".to_owned(),
                    ));
                }
                let counts = hardlink_counts.entry(group).or_insert((expected.len(), 0));
                if counts.0 != expected.len() {
                    return Err(NativeBackendError::Invalid(
                        "hardlink group length changed during preflight".to_owned(),
                    ));
                }
                counts.1 = counts.1.saturating_add(1);
            }
            require_declared_capability(
                node.kind == FileKindV3::Symlink,
                required_capabilities,
                CAP_SYMLINK,
                "symlink",
            )?;
            require_declared_capability(
                node.kind == FileKindV3::Device,
                required_capabilities,
                CAP_DEVICE,
                "device",
            )?;
            require_declared_capability(
                node.kind == FileKindV3::Fifo,
                required_capabilities,
                CAP_FIFO,
                "FIFO",
            )?;
            if let Some(segments) = node.segments {
                let uses_sparse_hole = pages
                    .reconstruct_segments(segments)
                    .map_err(|error| NativeBackendError::Tree(error.to_string()))?
                    .iter()
                    .any(|segment| segment.kind == SegmentKind::Hole);
                require_declared_capability(
                    uses_sparse_hole,
                    required_capabilities,
                    CAP_SPARSE,
                    "sparse hole",
                )?;
            }
            if (!privileged
                && (node.metadata.uid != effective_uid || node.metadata.gid != effective_gid))
                || (!privileged && matches!(node.kind, FileKindV3::Device))
            {
                return Err(NativeBackendError::Capability(
                    "exact ownership or device reconstruction requires privilege".to_owned(),
                ));
            }
            if node.kind == FileKindV3::Symlink && !node.metadata.xattrs.is_empty() {
                let supported_namespace = node
                    .metadata
                    .xattrs
                    .iter()
                    .all(|(key, _)| key.starts_with(b"user."));
                if !supported_namespace {
                    return Err(NativeBackendError::Capability(
                        "symlink xattr namespace is not materializable".to_owned(),
                    ));
                }
            }
            if let Some(directory) = node.directory {
                let entries = pages
                    .directory_entries(directory, MAX_DIRECTORY_ENTRIES)
                    .map_err(|error| NativeBackendError::Tree(error.to_string()))?;
                for entry in entries {
                    let child = child_path(relative, &entry.name)?;
                    self.preflight_node(
                        pages,
                        entry.file,
                        &child,
                        depth.saturating_add(1),
                        effective_uid,
                        effective_gid,
                        privileged,
                        required_capabilities,
                        hardlink_counts,
                        accounting,
                    )?;
                }
            }
            Ok(())
        }

        #[allow(clippy::too_many_arguments)]
        fn populate_directory<C>(
            &self,
            pages: &mut PersistentPages<'_>,
            relative: &[u8],
            directory_node: &MaterializationNodeV3,
            depth: u8,
            context: &mut BuildContext<'_>,
            check: &mut C,
        ) -> Result<(), NativeBackendError>
        where
            C: FnMut() -> Result<(), NativeBackendError>,
        {
            if depth > MAX_DEPTH {
                return Err(NativeBackendError::Limit("path depth".to_owned()));
            }
            let directory = directory_node.directory.ok_or_else(|| {
                NativeBackendError::Invalid("directory node has no tree page".to_owned())
            })?;
            let entries = pages
                .directory_entries(directory, MAX_DIRECTORY_ENTRIES)
                .map_err(|error| NativeBackendError::Tree(error.to_string()))?;
            for entry in entries {
                check()?;
                let directory_fd = open_relative_dir(context.root_fd, relative)?;
                let path = child_path(relative, &entry.name)?;
                let node = pages
                    .materialization_node(entry.file)
                    .map_err(|error| NativeBackendError::Tree(error.to_string()))?;
                context.observe_node(&path, entry.file);
                match node.kind {
                    FileKindV3::Directory => {
                        rustix::fs::mkdirat(
                            &directory_fd,
                            OsStr::from_bytes(&entry.name),
                            Mode::from_raw_mode(0o700),
                        )
                        .map_err(io_error)?;
                        let child = open_dir_at(&directory_fd, &entry.name)?;
                        drop(directory_fd);
                        drop(child);
                        self.populate_directory(
                            pages,
                            &path,
                            &node,
                            depth.saturating_add(1),
                            context,
                            check,
                        )?;
                        let child = open_relative_dir(context.root_fd, &path)?;
                        apply_fd_metadata(&child, &node.metadata)?;
                        context.observe_allocated(&rustix::fs::fstat(&child).map_err(io_error)?)?;
                    }
                    FileKindV3::Regular => self.emit_regular(
                        pages,
                        &directory_fd,
                        &entry.name,
                        &path,
                        &node,
                        context,
                        check,
                    )?,
                    FileKindV3::Symlink => {
                        let target = node.symlink_target.as_deref().ok_or_else(|| {
                            NativeBackendError::Invalid("symlink target missing".to_owned())
                        })?;
                        rustix::fs::symlinkat(
                            OsStr::from_bytes(target),
                            &directory_fd,
                            OsStr::from_bytes(&entry.name),
                        )
                        .map_err(io_error)?;
                        apply_symlink_metadata(context.carrier, &path, &node.metadata)?;
                        context.observe_allocated(
                            &rustix::fs::statat(
                                &directory_fd,
                                OsStr::from_bytes(&entry.name),
                                AtFlags::SYMLINK_NOFOLLOW,
                            )
                            .map_err(io_error)?,
                        )?;
                    }
                    FileKindV3::Device => {
                        let device = rustix::fs::makedev(
                            node.device_major.ok_or_else(|| {
                                NativeBackendError::Invalid("device major missing".to_owned())
                            })?,
                            node.device_minor.ok_or_else(|| {
                                NativeBackendError::Invalid("device minor missing".to_owned())
                            })?,
                        );
                        rustix::fs::mknodat(
                            &directory_fd,
                            OsStr::from_bytes(&entry.name),
                            FileType::CharacterDevice,
                            Mode::from_raw_mode(node.metadata.mode & 0o7777),
                            device,
                        )
                        .map_err(io_error)?;
                        apply_path_metadata(context.carrier, &path, &node.metadata)?;
                        context.observe_allocated(
                            &rustix::fs::statat(
                                &directory_fd,
                                OsStr::from_bytes(&entry.name),
                                AtFlags::SYMLINK_NOFOLLOW,
                            )
                            .map_err(io_error)?,
                        )?;
                    }
                    FileKindV3::Fifo => {
                        rustix::fs::mknodat(
                            &directory_fd,
                            OsStr::from_bytes(&entry.name),
                            FileType::Fifo,
                            Mode::from_raw_mode(node.metadata.mode & 0o7777),
                            rustix::fs::makedev(0, 0),
                        )
                        .map_err(io_error)?;
                        apply_path_metadata(context.carrier, &path, &node.metadata)?;
                        context.observe_allocated(
                            &rustix::fs::statat(
                                &directory_fd,
                                OsStr::from_bytes(&entry.name),
                                AtFlags::SYMLINK_NOFOLLOW,
                            )
                            .map_err(io_error)?,
                        )?;
                    }
                }
            }
            Ok(())
        }

        #[allow(clippy::too_many_arguments)]
        fn emit_regular<C>(
            &self,
            pages: &mut PersistentPages<'_>,
            directory_fd: &OwnedFd,
            name: &[u8],
            path: &[u8],
            node: &MaterializationNodeV3,
            context: &mut BuildContext<'_>,
            check: &mut C,
        ) -> Result<(), NativeBackendError>
        where
            C: FnMut() -> Result<(), NativeBackendError>,
        {
            let logical_length = node.logical_length.ok_or_else(|| {
                NativeBackendError::Invalid("regular file length missing".to_owned())
            })?;
            if let Some(group) = node.hardlink {
                if let Some(existing) = context.hardlinks.get(&group) {
                    rustix::fs::linkat(
                        context.root_fd,
                        OsStr::from_bytes(existing),
                        directory_fd,
                        OsStr::from_bytes(name),
                        AtFlags::empty(),
                    )
                    .map_err(io_error)?;
                    context.logical_bytes = context
                        .logical_bytes
                        .checked_add(logical_length)
                        .ok_or_else(|| {
                            NativeBackendError::Limit("logical byte count".to_owned())
                        })?;
                    return Ok(());
                }
                if context.hardlinks.len() >= MAX_HARDLINK_GROUPS {
                    return Err(NativeBackendError::Limit("hardlink group count".to_owned()));
                }
                context.hardlinks.insert(group, path.to_vec());
            }
            let file_fd = rustix::fs::openat(
                directory_fd,
                OsStr::from_bytes(name),
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_raw_mode(0o600),
            )
            .map_err(io_error)?;
            let mut file = File::from(file_fd);
            let segment_root = node.segments.ok_or_else(|| {
                NativeBackendError::Invalid("regular file segments missing".to_owned())
            })?;
            let store = pages.object_store();
            let mut batch = context
                .target
                .metadata_queue::<HydrationJob>(context.metadata_queue_depth)
                .map_err(|error| NativeBackendError::Limit(error.to_string()))?;
            let mut batch_bytes = 0_usize;
            let mut segment_count = 0_usize;
            let mut chunk_objects_read = 0_u64;
            let mut chunk_bytes_read = 0_u64;
            let mut deferred_error = None;
            file.set_len(logical_length)?;
            let stream_result = pages.stream_segments(segment_root, |segment| {
                if deferred_error.is_some() {
                    return Err(TreeError::Invalid("native segment emission stopped"));
                }
                let result = (|| {
                    check()?;
                    segment_count = segment_count.checked_add(1).ok_or_else(|| {
                        NativeBackendError::Limit("regular file segment count".to_owned())
                    })?;
                    if segment_count > MAX_FILE_SEGMENTS {
                        return Err(NativeBackendError::Limit(
                            "regular file segment count".to_owned(),
                        ));
                    }
                    let ending = segment.offset.checked_add(segment.length).ok_or_else(|| {
                        NativeBackendError::Invalid("segment range overflow".to_owned())
                    })?;
                    if ending > logical_length {
                        return Err(NativeBackendError::Invalid(
                            "segment exceeds logical file length".to_owned(),
                        ));
                    }
                    let reservation = hydration_reservation(segment)?;
                    if reservation > context.hydration_byte_permit_bytes {
                        return Err(NativeBackendError::Limit(
                            "one hydration item exceeds its byte permit".to_owned(),
                        ));
                    }
                    let next_batch_bytes =
                        batch_bytes.checked_add(reservation).ok_or_else(|| {
                            NativeBackendError::Limit("hydration batch byte accounting".to_owned())
                        })?;
                    if !batch.is_empty()
                        && (batch.is_full()
                            || next_batch_bytes > context.hydration_byte_permit_bytes)
                    {
                        flush_hydration_batch(
                            &store,
                            &mut file,
                            &mut batch,
                            &mut batch_bytes,
                            context,
                            check,
                            &mut chunk_objects_read,
                            &mut chunk_bytes_read,
                        )?;
                    }
                    batch_bytes = batch_bytes.checked_add(reservation).ok_or_else(|| {
                        NativeBackendError::Limit("hydration batch byte accounting".to_owned())
                    })?;
                    let queued = context.observation.as_ref().map(|observation| {
                        observation.enqueue(HYDRATION_ITEM_BOOKKEEPING_BYTES as u64)
                    });
                    batch
                        .push(
                            HydrationJob { segment, queued },
                            HYDRATION_ITEM_BOOKKEEPING_BYTES,
                        )
                        .map_err(|error| NativeBackendError::Limit(error.to_string()))?;
                    if batch.is_full() {
                        flush_hydration_batch(
                            &store,
                            &mut file,
                            &mut batch,
                            &mut batch_bytes,
                            context,
                            check,
                            &mut chunk_objects_read,
                            &mut chunk_bytes_read,
                        )?;
                    }
                    Ok(())
                })();
                match result {
                    Ok(()) => Ok(()),
                    Err(error) => {
                        deferred_error = Some(error);
                        Err(TreeError::Invalid("native segment emission stopped"))
                    }
                }
            });
            if let Some(error) = deferred_error {
                return Err(error);
            }
            stream_result.map_err(|error| NativeBackendError::Tree(error.to_string()))?;
            flush_hydration_batch(
                &store,
                &mut file,
                &mut batch,
                &mut batch_bytes,
                context,
                check,
                &mut chunk_objects_read,
                &mut chunk_bytes_read,
            )?;
            pages.record_authenticated_chunk_reads(chunk_objects_read, chunk_bytes_read);
            context.logical_bytes = context
                .logical_bytes
                .checked_add(logical_length)
                .ok_or_else(|| NativeBackendError::Limit("logical byte count".to_owned()))?;
            apply_file_metadata(&file, &node.metadata)?;
            context.observe_allocated(&rustix::fs::fstat(&file).map_err(io_error)?)?;
            Ok(())
        }

        fn verify_directory(
            &self,
            pages: &mut PersistentPages<'_>,
            relative: &[u8],
            directory_node: &MaterializationNodeV3,
            depth: u8,
            context: &mut VerifyContext<'_>,
        ) -> Result<(), NativeBackendError> {
            if depth > MAX_DEPTH {
                return Err(NativeBackendError::Limit("path depth".to_owned()));
            }
            let directory = directory_node.directory.ok_or_else(|| {
                NativeBackendError::Invalid("directory node has no tree page".to_owned())
            })?;
            let entries = pages
                .directory_entries(directory, MAX_DIRECTORY_ENTRIES)
                .map_err(|error| NativeBackendError::Tree(error.to_string()))?;
            let mut expected_names = BTreeSet::new();
            for entry in &entries {
                if !expected_names.insert(entry.name.clone()) {
                    return Err(NativeBackendError::Invalid(
                        "logical directory contains a duplicate name".to_owned(),
                    ));
                }
            }
            let directory_fd = open_relative_dir(context.root_fd, relative)?;
            let actual_names = directory_names(&directory_fd, context)?;
            drop(directory_fd);
            if actual_names != expected_names {
                return Err(NativeBackendError::Invalid(
                    "native directory entries differ from logical tree".to_owned(),
                ));
            }

            for entry in entries {
                let directory_fd = open_relative_dir(context.root_fd, relative)?;
                let path = child_path(relative, &entry.name)?;
                let node = pages
                    .materialization_node(entry.file)
                    .map_err(|error| NativeBackendError::Tree(error.to_string()))?;
                context.observe_node(&path, entry.file);
                let stat = rustix::fs::statat(
                    &directory_fd,
                    OsStr::from_bytes(&entry.name),
                    AtFlags::SYMLINK_NOFOLLOW,
                )
                .map_err(io_error)?;
                verify_stat(&stat, &node)?;
                if node.kind != FileKindV3::Regular {
                    context.observe_allocated(&stat)?;
                }
                verify_path_xattrs(context.carrier, &path, &node.metadata, context)?;
                match node.kind {
                    FileKindV3::Directory => {
                        let child = open_dir_at(&directory_fd, &entry.name)?;
                        verify_open_identity(&stat, &child)?;
                        drop(directory_fd);
                        drop(child);
                        self.verify_directory(
                            pages,
                            &path,
                            &node,
                            depth.saturating_add(1),
                            context,
                        )?;
                    }
                    FileKindV3::Regular => {
                        let child = rustix::fs::openat(
                            &directory_fd,
                            OsStr::from_bytes(&entry.name),
                            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                            Mode::empty(),
                        )
                        .map_err(io_error)?;
                        verify_open_identity(&stat, &child)?;
                        context.observe_hardlink(&stat, node.hardlink)?;
                        verify_regular(pages, File::from(child), &node, context)?;
                    }
                    FileKindV3::Symlink => {
                        let expected = node.symlink_target.as_deref().ok_or_else(|| {
                            NativeBackendError::Invalid("symlink target missing".to_owned())
                        })?;
                        let actual = rustix::fs::readlinkat(
                            &directory_fd,
                            OsStr::from_bytes(&entry.name),
                            Vec::new(),
                        )
                        .map_err(io_error)?;
                        if actual.as_bytes() != expected {
                            return Err(NativeBackendError::Invalid(
                                "native symlink target differs from logical tree".to_owned(),
                            ));
                        }
                    }
                    FileKindV3::Device => {
                        let expected_major = node.device_major.ok_or_else(|| {
                            NativeBackendError::Invalid("device major missing".to_owned())
                        })?;
                        let expected_minor = node.device_minor.ok_or_else(|| {
                            NativeBackendError::Invalid("device minor missing".to_owned())
                        })?;
                        if rustix::fs::major(stat.st_rdev) != expected_major
                            || rustix::fs::minor(stat.st_rdev) != expected_minor
                        {
                            return Err(NativeBackendError::Invalid(
                                "native device number differs from logical tree".to_owned(),
                            ));
                        }
                    }
                    FileKindV3::Fifo => {}
                }
            }
            Ok(())
        }
    }

    struct PreflightAccounting {
        allocation_unit: u64,
        predicted_allocated_bytes: u64,
    }

    impl PreflightAccounting {
        fn observe_node(
            &mut self,
            node: &MaterializationNodeV3,
            relative: &[u8],
        ) -> Result<(), NativeBackendError> {
            self.add(self.allocation_unit)?;
            self.add_rounded(
                u64::try_from(relative.len())
                    .map_err(|_| NativeBackendError::Limit("path length".to_owned()))?
                    .checked_add(32)
                    .ok_or_else(|| {
                        NativeBackendError::Limit("directory entry accounting".to_owned())
                    })?,
            )?;
            if let Some(logical_length) = node.logical_length {
                self.add_rounded(logical_length)?;
            }
            if let Some(target) = &node.symlink_target {
                self.add_rounded(
                    u64::try_from(target.len())
                        .map_err(|_| NativeBackendError::Limit("symlink length".to_owned()))?,
                )?;
            }
            for (key, value) in &node.metadata.xattrs {
                let bytes = u64::try_from(key.len())
                    .ok()
                    .and_then(|key| {
                        u64::try_from(value.len())
                            .ok()
                            .and_then(|value| key.checked_add(value))
                    })
                    .and_then(|value| value.checked_add(16))
                    .ok_or_else(|| {
                        NativeBackendError::Limit("xattr allocation accounting".to_owned())
                    })?;
                self.add_rounded(bytes)?;
            }
            Ok(())
        }

        fn add_rounded(&mut self, bytes: u64) -> Result<(), NativeBackendError> {
            if bytes == 0 {
                return Ok(());
            }
            let rounded = bytes
                .checked_add(self.allocation_unit - 1)
                .map(|value| value / self.allocation_unit)
                .and_then(|units| units.checked_mul(self.allocation_unit))
                .ok_or_else(|| NativeBackendError::Limit("allocated byte prediction".to_owned()))?;
            self.add(rounded)
        }

        fn add(&mut self, bytes: u64) -> Result<(), NativeBackendError> {
            self.predicted_allocated_bytes = self
                .predicted_allocated_bytes
                .checked_add(bytes)
                .ok_or_else(|| NativeBackendError::Limit("allocated byte prediction".to_owned()))?;
            Ok(())
        }
    }

    fn require_declared_capability(
        used: bool,
        required_capabilities: u64,
        capability: u64,
        feature: &str,
    ) -> Result<(), NativeBackendError> {
        if used && required_capabilities & capability == 0 {
            return Err(NativeBackendError::Capability(format!(
                "root uses undeclared {feature} capability"
            )));
        }
        Ok(())
    }

    struct HydrationJob {
        segment: SegmentDescriptor,
        queued: Option<HiddenQueuedWork>,
    }

    struct HydratedSegment {
        segment: SegmentDescriptor,
        encoded_len: Option<usize>,
        _task: Option<HiddenTaskWork>,
    }

    fn hydration_reservation(segment: SegmentDescriptor) -> Result<usize, NativeBackendError> {
        let encoded = match segment.kind {
            SegmentKind::Chunk(_) => {
                let payload = usize::try_from(segment.length)
                    .map_err(|_| NativeBackendError::Limit("chunk length conversion".to_owned()))?;
                if !(1..=MAX_CHUNK_BYTES).contains(&payload) {
                    return Err(NativeBackendError::Limit(
                        "chunk hydration length".to_owned(),
                    ));
                }
                payload
                    .checked_add(RECORD_HEADER_BYTES)
                    .ok_or_else(|| NativeBackendError::Limit("chunk hydration length".to_owned()))?
            }
            SegmentKind::Zero | SegmentKind::Hole => 0,
        };
        Ok(encoded)
    }

    #[allow(clippy::too_many_arguments)]
    fn flush_hydration_batch<C>(
        store: &LooseObjectStore,
        file: &mut File,
        batch: &mut MetadataQueue<HydrationJob>,
        batch_bytes: &mut usize,
        context: &mut BuildContext<'_>,
        check: &mut C,
        chunk_objects_read: &mut u64,
        chunk_bytes_read: &mut u64,
    ) -> Result<(), NativeBackendError>
    where
        C: FnMut() -> Result<(), NativeBackendError>,
    {
        assert_writer_lock_allows(WriterLockForbiddenWork::ProviderPayloadIo);
        if batch.is_empty() {
            return Ok(());
        }
        check()?;
        let reserved_bytes = *batch_bytes;
        if reserved_bytes > context.hydration_byte_permit_bytes {
            return Err(NativeBackendError::Limit(
                "hydration batch exceeds its byte permit".to_owned(),
            ));
        }
        context.maximum_buffer_bytes = context
            .maximum_buffer_bytes
            .max(u64::try_from(reserved_bytes).unwrap_or(u64::MAX))
            .max(u64::try_from(batch.encoded_bytes()).unwrap_or(u64::MAX));

        let observation = context.observation.clone();
        let jobs = batch.take();
        *batch_bytes = 0;
        let positional_file: &File = file;
        let hydrate = |job| hydrate_job(job, store, positional_file, observation.as_ref());
        let hydrated = context
            .target
            .run_on_workers(|| {
                jobs.into_par_iter()
                    .map(hydrate)
                    .collect::<Result<Vec<_>, NativeBackendError>>()
            })
            .map_err(|error| {
                NativeBackendError::Invalid(format!("storage worker pool unavailable: {error}"))
            })??;

        for hydrated in hydrated {
            check()?;
            match hydrated.segment.kind {
                SegmentKind::Chunk(_) => {
                    let encoded_len = hydrated.encoded_len.ok_or_else(|| {
                        NativeBackendError::Invalid("hydrated chunk result is absent".to_owned())
                    })?;
                    *chunk_objects_read = chunk_objects_read.saturating_add(1);
                    *chunk_bytes_read = chunk_bytes_read
                        .saturating_add(u64::try_from(encoded_len).unwrap_or(u64::MAX));
                }
                SegmentKind::Zero => {
                    file.seek(SeekFrom::Start(hydrated.segment.offset))?;
                    write_zeroes(file, hydrated.segment.length, context)?;
                }
                SegmentKind::Hole => {}
            }
        }
        Ok(())
    }

    fn hydrate_job(
        job: HydrationJob,
        store: &LooseObjectStore,
        positional_file: &File,
        observation: Option<&HiddenValidationObservation>,
    ) -> Result<HydratedSegment, NativeBackendError> {
        let task = job.queued.map(HiddenQueuedWork::start);
        let _worker = observation.map(HiddenValidationObservation::begin_worker);
        let encoded_len = match job.segment.kind {
            SegmentKind::Chunk(id) => {
                let chunk = store
                    .load_authenticated_chunk(id, &mut Sha256Digest)
                    .map_err(|error| NativeBackendError::Tree(error.to_string()))?;
                if chunk.payload().len() as u64 != job.segment.length {
                    return Err(NativeBackendError::Invalid(
                        "chunk length does not match segment".to_owned(),
                    ));
                }
                positional_file.write_all_at(chunk.payload(), job.segment.offset)?;
                Some(chunk.encoded_len())
            }
            SegmentKind::Zero | SegmentKind::Hole => None,
        };
        Ok(HydratedSegment {
            segment: job.segment,
            encoded_len,
            _task: task,
        })
    }

    struct BuildContext<'a> {
        root_fd: &'a OwnedFd,
        carrier: &'a Path,
        hardlinks: BTreeMap<HardlinkGroupIdV3, Vec<u8>>,
        hasher: Sha256,
        entry_count: u64,
        logical_bytes: u64,
        allocated_bytes: u64,
        maximum_buffer_bytes: u64,
        hydration_byte_permit_bytes: usize,
        metadata_queue_depth: usize,
        target: &'a MaterializationTarget,
        observation: Option<HiddenValidationObservation>,
    }

    impl BuildContext<'_> {
        fn observe_node(&mut self, path: &[u8], file: FileNodeId) {
            self.hasher.update((path.len() as u64).to_be_bytes());
            self.hasher.update(path);
            self.hasher.update(file.digest().as_bytes());
            self.entry_count = self.entry_count.saturating_add(1);
        }

        fn observe_allocated(&mut self, stat: &Stat) -> Result<(), NativeBackendError> {
            add_allocated_bytes(&mut self.allocated_bytes, stat)
        }
    }

    struct VerifyContext<'a> {
        root_fd: &'a OwnedFd,
        carrier: &'a Path,
        hardlinks: BTreeMap<HardlinkGroupIdV3, HardlinkObservation>,
        inodes: BTreeMap<(u64, u64), Option<HardlinkGroupIdV3>>,
        hasher: Sha256,
        entry_count: u64,
        logical_bytes: u64,
        allocated_bytes: u64,
        maximum_buffer_bytes: u64,
    }

    #[derive(Clone, Copy)]
    struct HardlinkObservation {
        identity: (u64, u64),
        link_count: u64,
        observed: u64,
    }

    impl VerifyContext<'_> {
        fn observe_node(&mut self, path: &[u8], file: FileNodeId) {
            self.hasher.update((path.len() as u64).to_be_bytes());
            self.hasher.update(path);
            self.hasher.update(file.digest().as_bytes());
            self.entry_count = self.entry_count.saturating_add(1);
        }

        fn observe_allocated(&mut self, stat: &Stat) -> Result<(), NativeBackendError> {
            add_allocated_bytes(&mut self.allocated_bytes, stat)
        }

        fn observe_hardlink(
            &mut self,
            stat: &Stat,
            group: Option<HardlinkGroupIdV3>,
        ) -> Result<(), NativeBackendError> {
            let identity = (stat.st_dev, stat.st_ino);
            let link_count = u64::from(stat.st_nlink);
            match self.inodes.get(&identity) {
                Some(observed_group) if *observed_group != group => {
                    return Err(NativeBackendError::Invalid(
                        "native inode aliases distinct logical files".to_owned(),
                    ));
                }
                Some(_) => {}
                None => {
                    self.inodes.insert(identity, group);
                    self.observe_allocated(stat)?;
                }
            }
            let Some(group) = group else {
                if link_count != 1 {
                    return Err(NativeBackendError::Invalid(
                        "ungrouped native file has multiple hardlinks".to_owned(),
                    ));
                }
                return Ok(());
            };
            if self.hardlinks.len() >= MAX_HARDLINK_GROUPS && !self.hardlinks.contains_key(&group) {
                return Err(NativeBackendError::Limit("hardlink group count".to_owned()));
            }
            match self.hardlinks.get_mut(&group) {
                Some(observation) => {
                    if observation.identity != identity || observation.link_count != link_count {
                        return Err(NativeBackendError::Invalid(
                            "native hardlink topology differs from logical tree".to_owned(),
                        ));
                    }
                    observation.observed = observation.observed.saturating_add(1);
                }
                None => {
                    self.hardlinks.insert(
                        group,
                        HardlinkObservation {
                            identity,
                            link_count,
                            observed: 1,
                        },
                    );
                }
            }
            Ok(())
        }

        fn finish_hardlinks(&self) -> Result<(), NativeBackendError> {
            if self
                .hardlinks
                .values()
                .any(|observation| observation.observed != observation.link_count)
            {
                return Err(NativeBackendError::Invalid(
                    "native hardlink count differs from logical tree".to_owned(),
                ));
            }
            Ok(())
        }
    }

    fn open_dir(path: &Path) -> Result<OwnedFd, NativeBackendError> {
        rustix::fs::open(
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(io_error)
    }

    fn open_dir_at(parent: &OwnedFd, name: &[u8]) -> Result<OwnedFd, NativeBackendError> {
        rustix::fs::openat(
            parent,
            OsStr::from_bytes(name),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(io_error)
    }

    fn open_relative_dir(root: &OwnedFd, relative: &[u8]) -> Result<OwnedFd, NativeBackendError> {
        let mut current = open_dir_at(root, b".")?;
        if relative.is_empty() {
            return Ok(current);
        }
        for component in relative.split(|byte| *byte == b'/') {
            if component.is_empty() {
                return Err(NativeBackendError::Invalid(
                    "empty relative path component".to_owned(),
                ));
            }
            current = open_dir_at(&current, component)?;
        }
        Ok(current)
    }

    fn directory_names(
        directory_fd: &OwnedFd,
        context: &mut VerifyContext<'_>,
    ) -> Result<BTreeSet<Vec<u8>>, NativeBackendError> {
        let mut names = BTreeSet::new();
        let mut directory = rustix::fs::Dir::read_from(directory_fd).map_err(io_error)?;
        while let Some(entry) = directory.read() {
            let entry = entry.map_err(io_error)?;
            let name = entry.file_name().to_bytes();
            if name == b"." || name == b".." {
                continue;
            }
            child_path(&[], name)?;
            if names.len() >= MAX_DIRECTORY_ENTRIES || !names.insert(name.to_vec()) {
                return Err(NativeBackendError::Limit(
                    "native directory entry count".to_owned(),
                ));
            }
            context.maximum_buffer_bytes = context
                .maximum_buffer_bytes
                .max(names.iter().map(Vec::len).sum::<usize>() as u64);
        }
        Ok(names)
    }

    fn child_path(parent: &[u8], name: &[u8]) -> Result<Vec<u8>, NativeBackendError> {
        if name.is_empty() || name.len() > 255 || name.contains(&0) || name.contains(&b'/') {
            return Err(NativeBackendError::Invalid(
                "invalid raw path component".to_owned(),
            ));
        }
        let capacity = parent
            .len()
            .checked_add(name.len())
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| NativeBackendError::Limit("path length".to_owned()))?;
        if capacity > 4097 {
            return Err(NativeBackendError::Limit("path length".to_owned()));
        }
        let mut output = Vec::with_capacity(capacity);
        output.extend_from_slice(parent);
        if !parent.is_empty() {
            output.push(b'/');
        }
        output.extend_from_slice(name);
        Ok(output)
    }

    fn verify_open_identity<F: AsFd>(expected: &Stat, fd: F) -> Result<(), NativeBackendError> {
        let actual = rustix::fs::fstat(fd).map_err(io_error)?;
        if actual.st_dev != expected.st_dev || actual.st_ino != expected.st_ino {
            return Err(NativeBackendError::Invalid(
                "native entry changed while being verified".to_owned(),
            ));
        }
        Ok(())
    }

    fn verify_open_metadata<F: AsFd>(
        fd: F,
        path: &Path,
        node: &MaterializationNodeV3,
        context: &mut VerifyContext<'_>,
    ) -> Result<(), NativeBackendError> {
        let stat = rustix::fs::fstat(fd).map_err(io_error)?;
        verify_stat(&stat, node)?;
        context.observe_allocated(&stat)?;
        verify_path_xattrs(path, &[], &node.metadata, context)
    }

    fn verify_stat(stat: &Stat, node: &MaterializationNodeV3) -> Result<(), NativeBackendError> {
        let expected_kind = match node.kind {
            FileKindV3::Directory => FileType::Directory,
            FileKindV3::Regular => FileType::RegularFile,
            FileKindV3::Symlink => FileType::Symlink,
            FileKindV3::Device => FileType::CharacterDevice,
            FileKindV3::Fifo => FileType::Fifo,
        };
        let metadata = &node.metadata;
        if FileType::from_raw_mode(stat.st_mode) != expected_kind
            || stat.st_mode & 0o7777 != metadata.mode & 0o7777
            || stat.st_uid != metadata.uid
            || stat.st_gid != metadata.gid
            || stat.mtime() < 0
            || stat.mtime() as u64 != metadata.mtime_seconds
            || stat.st_mtime_nsec != u64::from(metadata.mtime_nanoseconds)
        {
            return Err(NativeBackendError::Invalid(
                "native type or metadata differs from logical tree".to_owned(),
            ));
        }
        Ok(())
    }

    fn verify_path_xattrs(
        carrier: &Path,
        relative: &[u8],
        expected: &MetadataV3,
        context: &mut VerifyContext<'_>,
    ) -> Result<(), NativeBackendError> {
        let path = if relative.is_empty() {
            carrier.to_path_buf()
        } else {
            carrier.join(OsStr::from_bytes(relative))
        };
        let list_size = rustix::fs::llistxattr(&path, &mut []).map_err(io_error)?;
        if list_size > MAX_XATTR_LIST_BYTES {
            return Err(NativeBackendError::Limit("xattr name bytes".to_owned()));
        }
        let mut list = vec![0; list_size];
        let listed = rustix::fs::llistxattr(&path, &mut list).map_err(io_error)?;
        list.truncate(listed);
        context.maximum_buffer_bytes = context
            .maximum_buffer_bytes
            .max(list.len().try_into().unwrap_or(u64::MAX));
        let mut actual = BTreeMap::new();
        for raw_name in list.split(|byte| *byte == 0) {
            if raw_name.is_empty() {
                continue;
            }
            if actual.len() >= MAX_XATTRS {
                return Err(NativeBackendError::Limit("xattr count".to_owned()));
            }
            let name: Vec<u8> = raw_name.iter().map(|byte| *byte as u8).collect();
            let value_size = rustix::fs::lgetxattr(&path, OsStr::from_bytes(&name), &mut [])
                .map_err(io_error)?;
            if value_size > MAX_XATTR_VALUE_BYTES {
                return Err(NativeBackendError::Limit("xattr value bytes".to_owned()));
            }
            let mut value = vec![0; value_size];
            let read = rustix::fs::lgetxattr(&path, OsStr::from_bytes(&name), &mut value)
                .map_err(io_error)?;
            value.truncate(read);
            context.maximum_buffer_bytes = context
                .maximum_buffer_bytes
                .max(value.len().try_into().unwrap_or(u64::MAX));
            if actual.insert(name, value).is_some() {
                return Err(NativeBackendError::Invalid(
                    "native xattr list contains a duplicate".to_owned(),
                ));
            }
        }
        let expected: BTreeMap<_, _> = expected.xattrs.iter().cloned().collect();
        if actual != expected {
            return Err(NativeBackendError::Invalid(
                "native xattrs differ from logical tree".to_owned(),
            ));
        }
        Ok(())
    }

    fn verify_regular(
        pages: &mut PersistentPages<'_>,
        mut file: File,
        node: &MaterializationNodeV3,
        context: &mut VerifyContext<'_>,
    ) -> Result<(), NativeBackendError> {
        let logical_length = node
            .logical_length
            .ok_or_else(|| NativeBackendError::Invalid("regular file length missing".to_owned()))?;
        let actual_length = file.metadata()?.len();
        if actual_length != logical_length {
            return Err(NativeBackendError::Invalid(
                "native file length differs from logical tree".to_owned(),
            ));
        }
        let segments = pages
            .reconstruct_segments(node.segments.ok_or_else(|| {
                NativeBackendError::Invalid("regular file segments missing".to_owned())
            })?)
            .map_err(|error| NativeBackendError::Tree(error.to_string()))?;
        if segments.len() > MAX_FILE_SEGMENTS {
            return Err(NativeBackendError::Limit(
                "regular file segment count".to_owned(),
            ));
        }
        let mut cursor = 0;
        for segment in segments {
            if segment.offset < cursor {
                return Err(NativeBackendError::Invalid(
                    "regular file segments overlap".to_owned(),
                ));
            }
            verify_zero_range(&mut file, cursor, segment.offset - cursor, context)?;
            let ending = segment
                .offset
                .checked_add(segment.length)
                .ok_or_else(|| NativeBackendError::Invalid("segment range overflow".to_owned()))?;
            if ending > logical_length {
                return Err(NativeBackendError::Invalid(
                    "segment exceeds logical file length".to_owned(),
                ));
            }
            match segment.kind {
                SegmentKind::Chunk(id) => {
                    let expected = pages
                        .load_chunk(id)
                        .map_err(|error| NativeBackendError::Tree(error.to_string()))?;
                    if expected.len() as u64 != segment.length {
                        return Err(NativeBackendError::Invalid(
                            "chunk length does not match segment".to_owned(),
                        ));
                    }
                    verify_bytes(&mut file, segment.offset, &expected, context)?;
                }
                SegmentKind::Zero | SegmentKind::Hole => {
                    verify_zero_range(&mut file, segment.offset, segment.length, context)?;
                }
            }
            cursor = ending;
        }
        verify_zero_range(
            &mut file,
            cursor,
            logical_length.saturating_sub(cursor),
            context,
        )?;
        context.logical_bytes = context
            .logical_bytes
            .checked_add(logical_length)
            .ok_or_else(|| NativeBackendError::Limit("logical byte count".to_owned()))?;
        Ok(())
    }

    fn verify_bytes(
        file: &mut File,
        offset: u64,
        expected: &[u8],
        context: &mut VerifyContext<'_>,
    ) -> Result<(), NativeBackendError> {
        file.seek(SeekFrom::Start(offset))?;
        let mut buffer = [0_u8; ZERO_BUFFER_BYTES];
        context.maximum_buffer_bytes = context
            .maximum_buffer_bytes
            .max((buffer.len() + expected.len()) as u64);
        let mut compared = 0;
        while compared < expected.len() {
            let count = (expected.len() - compared).min(buffer.len());
            file.read_exact(&mut buffer[..count])?;
            if buffer[..count] != expected[compared..compared + count] {
                return Err(NativeBackendError::Invalid(
                    "native file bytes differ from logical tree".to_owned(),
                ));
            }
            compared += count;
        }
        Ok(())
    }

    fn verify_zero_range(
        file: &mut File,
        offset: u64,
        length: u64,
        context: &mut VerifyContext<'_>,
    ) -> Result<(), NativeBackendError> {
        file.seek(SeekFrom::Start(offset))?;
        let mut buffer = [0_u8; ZERO_BUFFER_BYTES];
        context.maximum_buffer_bytes = context
            .maximum_buffer_bytes
            .max(buffer.len().try_into().unwrap_or(u64::MAX));
        let mut remaining = length;
        while remaining != 0 {
            let count = usize::try_from(remaining.min(buffer.len() as u64))
                .map_err(|_| NativeBackendError::Limit("zero verification range".to_owned()))?;
            file.read_exact(&mut buffer[..count])?;
            if buffer[..count].iter().any(|byte| *byte != 0) {
                return Err(NativeBackendError::Invalid(
                    "native zero or hole bytes differ from logical tree".to_owned(),
                ));
            }
            remaining -= count as u64;
        }
        Ok(())
    }

    fn apply_file_metadata(file: &File, metadata: &MetadataV3) -> Result<(), NativeBackendError> {
        apply_fd_metadata(file, metadata)
    }

    fn apply_fd_metadata<F: AsFd>(fd: F, metadata: &MetadataV3) -> Result<(), NativeBackendError> {
        fchown(&fd, Some(metadata.uid), Some(metadata.gid))?;
        rustix::fs::fchmod(&fd, Mode::from_raw_mode(metadata.mode & 0o7777)).map_err(io_error)?;
        for (key, value) in &metadata.xattrs {
            rustix::fs::fsetxattr(&fd, OsStr::from_bytes(key), value, XattrFlags::empty())
                .map_err(io_error)?;
        }
        rustix::fs::futimens(&fd, &timestamps(metadata)?).map_err(io_error)
    }

    fn apply_path_metadata(
        carrier: &Path,
        relative: &[u8],
        metadata: &MetadataV3,
    ) -> Result<(), NativeBackendError> {
        let path = carrier.join(OsStr::from_bytes(relative));
        std::fs::set_permissions(
            &path,
            std::fs::Permissions::from_mode(metadata.mode & 0o7777),
        )?;
        std::os::unix::fs::chown(&path, Some(metadata.uid), Some(metadata.gid))?;
        for (key, value) in &metadata.xattrs {
            rustix::fs::lsetxattr(&path, OsStr::from_bytes(key), value, XattrFlags::empty())
                .map_err(io_error)?;
        }
        rustix::fs::utimensat(
            rustix::fs::CWD,
            &path,
            &timestamps(metadata)?,
            AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(io_error)
    }

    fn apply_symlink_metadata(
        carrier: &Path,
        relative: &[u8],
        metadata: &MetadataV3,
    ) -> Result<(), NativeBackendError> {
        let path = carrier.join(OsStr::from_bytes(relative));
        lchown(&path, Some(metadata.uid), Some(metadata.gid))?;
        for (key, value) in &metadata.xattrs {
            rustix::fs::lsetxattr(&path, OsStr::from_bytes(key), value, XattrFlags::empty())
                .map_err(io_error)?;
        }
        rustix::fs::utimensat(
            rustix::fs::CWD,
            &path,
            &timestamps(metadata)?,
            AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(io_error)
    }

    fn timestamps(metadata: &MetadataV3) -> Result<Timestamps, NativeBackendError> {
        let seconds = i64::try_from(metadata.mtime_seconds).map_err(|_| {
            NativeBackendError::Invalid("mtime seconds exceed platform range".to_owned())
        })?;
        let value = Timespec {
            tv_sec: seconds,
            tv_nsec: i64::from(metadata.mtime_nanoseconds),
        };
        Ok(Timestamps {
            last_access: value,
            last_modification: value,
        })
    }

    fn write_zeroes(
        file: &mut File,
        length: u64,
        context: &mut BuildContext<'_>,
    ) -> Result<(), NativeBackendError> {
        let zeroes = [0_u8; ZERO_BUFFER_BYTES];
        context.maximum_buffer_bytes = context.maximum_buffer_bytes.max(ZERO_BUFFER_BYTES as u64);
        let mut remaining = length;
        while remaining != 0 {
            let count = usize::try_from(remaining.min(ZERO_BUFFER_BYTES as u64))
                .map_err(|_| NativeBackendError::Limit("zero segment".to_owned()))?;
            file.write_all(&zeroes[..count])?;
            remaining -= count as u64;
        }
        Ok(())
    }

    fn add_allocated_bytes(total: &mut u64, stat: &Stat) -> Result<(), NativeBackendError> {
        let blocks = u64::try_from(stat.st_blocks).map_err(|_| {
            NativeBackendError::Invalid("negative allocated block count".to_owned())
        })?;
        let bytes = blocks.checked_mul(512).ok_or_else(|| {
            NativeBackendError::Limit("allocated byte accounting overflow".to_owned())
        })?;
        *total = total.checked_add(bytes).ok_or_else(|| {
            NativeBackendError::Limit("allocated byte accounting overflow".to_owned())
        })?;
        Ok(())
    }

    fn io_error(error: rustix::io::Errno) -> NativeBackendError {
        NativeBackendError::Io(error.to_string())
    }

    fn hex(bytes: &[u8]) -> String {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        let mut output = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            output.push(char::from(DIGITS[usize::from(byte >> 4)]));
            output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
        }
        output
    }
}

#[cfg(not(target_os = "linux"))]
impl NativeBackend {
    pub(crate) fn provided_capabilities(&self) -> CapabilityProfile {
        CapabilityProfile {
            feature_bits: 0,
            raw_byte_names: false,
            exact_metadata: false,
            sparse_files: false,
            hardlinks: false,
            symlinks: false,
            devices: false,
            fifos: false,
        }
    }

    pub(crate) fn preflight(
        &self,
        _pages: &mut PersistentPages<'_>,
        _root: RootId,
        _allocation_unit: u64,
    ) -> Result<NativePreflight, NativeBackendError> {
        Err(NativeBackendError::Unsupported(
            "linux-overlayfs-v1 requires Linux".to_owned(),
        ))
    }

    pub(crate) fn reconstruct_bounded<C>(
        &self,
        _pages: &mut PersistentPages<'_>,
        _root: RootId,
        _carrier: &Path,
        _resources: NativeReconstructionResources<'_>,
        _check: C,
    ) -> Result<NativeBuildResult, NativeBackendError>
    where
        C: FnMut() -> Result<(), NativeBackendError>,
    {
        Err(NativeBackendError::Unsupported(
            "linux-overlayfs-v1 requires Linux".to_owned(),
        ))
    }

    pub(crate) fn verify(
        &self,
        _pages: &mut PersistentPages<'_>,
        _root: RootId,
        _carrier: &Path,
    ) -> Result<NativeBuildResult, NativeBackendError> {
        Err(NativeBackendError::Unsupported(
            "linux-overlayfs-v1 requires Linux".to_owned(),
        ))
    }
}
