use crate::{
    CanonicalSink, CanonicalSource, DigestDomain, Error, ErrorKind, FieldClass, FormatVersion,
    ObjectId, ObjectKind, TypedDigest, ROOT_FORMAT_V2,
};

pub const MAX_RECORD_BYTES: u32 = 262_144;

pub(crate) const MAGIC: &[u8; 8] = b"EOS-LS2\0";
pub(crate) const BOUNDED_HEADER_BYTES: u64 = 15;
pub(crate) const TREE_HEADER_BYTES: u64 = 19;
pub(crate) const TLV_HEADER_BYTES: u64 = 5;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum RecordKind {
    FileSegments = 2,
    ChunkPayload = 3,
    Transition = 4,
    Root = 0x10,
    Tree = 0x11,
    Entry = 0x12,
    Metadata = 0x13,
    ObjectReference = 0x14,
}

impl RecordKind {
    pub(crate) fn from_u8(value: u8) -> Result<Self, Error> {
        match value {
            2 => Ok(Self::FileSegments),
            3 => Ok(Self::ChunkPayload),
            4 => Ok(Self::Transition),
            0x10 => Ok(Self::Root),
            0x11 => Ok(Self::Tree),
            0x12 => Ok(Self::Entry),
            0x13 => Ok(Self::Metadata),
            0x14 => Ok(Self::ObjectReference),
            _ => Err(error(
                ErrorKind::UnknownKind,
                FieldClass::Kind,
                u32::from(value),
            )),
        }
    }

    pub(crate) const fn from_object(kind: ObjectKind) -> Self {
        match kind {
            ObjectKind::FileSegments => Self::FileSegments,
            ObjectKind::ChunkPayload => Self::ChunkPayload,
            ObjectKind::Transition => Self::Transition,
        }
    }

    pub(crate) fn object_kind(self) -> Result<ObjectKind, Error> {
        match self {
            Self::FileSegments => Ok(ObjectKind::FileSegments),
            Self::ChunkPayload => Ok(ObjectKind::ChunkPayload),
            Self::Transition => Ok(ObjectKind::Transition),
            _ => Err(error(
                ErrorKind::UnknownKind,
                FieldClass::Kind,
                u32::from(self as u8),
            )),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectRecord {
    kind: ObjectKind,
    payload: Vec<u8>,
}

impl ObjectRecord {
    pub fn new(kind: ObjectKind, payload: Vec<u8>) -> Result<Self, Error> {
        let payload_len = bounded_u32(payload.len(), FieldClass::Length, 0)?;
        ensure_record_bound(RecordKind::from_object(kind), u64::from(payload_len))?;
        Ok(Self { kind, payload })
    }

    #[must_use]
    pub const fn kind(&self) -> ObjectKind {
        self.kind
    }

    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct RecordHeader {
    pub(crate) kind: RecordKind,
    pub(crate) version: FormatVersion,
    pub(crate) payload_len: u64,
}

pub(crate) const fn error(kind: ErrorKind, field: FieldClass, ordinal: u32) -> Error {
    Error::new(kind, ROOT_FORMAT_V2, field, ordinal)
}

pub(crate) fn bounded_u32(length: usize, field: FieldClass, ordinal: u32) -> Result<u32, Error> {
    let length =
        u32::try_from(length).map_err(|_| error(ErrorKind::LimitExceeded, field, ordinal))?;
    if length > MAX_RECORD_BYTES {
        return Err(error(ErrorKind::LimitExceeded, field, ordinal));
    }
    Ok(length)
}

pub(crate) fn checked_add(left: u64, right: u64, field: FieldClass) -> Result<u64, Error> {
    left.checked_add(right)
        .ok_or_else(|| error(ErrorKind::Overflow, field, 0))
}

pub(crate) fn checked_mul(left: u64, right: u64, field: FieldClass) -> Result<u64, Error> {
    left.checked_mul(right)
        .ok_or_else(|| error(ErrorKind::Overflow, field, 0))
}

pub(crate) fn write_u8(sink: &mut dyn CanonicalSink, value: u8) -> Result<(), Error> {
    sink.write_all(&[value])
}

pub(crate) fn write_u16(sink: &mut dyn CanonicalSink, value: u16) -> Result<(), Error> {
    sink.write_all(&value.to_be_bytes())
}

pub(crate) fn write_u32(sink: &mut dyn CanonicalSink, value: u32) -> Result<(), Error> {
    sink.write_all(&value.to_be_bytes())
}

pub(crate) fn write_u64(sink: &mut dyn CanonicalSink, value: u64) -> Result<(), Error> {
    sink.write_all(&value.to_be_bytes())
}

pub(crate) fn write_u128(sink: &mut dyn CanonicalSink, value: u128) -> Result<(), Error> {
    sink.write_all(&value.to_be_bytes())
}

pub(crate) fn read_u8(source: &mut dyn CanonicalSource) -> Result<u8, Error> {
    let mut bytes = [0_u8; 1];
    source.read_exact(&mut bytes)?;
    Ok(bytes[0])
}

pub(crate) fn read_u16(source: &mut dyn CanonicalSource) -> Result<u16, Error> {
    let mut bytes = [0_u8; 2];
    source.read_exact(&mut bytes)?;
    Ok(u16::from_be_bytes(bytes))
}

pub(crate) fn read_u32(source: &mut dyn CanonicalSource) -> Result<u32, Error> {
    let mut bytes = [0_u8; 4];
    source.read_exact(&mut bytes)?;
    Ok(u32::from_be_bytes(bytes))
}

pub(crate) fn read_u64(source: &mut dyn CanonicalSource) -> Result<u64, Error> {
    let mut bytes = [0_u8; 8];
    source.read_exact(&mut bytes)?;
    Ok(u64::from_be_bytes(bytes))
}

pub(crate) fn read_u128(source: &mut dyn CanonicalSource) -> Result<u128, Error> {
    let mut bytes = [0_u8; 16];
    source.read_exact(&mut bytes)?;
    Ok(u128::from_be_bytes(bytes))
}

pub(crate) fn write_record_header(
    sink: &mut dyn CanonicalSink,
    kind: RecordKind,
    version: FormatVersion,
    payload_len: u64,
) -> Result<(), Error> {
    if version != ROOT_FORMAT_V2 {
        return Err(error(
            ErrorKind::UnsupportedVersion,
            FieldClass::Version,
            u32::from(version.get()),
        ));
    }
    ensure_record_bound(kind, payload_len)?;
    sink.write_all(MAGIC)?;
    write_u8(sink, kind as u8)?;
    write_u16(sink, version.get())?;
    if kind == RecordKind::Tree {
        write_u64(sink, payload_len)
    } else {
        let length = u32::try_from(payload_len)
            .map_err(|_| error(ErrorKind::LimitExceeded, FieldClass::Length, 0))?;
        write_u32(sink, length)
    }
}

const fn record_header_bytes(kind: RecordKind) -> u64 {
    if matches!(kind, RecordKind::Tree) {
        TREE_HEADER_BYTES
    } else {
        BOUNDED_HEADER_BYTES
    }
}

fn ensure_record_bound(kind: RecordKind, payload_len: u64) -> Result<(), Error> {
    let total = record_header_bytes(kind)
        .checked_add(payload_len)
        .ok_or_else(|| error(ErrorKind::Overflow, FieldClass::Length, 0))?;
    if total > u64::from(MAX_RECORD_BYTES) {
        return Err(error(
            ErrorKind::LimitExceeded,
            FieldClass::Length,
            u32::try_from(total).unwrap_or(u32::MAX),
        ));
    }
    Ok(())
}

pub(crate) fn read_record_header(source: &mut dyn CanonicalSource) -> Result<RecordHeader, Error> {
    let mut magic = [0_u8; 8];
    source.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(error(ErrorKind::Malformed, FieldClass::Header, 0));
    }
    let kind = RecordKind::from_u8(read_u8(source)?)?;
    let version = FormatVersion::new_const(read_u16(source)?);
    if version != ROOT_FORMAT_V2 {
        return Err(error(
            ErrorKind::UnsupportedVersion,
            FieldClass::Version,
            u32::from(version.get()),
        ));
    }
    let payload_len = if kind == RecordKind::Tree {
        read_u64(source)?
    } else {
        u64::from(read_u32(source)?)
    };
    ensure_record_bound(kind, payload_len)?;
    Ok(RecordHeader {
        kind,
        version,
        payload_len,
    })
}

pub(crate) fn require_kind(header: RecordHeader, expected: RecordKind) -> Result<(), Error> {
    if header.kind != expected {
        return Err(error(
            ErrorKind::UnknownKind,
            FieldClass::Kind,
            header.kind as u32,
        ));
    }
    Ok(())
}

pub(crate) fn write_tlv_header(
    sink: &mut dyn CanonicalSink,
    tag: u8,
    value_len: u32,
) -> Result<(), Error> {
    write_u8(sink, tag)?;
    write_u32(sink, value_len)
}

pub(crate) fn write_tlv(sink: &mut dyn CanonicalSink, tag: u8, bytes: &[u8]) -> Result<(), Error> {
    let length = u32::try_from(bytes.len())
        .map_err(|_| error(ErrorKind::LimitExceeded, FieldClass::Length, u32::from(tag)))?;
    write_tlv_header(sink, tag, length)?;
    sink.write_all(bytes)
}

pub(crate) fn write_option_tlv(
    sink: &mut dyn CanonicalSink,
    tag: u8,
    bytes: Option<&[u8]>,
) -> Result<(), Error> {
    let body_len = bytes.map_or(0_u32, |value| {
        u32::try_from(value.len()).unwrap_or(u32::MAX)
    });
    if body_len == u32::MAX {
        return Err(error(
            ErrorKind::LimitExceeded,
            FieldClass::Length,
            u32::from(tag),
        ));
    }
    let value_len = body_len
        .checked_add(1)
        .ok_or_else(|| error(ErrorKind::Overflow, FieldClass::Length, u32::from(tag)))?;
    write_tlv_header(sink, tag, value_len)?;
    write_u8(sink, u8::from(bytes.is_some()))?;
    if let Some(value) = bytes {
        sink.write_all(value)?;
    }
    Ok(())
}

pub(crate) fn read_tlv(
    source: &mut LimitedSource<'_>,
    expected_tag: u8,
    field: FieldClass,
    maximum_len: u32,
) -> Result<Vec<u8>, Error> {
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
    let allocation = usize::try_from(length)
        .map_err(|_| error(ErrorKind::LimitExceeded, field, u32::from(expected_tag)))?;
    let mut bytes = vec![0_u8; allocation];
    source.read_exact(&mut bytes)?;
    Ok(bytes)
}

pub(crate) fn decode_option(
    bytes: &[u8],
    field: FieldClass,
    ordinal: u32,
) -> Result<Option<&[u8]>, Error> {
    let Some((presence, value)) = bytes.split_first() else {
        return Err(error(ErrorKind::Malformed, field, ordinal));
    };
    match *presence {
        0 if value.is_empty() => Ok(None),
        1 => Ok(Some(value)),
        _ => Err(error(ErrorKind::Malformed, field, ordinal)),
    }
}

pub(crate) struct LimitedSource<'a> {
    source: &'a mut dyn CanonicalSource,
    remaining: u64,
}

impl<'a> LimitedSource<'a> {
    pub(crate) const fn new(source: &'a mut dyn CanonicalSource, remaining: u64) -> Self {
        Self { source, remaining }
    }

    pub(crate) const fn remaining(&self) -> u64 {
        self.remaining
    }

    pub(crate) fn finish(self, field: FieldClass) -> Result<(), Error> {
        if self.remaining == 0 {
            Ok(())
        } else {
            Err(error(
                ErrorKind::Malformed,
                field,
                u32::try_from(self.remaining).unwrap_or(u32::MAX),
            ))
        }
    }
}

impl CanonicalSource for LimitedSource<'_> {
    fn read_exact(&mut self, bytes: &mut [u8]) -> Result<(), Error> {
        let requested = u64::try_from(bytes.len())
            .map_err(|_| error(ErrorKind::LimitExceeded, FieldClass::Length, 0))?;
        if requested > self.remaining {
            return Err(error(
                ErrorKind::UnexpectedEnd,
                FieldClass::Length,
                u32::try_from(requested).unwrap_or(u32::MAX),
            ));
        }
        self.source.read_exact(bytes)?;
        self.remaining -= requested;
        Ok(())
    }

    fn ensure_exhausted(&mut self) -> Result<(), Error> {
        if self.remaining == 0 {
            Ok(())
        } else {
            Err(error(
                ErrorKind::TrailingBytes,
                FieldClass::Record,
                u32::try_from(self.remaining).unwrap_or(u32::MAX),
            ))
        }
    }
}

pub(crate) struct SliceSource<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> SliceSource<'a> {
    pub(crate) const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    pub(crate) const fn remaining_len(&self) -> usize {
        self.bytes.len() - self.position
    }
}

impl CanonicalSource for SliceSource<'_> {
    fn read_exact(&mut self, output: &mut [u8]) -> Result<(), Error> {
        let end = self
            .position
            .checked_add(output.len())
            .ok_or_else(|| error(ErrorKind::Overflow, FieldClass::Source, 0))?;
        let Some(input) = self.bytes.get(self.position..end) else {
            return Err(error(
                ErrorKind::UnexpectedEnd,
                FieldClass::Source,
                u32::try_from(self.position).unwrap_or(u32::MAX),
            ));
        };
        output.copy_from_slice(input);
        self.position = end;
        Ok(())
    }

    fn ensure_exhausted(&mut self) -> Result<(), Error> {
        if self.position == self.bytes.len() {
            Ok(())
        } else {
            Err(error(
                ErrorKind::TrailingBytes,
                FieldClass::Source,
                u32::try_from(self.position).unwrap_or(u32::MAX),
            ))
        }
    }
}

pub(crate) struct ExactLengthSink<'a> {
    sink: &'a mut dyn CanonicalSink,
    remaining: u64,
}

impl<'a> ExactLengthSink<'a> {
    pub(crate) const fn new(sink: &'a mut dyn CanonicalSink, expected: u64) -> Self {
        Self {
            sink,
            remaining: expected,
        }
    }

    pub(crate) fn finish(self, field: FieldClass) -> Result<(), Error> {
        if self.remaining == 0 {
            Ok(())
        } else {
            Err(error(
                ErrorKind::Malformed,
                field,
                u32::try_from(self.remaining).unwrap_or(u32::MAX),
            ))
        }
    }
}

impl CanonicalSink for ExactLengthSink<'_> {
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let count = u64::try_from(bytes.len())
            .map_err(|_| error(ErrorKind::LimitExceeded, FieldClass::Length, 0))?;
        if count > self.remaining {
            return Err(error(
                ErrorKind::Malformed,
                FieldClass::Length,
                u32::try_from(count).unwrap_or(u32::MAX),
            ));
        }
        self.sink.write_all(bytes)?;
        self.remaining -= count;
        Ok(())
    }
}

#[must_use]
pub fn object_record_payload_len(record: &ObjectRecord) -> u32 {
    u32::try_from(record.payload.len()).unwrap_or(u32::MAX)
}

pub fn encode_object_record(
    record: &ObjectRecord,
    sink: &mut dyn CanonicalSink,
) -> Result<(), Error> {
    let payload_len = object_record_payload_len(record);
    write_record_header(
        sink,
        RecordKind::from_object(record.kind),
        ROOT_FORMAT_V2,
        u64::from(payload_len),
    )?;
    sink.write_all(&record.payload)
}

pub fn decode_object_record(source: &mut dyn CanonicalSource) -> Result<ObjectRecord, Error> {
    let header = read_record_header(source)?;
    let kind = header.kind.object_kind()?;
    let length = u32::try_from(header.payload_len)
        .map_err(|_| error(ErrorKind::LimitExceeded, FieldClass::Length, 0))?;
    if length > MAX_RECORD_BYTES {
        return Err(error(ErrorKind::LimitExceeded, FieldClass::Length, length));
    }
    let mut payload = vec![
        0_u8;
        usize::try_from(length).map_err(|_| {
            error(ErrorKind::LimitExceeded, FieldClass::Length, length)
        })?
    ];
    source.read_exact(&mut payload)?;
    source.ensure_exhausted()?;
    ObjectRecord::new(kind, payload)
}

pub fn object_id(record: &ObjectRecord, digest: &mut dyn TypedDigest) -> Result<ObjectId, Error> {
    let mut invocations = 0_u8;
    let mut encode_payload = |sink: &mut dyn CanonicalSink| {
        invocations = invocations
            .checked_add(1)
            .ok_or_else(|| error(ErrorKind::Overflow, FieldClass::Digest, 0))?;
        sink.write_all(&record.payload)
    };
    let value = digest.digest(
        DigestDomain::Object(record.kind),
        ROOT_FORMAT_V2,
        u64::from(object_record_payload_len(record)),
        &mut encode_payload,
    )?;
    if invocations != 1 {
        return Err(error(
            ErrorKind::DigestFailure,
            FieldClass::Digest,
            u32::from(invocations),
        ));
    }
    Ok(ObjectId::new(record.kind, value))
}

pub fn encode_digest_preimage_header(
    domain: DigestDomain,
    version: FormatVersion,
    payload_len: u64,
    sink: &mut dyn CanonicalSink,
) -> Result<(), Error> {
    let kind = match domain {
        DigestDomain::RootRecord => RecordKind::Root,
        DigestDomain::TreeManifest => RecordKind::Tree,
        DigestDomain::Object(kind) => RecordKind::from_object(kind),
    };
    write_record_header(sink, kind, version, payload_len)
}

#[must_use]
pub const fn digest_preimage_header_len(domain: DigestDomain) -> u64 {
    match domain {
        DigestDomain::TreeManifest => TREE_HEADER_BYTES,
        DigestDomain::RootRecord | DigestDomain::Object(_) => BOUNDED_HEADER_BYTES,
    }
}
