use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{AttributionInput, AttributionRootId, CanonicalRootPair, PocError, PocResult, RootId};

use super::attribution;
use super::record::{digest_key, RecordMutation, SemanticRecord, MAX_KEY_BYTES, MAX_RECORD_BYTES};
use super::spool::SortedSpool;

const CONTENT_NODE_MAGIC: &[u8; 8] = b"MPLACND1";
const ATTR_NODE_MAGIC: &[u8; 8] = b"MPLAAND1";
const CONTENT_LEAF_MAGIC: &[u8; 8] = b"MPLACLE1";
const ATTR_LEAF_MAGIC: &[u8; 8] = b"MPLAALE1";
const OBJECT_DOMAIN: &[u8] = b"mpla-poc-semantic-v1/object\0";
const MAX_OBJECT_BYTES: u64 = 320 * 1024;
const TRIE_DEPTH: usize = 64;
const FAN_OUT: usize = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TrieRoots {
    pub content: [u8; 32],
    pub attribution: [u8; 32],
}

impl TrieRoots {
    pub fn from_hex(content: &str, attribution: &str) -> PocResult<Self> {
        Ok(Self {
            content: parse_hex_digest(content)?,
            attribution: parse_hex_digest(attribution)?,
        })
    }

    pub fn content_hex(&self) -> String {
        super::hex_digest(self.content)
    }

    pub fn attribution_hex(&self) -> String {
        super::hex_digest(self.attribution)
    }

    pub fn record_stream_sha256(&self) -> String {
        let mut digest = Sha256::new();
        digest.update(b"mpla-poc-semantic-v1/record-stream\0");
        digest.update(self.content);
        super::hex_digest(digest.finalize().into())
    }

    pub fn to_root_pair(&self) -> PocResult<CanonicalRootPair> {
        Ok(CanonicalRootPair {
            root_id: RootId::from_digest_bytes(self.content),
            attribution_root_id: AttributionRootId::from_digest_bytes(self.attribution),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MutationOutcome {
    pub roots: TrieRoots,
    pub existed: bool,
}

pub struct ImmutableObjectStore {
    root: PathBuf,
    objects: PathBuf,
    objects_written: u64,
    bytes_written: u64,
    bytes_read: u64,
    object_set: Sha256,
    touched_prefixes: [bool; 256],
}

impl ImmutableObjectStore {
    pub fn new(root: &Path) -> PocResult<Self> {
        let objects = root.join("objects");
        std::fs::create_dir_all(&objects)
            .map_err(|error| PocError::io("create semantic object store", &objects, error))?;
        let mut object_set = Sha256::new();
        object_set.update(b"mpla-poc-semantic-v1/installed-object-set\0");
        Ok(Self {
            root: root.to_path_buf(),
            objects,
            objects_written: 0,
            bytes_written: 0,
            bytes_read: 0,
            object_set,
            touched_prefixes: [false; 256],
        })
    }

    pub const fn objects_written(&self) -> u64 {
        self.objects_written
    }

    pub const fn bytes_written(&self) -> u64 {
        self.bytes_written
    }

    pub const fn bytes_read(&self) -> u64 {
        self.bytes_read
    }

    pub fn object_set_sha256(&self) -> String {
        super::hex_digest(self.object_set.clone().finalize().into())
    }

    pub fn sync_directory(&self) -> PocResult<()> {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            let filesystem = File::open(&self.objects).map_err(|error| {
                PocError::io(
                    "open semantic object filesystem for sync",
                    &self.objects,
                    error,
                )
            })?;
            rustix::fs::syncfs(&filesystem).map_err(|error| {
                PocError::io(
                    "sync semantic object filesystem",
                    &self.objects,
                    std::io::Error::from(error),
                )
            })?;
        }
        for (prefix, touched) in self.touched_prefixes.iter().enumerate() {
            if *touched {
                sync_directory(&self.objects.join(format!("{prefix:02x}")))?;
            }
        }
        sync_directory(&self.objects)?;
        sync_directory(&self.root)
    }

    fn install(&mut self, bytes: &[u8]) -> PocResult<[u8; 32]> {
        if bytes.is_empty() || bytes.len() as u64 > MAX_OBJECT_BYTES {
            return Err(PocError::Integrity(
                "semantic immutable object exceeds fixed bound".to_owned(),
            ));
        }
        let digest = object_digest(bytes);
        let prefix = usize::from(digest[0]);
        let directory = self.objects.join(format!("{prefix:02x}"));
        std::fs::create_dir_all(&directory)
            .map_err(|error| PocError::io("create semantic object shard", &directory, error))?;
        let path = directory.join(super::hex_digest(digest));
        if path.exists() {
            return Ok(digest);
        }
        let temporary = directory.join(format!(
            ".{}-{}.tmp",
            super::hex_digest(digest),
            Uuid::new_v4()
        ));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| PocError::io("create semantic immutable object", &temporary, error))?;
        file.write_all(bytes)
            .map_err(|error| PocError::io("write semantic immutable object", &temporary, error))?;
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        file.sync_all()
            .map_err(|error| PocError::io("fsync semantic immutable object", &temporary, error))?;
        drop(file);
        match std::fs::hard_link(&temporary, &path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                verify_existing_object(&path, digest)?;
                std::fs::remove_file(&temporary).map_err(|remove_error| {
                    PocError::io(
                        "remove redundant semantic object temporary",
                        &temporary,
                        remove_error,
                    )
                })?;
                let _ = error;
                return Ok(digest);
            }
            Err(error) => {
                return Err(PocError::io(
                    "install semantic immutable object",
                    &path,
                    error,
                ));
            }
        }
        std::fs::remove_file(&temporary).map_err(|error| {
            PocError::io(
                "remove installed semantic object temporary",
                &temporary,
                error,
            )
        })?;
        self.touched_prefixes[prefix] = true;
        self.objects_written = self.objects_written.saturating_add(1);
        self.bytes_written = self
            .bytes_written
            .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        self.object_set.update(digest);
        Ok(digest)
    }

    fn load(&mut self, digest: [u8; 32]) -> PocResult<Vec<u8>> {
        let path = self
            .objects
            .join(format!("{:02x}", digest[0]))
            .join(super::hex_digest(digest));
        let metadata = std::fs::metadata(&path)
            .map_err(|error| PocError::io("stat semantic immutable object", &path, error))?;
        if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_OBJECT_BYTES {
            return Err(PocError::Integrity(
                "semantic immutable object is outside fixed bounds".to_owned(),
            ));
        }
        let mut file = File::open(&path)
            .map_err(|error| PocError::io("open semantic immutable object", &path, error))?;
        let mut bytes = Vec::with_capacity(
            usize::try_from(metadata.len())
                .map_err(|_| PocError::Integrity("semantic object size overflow".to_owned()))?,
        );
        file.read_to_end(&mut bytes)
            .map_err(|error| PocError::io("read semantic immutable object", &path, error))?;
        if object_digest(&bytes) != digest {
            return Err(PocError::Integrity(
                "semantic immutable object digest mismatch".to_owned(),
            ));
        }
        self.bytes_read = self
            .bytes_read
            .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        Ok(bytes)
    }
}

pub fn build_from_sorted_records(
    sorted: &SortedSpool,
    attribution_input: &AttributionInput,
    store: &mut ImmutableObjectStore,
) -> PocResult<TrieRoots> {
    let mut content_builder = StreamingTrieBuilder::new(TrieKind::Content);
    let mut attribution_builder = StreamingTrieBuilder::new(TrieKind::Attribution);
    sorted.for_each(|key, payload| {
        if key.len() != 32 {
            return Err(PocError::Integrity(
                "semantic record spool key is not SHA-256".to_owned(),
            ));
        }
        let key_digest: [u8; 32] = key
            .try_into()
            .map_err(|_| PocError::Integrity("semantic key digest length mismatch".to_owned()))?;
        let record = SemanticRecord::decode(payload)?;
        if record.key_digest()? != key_digest {
            return Err(PocError::Integrity(
                "semantic record and sorted key disagree".to_owned(),
            ));
        }
        let record_digest = record.record_digest()?;
        let content_leaf = store.install(&encode_content_leaf(
            key_digest,
            &record.canonical_key()?,
            payload,
        )?)?;
        let attribution_leaf = object_digest(&encode_attribution_leaf(
            key_digest,
            attribution::leaf_digest(record_digest, attribution_input),
        ));
        content_builder.add(ChildRef::leaf(key_digest, content_leaf), store)?;
        attribution_builder.add(ChildRef::leaf(key_digest, attribution_leaf), store)
    })?;
    Ok(TrieRoots {
        content: content_builder.finish(store)?,
        attribution: attribution_builder.finish(store)?,
    })
}

pub fn apply_mutation(
    roots: &TrieRoots,
    mutation: &RecordMutation,
    attribution_input: &AttributionInput,
    store: &mut ImmutableObjectStore,
) -> PocResult<MutationOutcome> {
    let canonical_key = mutation.canonical_key()?;
    let key_digest = digest_key(&canonical_key)?;
    let content_leaf = match mutation {
        RecordMutation::Replace(record) => Some(ChildRef::leaf(
            key_digest,
            store.install(&encode_content_leaf(
                key_digest,
                &canonical_key,
                &record.encode()?,
            )?)?,
        )),
        RecordMutation::Delete { .. } => None,
    };
    let (content, content_existed) = update_one(
        roots.content,
        key_digest,
        content_leaf,
        TrieKind::Content,
        store,
    )?;

    let attribution_leaf = match mutation {
        RecordMutation::Replace(record) => Some(ChildRef::leaf(
            key_digest,
            object_digest(&encode_attribution_leaf(
                key_digest,
                attribution::leaf_digest(record.record_digest()?, attribution_input),
            )),
        )),
        RecordMutation::Delete { .. } => None,
    };
    let (attribution, attribution_existed) = update_one(
        roots.attribution,
        key_digest,
        attribution_leaf,
        TrieKind::Attribution,
        store,
    )?;
    if content_existed != attribution_existed {
        return Err(PocError::Integrity(
            "content and attribution tries disagree about canonical key existence".to_owned(),
        ));
    }
    Ok(MutationOutcome {
        roots: TrieRoots {
            content,
            attribution,
        },
        existed: content_existed,
    })
}

pub(super) fn validate_roots(roots: &TrieRoots, store: &mut ImmutableObjectStore) -> PocResult<()> {
    validate_root(roots.content, TrieKind::Content, store)?;
    validate_root(roots.attribution, TrieKind::Attribution, store)
}

pub fn visit_records(
    roots: &TrieRoots,
    store: &mut ImmutableObjectStore,
    mut visitor: impl FnMut(SemanticRecord) -> PocResult<()>,
) -> PocResult<()> {
    let mut previous = None;
    visit_root(
        roots.content,
        TrieKind::Content,
        store,
        &mut |key_digest, bytes| {
            if previous
                .as_ref()
                .is_some_and(|value: &[u8; 32]| value >= &key_digest)
            {
                return Err(PocError::Integrity(
                    "semantic trie traversal is not strictly ordered".to_owned(),
                ));
            }
            let record = SemanticRecord::decode(bytes)?;
            if record.key_digest()? != key_digest {
                return Err(PocError::Integrity(
                    "semantic trie leaf key does not match record".to_owned(),
                ));
            }
            previous = Some(key_digest);
            visitor(record)
        },
    )
}

fn validate_root(
    root: [u8; 32],
    kind: TrieKind,
    store: &mut ImmutableObjectStore,
) -> PocResult<()> {
    if root != empty_node_digest(kind)? {
        decode_node(&store.load(root)?, kind, Some(0), true)?;
    }
    Ok(())
}

fn update_one(
    root: [u8; 32],
    key: [u8; 32],
    replacement_leaf: Option<ChildRef>,
    kind: TrieKind,
    store: &mut ImmutableObjectStore,
) -> PocResult<([u8; 32], bool)> {
    let mut frame = if root == empty_node_digest(kind)? {
        Frame::new(0)
    } else {
        decode_node(&store.load(root)?, kind, Some(0), true)?
    };
    let index = nibble(&key, 0);
    let (replacement, existed) =
        update_child(frame.children[index], key, replacement_leaf, 0, kind, store)?;
    frame.children[index] = replacement;
    if frame.children.iter().all(Option::is_none) {
        Ok((empty_node_digest(kind)?, existed))
    } else {
        Ok((store.install(&encode_node(&frame, kind, true)?)?, existed))
    }
}

fn update_child(
    existing: Option<ChildRef>,
    key: [u8; 32],
    replacement: Option<ChildRef>,
    parent_depth: usize,
    kind: TrieKind,
    store: &mut ImmutableObjectStore,
) -> PocResult<(Option<ChildRef>, bool)> {
    let Some(existing) = existing else {
        return Ok((replacement, false));
    };
    existing.validate_for_parent(parent_depth)?;
    match existing.kind {
        ChildKind::Leaf => {
            if existing.min_key == key {
                return Ok((replacement, true));
            }
            let Some(replacement) = replacement else {
                return Ok((Some(existing), false));
            };
            let depth = common_nibbles(&existing.min_key, &key);
            if depth <= parent_depth || depth >= TRIE_DEPTH {
                return Err(PocError::Integrity(
                    "semantic compressed trie leaf divergence is invalid".to_owned(),
                ));
            }
            let mut branch = Frame::new(depth);
            branch.insert(existing)?;
            branch.insert(replacement)?;
            Ok((Some(install_frame(branch, kind, store)?), false))
        }
        ChildKind::Node => {
            let mut frame = decode_node(&store.load(existing.digest)?, kind, None, false)?;
            if frame.min_key()? != existing.min_key || frame.depth <= parent_depth {
                return Err(PocError::Integrity(
                    "semantic compressed trie child summary mismatch".to_owned(),
                ));
            }
            let common = common_nibbles(&existing.min_key, &key);
            if common < frame.depth {
                let Some(replacement) = replacement else {
                    return Ok((Some(existing), false));
                };
                if common <= parent_depth {
                    return Err(PocError::Integrity(
                        "semantic compressed trie branch escaped its parent".to_owned(),
                    ));
                }
                let mut branch = Frame::new(common);
                branch.insert(existing)?;
                branch.insert(replacement)?;
                return Ok((Some(install_frame(branch, kind, store)?), false));
            }
            let index = nibble(&key, frame.depth);
            let (child, existed) = update_child(
                frame.children[index],
                key,
                replacement,
                frame.depth,
                kind,
                store,
            )?;
            frame.children[index] = child;
            let count = frame.child_count();
            if count == 0 {
                return Err(PocError::Integrity(
                    "semantic compressed trie internal node became empty".to_owned(),
                ));
            }
            if count == 1 {
                return Ok((frame.only_child(), existed));
            }
            Ok((Some(install_frame(frame, kind, store)?), existed))
        }
    }
}

fn visit_root(
    digest: [u8; 32],
    kind: TrieKind,
    store: &mut ImmutableObjectStore,
    visitor: &mut impl FnMut([u8; 32], &[u8]) -> PocResult<()>,
) -> PocResult<()> {
    if digest == empty_node_digest(kind)? {
        return Ok(());
    }
    let frame = decode_node(&store.load(digest)?, kind, Some(0), true)?;
    for child in frame.children.into_iter().flatten() {
        visit_child(child, 0, kind, store, visitor)?;
    }
    Ok(())
}

fn visit_child(
    child: ChildRef,
    parent_depth: usize,
    kind: TrieKind,
    store: &mut ImmutableObjectStore,
    visitor: &mut impl FnMut([u8; 32], &[u8]) -> PocResult<()>,
) -> PocResult<()> {
    child.validate_for_parent(parent_depth)?;
    match child.kind {
        ChildKind::Leaf => {
            if kind != TrieKind::Content {
                return Err(PocError::Integrity(
                    "attribution trie cannot materialize content records".to_owned(),
                ));
            }
            let leaf = store.load(child.digest)?;
            let decoded = decode_content_leaf(&leaf)?;
            if decoded.key_digest != child.min_key {
                return Err(PocError::Integrity(
                    "semantic trie leaf summary mismatch".to_owned(),
                ));
            }
            visitor(decoded.key_digest, decoded.record)
        }
        ChildKind::Node => {
            let frame = decode_node(&store.load(child.digest)?, kind, None, false)?;
            if frame.depth <= parent_depth || frame.min_key()? != child.min_key {
                return Err(PocError::Integrity(
                    "semantic compressed trie child node mismatch".to_owned(),
                ));
            }
            for grandchild in frame.children.into_iter().flatten() {
                visit_child(grandchild, frame.depth, kind, store, visitor)?;
            }
            Ok(())
        }
    }
}

struct StreamingTrieBuilder {
    kind: TrieKind,
    previous: Option<[u8; 32]>,
    frames: Vec<Frame>,
}

impl StreamingTrieBuilder {
    fn new(kind: TrieKind) -> Self {
        Self {
            kind,
            previous: None,
            frames: Vec::with_capacity(TRIE_DEPTH),
        }
    }

    fn add(&mut self, leaf: ChildRef, store: &mut ImmutableObjectStore) -> PocResult<()> {
        let key = leaf.min_key;
        if let Some(previous) = self.previous {
            if previous >= key {
                return Err(PocError::Integrity(
                    "semantic trie keys are not strictly ordered".to_owned(),
                ));
            }
            let common = common_nibbles(&previous, &key);
            while self.frames.len() > common + 1 {
                self.finish_deepest(previous, store)?;
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
            .ok_or_else(|| PocError::Integrity("semantic trie frame stack is empty".to_owned()))?;
        last.insert(leaf)?;
        self.previous = Some(key);
        Ok(())
    }

    fn finish(mut self, store: &mut ImmutableObjectStore) -> PocResult<[u8; 32]> {
        let Some(previous) = self.previous else {
            return empty_node_digest(self.kind);
        };
        while self.frames.len() > 1 {
            self.finish_deepest(previous, store)?;
        }
        let root = self
            .frames
            .pop()
            .ok_or_else(|| PocError::Integrity("semantic trie root frame is absent".to_owned()))?;
        store.install(&encode_node(&root, self.kind, true)?)
    }

    fn finish_deepest(
        &mut self,
        previous: [u8; 32],
        store: &mut ImmutableObjectStore,
    ) -> PocResult<()> {
        let frame = self
            .frames
            .pop()
            .ok_or_else(|| PocError::Integrity("semantic trie frame underflow".to_owned()))?;
        let depth = frame.depth;
        if depth == 0 {
            return Err(PocError::Integrity(
                "semantic trie attempted to finalize root early".to_owned(),
            ));
        }
        let child = if frame.child_count() == 1 {
            frame.only_child().ok_or_else(|| {
                PocError::Integrity("semantic trie unary frame lost its child".to_owned())
            })?
        } else {
            install_frame(frame, self.kind, store)?
        };
        let parent = self
            .frames
            .last_mut()
            .ok_or_else(|| PocError::Integrity("semantic trie parent is absent".to_owned()))?;
        if child.min_key > previous {
            return Err(PocError::Integrity(
                "semantic trie child minimum exceeds finalized key".to_owned(),
            ));
        }
        parent.insert(child)?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TrieKind {
    Content,
    Attribution,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum ChildKind {
    Leaf = 1,
    Node = 2,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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

    fn validate_for_parent(&self, parent_depth: usize) -> PocResult<()> {
        if parent_depth >= TRIE_DEPTH {
            return Err(PocError::Integrity(
                "semantic trie parent depth exceeds bound".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone)]
struct Frame {
    depth: usize,
    children: [Option<ChildRef>; FAN_OUT],
}

impl Frame {
    fn new(depth: usize) -> Self {
        Self {
            depth,
            children: [None; FAN_OUT],
        }
    }

    fn insert(&mut self, child: ChildRef) -> PocResult<()> {
        let index = nibble(&child.min_key, self.depth);
        if self.children[index].replace(child).is_some() {
            return Err(PocError::Integrity(
                "duplicate semantic trie child edge".to_owned(),
            ));
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

    fn min_key(&self) -> PocResult<[u8; 32]> {
        self.children
            .iter()
            .flatten()
            .map(|child| child.min_key)
            .min()
            .ok_or_else(|| PocError::Integrity("semantic trie node has no minimum key".to_owned()))
    }
}

struct ContentLeaf<'a> {
    key_digest: [u8; 32],
    record: &'a [u8],
}

fn install_frame(
    frame: Frame,
    kind: TrieKind,
    store: &mut ImmutableObjectStore,
) -> PocResult<ChildRef> {
    let min_key = frame.min_key()?;
    let digest = store.install(&encode_node(&frame, kind, false)?)?;
    Ok(ChildRef {
        kind: ChildKind::Node,
        min_key,
        digest,
    })
}

fn encode_node(frame: &Frame, kind: TrieKind, root: bool) -> PocResult<Vec<u8>> {
    if frame.depth >= TRIE_DEPTH {
        return Err(PocError::Integrity(
            "semantic trie node depth exceeds bound".to_owned(),
        ));
    }
    let count = frame.child_count();
    if count == 0 || (!root && count < 2) || (root && frame.depth != 0) {
        return Err(PocError::Integrity(
            "semantic compressed trie node cardinality is invalid".to_owned(),
        ));
    }
    let mut bytes = Vec::with_capacity(10 + count * 66);
    bytes.extend_from_slice(node_magic(kind));
    bytes.push(
        u8::try_from(frame.depth)
            .map_err(|_| PocError::Integrity("semantic trie depth overflow".to_owned()))?,
    );
    bytes.push(
        u8::try_from(count)
            .map_err(|_| PocError::Integrity("semantic trie fan-out overflow".to_owned()))?,
    );
    for (index, child) in frame.children.iter().enumerate() {
        if let Some(child) = child {
            bytes.push(
                u8::try_from(index)
                    .map_err(|_| PocError::Integrity("semantic trie index overflow".to_owned()))?,
            );
            bytes.push(child.kind as u8);
            bytes.extend_from_slice(&child.min_key);
            bytes.extend_from_slice(&child.digest);
        }
    }
    Ok(bytes)
}

fn decode_node(
    bytes: &[u8],
    kind: TrieKind,
    expected_depth: Option<usize>,
    root: bool,
) -> PocResult<Frame> {
    if bytes.len() < 10 || &bytes[..8] != node_magic(kind) {
        return Err(PocError::Integrity(
            "semantic trie node has wrong type or is truncated".to_owned(),
        ));
    }
    let depth = usize::from(bytes[8]);
    if expected_depth.is_some_and(|expected| depth != expected) {
        return Err(PocError::Integrity(
            "semantic trie node depth mismatch".to_owned(),
        ));
    }
    let count = usize::from(bytes[9]);
    if count == 0
        || count > FAN_OUT
        || (!root && count < 2)
        || (root && depth != 0)
        || bytes.len() != 10 + count * 66
    {
        return Err(PocError::Integrity(
            "semantic trie node fan-out is invalid".to_owned(),
        ));
    }
    let mut frame = Frame::new(depth);
    let mut offset = 10;
    let mut previous = None;
    for _ in 0..count {
        let index = usize::from(bytes[offset]);
        if index >= FAN_OUT || previous.is_some_and(|value| value >= index) {
            return Err(PocError::Integrity(
                "semantic trie node children are not canonical".to_owned(),
            ));
        }
        let child_kind = match bytes[offset + 1] {
            1 => ChildKind::Leaf,
            2 => ChildKind::Node,
            _ => {
                return Err(PocError::Integrity(
                    "semantic trie child kind is invalid".to_owned(),
                ))
            }
        };
        let min_key: [u8; 32] = bytes[offset + 2..offset + 34]
            .try_into()
            .map_err(|_| PocError::Integrity("semantic trie child key is truncated".to_owned()))?;
        let digest: [u8; 32] = bytes[offset + 34..offset + 66].try_into().map_err(|_| {
            PocError::Integrity("semantic trie child digest is truncated".to_owned())
        })?;
        if nibble(&min_key, depth) != index {
            return Err(PocError::Integrity(
                "semantic trie child edge and minimum key disagree".to_owned(),
            ));
        }
        frame.children[index] = Some(ChildRef {
            kind: child_kind,
            min_key,
            digest,
        });
        previous = Some(index);
        offset += 66;
    }
    Ok(frame)
}

fn encode_content_leaf(
    key_digest: [u8; 32],
    canonical_key: &[u8],
    record: &[u8],
) -> PocResult<Vec<u8>> {
    if canonical_key.is_empty()
        || canonical_key.len() > MAX_KEY_BYTES
        || record.is_empty()
        || record.len() > MAX_RECORD_BYTES
    {
        return Err(PocError::Integrity(
            "semantic content leaf exceeds bounds".to_owned(),
        ));
    }
    let mut bytes = Vec::with_capacity(48 + canonical_key.len() + record.len());
    bytes.extend_from_slice(CONTENT_LEAF_MAGIC);
    bytes.extend_from_slice(&key_digest);
    bytes.extend_from_slice(
        &u32::try_from(canonical_key.len())
            .map_err(|_| PocError::Integrity("semantic leaf key overflow".to_owned()))?
            .to_be_bytes(),
    );
    bytes.extend_from_slice(canonical_key);
    bytes.extend_from_slice(
        &u32::try_from(record.len())
            .map_err(|_| PocError::Integrity("semantic leaf record overflow".to_owned()))?
            .to_be_bytes(),
    );
    bytes.extend_from_slice(record);
    Ok(bytes)
}

fn decode_content_leaf(bytes: &[u8]) -> PocResult<ContentLeaf<'_>> {
    if bytes.len() < 48 || &bytes[..8] != CONTENT_LEAF_MAGIC {
        return Err(PocError::Integrity(
            "semantic content leaf has wrong type or is truncated".to_owned(),
        ));
    }
    let key_digest: [u8; 32] = bytes[8..40]
        .try_into()
        .map_err(|_| PocError::Integrity("semantic leaf key digest is truncated".to_owned()))?;
    let key_length =
        usize::try_from(u32::from_be_bytes(bytes[40..44].try_into().map_err(
            |_| PocError::Integrity("semantic leaf key length is truncated".to_owned()),
        )?))
        .map_err(|_| PocError::Integrity("semantic leaf key length overflow".to_owned()))?;
    let key_end = 44_usize
        .checked_add(key_length)
        .ok_or_else(|| PocError::Integrity("semantic leaf key offset overflow".to_owned()))?;
    let length_end = key_end
        .checked_add(4)
        .ok_or_else(|| PocError::Integrity("semantic leaf record offset overflow".to_owned()))?;
    let record_length = usize::try_from(u32::from_be_bytes(
        bytes
            .get(key_end..length_end)
            .ok_or_else(|| PocError::Integrity("semantic leaf record length missing".to_owned()))?
            .try_into()
            .map_err(|_| PocError::Integrity("semantic leaf record length invalid".to_owned()))?,
    ))
    .map_err(|_| PocError::Integrity("semantic leaf record length overflow".to_owned()))?;
    let record_end = length_end
        .checked_add(record_length)
        .ok_or_else(|| PocError::Integrity("semantic leaf record end overflow".to_owned()))?;
    if key_length == 0
        || key_length > MAX_KEY_BYTES
        || record_length == 0
        || record_length > MAX_RECORD_BYTES
        || record_end != bytes.len()
        || digest_key(&bytes[44..key_end])? != key_digest
    {
        return Err(PocError::Integrity(
            "semantic content leaf is not canonical".to_owned(),
        ));
    }
    Ok(ContentLeaf {
        key_digest,
        record: &bytes[length_end..record_end],
    })
}

fn encode_attribution_leaf(key_digest: [u8; 32], value_digest: [u8; 32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(72);
    bytes.extend_from_slice(ATTR_LEAF_MAGIC);
    bytes.extend_from_slice(&key_digest);
    bytes.extend_from_slice(&value_digest);
    bytes
}

fn empty_node_digest(kind: TrieKind) -> PocResult<[u8; 32]> {
    let mut bytes = Vec::with_capacity(10);
    bytes.extend_from_slice(node_magic(kind));
    bytes.push(0);
    bytes.push(0);
    Ok(object_digest(&bytes))
}

fn node_magic(kind: TrieKind) -> &'static [u8; 8] {
    match kind {
        TrieKind::Content => CONTENT_NODE_MAGIC,
        TrieKind::Attribution => ATTR_NODE_MAGIC,
    }
}

fn object_digest(bytes: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(OBJECT_DOMAIN);
    digest.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(bytes);
    digest.finalize().into()
}

fn nibble(key: &[u8; 32], depth: usize) -> usize {
    let byte = key[depth / 2];
    if depth % 2 == 0 {
        usize::from(byte >> 4)
    } else {
        usize::from(byte & 0x0f)
    }
}

fn common_nibbles(left: &[u8; 32], right: &[u8; 32]) -> usize {
    for depth in 0..TRIE_DEPTH {
        if nibble(left, depth) != nibble(right, depth) {
            return depth;
        }
    }
    TRIE_DEPTH
}

fn parse_hex_digest(value: &str) -> PocResult<[u8; 32]> {
    if value.len() != 64 {
        return Err(PocError::Integrity(
            "semantic digest must have 64 hexadecimal characters".to_owned(),
        ));
    }
    let mut bytes = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        bytes[index] = (hex_nibble(pair[0])? << 4) | hex_nibble(pair[1])?;
    }
    Ok(bytes)
}

fn verify_existing_object(path: &Path, expected: [u8; 32]) -> PocResult<()> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| PocError::io("stat concurrent semantic object", path, error))?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_OBJECT_BYTES {
        return Err(PocError::Integrity(
            "concurrent semantic object is not a bounded regular file".to_owned(),
        ));
    }
    let mut file = File::open(path)
        .map_err(|error| PocError::io("open concurrent semantic object", path, error))?;
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    file.read_to_end(&mut bytes)
        .map_err(|error| PocError::io("read concurrent semantic object", path, error))?;
    if object_digest(&bytes) != expected {
        return Err(PocError::Integrity(
            "immutable semantic object collision or corruption".to_owned(),
        ));
    }
    Ok(())
}

fn hex_nibble(value: u8) -> PocResult<u8> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        _ => Err(PocError::Integrity(
            "semantic digest contains non-lowercase-hex byte".to_owned(),
        )),
    }
}

fn sync_directory(path: &Path) -> PocResult<()> {
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|error| PocError::io("fsync semantic object directory", path, error))
}
