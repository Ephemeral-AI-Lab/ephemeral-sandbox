use crate::codec::{
    checked_add, decode_option, error, read_record_header, read_tlv, require_kind,
    write_option_tlv, write_record_header, write_tlv, LimitedSource, RecordKind, TLV_HEADER_BYTES,
};
use crate::{
    CanonicalSink, CanonicalSource, CapabilitySet, ChunkProfileId, Digest32, DigestDomain, Error,
    ErrorKind, FieldClass, FormatVersion, PublicationId, PublicationIdentity, RootId,
    TreeManifestId, TypedDigest, ValidatedTree, ROOT_FORMAT_V2,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RootRecordV2 {
    format: FormatVersion,
    required_capabilities: CapabilitySet,
    chunk_profile: ChunkProfileId,
    tree_manifest: TreeManifestId,
    parent: Option<RootId>,
    base: Option<RootId>,
    publication: PublicationIdentity,
}

impl RootRecordV2 {
    #[must_use]
    pub fn new(
        tree: &ValidatedTree,
        chunk_profile: ChunkProfileId,
        parent: Option<RootId>,
        base: Option<RootId>,
        publication: PublicationIdentity,
    ) -> Self {
        Self {
            format: ROOT_FORMAT_V2,
            required_capabilities: tree.required_capabilities(),
            chunk_profile,
            tree_manifest: tree.id(),
            parent,
            base,
            publication,
        }
    }

    #[must_use]
    pub const fn format(self) -> FormatVersion {
        self.format
    }

    #[must_use]
    pub const fn required_capabilities(self) -> CapabilitySet {
        self.required_capabilities
    }

    #[must_use]
    pub const fn chunk_profile(self) -> ChunkProfileId {
        self.chunk_profile
    }

    #[must_use]
    pub const fn tree_manifest(self) -> TreeManifestId {
        self.tree_manifest
    }

    #[must_use]
    pub const fn parent(self) -> Option<RootId> {
        self.parent
    }

    #[must_use]
    pub const fn base(self) -> Option<RootId> {
        self.base
    }

    #[must_use]
    pub const fn publication(self) -> PublicationIdentity {
        self.publication
    }
}

fn option_digest_len(value: Option<RootId>) -> u64 {
    if value.is_some() {
        33
    } else {
        1
    }
}

pub fn root_record_payload_len(record: &RootRecordV2) -> Result<u32, Error> {
    let fixed = TLV_HEADER_BYTES * 7 + 8 + 2 + 32 + 8 + 16;
    let length = checked_add(
        checked_add(fixed, option_digest_len(record.parent), FieldClass::Record)?,
        option_digest_len(record.base),
        FieldClass::Record,
    )?;
    u32::try_from(length).map_err(|_| error(ErrorKind::LimitExceeded, FieldClass::Length, 0))
}

fn encode_root_payload(record: &RootRecordV2, sink: &mut dyn CanonicalSink) -> Result<(), Error> {
    write_tlv(sink, 1, &record.required_capabilities.bits().to_be_bytes())?;
    write_tlv(sink, 2, &record.chunk_profile.get().to_be_bytes())?;
    write_tlv(sink, 3, record.tree_manifest.digest().as_bytes())?;
    let parent = record.parent.map(|value| value.digest().into_bytes());
    write_option_tlv(sink, 4, parent.as_ref().map(|bytes| bytes.as_slice()))?;
    let base = record.base.map(|value| value.digest().into_bytes());
    write_option_tlv(sink, 5, base.as_ref().map(|bytes| bytes.as_slice()))?;
    write_tlv(sink, 6, &record.publication.generation().to_be_bytes())?;
    write_tlv(sink, 7, record.publication.id().as_bytes())
}

pub fn encode_root_record(
    record: &RootRecordV2,
    sink: &mut dyn CanonicalSink,
) -> Result<(), Error> {
    write_record_header(
        sink,
        RecordKind::Root,
        record.format,
        u64::from(root_record_payload_len(record)?),
    )?;
    encode_root_payload(record, sink)
}

fn fixed<const N: usize>(bytes: &[u8], field: FieldClass, ordinal: u32) -> Result<[u8; N], Error> {
    bytes
        .try_into()
        .map_err(|_| error(ErrorKind::Malformed, field, ordinal))
}

fn decode_root_option(
    bytes: &[u8],
    field: FieldClass,
    ordinal: u32,
) -> Result<Option<RootId>, Error> {
    decode_option(bytes, field, ordinal)?
        .map(|value| Ok(RootId::new(Digest32::new(fixed(value, field, ordinal)?))))
        .transpose()
}

pub fn decode_root_record(
    source: &mut dyn CanonicalSource,
    tree: &ValidatedTree,
) -> Result<RootRecordV2, Error> {
    let header = read_record_header(source)?;
    require_kind(header, RecordKind::Root)?;
    let mut payload = LimitedSource::new(source, header.payload_len);
    let capabilities = CapabilitySet::from_bits(u64::from_be_bytes(fixed(
        &read_tlv(&mut payload, 1, FieldClass::Capability, 8)?,
        FieldClass::Capability,
        1,
    )?))?;
    let profile = ChunkProfileId::new(u16::from_be_bytes(fixed(
        &read_tlv(&mut payload, 2, FieldClass::Profile, 2)?,
        FieldClass::Profile,
        2,
    )?))?;
    let tree_manifest = TreeManifestId::new(Digest32::new(fixed(
        &read_tlv(&mut payload, 3, FieldClass::Digest, 32)?,
        FieldClass::Digest,
        3,
    )?));
    let parent = decode_root_option(
        &read_tlv(&mut payload, 4, FieldClass::Digest, 33)?,
        FieldClass::Digest,
        4,
    )?;
    let base = decode_root_option(
        &read_tlv(&mut payload, 5, FieldClass::Digest, 33)?,
        FieldClass::Digest,
        5,
    )?;
    let generation = u64::from_be_bytes(fixed(
        &read_tlv(&mut payload, 6, FieldClass::Publication, 8)?,
        FieldClass::Publication,
        6,
    )?);
    let publication_id = PublicationId::new(fixed(
        &read_tlv(&mut payload, 7, FieldClass::Publication, 16)?,
        FieldClass::Publication,
        7,
    )?)?;
    payload.finish(FieldClass::Record)?;
    source.ensure_exhausted()?;
    if capabilities != tree.required_capabilities() {
        return Err(error(ErrorKind::NonCanonical, FieldClass::Capability, 1));
    }
    if tree_manifest != tree.id() {
        return Err(error(ErrorKind::MissingReference, FieldClass::Digest, 3));
    }
    Ok(RootRecordV2::new(
        tree,
        profile,
        parent,
        base,
        PublicationIdentity::new(generation, publication_id),
    ))
}

pub fn root_id(record: &RootRecordV2, digest: &mut dyn TypedDigest) -> Result<RootId, Error> {
    let payload_len = u64::from(root_record_payload_len(record)?);
    let mut invocation_count = 0_u8;
    let value = {
        let mut encode_payload = |sink: &mut dyn CanonicalSink| {
            invocation_count = invocation_count
                .checked_add(1)
                .ok_or_else(|| error(ErrorKind::Overflow, FieldClass::Digest, 0))?;
            encode_root_payload(record, sink)
        };
        digest.digest(
            DigestDomain::RootRecord,
            record.format,
            payload_len,
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
    Ok(RootId::new(value))
}
