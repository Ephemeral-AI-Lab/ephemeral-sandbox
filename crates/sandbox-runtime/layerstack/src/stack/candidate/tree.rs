use std::fmt;

use sandbox_runtime_layerstack_core::{
    encode_v3_record, ActorId, AttributionPageId, AttributionRootId, CanonicalRecordV3,
    CanonicalSink, Digest32, Error, FileNodeId, HardlinkGroupIdV3, RawDigest, RecordKindV3, RootId,
    SegmentPageId, TlvV3, TreePageId,
};

use crate::Sha256Digest;

use super::object_store::{InstallDisposition, LooseObjectStore, ObjectStoreError};

const MAX_TREE_ENTRIES: usize = 192;
const MAX_SEGMENT_ENTRIES: usize = 1024;
const MAX_ATTRIBUTION_ENTRIES: usize = 128;
const MAX_PAGE_ENCODED_BYTES: usize = 65_536;
const PAGE_FIXED_ENCODED_BYTES: usize = 39;
const MAX_PAGE_DEPTH: u8 = 16;
const MAX_QUERY_INPUT_BYTES: usize = 1024;
const MAX_QUERY_PAGES: u64 = 4096;
const MAX_QUERY_FACTS: usize = 1024;
const MAX_QUERY_OUTPUT_BYTES: usize = 262_144;
const TREE_ANCHOR_DOMAIN: &[u8] = b"EOS-LS3-TREE-ANCHOR\0";
const SEGMENT_ANCHOR_DOMAIN: &[u8] = b"EOS-LS3-SEGMENT-ANCHOR\0";
const ATTRIBUTION_ANCHOR_DOMAIN: &[u8] = b"EOS-LS3-ATTR-ANCHOR\0";

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct PageCounters {
    pub(crate) objects_read: u64,
    pub(crate) object_bytes_read: u64,
    pub(crate) objects_written: u64,
    pub(crate) object_bytes_written: u64,
    pub(crate) objects_reused: u64,
    pub(crate) tree_pages_read: u64,
    pub(crate) tree_pages_written: u64,
    pub(crate) tree_pages_shared: u64,
    pub(crate) segment_pages_read: u64,
    pub(crate) segment_pages_written: u64,
    pub(crate) attribution_pages_read: u64,
    pub(crate) attribution_pages_written: u64,
    pub(crate) normal_complete_tree_scans: u64,
    pub(crate) normal_flat_inputs: u64,
    pub(crate) normal_flat_outputs: u64,
    pub(crate) attribution_history_scans: u64,
    pub(crate) diagnostic_flat_scans: u64,
    pub(crate) diagnostic_flat_entries: u64,
    pub(crate) query_pages: u64,
    pub(crate) query_facts: u64,
    pub(crate) maximum_page_buffer_bytes: u64,
}

#[derive(Debug)]
pub(crate) enum TreeError {
    Core(Error),
    Store(ObjectStoreError),
    Invalid(&'static str),
    Limit(&'static str),
    Missing,
}

impl fmt::Display for TreeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Core(error) => write!(formatter, "v3 page codec failed: {error}"),
            Self::Store(error) => write!(formatter, "v3 page store failed: {error}"),
            Self::Invalid(message) => write!(formatter, "invalid v3 page input: {message}"),
            Self::Limit(message) => write!(formatter, "v3 page bound exceeded: {message}"),
            Self::Missing => write!(formatter, "v3 tree key was not found"),
        }
    }
}

impl std::error::Error for TreeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Core(error) => Some(error),
            Self::Store(error) => Some(error),
            Self::Invalid(_) | Self::Limit(_) | Self::Missing => None,
        }
    }
}

impl From<Error> for TreeError {
    fn from(error: Error) -> Self {
        Self::Core(error)
    }
}

impl From<ObjectStoreError> for TreeError {
    fn from(error: ObjectStoreError) -> Self {
        Self::Store(error)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MetadataV3 {
    pub(crate) mode: u32,
    pub(crate) uid: u32,
    pub(crate) gid: u32,
    pub(crate) mtime_seconds: u64,
    pub(crate) mtime_nanoseconds: u32,
    pub(crate) xattrs: Vec<(Vec<u8>, Vec<u8>)>,
}

impl MetadataV3 {
    pub(crate) fn directory(mode: u32) -> Self {
        Self {
            mode,
            uid: 0,
            gid: 0,
            mtime_seconds: 0,
            mtime_nanoseconds: 0,
            xattrs: Vec::new(),
        }
    }

    fn record(&self) -> Result<CanonicalRecordV3, TreeError> {
        let mut xattrs = self.xattrs.clone();
        xattrs.sort_by(|left, right| left.0.cmp(&right.0));
        if xattrs.windows(2).any(|pair| pair[0].0 == pair[1].0) {
            return Err(TreeError::Invalid("duplicate xattr"));
        }
        let mut packed = Vec::new();
        push_u32(&mut packed, xattrs.len())?;
        for (key, value) in xattrs {
            push_len_u32(&mut packed, &key)?;
            push_len_u32(&mut packed, &value)?;
        }
        Ok(CanonicalRecordV3::immutable(
            RecordKindV3::Metadata,
            vec![
                TlvV3::new(1, self.mode.to_be_bytes().to_vec()),
                TlvV3::new(2, self.uid.to_be_bytes().to_vec()),
                TlvV3::new(3, self.gid.to_be_bytes().to_vec()),
                TlvV3::new(4, self.mtime_seconds.to_be_bytes().to_vec()),
                TlvV3::new(5, self.mtime_nanoseconds.to_be_bytes().to_vec()),
                TlvV3::new(6, packed),
            ],
        )?)
    }

    fn encoded(&self) -> Result<Vec<u8>, TreeError> {
        encode_record(&self.record()?)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FileKindV3 {
    Directory,
    Regular,
    Symlink,
    Device,
    Fifo,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FileNodeV3 {
    pub(crate) kind: FileKindV3,
    pub(crate) metadata: MetadataV3,
    pub(crate) directory: Option<TreePageId>,
    pub(crate) logical_length: Option<u64>,
    pub(crate) segments: Option<SegmentPageId>,
    pub(crate) symlink_target: Option<Vec<u8>>,
    pub(crate) device_major: Option<u32>,
    pub(crate) device_minor: Option<u32>,
    pub(crate) hardlink: Option<HardlinkGroupIdV3>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum FileSnapshotV3 {
    Directory,
    Regular {
        logical_length: u64,
        segments: Vec<SegmentDescriptor>,
    },
    Symlink(Vec<u8>),
    Other,
}

impl FileNodeV3 {
    pub(crate) fn directory(metadata: MetadataV3, directory: TreePageId) -> Self {
        Self {
            kind: FileKindV3::Directory,
            metadata,
            directory: Some(directory),
            logical_length: None,
            segments: None,
            symlink_target: None,
            device_major: None,
            device_minor: None,
            hardlink: None,
        }
    }

    pub(crate) fn regular(
        metadata: MetadataV3,
        logical_length: u64,
        segments: SegmentPageId,
        hardlink: Option<HardlinkGroupIdV3>,
    ) -> Self {
        Self {
            kind: FileKindV3::Regular,
            metadata,
            directory: None,
            logical_length: Some(logical_length),
            segments: Some(segments),
            symlink_target: None,
            device_major: None,
            device_minor: None,
            hardlink,
        }
    }

    pub(crate) fn symlink(metadata: MetadataV3, target: Vec<u8>) -> Self {
        Self {
            kind: FileKindV3::Symlink,
            metadata,
            directory: None,
            logical_length: None,
            segments: None,
            symlink_target: Some(target),
            device_major: None,
            device_minor: None,
            hardlink: None,
        }
    }

    fn record(&self) -> Result<CanonicalRecordV3, TreeError> {
        let kind = match self.kind {
            FileKindV3::Directory => 1,
            FileKindV3::Regular => 2,
            FileKindV3::Symlink => 3,
            FileKindV3::Device => 4,
            FileKindV3::Fifo => 5,
        };
        Ok(CanonicalRecordV3::immutable(
            RecordKindV3::FileNode,
            vec![
                TlvV3::new(1, vec![kind]),
                TlvV3::new(2, self.metadata.encoded()?),
                TlvV3::new(3, option_digest(self.directory.map(TreePageId::digest))),
                TlvV3::new(4, option_u64(self.logical_length)),
                TlvV3::new(5, option_digest(self.segments.map(SegmentPageId::digest))),
                TlvV3::new(6, option_bytes(self.symlink_target.as_deref())),
                TlvV3::new(7, option_u32(self.device_major)),
                TlvV3::new(8, option_u32(self.device_minor)),
                TlvV3::new(
                    9,
                    option_digest(self.hardlink.map(HardlinkGroupIdV3::digest)),
                ),
            ],
        )?)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TreeEntryV3 {
    pub(crate) name: Vec<u8>,
    pub(crate) file: FileNodeId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SegmentKind {
    Chunk(Digest32),
    Zero,
    Hole,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SegmentDescriptor {
    pub(crate) offset: u64,
    pub(crate) length: u64,
    pub(crate) kind: SegmentKind,
}

impl SegmentDescriptor {
    fn ending_offset(self) -> Result<u64, TreeError> {
        self.offset
            .checked_add(self.length)
            .ok_or(TreeError::Invalid("segment offset overflow"))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AttributionFact {
    pub(crate) path: Vec<u8>,
    pub(crate) scope: u8,
    pub(crate) offset: u64,
    pub(crate) length: u64,
    pub(crate) actor: ActorId,
    pub(crate) publication: [u8; 16],
}

impl AttributionFact {
    fn key(&self) -> Result<Vec<u8>, TreeError> {
        let mut output = Vec::new();
        push_len_u16(&mut output, &self.path)?;
        output.push(self.scope);
        output.extend_from_slice(&self.offset.to_be_bytes());
        output.extend_from_slice(&self.length.to_be_bytes());
        output.extend_from_slice(self.actor.as_bytes());
        output.extend_from_slice(&self.publication);
        Ok(output)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AttributionQuery {
    pub(crate) path: Vec<u8>,
    pub(crate) offset: u64,
    pub(crate) length: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TreePageRef {
    upper: Vec<u8>,
    id: TreePageId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SegmentPageRef {
    global_end: u64,
    length: u64,
    id: SegmentPageId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AttributionPageRef {
    upper: Vec<u8>,
    id: AttributionPageId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DecodedTreePage {
    depth: u8,
    entries: Vec<TreePageRef>,
}

pub(crate) struct PersistentPages<'a> {
    store: &'a LooseObjectStore,
    digest: Sha256Digest,
    counters: PageCounters,
}

impl<'a> PersistentPages<'a> {
    pub(crate) fn new(store: &'a LooseObjectStore) -> Self {
        Self {
            store,
            digest: Sha256Digest,
            counters: PageCounters::default(),
        }
    }

    pub(crate) const fn counters(&self) -> PageCounters {
        self.counters
    }

    pub(crate) fn install_file_node(&mut self, node: &FileNodeV3) -> Result<FileNodeId, TreeError> {
        let record = node.record()?;
        Ok(FileNodeId::new(self.install_record(&record)?))
    }

    pub(crate) fn install_chunk_slices(
        &mut self,
        first: &[u8],
        second: &[u8],
    ) -> Result<Digest32, TreeError> {
        let stored = self
            .store
            .install_chunk_slices(first, second, &mut self.digest)?;
        match stored.disposition() {
            InstallDisposition::Installed => {
                self.counters.objects_written = self.counters.objects_written.saturating_add(1);
                self.counters.object_bytes_written = self
                    .counters
                    .object_bytes_written
                    .saturating_add((first.len() + second.len()) as u64);
            }
            InstallDisposition::AlreadyPresent => {
                self.counters.objects_reused = self.counters.objects_reused.saturating_add(1);
            }
        }
        Ok(stored.id())
    }

    pub(crate) fn install_root(&mut self, root: FileNodeId) -> Result<RootId, TreeError> {
        let record = CanonicalRecordV3::immutable(
            RecordKindV3::Root,
            vec![
                TlvV3::new(1, 1_u64.to_be_bytes().to_vec()),
                TlvV3::new(2, 1_u16.to_be_bytes().to_vec()),
                TlvV3::new(3, root.digest().as_bytes().to_vec()),
            ],
        )?;
        Ok(RootId::new(self.install_record(&record)?))
    }

    pub(crate) fn install_hardlink_group<I>(
        &mut self,
        paths: I,
    ) -> Result<HardlinkGroupIdV3, TreeError>
    where
        I: IntoIterator<Item = Vec<u8>>,
    {
        let mut paths = paths.into_iter().collect::<Vec<_>>();
        paths.sort();
        if paths.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(TreeError::Invalid("duplicate hardlink member"));
        }
        let mut packed = Vec::new();
        for path in &paths {
            push_len_u16(&mut packed, path)?;
        }
        let count =
            u16::try_from(paths.len()).map_err(|_| TreeError::Limit("hardlink group count"))?;
        let record = CanonicalRecordV3::immutable(
            RecordKindV3::HardlinkGroup,
            vec![
                TlvV3::new(1, count.to_be_bytes().to_vec()),
                TlvV3::new(2, packed),
            ],
        )?;
        Ok(HardlinkGroupIdV3::new(self.install_record(&record)?))
    }

    pub(crate) fn build_tree<I>(&mut self, entries: I) -> Result<TreePageId, TreeError>
    where
        I: IntoIterator<Item = TreeEntryV3>,
    {
        let mut previous: Option<Vec<u8>> = None;
        let mut leaf_entries = Vec::new();
        for entry in entries {
            validate_component(&entry.name)?;
            if previous
                .as_ref()
                .is_some_and(|value| value.as_slice() >= entry.name.as_slice())
            {
                return Err(TreeError::Invalid("tree entries are not strictly ordered"));
            }
            previous = Some(entry.name.clone());
            leaf_entries.push(TreePageRef {
                upper: entry.name,
                id: TreePageId::new(entry.file.digest()),
            });
        }
        if leaf_entries.is_empty() {
            return self.install_tree_page(0, &[]);
        }

        let groups = partition_tree_entries(leaf_entries, false, &mut self.digest)?;
        let mut level = groups
            .iter()
            .map(|group| {
                self.install_tree_page(0, group).map(|id| TreePageRef {
                    upper: group.last().expect("nonempty group").upper.clone(),
                    id,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut depth = 1_u8;
        while level.len() > 1 {
            if depth > MAX_PAGE_DEPTH {
                return Err(TreeError::Limit("tree page depth"));
            }
            let groups = partition_tree_entries(level, true, &mut self.digest)?;
            level = groups
                .iter()
                .map(|group| {
                    self.install_tree_page(depth, group).map(|id| TreePageRef {
                        upper: group.last().expect("nonempty group").upper.clone(),
                        id,
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            depth += 1;
        }
        Ok(level[0].id)
    }

    pub(crate) fn lookup_tree_entry(
        &mut self,
        root: TreePageId,
        name: &[u8],
    ) -> Result<Option<FileNodeId>, TreeError> {
        validate_component(name)?;
        let mut page_id = root;
        loop {
            let page = self.load_tree_page(page_id)?;
            let index = page
                .entries
                .partition_point(|entry| entry.upper.as_slice() < name);
            if page.depth == 0 {
                return Ok(page
                    .entries
                    .get(index)
                    .filter(|entry| entry.upper.as_slice() == name)
                    .map(|entry| FileNodeId::new(entry.id.digest())));
            }
            let child = page
                .entries
                .get(index)
                .or_else(|| page.entries.last())
                .ok_or(TreeError::Invalid("empty internal tree page"))?;
            page_id = child.id;
        }
    }

    pub(crate) fn lookup_path(
        &mut self,
        root: RootId,
        path: &[u8],
    ) -> Result<Option<FileNodeId>, TreeError> {
        validate_path(path, true)?;
        let record = self.load_record(RecordKindV3::Root, root.digest())?;
        let fields = record
            .fields()
            .ok_or(TreeError::Invalid("root has chunk payload"))?;
        let mut current = FileNodeId::new(Digest32::new(fixed_32(fields[2].value())?));
        if path.is_empty() {
            return Ok(Some(current));
        }
        for component in path.split(|byte| *byte == b'/') {
            let node = self.load_record(RecordKindV3::FileNode, current.digest())?;
            let fields = node
                .fields()
                .ok_or(TreeError::Invalid("file node has chunk payload"))?;
            if one_byte(&fields[0])? != 1 {
                return Ok(None);
            }
            let directory = optional_digest(fields[2].value())?
                .ok_or(TreeError::Invalid("directory tree missing"))?;
            let Some(next) = self.lookup_tree_entry(TreePageId::new(directory), component)? else {
                return Ok(None);
            };
            current = next;
        }
        Ok(Some(current))
    }

    pub(crate) fn root_directory(&mut self, root: RootId) -> Result<TreePageId, TreeError> {
        let record = self.load_record(RecordKindV3::Root, root.digest())?;
        let fields = record
            .fields()
            .ok_or(TreeError::Invalid("root has chunk payload"))?;
        let file = FileNodeId::new(Digest32::new(fixed_32(fields[2].value())?));
        self.file_directory(file)?
            .ok_or(TreeError::Invalid("root file node is not a directory"))
    }

    pub(crate) fn file_directory(
        &mut self,
        file: FileNodeId,
    ) -> Result<Option<TreePageId>, TreeError> {
        let record = self.load_record(RecordKindV3::FileNode, file.digest())?;
        let fields = record
            .fields()
            .ok_or(TreeError::Invalid("file node has chunk payload"))?;
        if one_byte(&fields[0])? != 1 {
            return Ok(None);
        }
        Ok(optional_digest(fields[2].value())?.map(TreePageId::new))
    }

    pub(crate) fn file_snapshot(&mut self, file: FileNodeId) -> Result<FileSnapshotV3, TreeError> {
        let record = self.load_record(RecordKindV3::FileNode, file.digest())?;
        let fields = record
            .fields()
            .ok_or(TreeError::Invalid("file node has chunk payload"))?;
        match one_byte(&fields[0])? {
            1 => Ok(FileSnapshotV3::Directory),
            2 => {
                let logical_length = optional_u64(fields[3].value())?
                    .ok_or(TreeError::Invalid("regular file length missing"))?;
                let segments = optional_digest(fields[4].value())?
                    .ok_or(TreeError::Invalid("regular file segments missing"))?;
                Ok(FileSnapshotV3::Regular {
                    logical_length,
                    segments: self.reconstruct_segments(SegmentPageId::new(segments))?,
                })
            }
            3 => Ok(FileSnapshotV3::Symlink(
                optional_bytes(fields[5].value())?
                    .ok_or(TreeError::Invalid("symlink target missing"))?
                    .to_vec(),
            )),
            4 | 5 => Ok(FileSnapshotV3::Other),
            _ => Err(TreeError::Invalid("unknown file node kind")),
        }
    }

    pub(crate) fn load_chunk(&mut self, id: Digest32) -> Result<Vec<u8>, TreeError> {
        let record = self.load_record(RecordKindV3::Chunk, id)?;
        record
            .chunk_payload()
            .map(ToOwned::to_owned)
            .ok_or(TreeError::Invalid("chunk object has fields"))
    }

    pub(crate) fn mutate_tree_entry(
        &mut self,
        root: TreePageId,
        name: &[u8],
        replacement: Option<FileNodeId>,
    ) -> Result<TreePageId, TreeError> {
        validate_component(name)?;
        let depth = self.load_tree_page(root)?.depth;
        let mut level = self.mutate_tree_entry_at(root, name, replacement)?;
        if level.is_empty() {
            return self.install_tree_page(0, &[]);
        }
        let mut parent_depth = depth
            .checked_add(1)
            .ok_or(TreeError::Limit("tree page depth"))?;
        while level.len() > 1 {
            if parent_depth > MAX_PAGE_DEPTH {
                return Err(TreeError::Limit("tree page depth"));
            }
            let groups = partition_tree_entries(level, true, &mut self.digest)?;
            level = groups
                .iter()
                .map(|group| {
                    self.install_tree_page(parent_depth, group)
                        .map(|id| TreePageRef {
                            upper: group.last().expect("nonempty group").upper.clone(),
                            id,
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            parent_depth += 1;
        }
        Ok(level[0].id)
    }

    fn mutate_tree_entry_at(
        &mut self,
        page_id: TreePageId,
        name: &[u8],
        replacement: Option<FileNodeId>,
    ) -> Result<Vec<TreePageRef>, TreeError> {
        let mut page = self.load_tree_page(page_id)?;
        let index = page
            .entries
            .partition_point(|entry| entry.upper.as_slice() < name);
        if page.depth == 0 {
            let present = page
                .entries
                .get(index)
                .is_some_and(|entry| entry.upper.as_slice() == name);
            match (present, replacement) {
                (true, Some(file)) => {
                    page.entries[index].id = TreePageId::new(file.digest());
                }
                (true, None) => {
                    page.entries.remove(index);
                }
                (false, Some(file)) => page.entries.insert(
                    index,
                    TreePageRef {
                        upper: name.to_vec(),
                        id: TreePageId::new(file.digest()),
                    },
                ),
                (false, None) => {}
            }
        } else {
            if page.entries.is_empty() {
                return Err(TreeError::Invalid("empty internal tree page"));
            }
            let selected = index.min(page.entries.len() - 1);
            let child = page.entries[selected].id;
            let replacements = self.mutate_tree_entry_at(child, name, replacement)?;
            page.entries.splice(selected..=selected, replacements);
        }
        if page.entries.is_empty() {
            return Ok(Vec::new());
        }
        partition_tree_entries(page.entries, page.depth != 0, &mut self.digest)?
            .iter()
            .map(|group| {
                self.install_tree_page(page.depth, group)
                    .map(|id| TreePageRef {
                        upper: group.last().expect("nonempty group").upper.clone(),
                        id,
                    })
            })
            .collect()
    }

    pub(crate) fn replace_tree_entry(
        &mut self,
        root: TreePageId,
        name: &[u8],
        replacement: FileNodeId,
    ) -> Result<TreePageId, TreeError> {
        validate_component(name)?;
        self.replace_tree_entry_at(root, name, replacement)
    }

    fn replace_tree_entry_at(
        &mut self,
        page_id: TreePageId,
        name: &[u8],
        replacement: FileNodeId,
    ) -> Result<TreePageId, TreeError> {
        let mut page = self.load_tree_page(page_id)?;
        let index = page
            .entries
            .partition_point(|entry| entry.upper.as_slice() < name);
        if page.depth == 0 {
            let entry = page.entries.get_mut(index).ok_or(TreeError::Missing)?;
            if entry.upper.as_slice() != name {
                return Err(TreeError::Missing);
            }
            if entry.id.digest() == replacement.digest() {
                self.counters.tree_pages_shared += 1;
                return Ok(page_id);
            }
            entry.id = TreePageId::new(replacement.digest());
        } else {
            if page.entries.is_empty() {
                return Err(TreeError::Invalid("empty internal tree page"));
            }
            let selected = index.min(page.entries.len() - 1);
            let entry = &mut page.entries[selected];
            let child = self.replace_tree_entry_at(entry.id, name, replacement)?;
            if child == entry.id {
                self.counters.tree_pages_shared += 1;
                return Ok(page_id);
            }
            entry.id = child;
        }
        self.install_tree_page(page.depth, &page.entries)
    }

    pub(crate) fn export_tree_diagnostic<F>(
        &mut self,
        root: TreePageId,
        mut emit: F,
    ) -> Result<(), TreeError>
    where
        F: FnMut(TreeEntryV3) -> Result<(), TreeError>,
    {
        self.counters.diagnostic_flat_scans += 1;
        self.export_tree_page(root, &mut emit)
    }

    fn export_tree_page<F>(&mut self, id: TreePageId, emit: &mut F) -> Result<(), TreeError>
    where
        F: FnMut(TreeEntryV3) -> Result<(), TreeError>,
    {
        let page = self.load_tree_page(id)?;
        if page.depth == 0 {
            for entry in page.entries {
                self.counters.diagnostic_flat_entries += 1;
                emit(TreeEntryV3 {
                    name: entry.upper,
                    file: FileNodeId::new(entry.id.digest()),
                })?;
            }
        } else {
            for entry in page.entries {
                self.export_tree_page(entry.id, emit)?;
            }
        }
        Ok(())
    }

    pub(crate) fn build_segments<I>(&mut self, descriptors: I) -> Result<SegmentPageId, TreeError>
    where
        I: IntoIterator<Item = SegmentDescriptor>,
    {
        let descriptors = descriptors.into_iter().collect::<Vec<_>>();
        validate_segments(&descriptors)?;
        if descriptors.is_empty() {
            return self.install_segment_leaf(&[]);
        }
        let groups = partition_segments(&descriptors, &mut self.digest)?;
        let mut level = Vec::with_capacity(groups.len());
        for group in groups {
            let id = self.install_segment_leaf(group)?;
            let start = group.first().expect("nonempty group").offset;
            let end = group.last().expect("nonempty group").ending_offset()?;
            level.push(SegmentPageRef {
                global_end: end,
                length: end - start,
                id,
            });
        }
        let mut depth = 1_u8;
        while level.len() > 1 {
            if depth > MAX_PAGE_DEPTH {
                return Err(TreeError::Limit("segment page depth"));
            }
            let groups = partition_segment_refs(level, &mut self.digest)?;
            level = groups
                .iter()
                .map(|group| {
                    let id = self.install_segment_internal(depth, group)?;
                    let global_end = group.last().expect("nonempty group").global_end;
                    let length = group.iter().try_fold(0_u64, |total, entry| {
                        total
                            .checked_add(entry.length)
                            .ok_or(TreeError::Invalid("segment covered length overflow"))
                    })?;
                    Ok(SegmentPageRef {
                        global_end,
                        length,
                        id,
                    })
                })
                .collect::<Result<Vec<_>, TreeError>>()?;
            depth += 1;
        }
        Ok(level[0].id)
    }

    pub(crate) fn reconstruct_segments(
        &mut self,
        root: SegmentPageId,
    ) -> Result<Vec<SegmentDescriptor>, TreeError> {
        let mut descriptors = Vec::new();
        self.reconstruct_segment_page(root, 0, &mut descriptors)?;
        Ok(descriptors)
    }

    pub(crate) fn build_attribution<I>(&mut self, facts: I) -> Result<AttributionPageId, TreeError>
    where
        I: IntoIterator<Item = AttributionFact>,
    {
        let mut facts = facts.into_iter().collect::<Vec<_>>();
        validate_attribution_facts(&facts)?;
        facts.sort_by_cached_key(|fact| fact.key().expect("validated attribution key"));
        if facts.is_empty() {
            return self.install_attribution_leaf(&[]);
        }
        let groups = partition_attribution_facts(&facts, &mut self.digest)?;
        let mut level = groups
            .iter()
            .map(|group| {
                let id = self.install_attribution_leaf(group)?;
                Ok(AttributionPageRef {
                    upper: group.last().expect("nonempty group").key()?,
                    id,
                })
            })
            .collect::<Result<Vec<_>, TreeError>>()?;
        let mut depth = 1_u8;
        while level.len() > 1 {
            if depth > MAX_PAGE_DEPTH {
                return Err(TreeError::Limit("attribution page depth"));
            }
            let groups = partition_attribution_refs(level, &mut self.digest)?;
            level = groups
                .iter()
                .map(|group| {
                    let id = self.install_attribution_internal(depth, group)?;
                    Ok(AttributionPageRef {
                        upper: group.last().expect("nonempty group").upper.clone(),
                        id,
                    })
                })
                .collect::<Result<Vec<_>, TreeError>>()?;
            depth += 1;
        }
        Ok(level[0].id)
    }

    pub(crate) fn install_attribution_root(
        &mut self,
        content: RootId,
        page: AttributionPageId,
    ) -> Result<AttributionRootId, TreeError> {
        let record = CanonicalRecordV3::immutable(
            RecordKindV3::AttributionRoot,
            vec![
                TlvV3::new(1, 7_u64.to_be_bytes().to_vec()),
                TlvV3::new(2, content.digest().as_bytes().to_vec()),
                TlvV3::new(3, page.digest().as_bytes().to_vec()),
            ],
        )?;
        Ok(AttributionRootId::new(self.install_record(&record)?))
    }

    pub(crate) fn load_attribution_root(
        &mut self,
        root: AttributionRootId,
    ) -> Result<(RootId, AttributionPageId), TreeError> {
        let record = self.load_record(RecordKindV3::AttributionRoot, root.digest())?;
        let fields = record
            .fields()
            .ok_or(TreeError::Invalid("attribution root has chunk payload"))?;
        Ok((
            RootId::new(Digest32::new(fixed_32(fields[1].value())?)),
            AttributionPageId::new(Digest32::new(fixed_32(fields[2].value())?)),
        ))
    }

    pub(crate) fn query_attribution(
        &mut self,
        root: AttributionPageId,
        query: &AttributionQuery,
    ) -> Result<Vec<AttributionFact>, TreeError> {
        if query.path.len() > MAX_QUERY_INPUT_BYTES {
            return Err(TreeError::Limit("attribution query input"));
        }
        let query_end = query
            .offset
            .checked_add(query.length)
            .ok_or(TreeError::Invalid("attribution query range overflow"))?;
        let mut output = Vec::new();
        let mut output_bytes = 0_usize;
        self.query_attribution_page(root, query, query_end, &mut output, &mut output_bytes)?;
        Ok(output)
    }

    fn install_tree_page(
        &mut self,
        depth: u8,
        entries: &[TreePageRef],
    ) -> Result<TreePageId, TreeError> {
        let mut packed = Vec::new();
        for entry in entries {
            push_len_u16(&mut packed, &entry.upper)?;
            packed.extend_from_slice(entry.id.digest().as_bytes());
        }
        self.observe_page_buffer(packed.len());
        let record = page_record(RecordKindV3::TreePage, depth, entries.len(), packed, None)?;
        let id = self.install_record(&record)?;
        self.counters.tree_pages_written += 1;
        Ok(TreePageId::new(id))
    }

    fn load_tree_page(&mut self, id: TreePageId) -> Result<DecodedTreePage, TreeError> {
        let record = self.load_record(RecordKindV3::TreePage, id.digest())?;
        self.counters.tree_pages_read += 1;
        let fields = record
            .fields()
            .ok_or(TreeError::Invalid("tree page has chunk payload"))?;
        let depth = one_byte(&fields[1])?;
        let count = be_u16(fields[2].value())? as usize;
        let mut cursor = ByteCursor::new(fields[3].value());
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            let upper = cursor.len_u16(255)?.to_vec();
            let id = TreePageId::new(Digest32::new(cursor.array_32()?));
            entries.push(TreePageRef { upper, id });
        }
        cursor.finish()?;
        Ok(DecodedTreePage { depth, entries })
    }

    fn install_segment_leaf(
        &mut self,
        descriptors: &[SegmentDescriptor],
    ) -> Result<SegmentPageId, TreeError> {
        let base = descriptors
            .first()
            .map_or(0, |descriptor| descriptor.offset);
        let mut packed = Vec::new();
        let mut covered = 0_u64;
        for descriptor in descriptors {
            let kind = match descriptor.kind {
                SegmentKind::Chunk(_) => 1,
                SegmentKind::Zero => 2,
                SegmentKind::Hole => 3,
            };
            packed.push(kind);
            packed.extend_from_slice(&(descriptor.offset - base).to_be_bytes());
            packed.extend_from_slice(&descriptor.length.to_be_bytes());
            if let SegmentKind::Chunk(id) = descriptor.kind {
                packed.extend_from_slice(id.as_bytes());
            }
            covered = covered
                .checked_add(descriptor.length)
                .ok_or(TreeError::Invalid("segment covered length overflow"))?;
        }
        self.observe_page_buffer(packed.len());
        let record = page_record(
            RecordKindV3::SegmentPage,
            0,
            descriptors.len(),
            packed,
            Some(covered),
        )?;
        let id = self.install_record(&record)?;
        self.counters.segment_pages_written += 1;
        Ok(SegmentPageId::new(id))
    }

    fn install_segment_internal(
        &mut self,
        depth: u8,
        entries: &[SegmentPageRef],
    ) -> Result<SegmentPageId, TreeError> {
        let mut packed = Vec::new();
        let mut covered = 0_u64;
        for entry in entries {
            covered = covered
                .checked_add(entry.length)
                .ok_or(TreeError::Invalid("segment covered length overflow"))?;
            packed.extend_from_slice(&covered.to_be_bytes());
            packed.extend_from_slice(entry.id.digest().as_bytes());
        }
        self.observe_page_buffer(packed.len());
        let record = page_record(
            RecordKindV3::SegmentPage,
            depth,
            entries.len(),
            packed,
            Some(covered),
        )?;
        let id = self.install_record(&record)?;
        self.counters.segment_pages_written += 1;
        Ok(SegmentPageId::new(id))
    }

    fn reconstruct_segment_page(
        &mut self,
        id: SegmentPageId,
        base: u64,
        output: &mut Vec<SegmentDescriptor>,
    ) -> Result<u64, TreeError> {
        let record = self.load_record(RecordKindV3::SegmentPage, id.digest())?;
        self.counters.segment_pages_read += 1;
        let fields = record
            .fields()
            .ok_or(TreeError::Invalid("segment page has chunk payload"))?;
        let kind = one_byte(&fields[0])?;
        let count = be_u16(fields[2].value())? as usize;
        let covered = be_u64(fields[3].value())?;
        let mut cursor = ByteCursor::new(fields[4].value());
        if kind == 1 {
            for _ in 0..count {
                let descriptor_kind = cursor.byte()?;
                let offset = base
                    .checked_add(cursor.u64()?)
                    .ok_or(TreeError::Invalid("segment offset overflow"))?;
                let length = cursor.u64()?;
                let descriptor_kind = match descriptor_kind {
                    1 => SegmentKind::Chunk(Digest32::new(cursor.array_32()?)),
                    2 => SegmentKind::Zero,
                    3 => SegmentKind::Hole,
                    _ => return Err(TreeError::Invalid("unknown segment descriptor kind")),
                };
                output.push(SegmentDescriptor {
                    offset,
                    length,
                    kind: descriptor_kind,
                });
            }
        } else {
            let mut previous = 0_u64;
            for _ in 0..count {
                let end = cursor.u64()?;
                let child = SegmentPageId::new(Digest32::new(cursor.array_32()?));
                let child_base = base
                    .checked_add(previous)
                    .ok_or(TreeError::Invalid("segment offset overflow"))?;
                self.reconstruct_segment_page(child, child_base, output)?;
                previous = end;
            }
        }
        cursor.finish()?;
        Ok(covered)
    }

    fn install_attribution_leaf(
        &mut self,
        facts: &[AttributionFact],
    ) -> Result<AttributionPageId, TreeError> {
        let mut packed = Vec::new();
        for fact in facts {
            packed.extend_from_slice(&fact.key()?);
        }
        self.observe_page_buffer(packed.len());
        let record = page_record(RecordKindV3::AttributionPage, 0, facts.len(), packed, None)?;
        let id = self.install_record(&record)?;
        self.counters.attribution_pages_written += 1;
        Ok(AttributionPageId::new(id))
    }

    fn install_attribution_internal(
        &mut self,
        depth: u8,
        entries: &[AttributionPageRef],
    ) -> Result<AttributionPageId, TreeError> {
        let mut packed = Vec::new();
        for entry in entries {
            packed.extend_from_slice(&entry.upper);
            packed.extend_from_slice(entry.id.digest().as_bytes());
        }
        self.observe_page_buffer(packed.len());
        let record = page_record(
            RecordKindV3::AttributionPage,
            depth,
            entries.len(),
            packed,
            None,
        )?;
        let id = self.install_record(&record)?;
        self.counters.attribution_pages_written += 1;
        Ok(AttributionPageId::new(id))
    }

    fn query_attribution_page(
        &mut self,
        id: AttributionPageId,
        query: &AttributionQuery,
        query_end: u64,
        output: &mut Vec<AttributionFact>,
        output_bytes: &mut usize,
    ) -> Result<(), TreeError> {
        self.counters.query_pages += 1;
        if self.counters.query_pages > MAX_QUERY_PAGES {
            return Err(TreeError::Limit("attribution query pages"));
        }
        let record = self.load_record(RecordKindV3::AttributionPage, id.digest())?;
        self.counters.attribution_pages_read += 1;
        let fields = record
            .fields()
            .ok_or(TreeError::Invalid("attribution page has chunk payload"))?;
        let kind = one_byte(&fields[0])?;
        let count = be_u16(fields[2].value())? as usize;
        let mut cursor = ByteCursor::new(fields[3].value());
        if kind == 1 {
            for _ in 0..count {
                let fact = cursor.fact()?;
                if attribution_matches(&fact, query, query_end) {
                    *output_bytes = output_bytes
                        .checked_add(fact.key()?.len())
                        .ok_or(TreeError::Limit("attribution output bytes"))?;
                    if output.len() >= MAX_QUERY_FACTS || *output_bytes > MAX_QUERY_OUTPUT_BYTES {
                        return Err(TreeError::Limit("attribution query output"));
                    }
                    output.push(fact);
                    self.counters.query_facts += 1;
                }
            }
        } else {
            let mut previous_upper: Option<Vec<u8>> = None;
            for _ in 0..count {
                let upper_fact = cursor.fact()?;
                let upper = upper_fact.key()?;
                let child = AttributionPageId::new(Digest32::new(cursor.array_32()?));
                let lower_path = previous_upper
                    .as_deref()
                    .and_then(attribution_key_path)
                    .unwrap_or_default();
                let upper_path = upper_fact.path.as_slice();
                if lower_path <= query.path.as_slice() && query.path.as_slice() <= upper_path {
                    self.query_attribution_page(child, query, query_end, output, output_bytes)?;
                }
                previous_upper = Some(upper);
            }
        }
        cursor.finish()
    }

    fn install_record(&mut self, record: &CanonicalRecordV3) -> Result<Digest32, TreeError> {
        let stored = self.store.install(record, &mut self.digest)?;
        match stored.disposition() {
            InstallDisposition::Installed => {
                self.counters.objects_written += 1;
                self.counters.object_bytes_written += std::fs::metadata(stored.path())
                    .map_err(ObjectStoreError::from)?
                    .len();
            }
            InstallDisposition::AlreadyPresent => {
                self.counters.objects_reused += 1;
            }
        }
        Ok(stored.id())
    }

    fn load_record(
        &mut self,
        kind: RecordKindV3,
        id: Digest32,
    ) -> Result<CanonicalRecordV3, TreeError> {
        let path = self.store.object_path(kind, id);
        let record = self.store.load(kind, id, &mut self.digest)?;
        self.counters.objects_read += 1;
        self.counters.object_bytes_read += std::fs::metadata(path)
            .map_err(ObjectStoreError::from)?
            .len();
        Ok(record)
    }

    fn observe_page_buffer(&mut self, length: usize) {
        self.counters.maximum_page_buffer_bytes = self
            .counters
            .maximum_page_buffer_bytes
            .max(u64::try_from(length).unwrap_or(u64::MAX));
    }
}

fn page_record(
    kind: RecordKindV3,
    depth: u8,
    count: usize,
    packed: Vec<u8>,
    covered: Option<u64>,
) -> Result<CanonicalRecordV3, TreeError> {
    let count = u16::try_from(count).map_err(|_| TreeError::Limit("page entry count"))?;
    let mut fields = vec![
        TlvV3::new(1, vec![if depth == 0 { 1 } else { 2 }]),
        TlvV3::new(2, vec![depth]),
        TlvV3::new(3, count.to_be_bytes().to_vec()),
    ];
    if let Some(covered) = covered {
        fields.push(TlvV3::new(4, covered.to_be_bytes().to_vec()));
        fields.push(TlvV3::new(5, packed));
    } else {
        fields.push(TlvV3::new(4, packed));
    }
    Ok(CanonicalRecordV3::immutable(kind, fields)?)
}

fn partition_tree_entries(
    entries: Vec<TreePageRef>,
    internal: bool,
    digest: &mut Sha256Digest,
) -> Result<Vec<Vec<TreePageRef>>, TreeError> {
    partition_by(
        entries,
        MAX_TREE_ENTRIES,
        internal,
        |entry| 2 + entry.upper.len() + 32,
        |entry, digest| anchored(TREE_ANCHOR_DOMAIN, &tree_anchor_key(&entry.upper)?, digest),
        digest,
    )
}

fn partition_segments<'a>(
    entries: &'a [SegmentDescriptor],
    digest: &mut Sha256Digest,
) -> Result<Vec<&'a [SegmentDescriptor]>, TreeError> {
    let mut groups = Vec::new();
    let mut start = 0;
    let mut bytes = 0;
    for (index, entry) in entries.iter().enumerate() {
        let encoded = if matches!(entry.kind, SegmentKind::Chunk(_)) {
            49
        } else {
            17
        };
        if index > start
            && (index - start == MAX_SEGMENT_ENTRIES
                || PAGE_FIXED_ENCODED_BYTES + bytes + encoded > MAX_PAGE_ENCODED_BYTES)
        {
            groups.push(&entries[start..index]);
            start = index;
            bytes = 0;
        }
        bytes += encoded;
        let key = entry.ending_offset()?.to_be_bytes();
        if anchored(SEGMENT_ANCHOR_DOMAIN, &key, digest)? {
            groups.push(&entries[start..=index]);
            start = index + 1;
            bytes = 0;
        }
    }
    if start < entries.len() {
        groups.push(&entries[start..]);
    }
    Ok(groups)
}

fn partition_segment_refs(
    entries: Vec<SegmentPageRef>,
    digest: &mut Sha256Digest,
) -> Result<Vec<Vec<SegmentPageRef>>, TreeError> {
    partition_by(
        entries,
        MAX_SEGMENT_ENTRIES,
        true,
        |_| 40,
        |entry, digest| {
            anchored(
                SEGMENT_ANCHOR_DOMAIN,
                &entry.global_end.to_be_bytes(),
                digest,
            )
        },
        digest,
    )
}

fn partition_attribution_facts<'a>(
    facts: &'a [AttributionFact],
    digest: &mut Sha256Digest,
) -> Result<Vec<&'a [AttributionFact]>, TreeError> {
    let keys = facts
        .iter()
        .map(AttributionFact::key)
        .collect::<Result<Vec<_>, _>>()?;
    let groups = partition_ranges(
        &keys,
        MAX_ATTRIBUTION_ENTRIES,
        false,
        |key| key.len(),
        |key, digest| anchored(ATTRIBUTION_ANCHOR_DOMAIN, key, digest),
        digest,
    )?;
    Ok(groups
        .into_iter()
        .map(|(start, end)| &facts[start..end])
        .collect())
}

fn partition_attribution_refs(
    entries: Vec<AttributionPageRef>,
    digest: &mut Sha256Digest,
) -> Result<Vec<Vec<AttributionPageRef>>, TreeError> {
    partition_by(
        entries,
        MAX_ATTRIBUTION_ENTRIES,
        true,
        |entry| entry.upper.len() + 32,
        |entry, digest| anchored(ATTRIBUTION_ANCHOR_DOMAIN, &entry.upper, digest),
        digest,
    )
}

fn partition_by<T, S, A>(
    entries: Vec<T>,
    maximum_count: usize,
    minimum_two: bool,
    size: S,
    anchor: A,
    digest: &mut Sha256Digest,
) -> Result<Vec<Vec<T>>, TreeError>
where
    S: Fn(&T) -> usize,
    A: Fn(&T, &mut Sha256Digest) -> Result<bool, TreeError>,
{
    let ranges = partition_ranges(
        &entries,
        maximum_count,
        minimum_two,
        |entry| size(entry),
        |entry, digest| anchor(entry, digest),
        digest,
    )?;
    let mut entries = entries.into_iter();
    let mut groups = Vec::with_capacity(ranges.len());
    for (start, end) in ranges {
        let length = end - start;
        groups.push(entries.by_ref().take(length).collect());
    }
    Ok(groups)
}

fn partition_ranges<T, S, A>(
    entries: &[T],
    maximum_count: usize,
    minimum_two: bool,
    size: S,
    anchor: A,
    digest: &mut Sha256Digest,
) -> Result<Vec<(usize, usize)>, TreeError>
where
    S: Fn(&T) -> usize,
    A: Fn(&T, &mut Sha256Digest) -> Result<bool, TreeError>,
{
    let mut ranges = Vec::new();
    let mut start = 0_usize;
    let mut bytes = 0_usize;
    for (index, entry) in entries.iter().enumerate() {
        let encoded = size(entry);
        if encoded + PAGE_FIXED_ENCODED_BYTES > MAX_PAGE_ENCODED_BYTES {
            return Err(TreeError::Limit("single page entry"));
        }
        if index > start
            && (index - start == maximum_count
                || PAGE_FIXED_ENCODED_BYTES + bytes + encoded > MAX_PAGE_ENCODED_BYTES)
        {
            ranges.push((start, index));
            start = index;
            bytes = 0;
        }
        bytes += encoded;
        let count = index + 1 - start;
        if (!minimum_two || count >= 2) && anchor(entry, digest)? {
            ranges.push((start, index + 1));
            start = index + 1;
            bytes = 0;
        }
    }
    if start < entries.len() {
        ranges.push((start, entries.len()));
    }
    if minimum_two && ranges.len() > 1 && ranges.last().is_some_and(|(start, end)| end - start == 1)
    {
        let singleton = ranges.pop().expect("last range exists");
        let previous = ranges.last_mut().expect("previous range exists");
        if singleton.1 - previous.0 <= maximum_count
            && PAGE_FIXED_ENCODED_BYTES
                + entries[previous.0..singleton.1]
                    .iter()
                    .map(&size)
                    .sum::<usize>()
                <= MAX_PAGE_ENCODED_BYTES
        {
            previous.1 = singleton.1;
        } else if previous.1 - previous.0 > 2 {
            let moved = previous.1 - 1;
            previous.1 = moved;
            ranges.push((moved, singleton.1));
        } else {
            return Err(TreeError::Limit("non-root internal page minimum"));
        }
    }
    Ok(ranges)
}

fn validate_segments(descriptors: &[SegmentDescriptor]) -> Result<(), TreeError> {
    let mut expected = 0_u64;
    let mut previous = None;
    for descriptor in descriptors {
        if descriptor.offset != expected || descriptor.length == 0 {
            return Err(TreeError::Invalid("segments are not contiguous"));
        }
        if matches!(descriptor.kind, SegmentKind::Chunk(_)) && descriptor.length > 32_768 {
            return Err(TreeError::Limit("chunk descriptor length"));
        }
        if previous == Some(descriptor.kind)
            && matches!(descriptor.kind, SegmentKind::Zero | SegmentKind::Hole)
        {
            return Err(TreeError::Invalid(
                "adjacent sparse descriptors must be coalesced",
            ));
        }
        expected = descriptor.ending_offset()?;
        previous = Some(descriptor.kind);
    }
    Ok(())
}

fn validate_attribution_facts(facts: &[AttributionFact]) -> Result<(), TreeError> {
    for fact in facts {
        validate_path(&fact.path, fact.scope == 0)?;
        if !matches!(fact.scope, 0 | 1)
            || (fact.scope == 0 && (fact.offset != 0 || fact.length != 0))
            || (fact.scope == 1
                && (fact.length == 0 || fact.offset.checked_add(fact.length).is_none()))
            || fact.publication == [0; 16]
        {
            return Err(TreeError::Invalid("invalid attribution fact"));
        }
    }
    let mut sorted = facts.to_vec();
    sorted.sort_by_cached_key(|fact| fact.key().expect("validated attribution key"));
    if sorted
        .windows(2)
        .any(|pair| pair[0].key().ok() == pair[1].key().ok())
    {
        return Err(TreeError::Invalid("duplicate attribution fact"));
    }
    for pair in sorted.windows(2) {
        let left = &pair[0];
        let right = &pair[1];
        if left.path == right.path && left.scope == 1 && right.scope == 1 {
            let left_end = left
                .offset
                .checked_add(left.length)
                .ok_or(TreeError::Invalid("attribution range overflow"))?;
            if left_end > right.offset {
                return Err(TreeError::Invalid("overlapping attribution facts"));
            }
            if left_end == right.offset
                && left.actor == right.actor
                && left.publication == right.publication
            {
                return Err(TreeError::Invalid(
                    "adjacent equivalent attribution facts must be coalesced",
                ));
            }
        }
    }
    Ok(())
}

fn attribution_matches(fact: &AttributionFact, query: &AttributionQuery, query_end: u64) -> bool {
    if fact.path != query.path {
        return false;
    }
    if fact.scope == 0 {
        return true;
    }
    let fact_end = fact.offset.saturating_add(fact.length);
    fact.offset < query_end && query.offset < fact_end
}

fn attribution_key_path(key: &[u8]) -> Option<&[u8]> {
    let length = usize::from(u16::from_be_bytes([*key.first()?, *key.get(1)?]));
    key.get(2..2 + length)
}

fn anchored(domain: &[u8], key: &[u8], digest: &mut Sha256Digest) -> Result<bool, TreeError> {
    let mut preimage = Vec::with_capacity(domain.len() + key.len());
    preimage.extend_from_slice(domain);
    preimage.extend_from_slice(key);
    let value = digest.digest_bytes(&preimage)?;
    Ok(value.as_bytes()[0] == 0 && value.as_bytes()[1] & 0xf0 == 0)
}

fn tree_anchor_key(name: &[u8]) -> Result<Vec<u8>, TreeError> {
    let mut key = Vec::with_capacity(2 + name.len());
    push_len_u16(&mut key, name)?;
    Ok(key)
}

fn option_digest(value: Option<Digest32>) -> Vec<u8> {
    match value {
        Some(value) => {
            let mut output = Vec::with_capacity(33);
            output.push(1);
            output.extend_from_slice(value.as_bytes());
            output
        }
        None => vec![0],
    }
}

fn option_u64(value: Option<u64>) -> Vec<u8> {
    value.map_or_else(
        || vec![0],
        |value| {
            let mut output = vec![1];
            output.extend_from_slice(&value.to_be_bytes());
            output
        },
    )
}

fn option_u32(value: Option<u32>) -> Vec<u8> {
    value.map_or_else(
        || vec![0],
        |value| {
            let mut output = vec![1];
            output.extend_from_slice(&value.to_be_bytes());
            output
        },
    )
}

fn option_bytes(value: Option<&[u8]>) -> Vec<u8> {
    value.map_or_else(
        || vec![0],
        |value| {
            let mut output = Vec::with_capacity(1 + value.len());
            output.push(1);
            output.extend_from_slice(value);
            output
        },
    )
}

fn encode_record(record: &CanonicalRecordV3) -> Result<Vec<u8>, TreeError> {
    let mut sink = VecSink::default();
    encode_v3_record(record, &mut sink)?;
    Ok(sink.bytes)
}

fn validate_component(name: &[u8]) -> Result<(), TreeError> {
    if name.is_empty() || name.len() > 255 || name.iter().any(|byte| matches!(*byte, 0 | b'/')) {
        return Err(TreeError::Invalid("invalid tree component"));
    }
    Ok(())
}

fn validate_path(path: &[u8], allow_empty: bool) -> Result<(), TreeError> {
    if path.len() > 4096 || (!allow_empty && path.is_empty()) || path.contains(&0) {
        return Err(TreeError::Invalid("invalid attribution path"));
    }
    if path.is_empty() {
        return Ok(());
    }
    let mut depth = 0_u8;
    for component in path.split(|byte| *byte == b'/') {
        validate_component(component)?;
        depth = depth.checked_add(1).ok_or(TreeError::Limit("path depth"))?;
        if depth > 64 {
            return Err(TreeError::Limit("path depth"));
        }
    }
    Ok(())
}

fn push_len_u16(output: &mut Vec<u8>, value: &[u8]) -> Result<(), TreeError> {
    let length = u16::try_from(value.len()).map_err(|_| TreeError::Limit("u16 byte string"))?;
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(value);
    Ok(())
}

fn push_len_u32(output: &mut Vec<u8>, value: &[u8]) -> Result<(), TreeError> {
    let length = u32::try_from(value.len()).map_err(|_| TreeError::Limit("u32 byte string"))?;
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(value);
    Ok(())
}

fn push_u32(output: &mut Vec<u8>, value: usize) -> Result<(), TreeError> {
    let value = u32::try_from(value).map_err(|_| TreeError::Limit("u32 count"))?;
    output.extend_from_slice(&value.to_be_bytes());
    Ok(())
}

fn one_byte(field: &TlvV3) -> Result<u8, TreeError> {
    field
        .value()
        .first()
        .copied()
        .filter(|_| field.value().len() == 1)
        .ok_or(TreeError::Invalid("one-byte field"))
}

fn be_u16(bytes: &[u8]) -> Result<u16, TreeError> {
    let value: [u8; 2] = bytes
        .try_into()
        .map_err(|_| TreeError::Invalid("u16 field"))?;
    Ok(u16::from_be_bytes(value))
}

fn be_u64(bytes: &[u8]) -> Result<u64, TreeError> {
    let value: [u8; 8] = bytes
        .try_into()
        .map_err(|_| TreeError::Invalid("u64 field"))?;
    Ok(u64::from_be_bytes(value))
}

fn fixed_32(bytes: &[u8]) -> Result<[u8; 32], TreeError> {
    bytes
        .try_into()
        .map_err(|_| TreeError::Invalid("digest field"))
}

fn optional_digest(bytes: &[u8]) -> Result<Option<Digest32>, TreeError> {
    match bytes {
        [0] => Ok(None),
        [1, digest @ ..] if digest.len() == 32 => Ok(Some(Digest32::new(fixed_32(digest)?))),
        _ => Err(TreeError::Invalid("optional digest field")),
    }
}

fn optional_u64(bytes: &[u8]) -> Result<Option<u64>, TreeError> {
    match bytes {
        [0] => Ok(None),
        [1, value @ ..] if value.len() == 8 => Ok(Some(be_u64(value)?)),
        _ => Err(TreeError::Invalid("optional u64 field")),
    }
}

fn optional_bytes(bytes: &[u8]) -> Result<Option<&[u8]>, TreeError> {
    match bytes {
        [0] => Ok(None),
        [1, value @ ..] => Ok(Some(value)),
        _ => Err(TreeError::Invalid("optional byte string field")),
    }
}

#[derive(Default)]
struct VecSink {
    bytes: Vec<u8>,
}

impl CanonicalSink for VecSink {
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }
}

struct ByteCursor<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> ByteCursor<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], TreeError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(TreeError::Invalid("page cursor overflow"))?;
        let value = self
            .bytes
            .get(self.position..end)
            .ok_or(TreeError::Invalid("truncated page"))?;
        self.position = end;
        Ok(value)
    }

    fn byte(&mut self) -> Result<u8, TreeError> {
        Ok(self.take(1)?[0])
    }

    fn u64(&mut self) -> Result<u64, TreeError> {
        be_u64(self.take(8)?)
    }

    fn array_32(&mut self) -> Result<[u8; 32], TreeError> {
        self.take(32)?
            .try_into()
            .map_err(|_| TreeError::Invalid("digest field"))
    }

    fn len_u16(&mut self, maximum: usize) -> Result<&'a [u8], TreeError> {
        let length = usize::from(be_u16(self.take(2)?)?);
        if length > maximum {
            return Err(TreeError::Limit("length-prefixed page field"));
        }
        self.take(length)
    }

    fn fact(&mut self) -> Result<AttributionFact, TreeError> {
        let path = self.len_u16(4096)?.to_vec();
        let scope = self.byte()?;
        let offset = self.u64()?;
        let length = self.u64()?;
        let actor = ActorId::new(self.array_32()?)?;
        let publication: [u8; 16] = self
            .take(16)?
            .try_into()
            .map_err(|_| TreeError::Invalid("publication field"))?;
        Ok(AttributionFact {
            path,
            scope,
            offset,
            length,
            actor,
            publication,
        })
    }

    fn finish(self) -> Result<(), TreeError> {
        if self.position == self.bytes.len() {
            Ok(())
        } else {
            Err(TreeError::Invalid("trailing page bytes"))
        }
    }
}
