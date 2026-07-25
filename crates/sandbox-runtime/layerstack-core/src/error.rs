use core::fmt;

use crate::identity::FormatVersion;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    InvalidValue,
    UnsupportedVersion,
    UnknownKind,
    UnknownCapability,
    Malformed,
    NonCanonical,
    LimitExceeded,
    Overflow,
    UnexpectedEnd,
    TrailingBytes,
    SourceFailure,
    SinkFailure,
    DigestFailure,
    MissingReference,
    HardlinkMismatch,
    WrongKind,
    WrongDomain,
    NonCanonicalOrder,
    DuplicateEntry,
    CountLimit,
    LengthLimit,
    PageLimit,
    DepthLimit,
    DanglingEdge,
    SparseInvalid,
    UnknownRequiredCapability,
    NonCanonicalCapability,
    ChecksumMismatch,
    CorruptRecord,
    ArithmeticOverflow,
    InvalidIdentifier,
    ObjectCollisionOrCorruption,
    IdentifierCollision,
    IdempotencyMismatch,
    OutcomeExpired,
    GenerationOverflow,
    Conflict,
    ContentionLimit,
    ResourceExhausted,
    QueryLimit,
    HardlinkGroupLimit,
    LastLocatorMissing,
    LastLocatorCorrupt,
    UnsupportedRequiredCapability,
    DigestCollision,
    RequestDeadline,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FieldClass {
    Record,
    Header,
    Kind,
    Version,
    Length,
    Capability,
    Profile,
    Digest,
    Publication,
    Path,
    Entry,
    Metadata,
    Mode,
    Timestamp,
    Xattr,
    SymlinkTarget,
    Device,
    LogicalLength,
    SparseExtent,
    Hardlink,
    ObjectReference,
    Tree,
    Page,
    Segment,
    Attribution,
    Checksum,
    Identifier,
    Branch,
    Operation,
    Locator,
    Lease,
    Source,
    Sink,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Error {
    kind: ErrorKind,
    version: FormatVersion,
    field: FieldClass,
    ordinal: u32,
}

impl Error {
    #[must_use]
    pub const fn new(
        kind: ErrorKind,
        version: FormatVersion,
        field: FieldClass,
        ordinal: u32,
    ) -> Self {
        Self {
            kind,
            version,
            field,
            ordinal,
        }
    }

    #[must_use]
    pub const fn kind(self) -> ErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn version(self) -> FormatVersion {
        self.version
    }

    #[must_use]
    pub const fn field(self) -> FieldClass {
        self.field
    }

    #[must_use]
    pub const fn ordinal(self) -> u32 {
        self.ordinal
    }
}

impl ErrorKind {
    #[must_use]
    pub const fn stage03_code(self) -> Option<u16> {
        match self {
            Self::WrongKind => Some(1),
            Self::UnsupportedVersion => Some(2),
            Self::WrongDomain => Some(3),
            Self::TrailingBytes => Some(4),
            Self::NonCanonicalOrder => Some(5),
            Self::DuplicateEntry => Some(6),
            Self::CountLimit => Some(7),
            Self::LengthLimit => Some(8),
            Self::PageLimit => Some(9),
            Self::DepthLimit => Some(10),
            Self::DanglingEdge => Some(11),
            Self::SparseInvalid => Some(12),
            Self::UnknownRequiredCapability => Some(13),
            Self::ChecksumMismatch => Some(14),
            Self::CorruptRecord => Some(15),
            Self::ArithmeticOverflow => Some(16),
            Self::InvalidIdentifier => Some(17),
            Self::ObjectCollisionOrCorruption => Some(18),
            Self::IdentifierCollision => Some(19),
            Self::IdempotencyMismatch => Some(20),
            Self::OutcomeExpired => Some(21),
            Self::GenerationOverflow => Some(22),
            Self::Conflict => Some(23),
            Self::ContentionLimit => Some(24),
            Self::ResourceExhausted => Some(25),
            Self::QueryLimit => Some(26),
            Self::HardlinkGroupLimit => Some(27),
            Self::LastLocatorMissing => Some(28),
            Self::LastLocatorCorrupt => Some(29),
            Self::UnsupportedRequiredCapability => Some(30),
            Self::DigestCollision => Some(31),
            Self::RequestDeadline => Some(32),
            _ => None,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:?} in {:?} at {} for format {}",
            self.kind,
            self.field,
            self.ordinal,
            self.version.get()
        )
    }
}

impl std::error::Error for Error {}
