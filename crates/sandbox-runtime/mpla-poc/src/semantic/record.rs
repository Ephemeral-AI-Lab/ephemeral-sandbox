use std::io::Read;

use sha2::{Digest, Sha256};

use crate::{PocError, PocResult};

pub const MAX_PATH_BYTES: usize = 4_096;
pub const MAX_KEY_BYTES: usize = MAX_PATH_BYTES + 80;
pub const MAX_RECORD_BYTES: usize = 256 * 1024;
pub const MAX_XATTR_BYTES: usize = 64 * 1024;
const RECORD_VERSION: u8 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum NodeKind {
    Regular = 1,
    Directory = 2,
    Symlink = 3,
    Fifo = 4,
    CharacterDevice = 5,
    BlockDevice = 6,
    Socket = 7,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ExtentKind {
    Data = 1,
    Hole = 2,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NodeRecord {
    pub path: Vec<u8>,
    pub kind: NodeKind,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub mtime_seconds: i64,
    pub mtime_nanoseconds: u32,
    pub logical_size: u64,
    pub symlink_target: Vec<u8>,
    pub device_major: u32,
    pub device_minor: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SemanticRecord {
    Node(NodeRecord),
    Xattr {
        path: Vec<u8>,
        name: Vec<u8>,
        value: Vec<u8>,
    },
    Extent {
        path: Vec<u8>,
        offset: u64,
        length: u64,
        kind: ExtentKind,
    },
    Chunk {
        path: Vec<u8>,
        offset: u64,
        length: u32,
        sha256: [u8; 32],
    },
    Whiteout {
        path: Vec<u8>,
    },
    OpaqueDirectory {
        path: Vec<u8>,
    },
    HardlinkGroup {
        group_sha256: [u8; 32],
        content_sha256: [u8; 32],
        member_count: u64,
    },
    HardlinkMember {
        group_sha256: [u8; 32],
        path: Vec<u8>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecordMutation {
    Replace(SemanticRecord),
    Delete { canonical_key: Vec<u8> },
}

impl SemanticRecord {
    pub fn canonical_key(&self) -> PocResult<Vec<u8>> {
        let mut key = Vec::with_capacity(128);
        match self {
            Self::Node(record) => {
                key.push(0x10);
                push_path_key(&mut key, &record.path)?;
            }
            Self::Xattr { path, name, .. } => {
                key.push(0x11);
                push_path_key(&mut key, path)?;
                push_bytes(&mut key, name)?;
            }
            Self::Extent { path, offset, .. } => {
                key.push(0x12);
                push_path_key(&mut key, path)?;
                key.extend_from_slice(&offset.to_be_bytes());
            }
            Self::Chunk { path, offset, .. } => {
                key.push(0x13);
                push_path_key(&mut key, path)?;
                key.extend_from_slice(&offset.to_be_bytes());
            }
            Self::Whiteout { path } => {
                key.push(0x14);
                push_path_key(&mut key, path)?;
            }
            Self::OpaqueDirectory { path } => {
                key.push(0x15);
                push_path_key(&mut key, path)?;
            }
            Self::HardlinkGroup { group_sha256, .. } => {
                key.push(0x20);
                key.extend_from_slice(group_sha256);
            }
            Self::HardlinkMember { group_sha256, path } => {
                key.push(0x21);
                key.extend_from_slice(group_sha256);
                push_path_key(&mut key, path)?;
            }
        }
        if key.len() > MAX_KEY_BYTES {
            return Err(PocError::Integrity(
                "semantic canonical key exceeds bound".to_owned(),
            ));
        }
        Ok(key)
    }

    pub fn key_digest(&self) -> PocResult<[u8; 32]> {
        digest_key(&self.canonical_key()?)
    }

    pub fn encode(&self) -> PocResult<Vec<u8>> {
        self.validate()?;
        let mut output = Vec::with_capacity(256);
        output.push(RECORD_VERSION);
        output.push(self.kind_tag());
        match self {
            Self::Node(record) => {
                push_bytes(&mut output, &record.path)?;
                output.push(record.kind as u8);
                output.extend_from_slice(&record.mode.to_be_bytes());
                output.extend_from_slice(&record.uid.to_be_bytes());
                output.extend_from_slice(&record.gid.to_be_bytes());
                output.extend_from_slice(&record.mtime_seconds.to_be_bytes());
                output.extend_from_slice(&record.mtime_nanoseconds.to_be_bytes());
                output.extend_from_slice(&record.logical_size.to_be_bytes());
                push_bytes(&mut output, &record.symlink_target)?;
                output.extend_from_slice(&record.device_major.to_be_bytes());
                output.extend_from_slice(&record.device_minor.to_be_bytes());
            }
            Self::Xattr { path, name, value } => {
                push_bytes(&mut output, path)?;
                push_bytes(&mut output, name)?;
                push_bytes(&mut output, value)?;
            }
            Self::Extent {
                path,
                offset,
                length,
                kind,
            } => {
                push_bytes(&mut output, path)?;
                output.extend_from_slice(&offset.to_be_bytes());
                output.extend_from_slice(&length.to_be_bytes());
                output.push(*kind as u8);
            }
            Self::Chunk {
                path,
                offset,
                length,
                sha256,
            } => {
                push_bytes(&mut output, path)?;
                output.extend_from_slice(&offset.to_be_bytes());
                output.extend_from_slice(&length.to_be_bytes());
                output.extend_from_slice(sha256);
            }
            Self::Whiteout { path } | Self::OpaqueDirectory { path } => {
                push_bytes(&mut output, path)?;
            }
            Self::HardlinkGroup {
                group_sha256,
                content_sha256,
                member_count,
            } => {
                output.extend_from_slice(group_sha256);
                output.extend_from_slice(content_sha256);
                output.extend_from_slice(&member_count.to_be_bytes());
            }
            Self::HardlinkMember { group_sha256, path } => {
                output.extend_from_slice(group_sha256);
                push_bytes(&mut output, path)?;
            }
        }
        if output.len() > MAX_RECORD_BYTES {
            return Err(PocError::Integrity(
                "semantic record exceeds bound".to_owned(),
            ));
        }
        Ok(output)
    }

    pub fn encode_frame(&self) -> PocResult<Vec<u8>> {
        let record = self.encode()?;
        let length = u32::try_from(record.len())
            .map_err(|_| PocError::Integrity("semantic record length overflow".to_owned()))?;
        let mut frame = Vec::with_capacity(record.len() + 4);
        frame.extend_from_slice(&length.to_be_bytes());
        frame.extend_from_slice(&record);
        Ok(frame)
    }

    pub fn decode(bytes: &[u8]) -> PocResult<Self> {
        let mut decoder = Decoder::new(bytes);
        if decoder.u8()? != RECORD_VERSION {
            return Err(PocError::Integrity(
                "unsupported semantic record version".to_owned(),
            ));
        }
        let tag = decoder.u8()?;
        let record = match tag {
            0x10 => Self::Node(NodeRecord {
                path: decoder.bytes()?,
                kind: decode_node_kind(decoder.u8()?)?,
                mode: decoder.u32()?,
                uid: decoder.u32()?,
                gid: decoder.u32()?,
                mtime_seconds: decoder.i64()?,
                mtime_nanoseconds: decoder.u32()?,
                logical_size: decoder.u64()?,
                symlink_target: decoder.bytes()?,
                device_major: decoder.u32()?,
                device_minor: decoder.u32()?,
            }),
            0x11 => Self::Xattr {
                path: decoder.bytes()?,
                name: decoder.bytes()?,
                value: decoder.bytes()?,
            },
            0x12 => Self::Extent {
                path: decoder.bytes()?,
                offset: decoder.u64()?,
                length: decoder.u64()?,
                kind: decode_extent_kind(decoder.u8()?)?,
            },
            0x13 => Self::Chunk {
                path: decoder.bytes()?,
                offset: decoder.u64()?,
                length: decoder.u32()?,
                sha256: decoder.fixed()?,
            },
            0x14 => Self::Whiteout {
                path: decoder.bytes()?,
            },
            0x15 => Self::OpaqueDirectory {
                path: decoder.bytes()?,
            },
            0x20 => Self::HardlinkGroup {
                group_sha256: decoder.fixed()?,
                content_sha256: decoder.fixed()?,
                member_count: decoder.u64()?,
            },
            0x21 => Self::HardlinkMember {
                group_sha256: decoder.fixed()?,
                path: decoder.bytes()?,
            },
            _ => {
                return Err(PocError::Integrity(
                    "unknown semantic record kind".to_owned(),
                ));
            }
        };
        decoder.finish()?;
        record.validate()?;
        Ok(record)
    }

    pub fn record_digest(&self) -> PocResult<[u8; 32]> {
        let mut digest = Sha256::new();
        digest.update(b"mpla-poc-semantic-v1/record\0");
        digest.update(self.encode()?);
        Ok(digest.finalize().into())
    }

    fn kind_tag(&self) -> u8 {
        match self {
            Self::Node(_) => 0x10,
            Self::Xattr { .. } => 0x11,
            Self::Extent { .. } => 0x12,
            Self::Chunk { .. } => 0x13,
            Self::Whiteout { .. } => 0x14,
            Self::OpaqueDirectory { .. } => 0x15,
            Self::HardlinkGroup { .. } => 0x20,
            Self::HardlinkMember { .. } => 0x21,
        }
    }

    fn validate(&self) -> PocResult<()> {
        match self {
            Self::Node(record) => {
                validate_path(&record.path, true)?;
                if record.mtime_nanoseconds >= 1_000_000_000 {
                    return Err(PocError::Integrity(
                        "mtime nanoseconds are not normalized".to_owned(),
                    ));
                }
                match record.kind {
                    NodeKind::Regular if !record.symlink_target.is_empty() => {
                        return Err(PocError::Integrity(
                            "regular node carries a symlink target".to_owned(),
                        ));
                    }
                    NodeKind::Symlink if record.symlink_target.contains(&0) => {
                        return Err(PocError::Integrity(
                            "symlink target contains NUL".to_owned(),
                        ));
                    }
                    _ => {}
                }
            }
            Self::Xattr { path, name, value } => {
                validate_path(path, true)?;
                if name.is_empty()
                    || name.contains(&0)
                    || name.len() > MAX_XATTR_BYTES
                    || value.len() > MAX_XATTR_BYTES
                {
                    return Err(PocError::Integrity(
                        "xattr name or value exceeds semantic bounds".to_owned(),
                    ));
                }
            }
            Self::Extent { path, length, .. } => {
                validate_path(path, true)?;
                if *length == 0 {
                    return Err(PocError::Integrity("zero-length sparse extent".to_owned()));
                }
            }
            Self::Chunk { path, length, .. } => {
                validate_path(path, true)?;
                if *length == 0 || usize::try_from(*length).unwrap_or(usize::MAX) > 32 * 1024 {
                    return Err(PocError::Integrity(
                        "semantic chunk length exceeds scan window".to_owned(),
                    ));
                }
            }
            Self::Whiteout { path } => validate_path(path, false)?,
            Self::OpaqueDirectory { path } => validate_path(path, true)?,
            Self::HardlinkGroup { member_count, .. } => {
                if *member_count < 2 {
                    return Err(PocError::Integrity(
                        "hardlink group has fewer than two members".to_owned(),
                    ));
                }
            }
            Self::HardlinkMember { path, .. } => validate_path(path, false)?,
        }
        Ok(())
    }
}

impl RecordMutation {
    pub fn canonical_key(&self) -> PocResult<Vec<u8>> {
        match self {
            Self::Replace(record) => record.canonical_key(),
            Self::Delete { canonical_key } => {
                if canonical_key.is_empty() || canonical_key.len() > MAX_KEY_BYTES {
                    return Err(PocError::Integrity(
                        "deleted canonical key exceeds bounds".to_owned(),
                    ));
                }
                Ok(canonical_key.clone())
            }
        }
    }

    pub fn key_digest(&self) -> PocResult<[u8; 32]> {
        digest_key(&self.canonical_key()?)
    }

    pub fn encode(&self) -> PocResult<Vec<u8>> {
        let mut output = Vec::new();
        match self {
            Self::Replace(record) => {
                output.push(1);
                let key = record.canonical_key()?;
                push_bytes(&mut output, &key)?;
                push_bytes(&mut output, &record.encode()?)?;
            }
            Self::Delete { canonical_key } => {
                output.push(2);
                push_bytes(&mut output, canonical_key)?;
            }
        }
        Ok(output)
    }

    pub fn decode(bytes: &[u8]) -> PocResult<Self> {
        let mut decoder = Decoder::new(bytes);
        let action = decoder.u8()?;
        let key = decoder.bytes()?;
        let mutation = match action {
            1 => {
                let record = SemanticRecord::decode(&decoder.bytes()?)?;
                if record.canonical_key()? != key {
                    return Err(PocError::Integrity(
                        "affected replacement key does not match record".to_owned(),
                    ));
                }
                Self::Replace(record)
            }
            2 => Self::Delete { canonical_key: key },
            _ => {
                return Err(PocError::Integrity(
                    "unknown affected mutation action".to_owned(),
                ));
            }
        };
        decoder.finish()?;
        mutation.canonical_key()?;
        Ok(mutation)
    }
}

pub struct RecordStreamReader<R> {
    reader: R,
}

impl<R: Read> RecordStreamReader<R> {
    pub const fn new(reader: R) -> Self {
        Self { reader }
    }

    pub fn next_record(&mut self) -> PocResult<Option<SemanticRecord>> {
        let mut length = [0_u8; 4];
        if !read_exact_or_eof(&mut self.reader, &mut length).map_err(|error| {
            PocError::Integrity(format!("semantic record stream read failed: {error}"))
        })? {
            return Ok(None);
        }
        let length = usize::try_from(u32::from_be_bytes(length))
            .map_err(|_| PocError::Integrity("semantic record length overflow".to_owned()))?;
        if length > MAX_RECORD_BYTES {
            return Err(PocError::Integrity(
                "semantic record stream frame exceeds bound".to_owned(),
            ));
        }
        let mut record = vec![0_u8; length];
        self.reader.read_exact(&mut record).map_err(|error| {
            PocError::Integrity(format!("truncated semantic record stream: {error}"))
        })?;
        SemanticRecord::decode(&record).map(Some)
    }
}

fn read_exact_or_eof(reader: &mut impl Read, bytes: &mut [u8]) -> std::io::Result<bool> {
    let mut filled = 0;
    while filled < bytes.len() {
        let count = reader.read(&mut bytes[filled..])?;
        if count == 0 {
            if filled == 0 {
                return Ok(false);
            }
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "partial semantic record frame",
            ));
        }
        filled += count;
    }
    Ok(true)
}

pub fn validate_path(path: &[u8], allow_root: bool) -> PocResult<()> {
    if path.is_empty() {
        return if allow_root {
            Ok(())
        } else {
            Err(PocError::Integrity(
                "semantic path cannot name the root here".to_owned(),
            ))
        };
    }
    if path.len() > MAX_PATH_BYTES
        || path[0] == b'/'
        || path.contains(&0)
        || path.split(|byte| *byte == b'/').count() > 64
        || path.split(|byte| *byte == b'/').any(|component| {
            component.is_empty() || component.len() > 255 || matches!(component, b"." | b"..")
        })
    {
        return Err(PocError::Integrity(
            "invalid normalized raw relative semantic path".to_owned(),
        ));
    }
    Ok(())
}

pub fn digest_key(key: &[u8]) -> PocResult<[u8; 32]> {
    if key.is_empty() || key.len() > MAX_KEY_BYTES {
        return Err(PocError::Integrity(
            "semantic canonical key exceeds bounds".to_owned(),
        ));
    }
    let mut digest = Sha256::new();
    digest.update(b"mpla-poc-semantic-v1/key\0");
    digest.update(u64::try_from(key.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(key);
    Ok(digest.finalize().into())
}

fn push_path_key(output: &mut Vec<u8>, path: &[u8]) -> PocResult<()> {
    validate_path(path, true)?;
    push_bytes(output, path)
}

fn push_bytes(output: &mut Vec<u8>, bytes: &[u8]) -> PocResult<()> {
    let length = u32::try_from(bytes.len())
        .map_err(|_| PocError::Integrity("semantic byte field length overflow".to_owned()))?;
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(bytes);
    Ok(())
}

fn decode_node_kind(value: u8) -> PocResult<NodeKind> {
    match value {
        1 => Ok(NodeKind::Regular),
        2 => Ok(NodeKind::Directory),
        3 => Ok(NodeKind::Symlink),
        4 => Ok(NodeKind::Fifo),
        5 => Ok(NodeKind::CharacterDevice),
        6 => Ok(NodeKind::BlockDevice),
        7 => Ok(NodeKind::Socket),
        _ => Err(PocError::Integrity("unknown semantic node kind".to_owned())),
    }
}

fn decode_extent_kind(value: u8) -> PocResult<ExtentKind> {
    match value {
        1 => Ok(ExtentKind::Data),
        2 => Ok(ExtentKind::Hole),
        _ => Err(PocError::Integrity(
            "unknown semantic extent kind".to_owned(),
        )),
    }
}

struct Decoder<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Decoder<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn u8(&mut self) -> PocResult<u8> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> PocResult<u32> {
        Ok(u32::from_be_bytes(self.fixed()?))
    }

    fn i64(&mut self) -> PocResult<i64> {
        Ok(i64::from_be_bytes(self.fixed()?))
    }

    fn u64(&mut self) -> PocResult<u64> {
        Ok(u64::from_be_bytes(self.fixed()?))
    }

    fn fixed<const N: usize>(&mut self) -> PocResult<[u8; N]> {
        self.take(N)?
            .try_into()
            .map_err(|_| PocError::Integrity("semantic fixed field length mismatch".to_owned()))
    }

    fn bytes(&mut self) -> PocResult<Vec<u8>> {
        let length = usize::try_from(self.u32()?)
            .map_err(|_| PocError::Integrity("semantic field length overflow".to_owned()))?;
        Ok(self.take(length)?.to_vec())
    }

    fn take(&mut self, length: usize) -> PocResult<&'a [u8]> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or_else(|| PocError::Integrity("semantic decoder offset overflow".to_owned()))?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(|| PocError::Integrity("truncated semantic record".to_owned()))?;
        self.offset = end;
        Ok(value)
    }

    fn finish(self) -> PocResult<()> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err(PocError::Integrity(
                "semantic record has trailing bytes".to_owned(),
            ))
        }
    }
}
