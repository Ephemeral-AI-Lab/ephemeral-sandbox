use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use serde::Serialize;
use sha2::{Digest, Sha256};

pub type OracleResult<T> = Result<T, String>;

pub const MAX_PATH_BYTES: usize = 4_096;
pub const MAX_KEY_BYTES: usize = MAX_PATH_BYTES + 80;
pub const MAX_RECORD_BYTES: usize = 256 * 1024;
pub const MAX_XATTR_BYTES: usize = 64 * 1024;
pub const SCAN_WINDOW_BYTES: usize = 32 * 1024;
const RECORD_VERSION: u8 = 1;
const CONTENT_NODE_MAGIC: &[u8; 8] = b"MPLACND1";
const ATTR_NODE_MAGIC: &[u8; 8] = b"MPLAAND1";
const CONTENT_LEAF_MAGIC: &[u8; 8] = b"MPLACLE1";
const ATTR_LEAF_MAGIC: &[u8; 8] = b"MPLAALE1";
const OBJECT_DOMAIN: &[u8] = b"mpla-poc-semantic-v1/object\0";
const TRIE_DEPTH: usize = 64;

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
pub enum Record {
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OracleSummary {
    pub semantic_format: String,
    pub root_id: String,
    pub attribution_root_id: String,
    pub record_stream_sha256: String,
    pub record_stream_path: String,
    pub entry_count: u64,
    pub record_count: u64,
    pub bytes_read: u64,
    pub spool_runs: u64,
    pub spool_bytes: u64,
    pub peak_open_data_fds: u16,
    pub peak_managed_bytes: u64,
}

impl Record {
    pub fn canonical_key(&self) -> OracleResult<Vec<u8>> {
        let mut key = Vec::with_capacity(128);
        match self {
            Self::Node(record) => {
                key.push(0x10);
                push_path(&mut key, &record.path)?;
            }
            Self::Xattr { path, name, .. } => {
                key.push(0x11);
                push_path(&mut key, path)?;
                push_bytes(&mut key, name)?;
            }
            Self::Extent { path, offset, .. } => {
                key.push(0x12);
                push_path(&mut key, path)?;
                key.extend_from_slice(&offset.to_be_bytes());
            }
            Self::Chunk { path, offset, .. } => {
                key.push(0x13);
                push_path(&mut key, path)?;
                key.extend_from_slice(&offset.to_be_bytes());
            }
            Self::Whiteout { path } => {
                key.push(0x14);
                push_path(&mut key, path)?;
            }
            Self::OpaqueDirectory { path } => {
                key.push(0x15);
                push_path(&mut key, path)?;
            }
            Self::HardlinkGroup { group_sha256, .. } => {
                key.push(0x20);
                key.extend_from_slice(group_sha256);
            }
            Self::HardlinkMember { group_sha256, path } => {
                key.push(0x21);
                key.extend_from_slice(group_sha256);
                push_path(&mut key, path)?;
            }
        }
        if key.len() > MAX_KEY_BYTES {
            return Err("oracle canonical key exceeds fixed bound".to_owned());
        }
        Ok(key)
    }

    pub fn key_digest(&self) -> OracleResult<[u8; 32]> {
        digest_key(&self.canonical_key()?)
    }

    pub fn encode(&self) -> OracleResult<Vec<u8>> {
        self.validate()?;
        let mut output = Vec::with_capacity(256);
        output.push(RECORD_VERSION);
        output.push(self.tag());
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
            return Err("oracle semantic record exceeds fixed bound".to_owned());
        }
        Ok(output)
    }

    pub fn decode(bytes: &[u8]) -> OracleResult<Self> {
        let mut decoder = Decoder::new(bytes);
        if decoder.u8()? != RECORD_VERSION {
            return Err("oracle record version mismatch".to_owned());
        }
        let record = match decoder.u8()? {
            0x10 => Self::Node(NodeRecord {
                path: decoder.bytes()?,
                kind: match decoder.u8()? {
                    1 => NodeKind::Regular,
                    2 => NodeKind::Directory,
                    3 => NodeKind::Symlink,
                    4 => NodeKind::Fifo,
                    5 => NodeKind::CharacterDevice,
                    6 => NodeKind::BlockDevice,
                    7 => NodeKind::Socket,
                    _ => return Err("oracle node kind is unknown".to_owned()),
                },
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
                kind: match decoder.u8()? {
                    1 => ExtentKind::Data,
                    2 => ExtentKind::Hole,
                    _ => return Err("oracle extent kind is unknown".to_owned()),
                },
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
            _ => return Err("oracle record kind is unknown".to_owned()),
        };
        decoder.finish()?;
        record.validate()?;
        Ok(record)
    }

    fn record_digest(&self) -> OracleResult<[u8; 32]> {
        let mut digest = Sha256::new();
        digest.update(b"mpla-poc-semantic-v1/record\0");
        digest.update(self.encode()?);
        Ok(digest.finalize().into())
    }

    fn tag(&self) -> u8 {
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

    fn validate(&self) -> OracleResult<()> {
        match self {
            Self::Node(node) => {
                validate_path(&node.path, true)?;
                if node.mtime_nanoseconds >= 1_000_000_000 {
                    return Err("oracle mtime is not normalized".to_owned());
                }
            }
            Self::Xattr { path, name, value } => {
                validate_path(path, true)?;
                if name.is_empty()
                    || name.contains(&0)
                    || name.len() > MAX_XATTR_BYTES
                    || value.len() > MAX_XATTR_BYTES
                {
                    return Err("oracle xattr exceeds fixed bound".to_owned());
                }
            }
            Self::Extent { path, length, .. } => {
                validate_path(path, true)?;
                if *length == 0 {
                    return Err("oracle extent has zero length".to_owned());
                }
            }
            Self::Chunk { path, length, .. } => {
                validate_path(path, true)?;
                if *length == 0
                    || usize::try_from(*length).unwrap_or(usize::MAX) > SCAN_WINDOW_BYTES
                {
                    return Err("oracle chunk exceeds scan window".to_owned());
                }
            }
            Self::Whiteout { path } => validate_path(path, false)?,
            Self::OpaqueDirectory { path } => validate_path(path, true)?,
            Self::HardlinkGroup { member_count, .. } if *member_count < 2 => {
                return Err("oracle hardlink group is not a group".to_owned());
            }
            Self::HardlinkMember { path, .. } => validate_path(path, false)?,
            Self::HardlinkGroup { .. } => {}
        }
        Ok(())
    }
}

pub fn calculate_roots(
    record_stream: &Path,
    actor_id: &str,
    semantic_operation_id: &str,
) -> OracleResult<(String, String, String, u64)> {
    let file = File::open(record_stream).map_err(|error| {
        format!(
            "open oracle record stream {}: {error}",
            record_stream.display()
        )
    })?;
    let mut reader = BufReader::new(file);
    let mut content = TrieBuilder::new(TrieKind::Content);
    let mut attribution = TrieBuilder::new(TrieKind::Attribution);
    let mut previous = None;
    let mut count = 0_u64;
    loop {
        let mut length = [0_u8; 4];
        if !read_exact_or_eof(&mut reader, &mut length)
            .map_err(|error| format!("read oracle record frame: {error}"))?
        {
            break;
        }
        let length = usize::try_from(u32::from_be_bytes(length))
            .map_err(|_| "oracle record frame length overflow".to_owned())?;
        if length == 0 || length > MAX_RECORD_BYTES {
            return Err("oracle record frame exceeds fixed bound".to_owned());
        }
        let mut bytes = vec![0_u8; length];
        reader
            .read_exact(&mut bytes)
            .map_err(|error| format!("read oracle record payload: {error}"))?;
        let record = Record::decode(&bytes)?;
        let key = record.key_digest()?;
        if previous
            .as_ref()
            .is_some_and(|value: &[u8; 32]| value >= &key)
        {
            return Err("oracle record stream is not strictly key-sorted".to_owned());
        }
        let canonical_key = record.canonical_key()?;
        let content_leaf = object_digest(&encode_content_leaf(key, &canonical_key, &bytes)?);
        let attribution_leaf = object_digest(&encode_attribution_leaf(
            key,
            attribution_leaf_digest(record.record_digest()?, actor_id, semantic_operation_id),
        ));
        content.add(ChildRef::leaf(key, content_leaf))?;
        attribution.add(ChildRef::leaf(key, attribution_leaf))?;
        previous = Some(key);
        count = count.saturating_add(1);
    }
    let content_root = content.finish()?;
    let attribution_root = attribution.finish()?;
    let mut stream = Sha256::new();
    stream.update(b"mpla-poc-semantic-v1/record-stream\0");
    stream.update(content_root);
    Ok((
        hex(content_root),
        hex(attribution_root),
        hex(stream.finalize().into()),
        count,
    ))
}

pub fn digest_key(key: &[u8]) -> OracleResult<[u8; 32]> {
    if key.is_empty() || key.len() > MAX_KEY_BYTES {
        return Err("oracle canonical key exceeds fixed bound".to_owned());
    }
    let mut digest = Sha256::new();
    digest.update(b"mpla-poc-semantic-v1/key\0");
    digest.update(u64::try_from(key.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(key);
    Ok(digest.finalize().into())
}

pub fn validate_path(path: &[u8], allow_root: bool) -> OracleResult<()> {
    if path.is_empty() {
        return if allow_root {
            Ok(())
        } else {
            Err("oracle path cannot name root here".to_owned())
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
        return Err("oracle path is not normalized raw relative bytes".to_owned());
    }
    Ok(())
}

fn encode_content_leaf(key_digest: [u8; 32], key: &[u8], record: &[u8]) -> OracleResult<Vec<u8>> {
    let mut output = Vec::with_capacity(48 + key.len() + record.len());
    output.extend_from_slice(CONTENT_LEAF_MAGIC);
    output.extend_from_slice(&key_digest);
    output.extend_from_slice(
        &u32::try_from(key.len())
            .map_err(|_| "oracle leaf key overflow".to_owned())?
            .to_be_bytes(),
    );
    output.extend_from_slice(key);
    output.extend_from_slice(
        &u32::try_from(record.len())
            .map_err(|_| "oracle leaf record overflow".to_owned())?
            .to_be_bytes(),
    );
    output.extend_from_slice(record);
    Ok(output)
}

fn encode_attribution_leaf(key: [u8; 32], value: [u8; 32]) -> Vec<u8> {
    let mut output = Vec::with_capacity(72);
    output.extend_from_slice(ATTR_LEAF_MAGIC);
    output.extend_from_slice(&key);
    output.extend_from_slice(&value);
    output
}

fn attribution_leaf_digest(record: [u8; 32], actor: &str, operation: &str) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"mpla-poc-semantic-v1/attribution-leaf\0");
    digest.update(record);
    update_bytes(&mut digest, actor.as_bytes());
    update_bytes(&mut digest, operation.as_bytes());
    digest.finalize().into()
}

struct TrieBuilder {
    kind: TrieKind,
    previous: Option<[u8; 32]>,
    frames: Vec<Frame>,
}

impl TrieBuilder {
    fn new(kind: TrieKind) -> Self {
        Self {
            kind,
            previous: None,
            frames: Vec::with_capacity(TRIE_DEPTH),
        }
    }

    fn add(&mut self, leaf: ChildRef) -> OracleResult<()> {
        let key = leaf.min_key;
        if let Some(previous) = self.previous {
            if previous >= key {
                return Err("oracle trie keys are not strictly ordered".to_owned());
            }
            let common = common_nibbles(&previous, &key);
            while self.frames.len() > common + 1 {
                self.finish_deepest(previous)?;
            }
            while self.frames.len() < TRIE_DEPTH {
                self.frames.push(Frame::new(self.frames.len()));
            }
        } else {
            for depth in 0..TRIE_DEPTH {
                self.frames.push(Frame::new(depth));
            }
        }
        let last = self
            .frames
            .last_mut()
            .ok_or_else(|| "oracle trie frame stack is empty".to_owned())?;
        last.insert(leaf)?;
        self.previous = Some(key);
        Ok(())
    }

    fn finish(mut self) -> OracleResult<[u8; 32]> {
        let Some(previous) = self.previous else {
            return empty_node_digest(self.kind);
        };
        while self.frames.len() > 1 {
            self.finish_deepest(previous)?;
        }
        hash_node(
            &self
                .frames
                .pop()
                .ok_or_else(|| "oracle trie root missing".to_owned())?,
            self.kind,
            true,
        )
    }

    fn finish_deepest(&mut self, previous: [u8; 32]) -> OracleResult<()> {
        let frame = self
            .frames
            .pop()
            .ok_or_else(|| "oracle trie frame underflow".to_owned())?;
        if frame.depth == 0 {
            return Err("oracle trie finalized root early".to_owned());
        }
        let child = if frame.child_count() == 1 {
            frame
                .only_child()
                .ok_or_else(|| "oracle unary trie frame lost its child".to_owned())?
        } else {
            ChildRef::node(frame.min_key()?, hash_node(&frame, self.kind, false)?)
        };
        let parent = self
            .frames
            .last_mut()
            .ok_or_else(|| "oracle trie parent missing".to_owned())?;
        if child.min_key > previous {
            return Err("oracle trie child minimum exceeds finalized key".to_owned());
        }
        parent.insert(child)?;
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum TrieKind {
    Content,
    Attribution,
}

#[derive(Clone, Copy)]
#[repr(u8)]
enum ChildKind {
    Leaf = 1,
    Node = 2,
}

#[derive(Clone, Copy)]
struct ChildRef {
    kind: ChildKind,
    min_key: [u8; 32],
    digest: [u8; 32],
}

impl ChildRef {
    const fn leaf(min_key: [u8; 32], digest: [u8; 32]) -> Self {
        Self {
            kind: ChildKind::Leaf,
            min_key,
            digest,
        }
    }

    const fn node(min_key: [u8; 32], digest: [u8; 32]) -> Self {
        Self {
            kind: ChildKind::Node,
            min_key,
            digest,
        }
    }
}

struct Frame {
    depth: usize,
    children: [Option<ChildRef>; 16],
}

impl Frame {
    fn new(depth: usize) -> Self {
        Self {
            depth,
            children: [None; 16],
        }
    }

    fn insert(&mut self, child: ChildRef) -> OracleResult<()> {
        let index = nibble(&child.min_key, self.depth);
        if self.children[index].replace(child).is_some() {
            return Err("oracle trie duplicate child edge".to_owned());
        }
        Ok(())
    }

    fn child_count(&self) -> usize {
        self.children.iter().flatten().count()
    }

    fn only_child(&self) -> Option<ChildRef> {
        let mut children = self.children.iter().flatten().copied();
        let child = children.next()?;
        if children.next().is_some() {
            None
        } else {
            Some(child)
        }
    }

    fn min_key(&self) -> OracleResult<[u8; 32]> {
        self.children
            .iter()
            .flatten()
            .map(|child| child.min_key)
            .min()
            .ok_or_else(|| "oracle trie node has no minimum key".to_owned())
    }
}

fn hash_node(frame: &Frame, kind: TrieKind, root: bool) -> OracleResult<[u8; 32]> {
    let count = frame.child_count();
    if count == 0 || (!root && count < 2) || (root && frame.depth != 0) {
        return Err("oracle compressed trie node cardinality is invalid".to_owned());
    }
    let mut bytes = Vec::with_capacity(10 + count * 66);
    bytes.extend_from_slice(match kind {
        TrieKind::Content => CONTENT_NODE_MAGIC,
        TrieKind::Attribution => ATTR_NODE_MAGIC,
    });
    bytes.push(u8::try_from(frame.depth).map_err(|_| "oracle trie depth overflow".to_owned())?);
    bytes.push(u8::try_from(count).map_err(|_| "oracle trie fanout overflow".to_owned())?);
    for (index, child) in frame.children.iter().enumerate() {
        if let Some(child) = child {
            bytes.push(u8::try_from(index).map_err(|_| "oracle trie index overflow".to_owned())?);
            bytes.push(child.kind as u8);
            bytes.extend_from_slice(&child.min_key);
            bytes.extend_from_slice(&child.digest);
        }
    }
    Ok(object_digest(&bytes))
}

fn empty_node_digest(kind: TrieKind) -> OracleResult<[u8; 32]> {
    let mut bytes = Vec::with_capacity(10);
    bytes.extend_from_slice(match kind {
        TrieKind::Content => CONTENT_NODE_MAGIC,
        TrieKind::Attribution => ATTR_NODE_MAGIC,
    });
    bytes.push(0);
    bytes.push(0);
    Ok(object_digest(&bytes))
}

fn object_digest(bytes: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(OBJECT_DOMAIN);
    digest.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(bytes);
    digest.finalize().into()
}

fn nibble(key: &[u8; 32], depth: usize) -> usize {
    if depth % 2 == 0 {
        usize::from(key[depth / 2] >> 4)
    } else {
        usize::from(key[depth / 2] & 0x0f)
    }
}

fn common_nibbles(left: &[u8; 32], right: &[u8; 32]) -> usize {
    (0..TRIE_DEPTH)
        .find(|depth| nibble(left, *depth) != nibble(right, *depth))
        .unwrap_or(TRIE_DEPTH)
}

fn push_path(output: &mut Vec<u8>, path: &[u8]) -> OracleResult<()> {
    validate_path(path, true)?;
    push_bytes(output, path)
}

fn push_bytes(output: &mut Vec<u8>, bytes: &[u8]) -> OracleResult<()> {
    output.extend_from_slice(
        &u32::try_from(bytes.len())
            .map_err(|_| "oracle byte field overflow".to_owned())?
            .to_be_bytes(),
    );
    output.extend_from_slice(bytes);
    Ok(())
}

fn update_bytes(digest: &mut Sha256, bytes: &[u8]) {
    digest.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(bytes);
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
                "partial oracle frame",
            ));
        }
        filled += count;
    }
    Ok(true)
}

fn hex(bytes: [u8; 32]) -> String {
    use std::fmt::Write as _;
    let mut output = String::with_capacity(64);
    for byte in bytes {
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

struct Decoder<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Decoder<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn u8(&mut self) -> OracleResult<u8> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> OracleResult<u32> {
        Ok(u32::from_be_bytes(self.fixed()?))
    }

    fn i64(&mut self) -> OracleResult<i64> {
        Ok(i64::from_be_bytes(self.fixed()?))
    }

    fn u64(&mut self) -> OracleResult<u64> {
        Ok(u64::from_be_bytes(self.fixed()?))
    }

    fn fixed<const N: usize>(&mut self) -> OracleResult<[u8; N]> {
        self.take(N)?
            .try_into()
            .map_err(|_| "oracle fixed field is truncated".to_owned())
    }

    fn bytes(&mut self) -> OracleResult<Vec<u8>> {
        let length =
            usize::try_from(self.u32()?).map_err(|_| "oracle field length overflow".to_owned())?;
        Ok(self.take(length)?.to_vec())
    }

    fn take(&mut self, length: usize) -> OracleResult<&'a [u8]> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or_else(|| "oracle decoder offset overflow".to_owned())?;
        let bytes = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(|| "oracle record is truncated".to_owned())?;
        self.offset = end;
        Ok(bytes)
    }

    fn finish(self) -> OracleResult<()> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err("oracle record has trailing bytes".to_owned())
        }
    }
}
