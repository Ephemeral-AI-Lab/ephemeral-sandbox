use crate::{Error, ErrorKind, FieldClass};

#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct FormatVersion(u16);

impl FormatVersion {
    #[must_use]
    pub const fn new_const(value: u16) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

pub const ROOT_FORMAT_V2: FormatVersion = FormatVersion::new_const(2);
pub const ROOT_FORMAT_V3: FormatVersion = FormatVersion::new_const(3);

#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct Digest32([u8; 32]);

impl Digest32 {
    #[must_use]
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    #[must_use]
    pub const fn into_bytes(self) -> [u8; 32] {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct RootId(Digest32);

impl RootId {
    #[must_use]
    pub const fn new(digest: Digest32) -> Self {
        Self(digest)
    }

    #[must_use]
    pub const fn digest(self) -> Digest32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct TreeManifestId(Digest32);

impl TreeManifestId {
    #[must_use]
    pub const fn new(digest: Digest32) -> Self {
        Self(digest)
    }

    #[must_use]
    pub const fn digest(self) -> Digest32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum ObjectKind {
    FileSegments = 2,
    ChunkPayload = 3,
    Transition = 4,
}

impl ObjectKind {
    pub(crate) fn from_u8(value: u8) -> Result<Self, Error> {
        match value {
            2 => Ok(Self::FileSegments),
            3 => Ok(Self::ChunkPayload),
            4 => Ok(Self::Transition),
            _ => Err(Error::new(
                ErrorKind::UnknownKind,
                ROOT_FORMAT_V2,
                FieldClass::Kind,
                u32::from(value),
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ObjectId {
    kind: ObjectKind,
    digest: Digest32,
}

impl ObjectId {
    #[must_use]
    pub const fn new(kind: ObjectKind, digest: Digest32) -> Self {
        Self { kind, digest }
    }

    #[must_use]
    pub const fn kind(self) -> ObjectKind {
        self.kind
    }

    #[must_use]
    pub const fn digest(self) -> Digest32 {
        self.digest
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum EntryKind {
    Directory = 1,
    Regular = 2,
    Symlink = 3,
    Device = 4,
    Fifo = 5,
}

impl EntryKind {
    pub(crate) fn from_u8(value: u8) -> Result<Self, Error> {
        match value {
            1 => Ok(Self::Directory),
            2 => Ok(Self::Regular),
            3 => Ok(Self::Symlink),
            4 => Ok(Self::Device),
            5 => Ok(Self::Fifo),
            _ => Err(Error::new(
                ErrorKind::UnknownKind,
                ROOT_FORMAT_V2,
                FieldClass::Entry,
                u32::from(value),
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct PublicationId([u8; 16]);

impl PublicationId {
    pub fn new(bytes: [u8; 16]) -> Result<Self, Error> {
        if bytes == [0; 16] {
            return Err(Error::new(
                ErrorKind::InvalidValue,
                ROOT_FORMAT_V2,
                FieldClass::Publication,
                0,
            ));
        }
        Ok(Self(bytes))
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PublicationIdentity {
    generation: u64,
    id: PublicationId,
}

impl PublicationIdentity {
    #[must_use]
    pub const fn new(generation: u64, id: PublicationId) -> Self {
        Self { generation, id }
    }

    #[must_use]
    pub const fn generation(self) -> u64 {
        self.generation
    }

    #[must_use]
    pub const fn id(self) -> PublicationId {
        self.id
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct HardlinkGroupId(u128);

impl HardlinkGroupId {
    pub fn new(rank: u128) -> Result<Self, Error> {
        if rank == 0 {
            return Err(Error::new(
                ErrorKind::InvalidValue,
                ROOT_FORMAT_V2,
                FieldClass::Hardlink,
                0,
            ));
        }
        Ok(Self(rank))
    }

    #[must_use]
    pub const fn get(self) -> u128 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct ChunkProfileId(u16);

impl ChunkProfileId {
    pub const SEQ_CDC_V1: Self = Self(1);

    pub fn new(value: u16) -> Result<Self, Error> {
        if value != Self::SEQ_CDC_V1.0 {
            return Err(Error::new(
                ErrorKind::InvalidValue,
                ROOT_FORMAT_V2,
                FieldClass::Profile,
                u32::from(value),
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum Capability {
    Xattrs = 0,
    SparseHoles = 1,
    Hardlinks = 2,
    Symlinks = 3,
    Devices = 4,
    Fifo = 5,
}

impl Capability {
    #[must_use]
    pub const fn bit(self) -> u64 {
        1_u64 << (self as u8)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct CapabilitySet(u64);

impl CapabilitySet {
    pub const KNOWN_BITS: u64 = (1_u64 << 6) - 1;

    #[must_use]
    pub const fn empty() -> Self {
        Self(0)
    }

    pub fn from_bits(bits: u64) -> Result<Self, Error> {
        let unknown = bits & !Self::KNOWN_BITS;
        if unknown != 0 {
            return Err(Error::new(
                ErrorKind::UnknownCapability,
                ROOT_FORMAT_V2,
                FieldClass::Capability,
                unknown.trailing_zeros(),
            ));
        }
        Ok(Self(bits))
    }

    #[must_use]
    pub const fn bits(self) -> u64 {
        self.0
    }

    #[must_use]
    pub const fn contains(self, capability: Capability) -> bool {
        self.0 & capability.bit() != 0
    }

    pub(crate) fn insert(&mut self, capability: Capability) {
        self.0 |= capability.bit();
    }
}
