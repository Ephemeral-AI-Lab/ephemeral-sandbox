use crate::{Digest32, Error, FormatVersion, ObjectKind};

pub trait CanonicalSink {
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), Error>;
}

pub trait CanonicalSource {
    fn read_exact(&mut self, bytes: &mut [u8]) -> Result<(), Error>;

    fn ensure_exhausted(&mut self) -> Result<(), Error>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DigestDomain {
    RootRecord,
    TreeManifest,
    Object(ObjectKind),
    V3Record(u8),
}

pub trait TypedDigest {
    fn digest(
        &mut self,
        domain: DigestDomain,
        version: FormatVersion,
        payload_len: u64,
        encode_payload: &mut dyn FnMut(&mut dyn CanonicalSink) -> Result<(), Error>,
    ) -> Result<Digest32, Error>;
}

pub trait RawDigest {
    fn digest_bytes(&mut self, bytes: &[u8]) -> Result<Digest32, Error>;
}
