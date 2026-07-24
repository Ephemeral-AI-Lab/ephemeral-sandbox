use crate::codec::{
    bounded_u32, checked_add, checked_mul, decode_option, error, read_record_header, read_tlv,
    read_u128, read_u32, read_u64, read_u8, require_kind, write_option_tlv, write_record_header,
    write_tlv, write_tlv_header, write_u128, write_u32, write_u64, ExactLengthSink, LimitedSource,
    RecordKind, SliceSource, BOUNDED_HEADER_BYTES, TLV_HEADER_BYTES,
};
use crate::{
    CanonicalPath, CanonicalSink, CanonicalSource, Capability, CapabilitySet, DigestDomain, Error,
    ErrorKind, FieldClass, HardlinkGroupId, ObjectId, ObjectKind, TreeManifestId, TypedDigest,
    MAX_RECORD_BYTES, ROOT_FORMAT_V2,
};

pub use crate::path::{MAX_COMPONENT_BYTES, MAX_PATH_BYTES};

pub const MAX_SYMLINK_TARGET_BYTES: usize = 4096;
pub const MAX_XATTR_KEY_BYTES: usize = 255;
pub const MAX_ENTRY_METADATA_BYTES: u32 = 65_536;
pub const MAX_TINY_ENTRIES: u64 = 256;

const MIN_ENTRY_RECORD_BYTES: u64 = 147;
const OBJECT_REFERENCE_RECORD_BYTES: u64 = 58;
const REGULAR_METADATA_BASELINE_BYTES: u64 = 88;
const EMPTY_METADATA_RECORD_BYTES: u64 = 73;
const MAX_HARDLINK_FINGERPRINT_BYTES: u32 = MAX_ENTRY_METADATA_BYTES + 14;
const MAX_RECORD_BYTES_USIZE: usize = 262_144;
const MAX_PATH_BYTES_U32: u32 = 4096;
const MAX_XATTR_KEY_BYTES_U32: u32 = 255;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Xattr {
    key: Vec<u8>,
    value: Vec<u8>,
}

impl Xattr {
    pub fn new(key: Vec<u8>, value: Vec<u8>) -> Result<Self, Error> {
        if key.is_empty() || key.len() > MAX_XATTR_KEY_BYTES || key.contains(&0) {
            return Err(error(
                if key.len() > MAX_XATTR_KEY_BYTES {
                    ErrorKind::LimitExceeded
                } else {
                    ErrorKind::InvalidValue
                },
                FieldClass::Xattr,
                u32::try_from(key.len()).unwrap_or(u32::MAX),
            ));
        }
        bounded_u32(value.len(), FieldClass::Xattr, 0)?;
        Ok(Self { key, value })
    }

    #[must_use]
    pub fn key(&self) -> &[u8] {
        &self.key
    }

    #[must_use]
    pub fn value(&self) -> &[u8] {
        &self.value
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct XattrRef<'a> {
    key: &'a [u8],
    value: &'a [u8],
}

impl<'a> XattrRef<'a> {
    #[must_use]
    pub const fn key(self) -> &'a [u8] {
        self.key
    }

    #[must_use]
    pub const fn value(self) -> &'a [u8] {
        self.value
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct PackedXattrs {
    bytes: Vec<u8>,
    descriptors: Vec<[u32; 3]>,
}

impl PackedXattrs {
    fn from_owned(xattrs: Vec<Xattr>) -> Result<Self, Error> {
        let mut data_bytes = 0_usize;
        for xattr in &xattrs {
            data_bytes = data_bytes
                .checked_add(xattr.key.len())
                .and_then(|length| length.checked_add(xattr.value.len()))
                .ok_or_else(|| error(ErrorKind::Overflow, FieldClass::Xattr, 0))?;
        }
        let mut bytes = Vec::with_capacity(data_bytes);
        let mut descriptors = Vec::with_capacity(xattrs.len());
        for xattr in xattrs {
            let offset = u32::try_from(bytes.len())
                .map_err(|_| error(ErrorKind::LimitExceeded, FieldClass::Xattr, 0))?;
            let key_len = u32::try_from(xattr.key.len())
                .map_err(|_| error(ErrorKind::LimitExceeded, FieldClass::Xattr, 0))?;
            let value_len = u32::try_from(xattr.value.len())
                .map_err(|_| error(ErrorKind::LimitExceeded, FieldClass::Xattr, 0))?;
            bytes.extend_from_slice(&xattr.key);
            bytes.extend_from_slice(&xattr.value);
            descriptors.push([offset, key_len, value_len]);
        }
        Ok(Self { bytes, descriptors })
    }

    fn is_empty(&self) -> bool {
        self.descriptors.is_empty()
    }

    fn len(&self) -> usize {
        self.descriptors.len()
    }

    fn iter(&self) -> impl ExactSizeIterator<Item = XattrRef<'_>> + Clone {
        self.descriptors.iter().map(|descriptor| {
            let offset = usize::try_from(descriptor[0]).unwrap_or(usize::MAX);
            let key_len = usize::try_from(descriptor[1]).unwrap_or(usize::MAX);
            let value_len = usize::try_from(descriptor[2]).unwrap_or(usize::MAX);
            let key_end = offset.saturating_add(key_len);
            let value_end = key_end.saturating_add(value_len);
            XattrRef {
                key: self.bytes.get(offset..key_end).unwrap_or_default(),
                value: self.bytes.get(key_end..value_end).unwrap_or_default(),
            }
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NodeMetadata {
    mode: u32,
    uid: u32,
    gid: u32,
    mtime_seconds: i64,
    mtime_nanoseconds: u32,
    xattrs: PackedXattrs,
}

impl NodeMetadata {
    pub fn new(
        mode: u32,
        uid: u32,
        gid: u32,
        mtime_seconds: i64,
        mtime_nanoseconds: u32,
        xattrs: Vec<Xattr>,
    ) -> Result<Self, Error> {
        for (index, pair) in xattrs.windows(2).enumerate() {
            if pair[0].key >= pair[1].key {
                return Err(error(
                    ErrorKind::NonCanonical,
                    FieldClass::Xattr,
                    u32::try_from(index + 1).unwrap_or(u32::MAX),
                ));
            }
        }
        let mut record_len = EMPTY_METADATA_RECORD_BYTES;
        for xattr in &xattrs {
            record_len = checked_add(record_len, 8, FieldClass::Metadata)?;
            record_len = checked_add(
                record_len,
                u64::try_from(xattr.key.len())
                    .map_err(|_| error(ErrorKind::LimitExceeded, FieldClass::Xattr, 0))?,
                FieldClass::Metadata,
            )?;
            record_len = checked_add(
                record_len,
                u64::try_from(xattr.value.len())
                    .map_err(|_| error(ErrorKind::LimitExceeded, FieldClass::Xattr, 0))?,
                FieldClass::Metadata,
            )?;
        }
        if record_len > u64::from(MAX_ENTRY_METADATA_BYTES) {
            return Err(error(
                ErrorKind::LimitExceeded,
                FieldClass::Metadata,
                u32::try_from(record_len).unwrap_or(u32::MAX),
            ));
        }
        Self::from_packed(
            mode,
            uid,
            gid,
            mtime_seconds,
            mtime_nanoseconds,
            PackedXattrs::from_owned(xattrs)?,
        )
    }

    fn from_packed(
        mode: u32,
        uid: u32,
        gid: u32,
        mtime_seconds: i64,
        mtime_nanoseconds: u32,
        xattrs: PackedXattrs,
    ) -> Result<Self, Error> {
        if mode & !0o7777 != 0 {
            return Err(error(ErrorKind::InvalidValue, FieldClass::Mode, mode));
        }
        if mtime_nanoseconds >= 1_000_000_000 {
            return Err(error(
                ErrorKind::InvalidValue,
                FieldClass::Timestamp,
                mtime_nanoseconds,
            ));
        }
        let value = Self {
            mode,
            uid,
            gid,
            mtime_seconds,
            mtime_nanoseconds,
            xattrs,
        };
        if metadata_record_len(&value)? > u64::from(MAX_ENTRY_METADATA_BYTES) {
            return Err(error(ErrorKind::LimitExceeded, FieldClass::Metadata, 0));
        }
        Ok(value)
    }

    #[must_use]
    pub const fn mode(&self) -> u32 {
        self.mode
    }

    #[must_use]
    pub const fn uid(&self) -> u32 {
        self.uid
    }

    #[must_use]
    pub const fn gid(&self) -> u32 {
        self.gid
    }

    #[must_use]
    pub const fn mtime_seconds(&self) -> i64 {
        self.mtime_seconds
    }

    #[must_use]
    pub const fn mtime_nanoseconds(&self) -> u32 {
        self.mtime_nanoseconds
    }

    #[must_use]
    pub fn xattrs(&self) -> impl ExactSizeIterator<Item = XattrRef<'_>> + Clone {
        self.xattrs.iter()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SparseExtent {
    offset: u64,
    length: u64,
}

impl SparseExtent {
    pub fn new(offset: u64, length: u64) -> Result<Self, Error> {
        if length == 0 || offset.checked_add(length).is_none() {
            return Err(error(
                if length == 0 {
                    ErrorKind::InvalidValue
                } else {
                    ErrorKind::Overflow
                },
                FieldClass::SparseExtent,
                0,
            ));
        }
        Ok(Self { offset, length })
    }

    #[must_use]
    pub const fn offset(self) -> u64 {
        self.offset
    }

    #[must_use]
    pub const fn length(self) -> u64 {
        self.length
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum EntryData {
    Directory,
    Regular {
        logical_len: u64,
        holes: Vec<SparseExtent>,
        segments: ObjectId,
        hardlink_group: Option<HardlinkGroupId>,
    },
    Symlink {
        target: Vec<u8>,
    },
    Device {
        major: u32,
        minor: u32,
    },
    Fifo,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreeEntry {
    path: CanonicalPath,
    metadata: NodeMetadata,
    data: EntryData,
}

impl TreeEntry {
    #[must_use]
    pub const fn kind(&self) -> crate::EntryKind {
        match &self.data {
            EntryData::Directory => crate::EntryKind::Directory,
            EntryData::Regular { .. } => crate::EntryKind::Regular,
            EntryData::Symlink { .. } => crate::EntryKind::Symlink,
            EntryData::Device { .. } => crate::EntryKind::Device,
            EntryData::Fifo => crate::EntryKind::Fifo,
        }
    }

    #[must_use]
    pub const fn path(&self) -> &CanonicalPath {
        &self.path
    }

    #[must_use]
    pub const fn metadata(&self) -> &NodeMetadata {
        &self.metadata
    }

    pub fn directory(path: CanonicalPath, metadata: NodeMetadata) -> Result<Self, Error> {
        Self::finish(path, metadata, EntryData::Directory)
    }

    pub fn regular(
        path: CanonicalPath,
        metadata: NodeMetadata,
        logical_len: u64,
        holes: Vec<SparseExtent>,
        segments: ObjectId,
        hardlink_group: Option<HardlinkGroupId>,
    ) -> Result<Self, Error> {
        if segments.kind() != ObjectKind::FileSegments {
            return Err(error(
                ErrorKind::InvalidValue,
                FieldClass::ObjectReference,
                u32::from(segments.kind() as u8),
            ));
        }
        validate_holes(logical_len, &holes)?;
        let metadata_length = metadata_record_len(&metadata)?;
        let hole_bytes = checked_mul(
            u64::try_from(holes.len())
                .map_err(|_| error(ErrorKind::LimitExceeded, FieldClass::SparseExtent, 0))?,
            16,
            FieldClass::SparseExtent,
        )?;
        let hardlink_bytes = if hardlink_group.is_some() { 16 } else { 0 };
        let combined = checked_add(
            checked_add(
                checked_add(
                    REGULAR_METADATA_BASELINE_BYTES,
                    metadata_length
                        .checked_sub(73)
                        .ok_or_else(|| error(ErrorKind::Malformed, FieldClass::Metadata, 0))?,
                    FieldClass::Metadata,
                )?,
                hole_bytes,
                FieldClass::SparseExtent,
            )?,
            hardlink_bytes,
            FieldClass::Hardlink,
        )?;
        if combined > u64::from(MAX_ENTRY_METADATA_BYTES) {
            return Err(error(
                ErrorKind::LimitExceeded,
                FieldClass::Metadata,
                u32::try_from(combined).unwrap_or(u32::MAX),
            ));
        }
        Self::finish(
            path,
            metadata,
            EntryData::Regular {
                logical_len,
                holes,
                segments,
                hardlink_group,
            },
        )
    }

    pub fn symlink(
        path: CanonicalPath,
        metadata: NodeMetadata,
        target: Vec<u8>,
    ) -> Result<Self, Error> {
        if target.len() > MAX_SYMLINK_TARGET_BYTES || target.contains(&0) {
            return Err(error(
                if target.len() > MAX_SYMLINK_TARGET_BYTES {
                    ErrorKind::LimitExceeded
                } else {
                    ErrorKind::InvalidValue
                },
                FieldClass::SymlinkTarget,
                u32::try_from(target.len()).unwrap_or(u32::MAX),
            ));
        }
        Self::finish(path, metadata, EntryData::Symlink { target })
    }

    pub fn device(
        path: CanonicalPath,
        metadata: NodeMetadata,
        major: u32,
        minor: u32,
    ) -> Result<Self, Error> {
        if major == 0 && minor == 0 {
            return Err(error(ErrorKind::InvalidValue, FieldClass::Device, 0));
        }
        Self::finish(path, metadata, EntryData::Device { major, minor })
    }

    pub fn fifo(path: CanonicalPath, metadata: NodeMetadata) -> Result<Self, Error> {
        Self::finish(path, metadata, EntryData::Fifo)
    }

    fn finish(path: CanonicalPath, metadata: NodeMetadata, data: EntryData) -> Result<Self, Error> {
        let value = Self {
            path,
            metadata,
            data,
        };
        let length = entry_record_len(&value)?;
        if length > u64::from(MAX_RECORD_BYTES) {
            return Err(error(
                ErrorKind::LimitExceeded,
                FieldClass::Entry,
                u32::try_from(length).unwrap_or(u32::MAX),
            ));
        }
        Ok(value)
    }

    #[must_use]
    pub fn required_capabilities(&self) -> CapabilitySet {
        let mut capabilities = CapabilitySet::empty();
        if !self.metadata.xattrs.is_empty() {
            capabilities.insert(Capability::Xattrs);
        }
        match &self.data {
            EntryData::Directory | EntryData::Fifo => {}
            EntryData::Regular {
                holes,
                hardlink_group,
                ..
            } => {
                if !holes.is_empty() {
                    capabilities.insert(Capability::SparseHoles);
                }
                if hardlink_group.is_some() {
                    capabilities.insert(Capability::Hardlinks);
                }
            }
            EntryData::Symlink { .. } => capabilities.insert(Capability::Symlinks),
            EntryData::Device { .. } => capabilities.insert(Capability::Devices),
        }
        if matches!(&self.data, EntryData::Fifo) {
            capabilities.insert(Capability::Fifo);
        }
        capabilities
    }
}

fn validate_holes(logical_len: u64, holes: &[SparseExtent]) -> Result<(), Error> {
    let mut previous_end = 0_u64;
    for (index, hole) in holes.iter().enumerate() {
        let end = hole.offset.checked_add(hole.length).ok_or_else(|| {
            error(
                ErrorKind::Overflow,
                FieldClass::SparseExtent,
                index_u32(index),
            )
        })?;
        if end > logical_len || (index > 0 && hole.offset < previous_end) {
            return Err(error(
                ErrorKind::NonCanonical,
                FieldClass::SparseExtent,
                index_u32(index),
            ));
        }
        previous_end = end;
    }
    Ok(())
}

fn index_u32(index: usize) -> u32 {
    u32::try_from(index).unwrap_or(u32::MAX)
}

fn xattrs_payload_len(metadata: &NodeMetadata) -> Result<u64, Error> {
    let count_bytes = checked_mul(
        u64::try_from(metadata.xattrs.len())
            .map_err(|_| error(ErrorKind::LimitExceeded, FieldClass::Xattr, 0))?,
        8,
        FieldClass::Xattr,
    )?;
    checked_add(
        checked_add(4, count_bytes, FieldClass::Xattr)?,
        u64::try_from(metadata.xattrs.bytes.len())
            .map_err(|_| error(ErrorKind::LimitExceeded, FieldClass::Xattr, 0))?,
        FieldClass::Xattr,
    )
}

fn metadata_payload_len(metadata: &NodeMetadata) -> Result<u64, Error> {
    checked_add(
        checked_add(TLV_HEADER_BYTES * 6, 24, FieldClass::Metadata)?,
        xattrs_payload_len(metadata)?,
        FieldClass::Metadata,
    )
}

fn metadata_record_len(metadata: &NodeMetadata) -> Result<u64, Error> {
    checked_add(
        BOUNDED_HEADER_BYTES,
        metadata_payload_len(metadata)?,
        FieldClass::Metadata,
    )
}

fn object_reference_payload_len() -> u64 {
    TLV_HEADER_BYTES * 2 + 33
}

fn option_len(inner: Option<u64>) -> Result<u64, Error> {
    checked_add(1, inner.unwrap_or(0), FieldClass::Length)
}

fn entry_payload_len(entry: &TreeEntry) -> Result<u64, Error> {
    let mut value_bytes = 1_u64;
    value_bytes = checked_add(
        value_bytes,
        u64::try_from(entry.path.as_bytes().len())
            .map_err(|_| error(ErrorKind::LimitExceeded, FieldClass::Path, 0))?,
        FieldClass::Entry,
    )?;
    value_bytes = checked_add(
        value_bytes,
        metadata_record_len(&entry.metadata)?,
        FieldClass::Entry,
    )?;
    let option_values = match &entry.data {
        EntryData::Directory | EntryData::Fifo => option_len(None)? * 7,
        EntryData::Regular {
            holes,
            hardlink_group,
            ..
        } => {
            let holes_len = checked_add(
                4,
                checked_mul(
                    u64::try_from(holes.len()).map_err(|_| {
                        error(ErrorKind::LimitExceeded, FieldClass::SparseExtent, 0)
                    })?,
                    16,
                    FieldClass::SparseExtent,
                )?,
                FieldClass::SparseExtent,
            )?;
            option_len(None)? * 3
                + option_len(Some(8))?
                + option_len(Some(holes_len))?
                + option_len(hardlink_group.map(|_| 16))?
                + option_len(Some(OBJECT_REFERENCE_RECORD_BYTES))?
        }
        EntryData::Symlink { target } => {
            option_len(Some(u64::try_from(target.len()).map_err(|_| {
                error(ErrorKind::LimitExceeded, FieldClass::SymlinkTarget, 0)
            })?))?
                + option_len(None)? * 6
        }
        EntryData::Device { .. } => {
            option_len(None)? + option_len(Some(4))? * 2 + option_len(None)? * 4
        }
    };
    checked_add(
        TLV_HEADER_BYTES * 10,
        checked_add(value_bytes, option_values, FieldClass::Entry)?,
        FieldClass::Entry,
    )
}

fn entry_record_len(entry: &TreeEntry) -> Result<u64, Error> {
    checked_add(
        BOUNDED_HEADER_BYTES,
        entry_payload_len(entry)?,
        FieldClass::Entry,
    )
}

pub fn tree_entry_record_len(entry: &TreeEntry) -> Result<u32, Error> {
    u32::try_from(entry_record_len(entry)?)
        .map_err(|_| error(ErrorKind::LimitExceeded, FieldClass::Entry, 0))
}

fn encode_xattrs(metadata: &NodeMetadata, sink: &mut dyn CanonicalSink) -> Result<(), Error> {
    write_u32(
        sink,
        u32::try_from(metadata.xattrs.len())
            .map_err(|_| error(ErrorKind::LimitExceeded, FieldClass::Xattr, 0))?,
    )?;
    for xattr in metadata.xattrs.iter() {
        let key = xattr.key();
        let value = xattr.value();
        write_u32(
            sink,
            u32::try_from(key.len())
                .map_err(|_| error(ErrorKind::LimitExceeded, FieldClass::Xattr, 0))?,
        )?;
        sink.write_all(key)?;
        write_u32(
            sink,
            u32::try_from(value.len())
                .map_err(|_| error(ErrorKind::LimitExceeded, FieldClass::Xattr, 0))?,
        )?;
        sink.write_all(value)?;
    }
    Ok(())
}

fn encode_metadata(metadata: &NodeMetadata, sink: &mut dyn CanonicalSink) -> Result<(), Error> {
    let payload_len = metadata_payload_len(metadata)?;
    write_record_header(sink, RecordKind::Metadata, ROOT_FORMAT_V2, payload_len)?;
    write_tlv(sink, 1, &metadata.mode.to_be_bytes())?;
    write_tlv(sink, 2, &metadata.uid.to_be_bytes())?;
    write_tlv(sink, 3, &metadata.gid.to_be_bytes())?;
    write_tlv(sink, 4, &metadata.mtime_seconds.to_be_bytes())?;
    write_tlv(sink, 5, &metadata.mtime_nanoseconds.to_be_bytes())?;
    write_tlv_header(
        sink,
        6,
        u32::try_from(xattrs_payload_len(metadata)?)
            .map_err(|_| error(ErrorKind::LimitExceeded, FieldClass::Xattr, 0))?,
    )?;
    encode_xattrs(metadata, sink)
}

fn encode_object_reference(object: ObjectId, sink: &mut dyn CanonicalSink) -> Result<(), Error> {
    write_record_header(
        sink,
        RecordKind::ObjectReference,
        ROOT_FORMAT_V2,
        object_reference_payload_len(),
    )?;
    write_tlv(sink, 1, &[object.kind() as u8])?;
    write_tlv(sink, 2, object.digest().as_bytes())
}

fn encode_holes(holes: &[SparseExtent], sink: &mut dyn CanonicalSink) -> Result<(), Error> {
    write_u32(
        sink,
        u32::try_from(holes.len())
            .map_err(|_| error(ErrorKind::LimitExceeded, FieldClass::SparseExtent, 0))?,
    )?;
    for hole in holes {
        write_u64(sink, hole.offset)?;
        write_u64(sink, hole.length)?;
    }
    Ok(())
}

fn encode_entry(entry: &TreeEntry, sink: &mut dyn CanonicalSink) -> Result<(), Error> {
    let payload_len = entry_payload_len(entry)?;
    write_record_header(sink, RecordKind::Entry, ROOT_FORMAT_V2, payload_len)?;
    write_tlv(sink, 1, &[entry.kind() as u8])?;
    write_tlv(sink, 2, entry.path.as_bytes())?;
    write_tlv_header(
        sink,
        3,
        u32::try_from(metadata_record_len(&entry.metadata)?)
            .map_err(|_| error(ErrorKind::LimitExceeded, FieldClass::Metadata, 0))?,
    )?;
    encode_metadata(&entry.metadata, sink)?;
    match &entry.data {
        EntryData::Directory | EntryData::Fifo => {
            for tag in 4..=10 {
                write_option_tlv(sink, tag, None)?;
            }
        }
        EntryData::Regular {
            logical_len,
            holes,
            segments,
            hardlink_group,
        } => {
            write_option_tlv(sink, 4, None)?;
            write_option_tlv(sink, 5, None)?;
            write_option_tlv(sink, 6, None)?;
            write_option_tlv(sink, 7, Some(&logical_len.to_be_bytes()))?;
            let holes_len = 4_u64
                .checked_add(
                    u64::try_from(holes.len())
                        .map_err(|_| error(ErrorKind::LimitExceeded, FieldClass::SparseExtent, 0))?
                        .checked_mul(16)
                        .ok_or_else(|| error(ErrorKind::Overflow, FieldClass::SparseExtent, 0))?,
                )
                .ok_or_else(|| error(ErrorKind::Overflow, FieldClass::SparseExtent, 0))?;
            write_tlv_header(
                sink,
                8,
                u32::try_from(holes_len + 1)
                    .map_err(|_| error(ErrorKind::LimitExceeded, FieldClass::SparseExtent, 0))?,
            )?;
            sink.write_all(&[1])?;
            encode_holes(holes, sink)?;
            let hardlink_bytes = hardlink_group.map(|group| group.get().to_be_bytes());
            write_option_tlv(
                sink,
                9,
                hardlink_bytes.as_ref().map(|bytes| bytes.as_slice()),
            )?;
            write_tlv_header(
                sink,
                10,
                u32::try_from(OBJECT_REFERENCE_RECORD_BYTES + 1)
                    .map_err(|_| error(ErrorKind::LimitExceeded, FieldClass::ObjectReference, 0))?,
            )?;
            sink.write_all(&[1])?;
            encode_object_reference(*segments, sink)?;
        }
        EntryData::Symlink { target } => {
            write_option_tlv(sink, 4, Some(target))?;
            for tag in 5..=10 {
                write_option_tlv(sink, tag, None)?;
            }
        }
        EntryData::Device { major, minor } => {
            write_option_tlv(sink, 4, None)?;
            write_option_tlv(sink, 5, Some(&major.to_be_bytes()))?;
            write_option_tlv(sink, 6, Some(&minor.to_be_bytes()))?;
            for tag in 7..=10 {
                write_option_tlv(sink, tag, None)?;
            }
        }
    }
    Ok(())
}

pub fn tree_record_payload_len(entries_bytes: u64) -> Result<u64, Error> {
    checked_add(16, entries_bytes, FieldClass::Tree)
}

pub fn encode_tree_record(
    entry_count: u64,
    entries_bytes: u64,
    entries: &mut dyn Iterator<Item = &TreeEntry>,
    sink: &mut dyn CanonicalSink,
) -> Result<CapabilitySet, Error> {
    if entry_count > MAX_TINY_ENTRIES {
        return Err(error(
            ErrorKind::LimitExceeded,
            FieldClass::Tree,
            u32::try_from(entry_count).unwrap_or(u32::MAX),
        ));
    }
    let minimum = checked_mul(entry_count, MIN_ENTRY_RECORD_BYTES, FieldClass::Tree)?;
    if minimum > entries_bytes {
        return Err(error(
            ErrorKind::Malformed,
            FieldClass::Tree,
            u32::try_from(entry_count).unwrap_or(u32::MAX),
        ));
    }
    let payload_len = tree_record_payload_len(entries_bytes)?;
    write_record_header(sink, RecordKind::Tree, ROOT_FORMAT_V2, payload_len)?;
    write_u64(sink, entry_count)?;
    write_u64(sink, entries_bytes)?;
    let mut exact = ExactLengthSink::new(sink, entries_bytes);
    let mut previous_path: Option<Vec<u8>> = None;
    let mut actual_count = 0_u64;
    let mut capabilities = CapabilitySet::empty();
    for entry in entries {
        if previous_path
            .as_deref()
            .is_some_and(|previous| previous >= entry.path.as_bytes())
        {
            return Err(error(
                ErrorKind::NonCanonical,
                FieldClass::Path,
                u32::try_from(actual_count).unwrap_or(u32::MAX),
            ));
        }
        previous_path = Some(entry.path.as_bytes().to_vec());
        encode_entry(entry, &mut exact)?;
        actual_count = actual_count
            .checked_add(1)
            .ok_or_else(|| error(ErrorKind::Overflow, FieldClass::Tree, 0))?;
        merge_capabilities(&mut capabilities, entry.required_capabilities());
    }
    if actual_count != entry_count {
        return Err(error(
            ErrorKind::Malformed,
            FieldClass::Tree,
            u32::try_from(actual_count).unwrap_or(u32::MAX),
        ));
    }
    exact.finish(FieldClass::Tree)?;
    Ok(capabilities)
}

fn merge_capabilities(target: &mut CapabilitySet, source: CapabilitySet) {
    for capability in [
        Capability::Xattrs,
        Capability::SparseHoles,
        Capability::Hardlinks,
        Capability::Symlinks,
        Capability::Devices,
        Capability::Fifo,
    ] {
        if source.contains(capability) {
            target.insert(capability);
        }
    }
}

fn fixed<const N: usize>(bytes: &[u8], field: FieldClass, ordinal: u32) -> Result<[u8; N], Error> {
    bytes
        .try_into()
        .map_err(|_| error(ErrorKind::Malformed, field, ordinal))
}

fn read_tlv_length(
    source: &mut LimitedSource<'_>,
    expected_tag: u8,
    field: FieldClass,
    maximum_len: u32,
) -> Result<u32, Error> {
    let tag = read_u8(source)?;
    if tag != expected_tag {
        return Err(error(
            ErrorKind::NonCanonical,
            field,
            u32::from(expected_tag),
        ));
    }
    let length = read_u32(source)?;
    if length > maximum_len || u64::from(length) > source.remaining() {
        return Err(error(
            ErrorKind::LimitExceeded,
            field,
            u32::from(expected_tag),
        ));
    }
    Ok(length)
}

fn read_fixed_tlv<const N: usize>(
    source: &mut LimitedSource<'_>,
    expected_tag: u8,
    field: FieldClass,
) -> Result<[u8; N], Error> {
    let maximum = u32::try_from(N)
        .map_err(|_| error(ErrorKind::LimitExceeded, field, u32::from(expected_tag)))?;
    let length = read_tlv_length(source, expected_tag, field, maximum)?;
    if length != maximum {
        return Err(error(ErrorKind::Malformed, field, u32::from(expected_tag)));
    }
    let mut bytes = [0_u8; N];
    source.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn decode_xattrs(
    source: &mut dyn CanonicalSource,
    payload_len: u32,
) -> Result<PackedXattrs, Error> {
    let mut payload = LimitedSource::new(source, u64::from(payload_len));
    let count = read_u32(&mut payload)?;
    let minimum = u64::from(count)
        .checked_mul(9)
        .ok_or_else(|| error(ErrorKind::Overflow, FieldClass::Xattr, count))?;
    if minimum > payload.remaining() {
        return Err(error(ErrorKind::Malformed, FieldClass::Xattr, count));
    }
    let descriptor_count = usize::try_from(count)
        .map_err(|_| error(ErrorKind::LimitExceeded, FieldClass::Xattr, count))?;
    let framing_bytes = u64::from(count)
        .checked_mul(8)
        .ok_or_else(|| error(ErrorKind::Overflow, FieldClass::Xattr, count))?;
    let data_capacity = payload
        .remaining()
        .checked_sub(framing_bytes)
        .and_then(|length| usize::try_from(length).ok())
        .ok_or_else(|| error(ErrorKind::Overflow, FieldClass::Xattr, count))?;
    let mut bytes = Vec::with_capacity(data_capacity);
    let mut descriptors: Vec<[u32; 3]> = Vec::with_capacity(descriptor_count);
    for index in 0..count {
        let key_len = read_u32(&mut payload)?;
        if key_len == 0 || key_len > MAX_XATTR_KEY_BYTES_U32 {
            return Err(error(
                if key_len == 0 {
                    ErrorKind::InvalidValue
                } else {
                    ErrorKind::LimitExceeded
                },
                FieldClass::Xattr,
                index,
            ));
        }
        if u64::from(key_len) > payload.remaining() {
            return Err(error(ErrorKind::Malformed, FieldClass::Xattr, index));
        }
        let offset = bytes.len();
        let key_end = offset
            .checked_add(
                usize::try_from(key_len)
                    .map_err(|_| error(ErrorKind::LimitExceeded, FieldClass::Xattr, index))?,
            )
            .ok_or_else(|| error(ErrorKind::Overflow, FieldClass::Xattr, index))?;
        bytes.resize(key_end, 0);
        payload.read_exact(&mut bytes[offset..key_end])?;
        let key = &bytes[offset..key_end];
        if key.contains(&0) {
            return Err(error(ErrorKind::InvalidValue, FieldClass::Xattr, index));
        }
        if let Some(previous) = descriptors.last() {
            let previous_start = usize::try_from(previous[0]).unwrap_or(usize::MAX);
            let previous_end =
                previous_start.saturating_add(usize::try_from(previous[1]).unwrap_or(usize::MAX));
            if bytes
                .get(previous_start..previous_end)
                .is_none_or(|previous_key| previous_key >= key)
            {
                return Err(error(ErrorKind::NonCanonical, FieldClass::Xattr, index));
            }
        }
        let value_len = read_u32(&mut payload)?;
        if u64::from(value_len) > payload.remaining() {
            return Err(error(ErrorKind::Malformed, FieldClass::Xattr, index));
        }
        let value_end = key_end
            .checked_add(
                usize::try_from(value_len)
                    .map_err(|_| error(ErrorKind::LimitExceeded, FieldClass::Xattr, index))?,
            )
            .ok_or_else(|| error(ErrorKind::Overflow, FieldClass::Xattr, index))?;
        bytes.resize(value_end, 0);
        payload.read_exact(&mut bytes[key_end..value_end])?;
        descriptors.push([
            u32::try_from(offset)
                .map_err(|_| error(ErrorKind::LimitExceeded, FieldClass::Xattr, index))?,
            key_len,
            value_len,
        ]);
    }
    payload.finish(FieldClass::Xattr)?;
    Ok(PackedXattrs { bytes, descriptors })
}

fn decode_metadata(
    source: &mut dyn CanonicalSource,
    record_len: u32,
) -> Result<NodeMetadata, Error> {
    let mut record = LimitedSource::new(source, u64::from(record_len));
    let header = read_record_header(&mut record)?;
    require_kind(header, RecordKind::Metadata)?;
    let mut payload = LimitedSource::new(&mut record, header.payload_len);
    let mode = u32::from_be_bytes(read_fixed_tlv(&mut payload, 1, FieldClass::Mode)?);
    let uid = u32::from_be_bytes(read_fixed_tlv(&mut payload, 2, FieldClass::Metadata)?);
    let gid = u32::from_be_bytes(read_fixed_tlv(&mut payload, 3, FieldClass::Metadata)?);
    let mtime_seconds = i64::from_be_bytes(read_fixed_tlv(&mut payload, 4, FieldClass::Timestamp)?);
    let mtime_nanoseconds =
        u32::from_be_bytes(read_fixed_tlv(&mut payload, 5, FieldClass::Timestamp)?);
    let xattrs_len = read_tlv_length(&mut payload, 6, FieldClass::Xattr, MAX_ENTRY_METADATA_BYTES)?;
    let xattrs = decode_xattrs(&mut payload, xattrs_len)?;
    payload.finish(FieldClass::Metadata)?;
    record.finish(FieldClass::Metadata)?;
    NodeMetadata::from_packed(mode, uid, gid, mtime_seconds, mtime_nanoseconds, xattrs)
}

fn decode_object_reference(bytes: &[u8]) -> Result<ObjectId, Error> {
    let mut source = SliceSource::new(bytes);
    let header = read_record_header(&mut source)?;
    require_kind(header, RecordKind::ObjectReference)?;
    let mut payload = LimitedSource::new(&mut source, header.payload_len);
    let kind_bytes = read_tlv(&mut payload, 1, FieldClass::ObjectReference, 1)?;
    let kind = ObjectKind::from_u8(
        *kind_bytes
            .first()
            .ok_or_else(|| error(ErrorKind::Malformed, FieldClass::ObjectReference, 1))?,
    )?;
    if kind_bytes.len() != 1 {
        return Err(error(ErrorKind::Malformed, FieldClass::ObjectReference, 1));
    }
    let digest = crate::Digest32::new(fixed(
        &read_tlv(&mut payload, 2, FieldClass::Digest, 32)?,
        FieldClass::Digest,
        2,
    )?);
    payload.finish(FieldClass::ObjectReference)?;
    source.ensure_exhausted()?;
    Ok(ObjectId::new(kind, digest))
}

fn decode_holes(bytes: &[u8], logical_len: u64) -> Result<Vec<SparseExtent>, Error> {
    let mut source = SliceSource::new(bytes);
    let count = read_u32(&mut source)?;
    let expected = usize::try_from(count)
        .ok()
        .and_then(|value| value.checked_mul(16))
        .ok_or_else(|| error(ErrorKind::Overflow, FieldClass::SparseExtent, count))?;
    if expected != source.remaining_len() {
        return Err(error(ErrorKind::Malformed, FieldClass::SparseExtent, count));
    }
    let mut holes = Vec::with_capacity(
        usize::try_from(count)
            .map_err(|_| error(ErrorKind::LimitExceeded, FieldClass::SparseExtent, count))?,
    );
    for _ in 0..count {
        holes.push(SparseExtent::new(
            read_u64(&mut source)?,
            read_u64(&mut source)?,
        )?);
    }
    source.ensure_exhausted()?;
    validate_holes(logical_len, &holes)?;
    Ok(holes)
}

fn require_none(bytes: &[u8], field: FieldClass, ordinal: u32) -> Result<(), Error> {
    if decode_option(bytes, field, ordinal)?.is_none() {
        Ok(())
    } else {
        Err(error(ErrorKind::InvalidValue, field, ordinal))
    }
}

fn decode_entry(source: &mut dyn CanonicalSource) -> Result<TreeEntry, Error> {
    let header = read_record_header(source)?;
    require_kind(header, RecordKind::Entry)?;
    if header.payload_len > u64::from(MAX_RECORD_BYTES) {
        return Err(error(
            ErrorKind::LimitExceeded,
            FieldClass::Entry,
            u32::try_from(header.payload_len).unwrap_or(u32::MAX),
        ));
    }
    let mut payload = LimitedSource::new(source, header.payload_len);
    let kind_value = read_tlv(&mut payload, 1, FieldClass::Entry, 1)?;
    if kind_value.len() != 1 {
        return Err(error(ErrorKind::Malformed, FieldClass::Entry, 1));
    }
    let kind = crate::EntryKind::from_u8(kind_value[0])?;
    let path = CanonicalPath::new(read_tlv(
        &mut payload,
        2,
        FieldClass::Path,
        MAX_PATH_BYTES_U32,
    )?)?;
    let metadata_len = read_tlv_length(
        &mut payload,
        3,
        FieldClass::Metadata,
        MAX_ENTRY_METADATA_BYTES,
    )?;
    let metadata = decode_metadata(&mut payload, metadata_len)?;
    let target = read_tlv(
        &mut payload,
        4,
        FieldClass::SymlinkTarget,
        u32::try_from(MAX_SYMLINK_TARGET_BYTES + 1).unwrap_or(u32::MAX),
    )?;
    let major = read_tlv(&mut payload, 5, FieldClass::Device, 5)?;
    let minor = read_tlv(&mut payload, 6, FieldClass::Device, 5)?;
    let logical_len = read_tlv(&mut payload, 7, FieldClass::LogicalLength, 9)?;
    let holes = read_tlv(
        &mut payload,
        8,
        FieldClass::SparseExtent,
        MAX_ENTRY_METADATA_BYTES,
    )?;
    let hardlink = read_tlv(&mut payload, 9, FieldClass::Hardlink, 17)?;
    let object = read_tlv(
        &mut payload,
        10,
        FieldClass::ObjectReference,
        u32::try_from(OBJECT_REFERENCE_RECORD_BYTES + 1).unwrap_or(u32::MAX),
    )?;
    payload.finish(FieldClass::Entry)?;
    match kind {
        crate::EntryKind::Directory => {
            require_none(&target, FieldClass::SymlinkTarget, 4)?;
            require_none(&major, FieldClass::Device, 5)?;
            require_none(&minor, FieldClass::Device, 6)?;
            require_none(&logical_len, FieldClass::LogicalLength, 7)?;
            require_none(&holes, FieldClass::SparseExtent, 8)?;
            require_none(&hardlink, FieldClass::Hardlink, 9)?;
            require_none(&object, FieldClass::ObjectReference, 10)?;
            TreeEntry::directory(path, metadata)
        }
        crate::EntryKind::Regular => {
            require_none(&target, FieldClass::SymlinkTarget, 4)?;
            require_none(&major, FieldClass::Device, 5)?;
            require_none(&minor, FieldClass::Device, 6)?;
            let logical_len = u64::from_be_bytes(fixed(
                decode_option(&logical_len, FieldClass::LogicalLength, 7)?
                    .ok_or_else(|| error(ErrorKind::InvalidValue, FieldClass::LogicalLength, 7))?,
                FieldClass::LogicalLength,
                7,
            )?);
            let holes = decode_holes(
                decode_option(&holes, FieldClass::SparseExtent, 8)?
                    .ok_or_else(|| error(ErrorKind::InvalidValue, FieldClass::SparseExtent, 8))?,
                logical_len,
            )?;
            let hardlink = decode_option(&hardlink, FieldClass::Hardlink, 9)?
                .map(|bytes| {
                    HardlinkGroupId::new(u128::from_be_bytes(fixed(
                        bytes,
                        FieldClass::Hardlink,
                        9,
                    )?))
                })
                .transpose()?;
            let object = decode_object_reference(
                decode_option(&object, FieldClass::ObjectReference, 10)?.ok_or_else(|| {
                    error(ErrorKind::InvalidValue, FieldClass::ObjectReference, 10)
                })?,
            )?;
            TreeEntry::regular(path, metadata, logical_len, holes, object, hardlink)
        }
        crate::EntryKind::Symlink => {
            let target = decode_option(&target, FieldClass::SymlinkTarget, 4)?
                .ok_or_else(|| error(ErrorKind::InvalidValue, FieldClass::SymlinkTarget, 4))?
                .to_vec();
            require_none(&major, FieldClass::Device, 5)?;
            require_none(&minor, FieldClass::Device, 6)?;
            require_none(&logical_len, FieldClass::LogicalLength, 7)?;
            require_none(&holes, FieldClass::SparseExtent, 8)?;
            require_none(&hardlink, FieldClass::Hardlink, 9)?;
            require_none(&object, FieldClass::ObjectReference, 10)?;
            TreeEntry::symlink(path, metadata, target)
        }
        crate::EntryKind::Device => {
            require_none(&target, FieldClass::SymlinkTarget, 4)?;
            let major = u32::from_be_bytes(fixed(
                decode_option(&major, FieldClass::Device, 5)?
                    .ok_or_else(|| error(ErrorKind::InvalidValue, FieldClass::Device, 5))?,
                FieldClass::Device,
                5,
            )?);
            let minor = u32::from_be_bytes(fixed(
                decode_option(&minor, FieldClass::Device, 6)?
                    .ok_or_else(|| error(ErrorKind::InvalidValue, FieldClass::Device, 6))?,
                FieldClass::Device,
                6,
            )?);
            require_none(&logical_len, FieldClass::LogicalLength, 7)?;
            require_none(&holes, FieldClass::SparseExtent, 8)?;
            require_none(&hardlink, FieldClass::Hardlink, 9)?;
            require_none(&object, FieldClass::ObjectReference, 10)?;
            TreeEntry::device(path, metadata, major, minor)
        }
        crate::EntryKind::Fifo => {
            require_none(&target, FieldClass::SymlinkTarget, 4)?;
            require_none(&major, FieldClass::Device, 5)?;
            require_none(&minor, FieldClass::Device, 6)?;
            require_none(&logical_len, FieldClass::LogicalLength, 7)?;
            require_none(&holes, FieldClass::SparseExtent, 8)?;
            require_none(&hardlink, FieldClass::Hardlink, 9)?;
            require_none(&object, FieldClass::ObjectReference, 10)?;
            TreeEntry::fifo(path, metadata)
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct TreeSummary {
    entry_count: u64,
    capabilities: CapabilitySet,
    hardlink_claim_count: u64,
    reference_claim_count: u64,
}

fn decode_tree_payload(
    source: &mut dyn CanonicalSource,
    payload_len: u64,
    on_entry: &mut dyn FnMut(&TreeEntry) -> Result<(), Error>,
) -> Result<TreeSummary, Error> {
    if payload_len < 16 {
        return Err(error(ErrorKind::Malformed, FieldClass::Tree, 0));
    }
    let mut payload = LimitedSource::new(source, payload_len);
    let entry_count = read_u64(&mut payload)?;
    let entries_bytes = read_u64(&mut payload)?;
    if entry_count > MAX_TINY_ENTRIES {
        return Err(error(
            ErrorKind::LimitExceeded,
            FieldClass::Tree,
            u32::try_from(entry_count).unwrap_or(u32::MAX),
        ));
    }
    if checked_add(16, entries_bytes, FieldClass::Tree)? != payload_len {
        return Err(error(ErrorKind::Malformed, FieldClass::Tree, 0));
    }
    let minimum = checked_mul(entry_count, MIN_ENTRY_RECORD_BYTES, FieldClass::Tree)?;
    if minimum > entries_bytes {
        return Err(error(
            ErrorKind::Malformed,
            FieldClass::Tree,
            u32::try_from(entry_count).unwrap_or(u32::MAX),
        ));
    }
    let mut entries_source = LimitedSource::new(&mut payload, entries_bytes);
    let mut previous_path: Option<Vec<u8>> = None;
    let mut capabilities = CapabilitySet::empty();
    let mut hardlink_claim_count = 0_u64;
    let mut reference_claim_count = 0_u64;
    let mut maximum_hardlink_rank = 0_u128;
    for index in 0..entry_count {
        let entry = decode_entry(&mut entries_source)?;
        if previous_path
            .as_deref()
            .is_some_and(|previous| previous >= entry.path.as_bytes())
        {
            return Err(error(
                ErrorKind::NonCanonical,
                FieldClass::Path,
                u32::try_from(index).unwrap_or(u32::MAX),
            ));
        }
        previous_path = Some(entry.path.as_bytes().to_vec());
        if let EntryData::Regular { hardlink_group, .. } = &entry.data {
            reference_claim_count = reference_claim_count
                .checked_add(1)
                .ok_or_else(|| error(ErrorKind::Overflow, FieldClass::ObjectReference, 0))?;
            if let Some(group) = hardlink_group {
                if group.get() > maximum_hardlink_rank.saturating_add(1) {
                    return Err(error(
                        ErrorKind::NonCanonical,
                        FieldClass::Hardlink,
                        u32::try_from(index).unwrap_or(u32::MAX),
                    ));
                }
                maximum_hardlink_rank = maximum_hardlink_rank.max(group.get());
                hardlink_claim_count = hardlink_claim_count
                    .checked_add(1)
                    .ok_or_else(|| error(ErrorKind::Overflow, FieldClass::Hardlink, 0))?;
            }
        }
        merge_capabilities(&mut capabilities, entry.required_capabilities());
        on_entry(&entry)?;
    }
    entries_source.finish(FieldClass::Tree)?;
    payload.finish(FieldClass::Tree)?;
    Ok(TreeSummary {
        entry_count,
        capabilities,
        hardlink_claim_count,
        reference_claim_count,
    })
}

pub fn decode_tree_record(
    source: &mut dyn CanonicalSource,
    on_entry: &mut dyn FnMut(&TreeEntry) -> Result<(), Error>,
) -> Result<CapabilitySet, Error> {
    let header = read_record_header(source)?;
    require_kind(header, RecordKind::Tree)?;
    let summary = decode_tree_payload(source, header.payload_len, on_entry)?;
    source.ensure_exhausted()?;
    Ok(summary.capabilities)
}

struct TeeSource<'a> {
    source: &'a mut dyn CanonicalSource,
    sink: &'a mut dyn CanonicalSink,
}

impl CanonicalSource for TeeSource<'_> {
    fn read_exact(&mut self, bytes: &mut [u8]) -> Result<(), Error> {
        self.source.read_exact(bytes)?;
        self.sink.write_all(bytes)
    }

    fn ensure_exhausted(&mut self) -> Result<(), Error> {
        self.source.ensure_exhausted()
    }
}

#[derive(Default)]
struct BufferSink {
    bytes: Vec<u8>,
}

impl BufferSink {
    fn with_capacity(length: u64, field: FieldClass) -> Result<Self, Error> {
        let capacity =
            usize::try_from(length).map_err(|_| error(ErrorKind::LimitExceeded, field, 0))?;
        if capacity > MAX_RECORD_BYTES_USIZE {
            return Err(error(
                ErrorKind::LimitExceeded,
                field,
                u32::try_from(capacity).unwrap_or(u32::MAX),
            ));
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(capacity)
            .map_err(|_| error(ErrorKind::LimitExceeded, field, 0))?;
        Ok(Self { bytes })
    }
}

impl CanonicalSink for BufferSink {
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let next = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .ok_or_else(|| error(ErrorKind::Overflow, FieldClass::Sink, 0))?;
        if next > MAX_RECORD_BYTES_USIZE {
            return Err(error(
                ErrorKind::LimitExceeded,
                FieldClass::Sink,
                u32::try_from(next).unwrap_or(u32::MAX),
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }
}

fn emit_reference_claim(object: ObjectId, sink: &mut dyn CanonicalSink) -> Result<(), Error> {
    let mut bytes = [0_u8; 33];
    bytes[0] = object.kind() as u8;
    bytes[1..].copy_from_slice(object.digest().as_bytes());
    sink.write_all(&bytes)
}

fn emit_hardlink_claim(
    entry: &TreeEntry,
    group: HardlinkGroupId,
    sink: &mut dyn CanonicalSink,
) -> Result<(), Error> {
    let EntryData::Regular {
        logical_len,
        holes,
        segments,
        ..
    } = &entry.data
    else {
        return Err(error(ErrorKind::InvalidValue, FieldClass::Hardlink, 0));
    };
    let hole_bytes = checked_mul(
        u64::try_from(holes.len())
            .map_err(|_| error(ErrorKind::LimitExceeded, FieldClass::SparseExtent, 0))?,
        16,
        FieldClass::SparseExtent,
    )?;
    let fingerprint_len = checked_add(
        checked_add(
            checked_add(
                metadata_record_len(&entry.metadata)?,
                8,
                FieldClass::Hardlink,
            )?,
            checked_add(4, hole_bytes, FieldClass::SparseExtent)?,
            FieldClass::Hardlink,
        )?,
        33,
        FieldClass::Hardlink,
    )?;
    if fingerprint_len > u64::from(MAX_HARDLINK_FINGERPRINT_BYTES) {
        return Err(error(
            ErrorKind::LimitExceeded,
            FieldClass::Hardlink,
            u32::try_from(fingerprint_len).unwrap_or(u32::MAX),
        ));
    }
    let path_len = u32::try_from(entry.path.as_bytes().len())
        .map_err(|_| error(ErrorKind::LimitExceeded, FieldClass::Path, 0))?;
    let body_len = 4_u64
        .checked_add(u64::from(path_len))
        .and_then(|value| value.checked_add(4))
        .and_then(|value| value.checked_add(fingerprint_len))
        .ok_or_else(|| error(ErrorKind::Overflow, FieldClass::Hardlink, 0))?;
    let claim_len = checked_add(20, body_len, FieldClass::Hardlink)?;
    let mut claim = BufferSink::with_capacity(claim_len, FieldClass::Hardlink)?;
    write_u128(&mut claim, group.get())?;
    write_u32(
        &mut claim,
        u32::try_from(body_len)
            .map_err(|_| error(ErrorKind::LimitExceeded, FieldClass::Hardlink, 0))?,
    )?;
    write_u32(&mut claim, path_len)?;
    claim.write_all(entry.path.as_bytes())?;
    write_u32(
        &mut claim,
        u32::try_from(fingerprint_len)
            .map_err(|_| error(ErrorKind::LimitExceeded, FieldClass::Hardlink, 0))?,
    )?;
    encode_metadata(&entry.metadata, &mut claim)?;
    write_u64(&mut claim, *logical_len)?;
    encode_holes(holes, &mut claim)?;
    emit_reference_claim(*segments, &mut claim)?;
    if u64::try_from(claim.bytes.len()).ok() != Some(claim_len) {
        return Err(error(
            ErrorKind::Malformed,
            FieldClass::Hardlink,
            u32::try_from(claim.bytes.len()).unwrap_or(u32::MAX),
        ));
    }
    sink.write_all(&claim.bytes)
}

#[derive(Debug)]
pub struct PendingTree {
    digest: crate::Digest32,
    summary: TreeSummary,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ValidatedTree {
    id: TreeManifestId,
    entry_count: u64,
    required_capabilities: CapabilitySet,
}

impl ValidatedTree {
    #[must_use]
    pub const fn id(self) -> TreeManifestId {
        self.id
    }

    #[must_use]
    pub const fn entry_count(self) -> u64 {
        self.entry_count
    }

    #[must_use]
    pub const fn required_capabilities(self) -> CapabilitySet {
        self.required_capabilities
    }
}

pub fn stage_tree_candidate(
    tree_source: &mut dyn CanonicalSource,
    hardlink_claim_sink: &mut dyn CanonicalSink,
    reference_claim_sink: &mut dyn CanonicalSink,
    digest: &mut dyn TypedDigest,
) -> Result<PendingTree, Error> {
    let header = read_record_header(tree_source)?;
    require_kind(header, RecordKind::Tree)?;
    let mut invocation_count = 0_u8;
    let mut summary = None;
    let tree_digest = {
        let mut encode_payload = |digest_sink: &mut dyn CanonicalSink| {
            invocation_count = invocation_count
                .checked_add(1)
                .ok_or_else(|| error(ErrorKind::Overflow, FieldClass::Digest, 0))?;
            let mut limited = LimitedSource::new(tree_source, header.payload_len);
            let mut tee = TeeSource {
                source: &mut limited,
                sink: digest_sink,
            };
            let mut on_entry = |entry: &TreeEntry| {
                if let EntryData::Regular {
                    segments,
                    hardlink_group,
                    ..
                } = &entry.data
                {
                    emit_reference_claim(*segments, reference_claim_sink)?;
                    if let Some(group) = hardlink_group {
                        emit_hardlink_claim(entry, *group, hardlink_claim_sink)?;
                    }
                }
                Ok(())
            };
            let decoded = decode_tree_payload(&mut tee, header.payload_len, &mut on_entry)?;
            tee.ensure_exhausted()?;
            limited.finish(FieldClass::Tree)?;
            summary = Some(decoded);
            Ok(())
        };
        digest.digest(
            DigestDomain::TreeManifest,
            header.version,
            header.payload_len,
            &mut encode_payload,
        )?
    };
    if invocation_count != 1 {
        return Err(error(
            ErrorKind::DigestFailure,
            FieldClass::Digest,
            u32::from(invocation_count),
        ));
    }
    tree_source.ensure_exhausted()?;
    let summary = summary.ok_or_else(|| error(ErrorKind::DigestFailure, FieldClass::Digest, 0))?;
    Ok(PendingTree {
        digest: tree_digest,
        summary,
    })
}

fn read_file_segments_reference(source: &mut dyn CanonicalSource) -> Result<ObjectId, Error> {
    let mut bytes = [0_u8; 33];
    source.read_exact(&mut bytes)?;
    let kind = ObjectKind::from_u8(bytes[0])?;
    if kind != ObjectKind::FileSegments {
        return Err(error(
            ErrorKind::InvalidValue,
            FieldClass::ObjectReference,
            u32::from(bytes[0]),
        ));
    }
    Ok(ObjectId::new(
        kind,
        crate::Digest32::new(
            bytes[1..]
                .try_into()
                .map_err(|_| error(ErrorKind::Malformed, FieldClass::ObjectReference, 0))?,
        ),
    ))
}

struct HardlinkClaim {
    group: HardlinkGroupId,
    path: CanonicalPath,
    fingerprint: Vec<u8>,
}

fn read_hardlink_claim(
    source: &mut dyn CanonicalSource,
    ordinal: u32,
) -> Result<HardlinkClaim, Error> {
    let group = HardlinkGroupId::new(read_u128(source)?)?;
    let body_len = read_u32(source)?;
    if body_len > MAX_RECORD_BYTES {
        return Err(error(
            ErrorKind::LimitExceeded,
            FieldClass::Hardlink,
            ordinal,
        ));
    }
    let mut body_source = LimitedSource::new(source, u64::from(body_len));
    let path_len = read_u32(&mut body_source)?;
    if path_len == 0 || path_len > MAX_PATH_BYTES_U32 {
        return Err(error(ErrorKind::LimitExceeded, FieldClass::Path, ordinal));
    }
    let mut path = vec![
        0_u8;
        usize::try_from(path_len).map_err(|_| error(
            ErrorKind::LimitExceeded,
            FieldClass::Path,
            ordinal
        ))?
    ];
    body_source.read_exact(&mut path)?;
    let path = CanonicalPath::new(path)?;
    let fingerprint_len = read_u32(&mut body_source)?;
    if fingerprint_len > MAX_HARDLINK_FINGERPRINT_BYTES
        || u64::from(fingerprint_len) != body_source.remaining()
    {
        return Err(error(ErrorKind::Malformed, FieldClass::Hardlink, ordinal));
    }
    let mut fingerprint = vec![
        0_u8;
        usize::try_from(fingerprint_len).map_err(|_| error(
            ErrorKind::LimitExceeded,
            FieldClass::Hardlink,
            ordinal
        ))?
    ];
    body_source.read_exact(&mut fingerprint)?;
    body_source.finish(FieldClass::Hardlink)?;
    Ok(HardlinkClaim {
        group,
        path,
        fingerprint,
    })
}

fn validate_hardlinks(count: u64, source: &mut dyn CanonicalSource) -> Result<(), Error> {
    let mut previous_group = None;
    let mut previous_path: Option<CanonicalPath> = None;
    let mut previous_fingerprint = Vec::new();
    let mut group_size = 0_u64;
    for ordinal in 0..count {
        let claim = read_hardlink_claim(source, u32::try_from(ordinal).unwrap_or(u32::MAX))?;
        match previous_group {
            None => {
                if claim.group.get() != 1 {
                    return Err(error(
                        ErrorKind::NonCanonical,
                        FieldClass::Hardlink,
                        u32::try_from(ordinal).unwrap_or(u32::MAX),
                    ));
                }
                previous_group = Some(claim.group);
                previous_path = Some(claim.path);
                previous_fingerprint = claim.fingerprint;
                group_size = 1;
            }
            Some(group) if group == claim.group => {
                if previous_path
                    .as_ref()
                    .is_some_and(|path| path >= &claim.path)
                    || previous_fingerprint != claim.fingerprint
                {
                    return Err(error(
                        ErrorKind::HardlinkMismatch,
                        FieldClass::Hardlink,
                        u32::try_from(ordinal).unwrap_or(u32::MAX),
                    ));
                }
                previous_path = Some(claim.path);
                group_size += 1;
            }
            Some(group) => {
                if group_size < 2 || claim.group.get() != group.get() + 1 {
                    return Err(error(
                        ErrorKind::HardlinkMismatch,
                        FieldClass::Hardlink,
                        u32::try_from(ordinal).unwrap_or(u32::MAX),
                    ));
                }
                previous_group = Some(claim.group);
                previous_path = Some(claim.path);
                previous_fingerprint = claim.fingerprint;
                group_size = 1;
            }
        }
    }
    if count > 0 && group_size < 2 {
        return Err(error(
            ErrorKind::HardlinkMismatch,
            FieldClass::Hardlink,
            u32::try_from(count).unwrap_or(u32::MAX),
        ));
    }
    source.ensure_exhausted()
}

fn validate_references(
    count: u64,
    claims: &mut dyn CanonicalSource,
    known: &mut dyn CanonicalSource,
) -> Result<(), Error> {
    if count > MAX_TINY_ENTRIES {
        return Err(error(
            ErrorKind::LimitExceeded,
            FieldClass::ObjectReference,
            u32::try_from(count).unwrap_or(u32::MAX),
        ));
    }
    let known_count = read_u64(known)?;
    if known_count > MAX_TINY_ENTRIES {
        return Err(error(
            ErrorKind::LimitExceeded,
            FieldClass::ObjectReference,
            u32::try_from(known_count).unwrap_or(u32::MAX),
        ));
    }
    let mut known_remaining = known_count;
    let mut previous_known = None;
    let mut known_ordinal = 0_u64;
    let read_next_known =
        |source: &mut dyn CanonicalSource, previous: &mut Option<ObjectId>, ordinal: u64| {
            let value = read_file_segments_reference(source)?;
            if previous.is_some_and(|prior| prior >= value) {
                return Err(error(
                    ErrorKind::NonCanonical,
                    FieldClass::ObjectReference,
                    u32::try_from(ordinal).unwrap_or(u32::MAX),
                ));
            }
            *previous = Some(value);
            Ok(value)
        };
    let mut current_known = if known_remaining > 0 {
        known_remaining -= 1;
        let value = read_next_known(known, &mut previous_known, known_ordinal)?;
        known_ordinal += 1;
        Some(value)
    } else {
        None
    };
    let mut previous_claim = None;
    for ordinal in 0..count {
        let claim = read_file_segments_reference(claims)?;
        if previous_claim.is_some_and(|previous| previous > claim) {
            return Err(error(
                ErrorKind::NonCanonical,
                FieldClass::ObjectReference,
                u32::try_from(ordinal).unwrap_or(u32::MAX),
            ));
        }
        previous_claim = Some(claim);
        while current_known.is_some_and(|known_value| known_value < claim) {
            current_known = if known_remaining > 0 {
                known_remaining -= 1;
                let value = read_next_known(known, &mut previous_known, known_ordinal)?;
                known_ordinal += 1;
                Some(value)
            } else {
                None
            };
        }
        if current_known != Some(claim) {
            return Err(error(
                ErrorKind::MissingReference,
                FieldClass::ObjectReference,
                u32::try_from(ordinal).unwrap_or(u32::MAX),
            ));
        }
    }
    while known_remaining > 0 {
        known_remaining -= 1;
        let _ = read_next_known(known, &mut previous_known, known_ordinal)?;
        known_ordinal += 1;
    }
    claims.ensure_exhausted()?;
    known.ensure_exhausted()
}

pub fn validate_tree_candidate(
    pending: PendingTree,
    sorted_hardlink_claim_source: &mut dyn CanonicalSource,
    sorted_reference_claim_source: &mut dyn CanonicalSource,
    sorted_known_file_segments_source: &mut dyn CanonicalSource,
) -> Result<ValidatedTree, Error> {
    validate_hardlinks(
        pending.summary.hardlink_claim_count,
        sorted_hardlink_claim_source,
    )?;
    validate_references(
        pending.summary.reference_claim_count,
        sorted_reference_claim_source,
        sorted_known_file_segments_source,
    )?;
    Ok(ValidatedTree {
        id: TreeManifestId::new(pending.digest),
        entry_count: pending.summary.entry_count,
        required_capabilities: pending.summary.capabilities,
    })
}
