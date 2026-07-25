use core::cmp::Ordering;

use crate::codec::{
    error_v3, read_record_header, write_record_header, RecordKind, BOUNDED_HEADER_BYTES,
    TLV_HEADER_BYTES,
};
use crate::{
    CanonicalSink, CanonicalSource, Digest32, DigestDomain, Error, ErrorKind, FieldClass,
    RawDigest, RootId, TypedDigest, ROOT_FORMAT_V3,
};

pub const MAX_V3_RECORD_BYTES: u32 = 262_144;
const MAX_PAGE_BYTES: u32 = 65_536;
const MAX_FILE_NODE_BYTES: u32 = 131_072;
const MAX_METADATA_BYTES: u32 = 65_536;
const MAX_OPERATION_BYTES: u32 = 4_096;
const MAX_MUTABLE_SMALL_BYTES: u32 = 512;
const MUTABLE_DOMAIN: &[u8; 16] = b"EOS-LS3-MUTABLE\0";

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum RecordKindV3 {
    Root = 0x10,
    Metadata = 0x13,
    TreePage = 0x20,
    FileNode = 0x21,
    SegmentPage = 0x22,
    Chunk = 0x23,
    AttributionRoot = 0x24,
    AttributionPage = 0x25,
    HardlinkGroup = 0x26,
    Head = 0x30,
    OperationState = 0x31,
    Locator = 0x32,
    SourceLease = 0x33,
}

impl RecordKindV3 {
    fn from_u8(value: u8) -> Result<Self, Error> {
        match value {
            0x10 => Ok(Self::Root),
            0x13 => Ok(Self::Metadata),
            0x20 => Ok(Self::TreePage),
            0x21 => Ok(Self::FileNode),
            0x22 => Ok(Self::SegmentPage),
            0x23 => Ok(Self::Chunk),
            0x24 => Ok(Self::AttributionRoot),
            0x25 => Ok(Self::AttributionPage),
            0x26 => Ok(Self::HardlinkGroup),
            0x30 => Ok(Self::Head),
            0x31 => Ok(Self::OperationState),
            0x32 => Ok(Self::Locator),
            0x33 => Ok(Self::SourceLease),
            _ => Err(error_v3(
                ErrorKind::WrongKind,
                FieldClass::Kind,
                u32::from(value),
            )),
        }
    }

    const fn is_mutable(self) -> bool {
        matches!(
            self,
            Self::Head | Self::OperationState | Self::Locator | Self::SourceLease
        )
    }

    const fn maximum_encoded_bytes(self) -> u32 {
        match self {
            Self::Root | Self::Head => 256,
            Self::Metadata | Self::TreePage | Self::SegmentPage | Self::AttributionPage => {
                MAX_PAGE_BYTES
            }
            Self::FileNode => MAX_FILE_NODE_BYTES,
            Self::Chunk => BOUNDED_HEADER_BYTES as u32 + 32_768,
            Self::OperationState => MAX_OPERATION_BYTES,
            Self::Locator | Self::SourceLease => MAX_MUTABLE_SMALL_BYTES,
            Self::AttributionRoot => 256,
            Self::HardlinkGroup => MAX_V3_RECORD_BYTES,
        }
    }

    fn record_kind(self) -> Result<RecordKind, Error> {
        RecordKind::from_u8(self as u8)
    }
}

macro_rules! digest_id {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
        #[repr(transparent)]
        pub struct $name(Digest32);

        impl $name {
            #[must_use]
            pub const fn new(digest: Digest32) -> Self {
                Self(digest)
            }

            #[must_use]
            pub const fn digest(self) -> Digest32 {
                self.0
            }
        }
    };
}

digest_id!(TreePageId);
digest_id!(FileNodeId);
digest_id!(SegmentPageId);
digest_id!(ChunkId);
digest_id!(AttributionRootId);
digest_id!(AttributionPageId);
digest_id!(HardlinkGroupIdV3);

macro_rules! nonzero_id {
    ($name:ident, $length:expr, $field:expr) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        #[repr(transparent)]
        pub struct $name([u8; $length]);

        impl $name {
            pub fn new(bytes: [u8; $length]) -> Result<Self, Error> {
                if bytes == [0; $length] {
                    return Err(error_v3(ErrorKind::InvalidIdentifier, $field, 0));
                }
                Ok(Self(bytes))
            }

            #[must_use]
            pub const fn as_bytes(&self) -> &[u8; $length] {
                &self.0
            }
        }
    };
}

nonzero_id!(ActorId, 32, FieldClass::Attribution);
nonzero_id!(PinId, 16, FieldClass::Identifier);
nonzero_id!(LeaseId, 16, FieldClass::Lease);

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BranchId(Vec<u8>);

impl BranchId {
    pub fn new(bytes: Vec<u8>) -> Result<Self, Error> {
        if bytes.is_empty()
            || bytes.len() > 128
            || bytes == b"."
            || bytes == b".."
            || (!bytes[0].is_ascii_lowercase() && !bytes[0].is_ascii_digit())
            || !bytes.iter().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(*byte, b'.' | b'_' | b'-')
            })
        {
            return Err(error_v3(
                ErrorKind::InvalidIdentifier,
                FieldClass::Branch,
                0,
            ));
        }
        Ok(Self(bytes))
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TlvV3 {
    tag: u8,
    value: Vec<u8>,
}

impl TlvV3 {
    #[must_use]
    pub const fn new(tag: u8, value: Vec<u8>) -> Self {
        Self { tag, value }
    }

    #[must_use]
    pub const fn tag(&self) -> u8 {
        self.tag
    }

    #[must_use]
    pub fn value(&self) -> &[u8] {
        &self.value
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum PayloadV3 {
    Chunk(Vec<u8>),
    Tlvs(Vec<TlvV3>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CanonicalRecordV3 {
    kind: RecordKindV3,
    payload: PayloadV3,
}

pub trait V3ReferenceLookup {
    fn contains(&mut self, kind: RecordKindV3, digest: Digest32) -> Result<bool, Error>;
}

impl CanonicalRecordV3 {
    pub fn chunk(payload: Vec<u8>) -> Result<Self, Error> {
        let record = Self {
            kind: RecordKindV3::Chunk,
            payload: PayloadV3::Chunk(payload),
        };
        validate_record(&record, None)?;
        Ok(record)
    }

    pub fn immutable(kind: RecordKindV3, fields: Vec<TlvV3>) -> Result<Self, Error> {
        if kind == RecordKindV3::Chunk || kind.is_mutable() {
            return Err(error_v3(
                ErrorKind::WrongKind,
                FieldClass::Kind,
                u32::from(kind as u8),
            ));
        }
        let record = Self {
            kind,
            payload: PayloadV3::Tlvs(fields),
        };
        validate_record(&record, None)?;
        Ok(record)
    }

    pub fn mutable(
        kind: RecordKindV3,
        mut fields: Vec<TlvV3>,
        digest: &mut dyn RawDigest,
    ) -> Result<Self, Error> {
        if !kind.is_mutable() || fields.iter().any(|field| field.tag == 255) {
            return Err(error_v3(
                ErrorKind::WrongKind,
                FieldClass::Kind,
                u32::from(kind as u8),
            ));
        }
        require_strict_tags(&fields)?;
        let preimage = mutable_checksum_preimage(kind, &fields)?;
        let checksum = digest.digest_bytes(&preimage)?;
        fields.push(TlvV3::new(255, checksum.into_bytes().to_vec()));
        let record = Self {
            kind,
            payload: PayloadV3::Tlvs(fields),
        };
        validate_record(&record, Some(digest))?;
        Ok(record)
    }

    #[must_use]
    pub const fn kind(&self) -> RecordKindV3 {
        self.kind
    }

    #[must_use]
    pub fn fields(&self) -> Option<&[TlvV3]> {
        match &self.payload {
            PayloadV3::Chunk(_) => None,
            PayloadV3::Tlvs(fields) => Some(fields),
        }
    }

    #[must_use]
    pub fn chunk_payload(&self) -> Option<&[u8]> {
        match &self.payload {
            PayloadV3::Chunk(bytes) => Some(bytes),
            PayloadV3::Tlvs(_) => None,
        }
    }

    fn payload_len(&self) -> Result<u32, Error> {
        let length = match &self.payload {
            PayloadV3::Chunk(bytes) => bytes.len(),
            PayloadV3::Tlvs(fields) => fields.iter().try_fold(0_usize, |total, field| {
                total
                    .checked_add(TLV_HEADER_BYTES as usize)
                    .and_then(|value| value.checked_add(field.value.len()))
                    .ok_or_else(|| error_v3(ErrorKind::ArithmeticOverflow, FieldClass::Length, 0))
            })?,
        };
        u32::try_from(length)
            .map_err(|_| error_v3(ErrorKind::LengthLimit, FieldClass::Length, u32::MAX))
    }
}

struct VecSink {
    bytes: Vec<u8>,
}

impl VecSink {
    const fn new() -> Self {
        Self { bytes: Vec::new() }
    }
}

impl CanonicalSink for VecSink {
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }
}

fn write_tlv_v3(field: &TlvV3, sink: &mut dyn CanonicalSink) -> Result<(), Error> {
    let length = u32::try_from(field.value.len()).map_err(|_| {
        error_v3(
            ErrorKind::LengthLimit,
            FieldClass::Length,
            u32::from(field.tag),
        )
    })?;
    sink.write_all(&[field.tag])?;
    sink.write_all(&length.to_be_bytes())?;
    sink.write_all(&field.value)
}

fn encode_payload(record: &CanonicalRecordV3, sink: &mut dyn CanonicalSink) -> Result<(), Error> {
    match &record.payload {
        PayloadV3::Chunk(bytes) => sink.write_all(bytes),
        PayloadV3::Tlvs(fields) => {
            for field in fields {
                write_tlv_v3(field, sink)?;
            }
            Ok(())
        }
    }
}

pub fn encode_v3_record(
    record: &CanonicalRecordV3,
    sink: &mut dyn CanonicalSink,
) -> Result<(), Error> {
    validate_record(record, None)?;
    write_record_header(
        sink,
        record.kind.record_kind()?,
        ROOT_FORMAT_V3,
        u64::from(record.payload_len()?),
    )?;
    encode_payload(record, sink)
}

pub fn decode_v3_record(
    source: &mut dyn CanonicalSource,
    digest: &mut dyn RawDigest,
) -> Result<CanonicalRecordV3, Error> {
    let header = read_record_header(source)?;
    if header.version != ROOT_FORMAT_V3 {
        return Err(error_v3(
            ErrorKind::UnsupportedVersion,
            FieldClass::Version,
            u32::from(header.version.get()),
        ));
    }
    let kind = RecordKindV3::from_u8(header.kind as u8)?;
    let total = header
        .payload_len
        .checked_add(BOUNDED_HEADER_BYTES)
        .ok_or_else(|| error_v3(ErrorKind::ArithmeticOverflow, FieldClass::Length, 0))?;
    if total > u64::from(kind.maximum_encoded_bytes()) {
        return Err(error_v3(
            if matches!(
                kind,
                RecordKindV3::TreePage | RecordKindV3::SegmentPage | RecordKindV3::AttributionPage
            ) {
                ErrorKind::PageLimit
            } else {
                ErrorKind::LengthLimit
            },
            FieldClass::Length,
            u32::try_from(total).unwrap_or(u32::MAX),
        ));
    }
    let allocation = usize::try_from(header.payload_len).map_err(|_| {
        error_v3(
            ErrorKind::LengthLimit,
            FieldClass::Length,
            u32::try_from(header.payload_len).unwrap_or(u32::MAX),
        )
    })?;
    let mut payload = vec![0_u8; allocation];
    source.read_exact(&mut payload)?;
    source.ensure_exhausted()?;

    let record = if kind == RecordKindV3::Chunk {
        SelfRecord::chunk_unchecked(payload)
    } else {
        SelfRecord::tlvs_unchecked(kind, parse_tlvs(&payload)?)
    };
    validate_record(&record, Some(digest))?;
    Ok(record)
}

pub fn decode_v3_record_as(
    source: &mut dyn CanonicalSource,
    expected: RecordKindV3,
    digest: &mut dyn RawDigest,
) -> Result<CanonicalRecordV3, Error> {
    let record = decode_v3_record(source, digest)?;
    if record.kind != expected {
        return Err(error_v3(
            ErrorKind::WrongKind,
            FieldClass::Kind,
            u32::from(record.kind as u8),
        ));
    }
    Ok(record)
}

fn digest32(bytes: &[u8], field: FieldClass, ordinal: u32) -> Result<Digest32, Error> {
    let value: [u8; 32] = bytes
        .try_into()
        .map_err(|_| error_v3(ErrorKind::CorruptRecord, field, ordinal))?;
    Ok(Digest32::new(value))
}

fn require_reference(
    lookup: &mut dyn V3ReferenceLookup,
    kind: RecordKindV3,
    bytes: &[u8],
    field: FieldClass,
    ordinal: u32,
) -> Result<(), Error> {
    let digest = digest32(bytes, field, ordinal)?;
    if lookup.contains(kind, digest)? {
        Ok(())
    } else {
        Err(error_v3(ErrorKind::DanglingEdge, field, ordinal))
    }
}

pub fn validate_v3_references(
    record: &CanonicalRecordV3,
    lookup: &mut dyn V3ReferenceLookup,
) -> Result<(), Error> {
    validate_record(record, None)?;
    let Some(fields) = record.fields() else {
        return Ok(());
    };
    match record.kind {
        RecordKindV3::Root => require_reference(
            lookup,
            RecordKindV3::FileNode,
            &fields[2].value,
            FieldClass::Tree,
            0,
        ),
        RecordKindV3::TreePage => {
            let page_kind = fields[0].value[0];
            let count = u16_value(&fields[2], FieldClass::Page)?;
            let mut cursor = Cursor::new(&fields[3].value);
            for ordinal in 0..count {
                let _ = cursor.length_prefixed_u16(255)?;
                let digest = cursor.take(32)?;
                require_reference(
                    lookup,
                    if page_kind == 1 {
                        RecordKindV3::FileNode
                    } else {
                        RecordKindV3::TreePage
                    },
                    digest,
                    FieldClass::Page,
                    u32::from(ordinal),
                )?;
            }
            cursor.finish(FieldClass::Page)
        }
        RecordKindV3::FileNode => {
            if let Some(value) = option_value(&fields[2], Some(32), FieldClass::Tree)? {
                require_reference(lookup, RecordKindV3::TreePage, value, FieldClass::Tree, 0)?;
            }
            if let Some(value) = option_value(&fields[4], Some(32), FieldClass::Segment)? {
                require_reference(
                    lookup,
                    RecordKindV3::SegmentPage,
                    value,
                    FieldClass::Segment,
                    0,
                )?;
            }
            if let Some(value) = option_value(&fields[8], Some(32), FieldClass::Hardlink)? {
                require_reference(
                    lookup,
                    RecordKindV3::HardlinkGroup,
                    value,
                    FieldClass::Hardlink,
                    0,
                )?;
            }
            Ok(())
        }
        RecordKindV3::SegmentPage => {
            let page_kind = fields[0].value[0];
            let count = u16_value(&fields[2], FieldClass::Segment)?;
            let mut cursor = Cursor::new(&fields[4].value);
            for ordinal in 0..count {
                if page_kind == 1 {
                    let descriptor_kind = cursor.u8()?;
                    let _ = cursor.u64()?;
                    let _ = cursor.u64()?;
                    if descriptor_kind == 1 {
                        require_reference(
                            lookup,
                            RecordKindV3::Chunk,
                            cursor.take(32)?,
                            FieldClass::Segment,
                            u32::from(ordinal),
                        )?;
                    }
                } else {
                    let _ = cursor.u64()?;
                    require_reference(
                        lookup,
                        RecordKindV3::SegmentPage,
                        cursor.take(32)?,
                        FieldClass::Segment,
                        u32::from(ordinal),
                    )?;
                }
            }
            cursor.finish(FieldClass::Segment)
        }
        RecordKindV3::AttributionRoot => {
            require_reference(
                lookup,
                RecordKindV3::Root,
                &fields[1].value,
                FieldClass::Digest,
                0,
            )?;
            require_reference(
                lookup,
                RecordKindV3::AttributionPage,
                &fields[2].value,
                FieldClass::Attribution,
                0,
            )
        }
        RecordKindV3::AttributionPage if fields[0].value[0] == 2 => {
            let count = u16_value(&fields[2], FieldClass::Attribution)?;
            let mut cursor = Cursor::new(&fields[3].value);
            for ordinal in 0..count {
                let _ = cursor.length_prefixed_u16(4096)?;
                let _ = cursor.take(1 + 8 + 8 + 32 + 16)?;
                require_reference(
                    lookup,
                    RecordKindV3::AttributionPage,
                    cursor.take(32)?,
                    FieldClass::Attribution,
                    u32::from(ordinal),
                )?;
            }
            cursor.finish(FieldClass::Attribution)
        }
        RecordKindV3::Head => {
            require_reference(
                lookup,
                RecordKindV3::Root,
                &fields[0].value,
                FieldClass::Digest,
                0,
            )?;
            require_reference(
                lookup,
                RecordKindV3::AttributionRoot,
                &fields[1].value,
                FieldClass::Attribution,
                0,
            )
        }
        RecordKindV3::OperationState => {
            for (index, kind, field) in [
                (4, RecordKindV3::Root, FieldClass::Digest),
                (5, RecordKindV3::AttributionRoot, FieldClass::Attribution),
                (8, RecordKindV3::Root, FieldClass::Digest),
                (9, RecordKindV3::AttributionRoot, FieldClass::Attribution),
            ] {
                if let Some(value) = option_value(&fields[index], Some(32), field)? {
                    require_reference(
                        lookup,
                        kind,
                        value,
                        field,
                        u32::try_from(index + 1).unwrap_or(u32::MAX),
                    )?;
                }
            }
            Ok(())
        }
        RecordKindV3::AttributionPage
        | RecordKindV3::Metadata
        | RecordKindV3::HardlinkGroup
        | RecordKindV3::Locator
        | RecordKindV3::SourceLease => Ok(()),
        RecordKindV3::Chunk => Ok(()),
    }
}

struct SelfRecord;

impl SelfRecord {
    fn chunk_unchecked(payload: Vec<u8>) -> CanonicalRecordV3 {
        CanonicalRecordV3 {
            kind: RecordKindV3::Chunk,
            payload: PayloadV3::Chunk(payload),
        }
    }

    fn tlvs_unchecked(kind: RecordKindV3, fields: Vec<TlvV3>) -> CanonicalRecordV3 {
        CanonicalRecordV3 {
            kind,
            payload: PayloadV3::Tlvs(fields),
        }
    }
}

fn parse_tlvs(payload: &[u8]) -> Result<Vec<TlvV3>, Error> {
    let mut position = 0_usize;
    let mut fields = Vec::new();
    let mut previous = None;
    while position < payload.len() {
        let header_end = position
            .checked_add(TLV_HEADER_BYTES as usize)
            .ok_or_else(|| error_v3(ErrorKind::ArithmeticOverflow, FieldClass::Length, 0))?;
        let Some(header) = payload.get(position..header_end) else {
            return Err(error_v3(ErrorKind::CorruptRecord, FieldClass::Record, 0));
        };
        let tag = header[0];
        if previous.is_some_and(|value| tag <= value) {
            return Err(error_v3(
                if previous == Some(tag) {
                    ErrorKind::DuplicateEntry
                } else {
                    ErrorKind::NonCanonicalOrder
                },
                FieldClass::Record,
                u32::from(tag),
            ));
        }
        let length = u32::from_be_bytes([header[1], header[2], header[3], header[4]]);
        let value_end = header_end
            .checked_add(length as usize)
            .ok_or_else(|| error_v3(ErrorKind::ArithmeticOverflow, FieldClass::Length, 0))?;
        let Some(value) = payload.get(header_end..value_end) else {
            return Err(error_v3(
                ErrorKind::CorruptRecord,
                FieldClass::Length,
                u32::from(tag),
            ));
        };
        fields.push(TlvV3::new(tag, value.to_vec()));
        previous = Some(tag);
        position = value_end;
    }
    Ok(fields)
}

pub fn v3_record_id(
    record: &CanonicalRecordV3,
    digest: &mut dyn TypedDigest,
) -> Result<Digest32, Error> {
    if record.kind.is_mutable() {
        return Err(error_v3(
            ErrorKind::WrongDomain,
            FieldClass::Digest,
            u32::from(record.kind as u8),
        ));
    }
    let payload_len = u64::from(record.payload_len()?);
    let mut invocations = 0_u8;
    let value = {
        let mut payload = |sink: &mut dyn CanonicalSink| {
            invocations = invocations
                .checked_add(1)
                .ok_or_else(|| error_v3(ErrorKind::ArithmeticOverflow, FieldClass::Digest, 0))?;
            encode_payload(record, sink)
        };
        digest.digest(
            DigestDomain::V3Record(record.kind as u8),
            ROOT_FORMAT_V3,
            payload_len,
            &mut payload,
        )?
    };
    if invocations != 1 {
        return Err(error_v3(
            ErrorKind::DigestFailure,
            FieldClass::Digest,
            u32::from(invocations),
        ));
    }
    Ok(value)
}

fn require_id_kind(
    record: &CanonicalRecordV3,
    expected: RecordKindV3,
    digest: &mut dyn TypedDigest,
) -> Result<Digest32, Error> {
    if record.kind != expected {
        return Err(error_v3(
            ErrorKind::WrongDomain,
            FieldClass::Digest,
            u32::from(record.kind as u8),
        ));
    }
    v3_record_id(record, digest)
}

pub fn root_id_v3(
    record: &CanonicalRecordV3,
    digest: &mut dyn TypedDigest,
) -> Result<RootId, Error> {
    require_id_kind(record, RecordKindV3::Root, digest).map(RootId::new)
}

macro_rules! typed_id_function {
    ($function:ident, $kind:ident, $id:ident) => {
        pub fn $function(
            record: &CanonicalRecordV3,
            digest: &mut dyn TypedDigest,
        ) -> Result<$id, Error> {
            require_id_kind(record, RecordKindV3::$kind, digest).map($id::new)
        }
    };
}

typed_id_function!(tree_page_id, TreePage, TreePageId);
typed_id_function!(file_node_id, FileNode, FileNodeId);
typed_id_function!(segment_page_id, SegmentPage, SegmentPageId);
typed_id_function!(chunk_id, Chunk, ChunkId);
typed_id_function!(attribution_root_id, AttributionRoot, AttributionRootId);
typed_id_function!(attribution_page_id, AttributionPage, AttributionPageId);
typed_id_function!(hardlink_group_id_v3, HardlinkGroup, HardlinkGroupIdV3);

fn require_strict_tags(fields: &[TlvV3]) -> Result<(), Error> {
    let mut previous = None;
    for field in fields {
        if previous.is_some_and(|value| field.tag <= value) {
            return Err(error_v3(
                if previous == Some(field.tag) {
                    ErrorKind::DuplicateEntry
                } else {
                    ErrorKind::NonCanonicalOrder
                },
                FieldClass::Record,
                u32::from(field.tag),
            ));
        }
        previous = Some(field.tag);
    }
    Ok(())
}

fn require_tags(fields: &[TlvV3], expected: &[u8]) -> Result<(), Error> {
    require_strict_tags(fields)?;
    if fields.len() != expected.len() {
        return Err(error_v3(
            ErrorKind::CorruptRecord,
            FieldClass::Record,
            u32::try_from(fields.len()).unwrap_or(u32::MAX),
        ));
    }
    for (field, tag) in fields.iter().zip(expected) {
        if field.tag != *tag {
            return Err(error_v3(
                ErrorKind::CorruptRecord,
                FieldClass::Record,
                u32::from(*tag),
            ));
        }
    }
    Ok(())
}

fn require_len(field: &TlvV3, length: usize, class: FieldClass) -> Result<(), Error> {
    if field.value.len() != length {
        return Err(error_v3(
            ErrorKind::LengthLimit,
            class,
            u32::from(field.tag),
        ));
    }
    Ok(())
}

fn option_value(
    field: &TlvV3,
    inner_len: Option<usize>,
    class: FieldClass,
) -> Result<Option<&[u8]>, Error> {
    let Some((discriminant, value)) = field.value.split_first() else {
        return Err(error_v3(
            ErrorKind::CorruptRecord,
            class,
            u32::from(field.tag),
        ));
    };
    match *discriminant {
        0 if value.is_empty() => Ok(None),
        1 if inner_len.is_none_or(|length| value.len() == length) => Ok(Some(value)),
        _ => Err(error_v3(
            ErrorKind::CorruptRecord,
            class,
            u32::from(field.tag),
        )),
    }
}

fn u16_value(field: &TlvV3, class: FieldClass) -> Result<u16, Error> {
    require_len(field, 2, class)?;
    Ok(u16::from_be_bytes([field.value[0], field.value[1]]))
}

fn u32_value(field: &TlvV3, class: FieldClass) -> Result<u32, Error> {
    require_len(field, 4, class)?;
    Ok(u32::from_be_bytes([
        field.value[0],
        field.value[1],
        field.value[2],
        field.value[3],
    ]))
}

fn u64_value(field: &TlvV3, class: FieldClass) -> Result<u64, Error> {
    require_len(field, 8, class)?;
    Ok(u64::from_be_bytes([
        field.value[0],
        field.value[1],
        field.value[2],
        field.value[3],
        field.value[4],
        field.value[5],
        field.value[6],
        field.value[7],
    ]))
}

fn validate_component(bytes: &[u8]) -> Result<(), Error> {
    if bytes.is_empty() || bytes.len() > 255 || bytes.iter().any(|byte| matches!(*byte, 0 | b'/')) {
        return Err(error_v3(
            ErrorKind::InvalidIdentifier,
            FieldClass::Path,
            u32::try_from(bytes.len()).unwrap_or(u32::MAX),
        ));
    }
    Ok(())
}

fn validate_path(bytes: &[u8], allow_empty: bool) -> Result<(), Error> {
    if bytes.len() > 4096 || (!allow_empty && bytes.is_empty()) || bytes.contains(&0) {
        return Err(error_v3(
            ErrorKind::InvalidIdentifier,
            FieldClass::Path,
            u32::try_from(bytes.len()).unwrap_or(u32::MAX),
        ));
    }
    if bytes.is_empty() {
        return Ok(());
    }
    let mut depth = 0_u8;
    for component in bytes.split(|byte| *byte == b'/') {
        validate_component(component)?;
        depth = depth
            .checked_add(1)
            .ok_or_else(|| error_v3(ErrorKind::DepthLimit, FieldClass::Path, 0))?;
        if depth > 64 {
            return Err(error_v3(
                ErrorKind::DepthLimit,
                FieldClass::Path,
                u32::from(depth),
            ));
        }
    }
    Ok(())
}

fn validate_metadata_bytes(bytes: &[u8]) -> Result<(), Error> {
    if bytes.len() > MAX_METADATA_BYTES as usize {
        return Err(error_v3(
            ErrorKind::LengthLimit,
            FieldClass::Metadata,
            u32::try_from(bytes.len()).unwrap_or(u32::MAX),
        ));
    }
    let mut source = ByteSource::new(bytes);
    let header = read_record_header(&mut source)?;
    if header.version != ROOT_FORMAT_V3 || header.kind as u8 != RecordKindV3::Metadata as u8 {
        return Err(error_v3(ErrorKind::WrongKind, FieldClass::Metadata, 0));
    }
    let payload_len = usize::try_from(header.payload_len)
        .map_err(|_| error_v3(ErrorKind::LengthLimit, FieldClass::Metadata, 0))?;
    let mut payload = vec![0_u8; payload_len];
    source.read_exact(&mut payload)?;
    source.ensure_exhausted()?;
    let fields = parse_tlvs(&payload)?;
    require_tags(&fields, &[1, 2, 3, 4, 5, 6])?;
    if u32_value(&fields[0], FieldClass::Mode)? & !0o7777 != 0 {
        return Err(error_v3(ErrorKind::InvalidValue, FieldClass::Mode, 1));
    }
    require_len(&fields[1], 4, FieldClass::Metadata)?;
    require_len(&fields[2], 4, FieldClass::Metadata)?;
    require_len(&fields[3], 8, FieldClass::Timestamp)?;
    if u32_value(&fields[4], FieldClass::Timestamp)? >= 1_000_000_000 {
        return Err(error_v3(ErrorKind::InvalidValue, FieldClass::Timestamp, 5));
    }
    validate_xattrs(&fields[5].value)
}

fn validate_xattrs(bytes: &[u8]) -> Result<(), Error> {
    let mut cursor = Cursor::new(bytes);
    let count = cursor.u32()?;
    let minimum = u64::from(count)
        .checked_mul(8)
        .and_then(|value| value.checked_add(4))
        .ok_or_else(|| error_v3(ErrorKind::ArithmeticOverflow, FieldClass::Xattr, 0))?;
    if minimum > bytes.len() as u64 {
        return Err(error_v3(ErrorKind::CountLimit, FieldClass::Xattr, count));
    }
    let mut previous: Option<Vec<u8>> = None;
    for ordinal in 0..count {
        let key = cursor.length_prefixed_u32(255)?;
        if key.is_empty() || key.contains(&0) {
            return Err(error_v3(
                ErrorKind::InvalidIdentifier,
                FieldClass::Xattr,
                ordinal,
            ));
        }
        if let Some(value) = &previous {
            match key.cmp(value.as_slice()) {
                Ordering::Less => {
                    return Err(error_v3(
                        ErrorKind::NonCanonicalOrder,
                        FieldClass::Xattr,
                        ordinal,
                    ));
                }
                Ordering::Equal => {
                    return Err(error_v3(
                        ErrorKind::DuplicateEntry,
                        FieldClass::Xattr,
                        ordinal,
                    ));
                }
                Ordering::Greater => {}
            }
        }
        previous = Some(key.to_vec());
        let _ = cursor.length_prefixed_u32(MAX_METADATA_BYTES as usize)?;
    }
    cursor.finish(FieldClass::Xattr)
}

fn validate_record(
    record: &CanonicalRecordV3,
    digest: Option<&mut dyn RawDigest>,
) -> Result<(), Error> {
    let total = record
        .payload_len()?
        .checked_add(BOUNDED_HEADER_BYTES as u32)
        .ok_or_else(|| error_v3(ErrorKind::ArithmeticOverflow, FieldClass::Length, 0))?;
    if total > record.kind.maximum_encoded_bytes() {
        return Err(error_v3(
            if matches!(
                record.kind,
                RecordKindV3::TreePage | RecordKindV3::SegmentPage | RecordKindV3::AttributionPage
            ) {
                ErrorKind::PageLimit
            } else {
                ErrorKind::LengthLimit
            },
            FieldClass::Length,
            total,
        ));
    }
    if record.kind == RecordKindV3::Chunk {
        let PayloadV3::Chunk(bytes) = &record.payload else {
            return Err(error_v3(ErrorKind::WrongKind, FieldClass::Kind, 0));
        };
        if bytes.is_empty() || bytes.len() > 32_768 {
            return Err(error_v3(
                ErrorKind::LengthLimit,
                FieldClass::Length,
                u32::try_from(bytes.len()).unwrap_or(u32::MAX),
            ));
        }
        return Ok(());
    }
    let PayloadV3::Tlvs(fields) = &record.payload else {
        return Err(error_v3(ErrorKind::WrongKind, FieldClass::Kind, 0));
    };
    match record.kind {
        RecordKindV3::Root => validate_root(fields),
        RecordKindV3::Metadata => validate_metadata(fields),
        RecordKindV3::TreePage => validate_tree_page(fields),
        RecordKindV3::FileNode => validate_file_node(fields),
        RecordKindV3::SegmentPage => validate_segment_page(fields),
        RecordKindV3::AttributionRoot => validate_attribution_root(fields),
        RecordKindV3::AttributionPage => validate_attribution_page(fields),
        RecordKindV3::HardlinkGroup => validate_hardlink_group(fields),
        RecordKindV3::Head
        | RecordKindV3::OperationState
        | RecordKindV3::Locator
        | RecordKindV3::SourceLease => validate_mutable(record.kind, fields, digest),
        RecordKindV3::Chunk => unreachable!("chunk returned above"),
    }
}

fn validate_root(fields: &[TlvV3]) -> Result<(), Error> {
    require_tags(fields, &[1, 2, 3])?;
    let capabilities = u64_value(&fields[0], FieldClass::Capability)?;
    if capabilities & !0x3f != 0 {
        return Err(error_v3(
            ErrorKind::UnknownRequiredCapability,
            FieldClass::Capability,
            capabilities.trailing_zeros(),
        ));
    }
    if u16_value(&fields[1], FieldClass::Profile)? != 1 {
        return Err(error_v3(ErrorKind::InvalidValue, FieldClass::Profile, 2));
    }
    require_len(&fields[2], 32, FieldClass::Digest)
}

fn validate_metadata(fields: &[TlvV3]) -> Result<(), Error> {
    require_tags(fields, &[1, 2, 3, 4, 5, 6])?;
    if u32_value(&fields[0], FieldClass::Mode)? & !0o7777 != 0 {
        return Err(error_v3(ErrorKind::InvalidValue, FieldClass::Mode, 1));
    }
    require_len(&fields[1], 4, FieldClass::Metadata)?;
    require_len(&fields[2], 4, FieldClass::Metadata)?;
    require_len(&fields[3], 8, FieldClass::Timestamp)?;
    if u32_value(&fields[4], FieldClass::Timestamp)? >= 1_000_000_000 {
        return Err(error_v3(ErrorKind::InvalidValue, FieldClass::Timestamp, 5));
    }
    validate_xattrs(&fields[5].value)
}

fn validate_tree_page(fields: &[TlvV3]) -> Result<(), Error> {
    require_tags(fields, &[1, 2, 3, 4])?;
    require_len(&fields[0], 1, FieldClass::Page)?;
    require_len(&fields[1], 1, FieldClass::Page)?;
    let page_kind = fields[0].value[0];
    let depth = fields[1].value[0];
    let count = u16_value(&fields[2], FieldClass::Page)?;
    if !matches!(page_kind, 1 | 2) || depth > 16 || (page_kind == 1) != (depth == 0) {
        return Err(error_v3(
            ErrorKind::DepthLimit,
            FieldClass::Page,
            u32::from(depth),
        ));
    }
    if count > 192 {
        return Err(error_v3(
            ErrorKind::CountLimit,
            FieldClass::Page,
            u32::from(count),
        ));
    }
    if page_kind == 2 && count < 2 {
        return Err(error_v3(
            ErrorKind::CountLimit,
            FieldClass::Page,
            u32::from(count),
        ));
    }
    let mut cursor = Cursor::new(&fields[3].value);
    let mut previous: Option<Vec<u8>> = None;
    for ordinal in 0..count {
        let name = cursor.length_prefixed_u16(255)?;
        validate_component(name)?;
        require_ascending(&mut previous, name, FieldClass::Page, u32::from(ordinal))?;
        cursor.take(32)?;
    }
    cursor.finish(FieldClass::Page)
}

fn validate_file_node(fields: &[TlvV3]) -> Result<(), Error> {
    require_tags(fields, &[1, 2, 3, 4, 5, 6, 7, 8, 9])?;
    require_len(&fields[0], 1, FieldClass::Entry)?;
    let kind = fields[0].value[0];
    validate_metadata_bytes(&fields[1].value)?;
    let directory = option_value(&fields[2], Some(32), FieldClass::Tree)?;
    let logical_length = option_value(&fields[3], Some(8), FieldClass::LogicalLength)?;
    let segments = option_value(&fields[4], Some(32), FieldClass::Segment)?;
    let symlink = option_value(&fields[5], None, FieldClass::SymlinkTarget)?;
    let major = option_value(&fields[6], Some(4), FieldClass::Device)?;
    let minor = option_value(&fields[7], Some(4), FieldClass::Device)?;
    let hardlink = option_value(&fields[8], Some(32), FieldClass::Hardlink)?;
    let valid = match kind {
        1 => {
            directory.is_some()
                && logical_length.is_none()
                && segments.is_none()
                && symlink.is_none()
                && major.is_none()
                && minor.is_none()
                && hardlink.is_none()
        }
        2 => {
            directory.is_none()
                && logical_length.is_some()
                && segments.is_some()
                && symlink.is_none()
                && major.is_none()
                && minor.is_none()
        }
        3 => {
            directory.is_none()
                && logical_length.is_none()
                && segments.is_none()
                && symlink.is_some()
                && major.is_none()
                && minor.is_none()
                && hardlink.is_none()
        }
        4 => {
            directory.is_none()
                && logical_length.is_none()
                && segments.is_none()
                && symlink.is_none()
                && major.is_some()
                && minor.is_some()
                && hardlink.is_none()
        }
        5 => {
            directory.is_none()
                && logical_length.is_none()
                && segments.is_none()
                && symlink.is_none()
                && major.is_none()
                && minor.is_none()
                && hardlink.is_none()
        }
        _ => false,
    };
    if !valid {
        return Err(error_v3(
            ErrorKind::CorruptRecord,
            FieldClass::Entry,
            u32::from(kind),
        ));
    }
    if let Some(target) = symlink {
        if target.len() > 4096 || target.contains(&0) {
            return Err(error_v3(
                ErrorKind::LengthLimit,
                FieldClass::SymlinkTarget,
                6,
            ));
        }
    }
    if kind == 4 && major == Some(&[0, 0, 0, 0][..]) && minor == Some(&[0, 0, 0, 0][..]) {
        return Err(error_v3(ErrorKind::InvalidValue, FieldClass::Device, 7));
    }
    Ok(())
}

fn validate_segment_page(fields: &[TlvV3]) -> Result<(), Error> {
    require_tags(fields, &[1, 2, 3, 4, 5])?;
    require_len(&fields[0], 1, FieldClass::Segment)?;
    require_len(&fields[1], 1, FieldClass::Segment)?;
    let page_kind = fields[0].value[0];
    let depth = fields[1].value[0];
    let count = u16_value(&fields[2], FieldClass::Segment)?;
    let covered = u64_value(&fields[3], FieldClass::LogicalLength)?;
    if !matches!(page_kind, 1 | 2) || depth > 16 || (page_kind == 1) != (depth == 0) {
        return Err(error_v3(
            ErrorKind::DepthLimit,
            FieldClass::Segment,
            u32::from(depth),
        ));
    }
    if count > 1024 {
        return Err(error_v3(
            ErrorKind::CountLimit,
            FieldClass::Segment,
            u32::from(count),
        ));
    }
    let mut cursor = Cursor::new(&fields[4].value);
    if page_kind == 1 {
        let mut expected_offset = 0_u64;
        let mut previous_kind = 0_u8;
        for ordinal in 0..count {
            let kind = cursor.u8()?;
            let offset = cursor.u64()?;
            let length = cursor.u64()?;
            if !matches!(kind, 1..=3)
                || length == 0
                || offset != expected_offset
                || (kind == 1 && length > 32_768)
                || (kind != 1 && kind == previous_kind)
            {
                return Err(error_v3(
                    ErrorKind::SparseInvalid,
                    FieldClass::SparseExtent,
                    u32::from(ordinal),
                ));
            }
            if kind == 1 {
                cursor.take(32)?;
            }
            expected_offset = offset.checked_add(length).ok_or_else(|| {
                error_v3(ErrorKind::ArithmeticOverflow, FieldClass::SparseExtent, 0)
            })?;
            previous_kind = kind;
        }
        if expected_offset != covered {
            return Err(error_v3(
                ErrorKind::SparseInvalid,
                FieldClass::SparseExtent,
                0,
            ));
        }
    } else {
        let mut previous_end = 0_u64;
        for ordinal in 0..count {
            let end = cursor.u64()?;
            if end <= previous_end {
                return Err(error_v3(
                    ErrorKind::NonCanonicalOrder,
                    FieldClass::Segment,
                    u32::from(ordinal),
                ));
            }
            cursor.take(32)?;
            previous_end = end;
        }
        if previous_end != covered {
            return Err(error_v3(ErrorKind::SparseInvalid, FieldClass::Segment, 4));
        }
    }
    cursor.finish(FieldClass::Segment)
}

fn validate_attribution_root(fields: &[TlvV3]) -> Result<(), Error> {
    require_tags(fields, &[1, 2, 3])?;
    if u64_value(&fields[0], FieldClass::Capability)? != 7 {
        return Err(error_v3(
            ErrorKind::UnknownRequiredCapability,
            FieldClass::Capability,
            1,
        ));
    }
    require_len(&fields[1], 32, FieldClass::Digest)?;
    require_len(&fields[2], 32, FieldClass::Attribution)
}

fn validate_attribution_page(fields: &[TlvV3]) -> Result<(), Error> {
    require_tags(fields, &[1, 2, 3, 4])?;
    require_len(&fields[0], 1, FieldClass::Attribution)?;
    require_len(&fields[1], 1, FieldClass::Attribution)?;
    let page_kind = fields[0].value[0];
    let depth = fields[1].value[0];
    let count = u16_value(&fields[2], FieldClass::Attribution)?;
    if !matches!(page_kind, 1 | 2) || depth > 16 || (page_kind == 1) != (depth == 0) {
        return Err(error_v3(
            ErrorKind::DepthLimit,
            FieldClass::Attribution,
            u32::from(depth),
        ));
    }
    if count > 128 {
        return Err(error_v3(
            ErrorKind::CountLimit,
            FieldClass::Attribution,
            u32::from(count),
        ));
    }
    let mut cursor = Cursor::new(&fields[3].value);
    let mut previous: Option<Vec<u8>> = None;
    for ordinal in 0..count {
        let key_start = cursor.position();
        let path = cursor.length_prefixed_u16(4096)?;
        if page_kind == 1 {
            let scope = cursor.u8()?;
            let offset = cursor.u64()?;
            let length = cursor.u64()?;
            validate_path(path, scope == 0)?;
            if !matches!(scope, 0 | 1)
                || (scope == 0 && (offset != 0 || length != 0))
                || (scope == 1 && length == 0)
                || offset.checked_add(length).is_none()
            {
                return Err(error_v3(
                    ErrorKind::CorruptRecord,
                    FieldClass::Attribution,
                    u32::from(ordinal),
                ));
            }
            let actor = cursor.take(32)?;
            if actor.iter().all(|byte| *byte == 0) {
                return Err(error_v3(
                    ErrorKind::InvalidIdentifier,
                    FieldClass::Attribution,
                    u32::from(ordinal),
                ));
            }
            let publication = cursor.take(16)?;
            if publication.iter().all(|byte| *byte == 0) {
                return Err(error_v3(
                    ErrorKind::InvalidIdentifier,
                    FieldClass::Publication,
                    u32::from(ordinal),
                ));
            }
        } else {
            validate_path(path, true)?;
            let scope = cursor.u8()?;
            if !matches!(scope, 0 | 1) {
                return Err(error_v3(
                    ErrorKind::CorruptRecord,
                    FieldClass::Attribution,
                    u32::from(ordinal),
                ));
            }
            cursor.take(8 + 8 + 32 + 16)?;
        }
        let key = cursor.slice_from(key_start);
        require_ascending(
            &mut previous,
            key,
            FieldClass::Attribution,
            u32::from(ordinal),
        )?;
        if page_kind == 2 {
            cursor.take(32)?;
        }
    }
    cursor.finish(FieldClass::Attribution)
}

fn validate_hardlink_group(fields: &[TlvV3]) -> Result<(), Error> {
    require_tags(fields, &[1, 2])?;
    let count = u16_value(&fields[0], FieldClass::Hardlink)?;
    if !(2..=1024).contains(&count) {
        return Err(error_v3(
            ErrorKind::HardlinkGroupLimit,
            FieldClass::Hardlink,
            u32::from(count),
        ));
    }
    let mut cursor = Cursor::new(&fields[1].value);
    let mut previous = None;
    for ordinal in 0..count {
        let path = cursor.length_prefixed_u16(4096)?;
        validate_path(path, false)?;
        require_ascending(
            &mut previous,
            path,
            FieldClass::Hardlink,
            u32::from(ordinal),
        )?;
    }
    cursor.finish(FieldClass::Hardlink)
}

fn validate_mutable(
    kind: RecordKindV3,
    fields: &[TlvV3],
    digest: Option<&mut dyn RawDigest>,
) -> Result<(), Error> {
    let expected: &[u8] = match kind {
        RecordKindV3::Head => &[1, 2, 3, 4, 255],
        RecordKindV3::OperationState => &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 255],
        RecordKindV3::Locator => &[1, 2, 3, 4, 5, 6, 7, 8, 255],
        RecordKindV3::SourceLease => &[1, 2, 3, 4, 5, 6, 255],
        _ => {
            return Err(error_v3(
                ErrorKind::WrongKind,
                FieldClass::Kind,
                u32::from(kind as u8),
            ));
        }
    };
    require_tags(fields, expected)?;
    let checksum = fields
        .last()
        .ok_or_else(|| error_v3(ErrorKind::ChecksumMismatch, FieldClass::Checksum, 255))?;
    require_len(checksum, 32, FieldClass::Checksum)?;
    validate_mutable_fields(kind, fields)?;
    if let Some(digest) = digest {
        let preimage = mutable_checksum_preimage(kind, &fields[..fields.len() - 1])?;
        let actual = digest.digest_bytes(&preimage)?;
        if actual.as_bytes() != checksum.value.as_slice() {
            return Err(error_v3(
                ErrorKind::ChecksumMismatch,
                FieldClass::Checksum,
                255,
            ));
        }
    }
    Ok(())
}

fn validate_mutable_fields(kind: RecordKindV3, fields: &[TlvV3]) -> Result<(), Error> {
    match kind {
        RecordKindV3::Head => {
            require_len(&fields[0], 32, FieldClass::Digest)?;
            require_len(&fields[1], 32, FieldClass::Attribution)?;
            let _ = u64_value(&fields[2], FieldClass::Publication)?;
            validate_publication(&fields[3].value)
        }
        RecordKindV3::OperationState => {
            require_len(&fields[0], 1, FieldClass::Operation)?;
            if !matches!(fields[0].value[0], 1..=5) {
                return Err(error_v3(ErrorKind::InvalidValue, FieldClass::Operation, 1));
            }
            let _ = BranchId::new(fields[1].value.clone())?;
            validate_publication(&fields[2].value)?;
            require_len(&fields[3], 32, FieldClass::Digest)?;
            let _ = option_value(&fields[4], Some(32), FieldClass::Digest)?;
            let _ = option_value(&fields[5], Some(32), FieldClass::Attribution)?;
            let _ = u64_value(&fields[6], FieldClass::Publication)?;
            require_len(&fields[7], 1, FieldClass::Operation)?;
            if !matches!(fields[7].value[0], 1..=7) {
                return Err(error_v3(ErrorKind::InvalidValue, FieldClass::Operation, 8));
            }
            let _ = option_value(&fields[8], Some(32), FieldClass::Digest)?;
            let _ = option_value(&fields[9], Some(32), FieldClass::Attribution)?;
            let _ = option_value(&fields[10], Some(32), FieldClass::Digest)?;
            require_len(&fields[11], 1, FieldClass::Operation)?;
            if fields[11].value[0] > 8 {
                return Err(error_v3(
                    ErrorKind::ContentionLimit,
                    FieldClass::Operation,
                    12,
                ));
            }
            if fields[12].value.is_empty() {
                return Err(error_v3(
                    ErrorKind::CorruptRecord,
                    FieldClass::Operation,
                    13,
                ));
            }
            let _ = u64_value(&fields[13], FieldClass::Operation)?;
            require_len(&fields[14], 1, FieldClass::Operation)?;
            if fields[14].value[0] > 1 {
                return Err(error_v3(ErrorKind::InvalidValue, FieldClass::Operation, 15));
            }
            Ok(())
        }
        RecordKindV3::Locator => {
            require_len(&fields[0], 1, FieldClass::Locator)?;
            RecordKindV3::from_u8(fields[0].value[0])?;
            require_len(&fields[1], 32, FieldClass::Digest)?;
            if u64_value(&fields[2], FieldClass::Locator)? == 0
                || fields[3].value.as_slice() != [1_u8]
                || u64_value(&fields[6], FieldClass::Length)? == 0
            {
                return Err(error_v3(ErrorKind::InvalidValue, FieldClass::Locator, 0));
            }
            require_len(&fields[4], 32, FieldClass::Locator)?;
            let offset = u64_value(&fields[5], FieldClass::Locator)?;
            let length = u64_value(&fields[6], FieldClass::Length)?;
            let _ = offset
                .checked_add(length)
                .ok_or_else(|| error_v3(ErrorKind::ArithmeticOverflow, FieldClass::Locator, 0))?;
            require_len(&fields[7], 32, FieldClass::Digest)
        }
        RecordKindV3::SourceLease => {
            if fields[0].value.len() != 16
                || fields[0].value.iter().all(|byte| *byte == 0)
                || u64_value(&fields[3], FieldClass::Lease)? == 0
                || u64_value(&fields[4], FieldClass::Lease)? == 0
            {
                return Err(error_v3(ErrorKind::InvalidIdentifier, FieldClass::Lease, 0));
            }
            require_len(&fields[1], 32, FieldClass::Digest)?;
            require_len(&fields[2], 32, FieldClass::Locator)?;
            let _ = u64_value(&fields[5], FieldClass::Length)?;
            Ok(())
        }
        _ => Err(error_v3(
            ErrorKind::WrongKind,
            FieldClass::Kind,
            u32::from(kind as u8),
        )),
    }
}

fn validate_publication(bytes: &[u8]) -> Result<(), Error> {
    if bytes.len() != 16 || bytes[..8].iter().all(|byte| *byte == 0) {
        return Err(error_v3(
            ErrorKind::InvalidIdentifier,
            FieldClass::Publication,
            0,
        ));
    }
    Ok(())
}

fn mutable_checksum_preimage(kind: RecordKindV3, fields: &[TlvV3]) -> Result<Vec<u8>, Error> {
    let mut sink = VecSink::new();
    sink.write_all(MUTABLE_DOMAIN)?;
    sink.write_all(&[kind as u8])?;
    sink.write_all(&ROOT_FORMAT_V3.get().to_be_bytes())?;
    for field in fields {
        write_tlv_v3(field, &mut sink)?;
    }
    Ok(sink.bytes)
}

fn require_ascending(
    previous: &mut Option<Vec<u8>>,
    value: &[u8],
    field: FieldClass,
    ordinal: u32,
) -> Result<(), Error> {
    if let Some(previous) = previous {
        match value.cmp(previous.as_slice()) {
            Ordering::Less => {
                return Err(error_v3(ErrorKind::NonCanonicalOrder, field, ordinal));
            }
            Ordering::Equal => {
                return Err(error_v3(ErrorKind::DuplicateEntry, field, ordinal));
            }
            Ordering::Greater => {}
        }
    }
    *previous = Some(value.to_vec());
    Ok(())
}

struct Cursor<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Cursor<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    const fn position(&self) -> usize {
        self.position
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], Error> {
        let end = self
            .position
            .checked_add(length)
            .ok_or_else(|| error_v3(ErrorKind::ArithmeticOverflow, FieldClass::Length, 0))?;
        let value = self
            .bytes
            .get(self.position..end)
            .ok_or_else(|| error_v3(ErrorKind::CorruptRecord, FieldClass::Length, 0))?;
        self.position = end;
        Ok(value)
    }

    fn slice_from(&self, start: usize) -> &'a [u8] {
        &self.bytes[start..self.position]
    }

    fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, Error> {
        let bytes = self.take(2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn u32(&mut self) -> Result<u32, Error> {
        let bytes = self.take(4)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn u64(&mut self) -> Result<u64, Error> {
        let bytes = self.take(8)?;
        Ok(u64::from_be_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    fn length_prefixed_u16(&mut self, maximum: usize) -> Result<&'a [u8], Error> {
        let length = usize::from(self.u16()?);
        if length > maximum {
            return Err(error_v3(
                ErrorKind::LengthLimit,
                FieldClass::Length,
                u32::try_from(length).unwrap_or(u32::MAX),
            ));
        }
        self.take(length)
    }

    fn length_prefixed_u32(&mut self, maximum: usize) -> Result<&'a [u8], Error> {
        let length = usize::try_from(self.u32()?)
            .map_err(|_| error_v3(ErrorKind::LengthLimit, FieldClass::Length, u32::MAX))?;
        if length > maximum {
            return Err(error_v3(
                ErrorKind::LengthLimit,
                FieldClass::Length,
                u32::try_from(length).unwrap_or(u32::MAX),
            ));
        }
        self.take(length)
    }

    fn finish(self, field: FieldClass) -> Result<(), Error> {
        if self.position == self.bytes.len() {
            Ok(())
        } else {
            Err(error_v3(
                ErrorKind::TrailingBytes,
                field,
                u32::try_from(self.position).unwrap_or(u32::MAX),
            ))
        }
    }
}

struct ByteSource<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> ByteSource<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }
}

impl CanonicalSource for ByteSource<'_> {
    fn read_exact(&mut self, output: &mut [u8]) -> Result<(), Error> {
        let end = self
            .position
            .checked_add(output.len())
            .ok_or_else(|| error_v3(ErrorKind::ArithmeticOverflow, FieldClass::Source, 0))?;
        let source = self
            .bytes
            .get(self.position..end)
            .ok_or_else(|| error_v3(ErrorKind::UnexpectedEnd, FieldClass::Source, 0))?;
        output.copy_from_slice(source);
        self.position = end;
        Ok(())
    }

    fn ensure_exhausted(&mut self) -> Result<(), Error> {
        if self.position == self.bytes.len() {
            Ok(())
        } else {
            Err(error_v3(
                ErrorKind::TrailingBytes,
                FieldClass::Source,
                u32::try_from(self.position).unwrap_or(u32::MAX),
            ))
        }
    }
}
