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
