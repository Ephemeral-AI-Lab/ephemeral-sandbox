#![allow(dead_code)]

use std::cell::Cell;
use std::cmp::Ordering;
use std::mem::size_of;
use std::ops::Deref;

use sandbox_runtime_layerstack_core::{
    encode_digest_preimage_header, encode_root_record, encode_tree_record, root_id,
    stage_tree_candidate, tree_entry_record_len, validate_tree_candidate, CanonicalPath,
    CanonicalSink, CanonicalSource, ChunkProfileId, Digest32, DigestDomain, Error, ErrorKind,
    FieldClass, HardlinkGroupId, NodeMetadata, ObjectId, ObjectKind, PublicationId,
    PublicationIdentity, RootId, RootRecordV2, SparseExtent, TreeEntry, TypedDigest, ValidatedTree,
    Xattr, ROOT_FORMAT_V2,
};

#[derive(Default)]
pub struct VecSink {
    pub bytes: Vec<u8>,
}

impl CanonicalSink for VecSink {
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }
}

pub struct FragmentingSink {
    pub bytes: Vec<u8>,
    fragment: usize,
}

impl FragmentingSink {
    pub fn new(fragment: usize) -> Self {
        Self {
            bytes: Vec::new(),
            fragment: fragment.max(1),
        }
    }
}

impl CanonicalSink for FragmentingSink {
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), Error> {
        for chunk in bytes.chunks(self.fragment) {
            self.bytes.extend_from_slice(chunk);
        }
        Ok(())
    }
}

pub struct FailingSink {
    remaining_bytes: usize,
}

impl FailingSink {
    pub const fn new(remaining_bytes: usize) -> Self {
        Self { remaining_bytes }
    }
}

impl CanonicalSink for FailingSink {
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), Error> {
        if bytes.len() > self.remaining_bytes {
            return Err(Error::new(
                ErrorKind::SinkFailure,
                ROOT_FORMAT_V2,
                FieldClass::Sink,
                u32::try_from(self.remaining_bytes).unwrap_or(u32::MAX),
            ));
        }
        self.remaining_bytes -= bytes.len();
        Ok(())
    }
}

pub struct BytesSource<'a> {
    bytes: &'a [u8],
    position: usize,
    fragment: usize,
    peak_read_bytes: usize,
}

impl<'a> BytesSource<'a> {
    pub const fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            position: 0,
            fragment: usize::MAX,
            peak_read_bytes: 0,
        }
    }

    pub fn fragmented(bytes: &'a [u8], fragment: usize) -> Self {
        Self {
            bytes,
            position: 0,
            fragment: fragment.max(1),
            peak_read_bytes: 0,
        }
    }

    pub const fn peak_read_bytes(&self) -> usize {
        self.peak_read_bytes
    }
}

impl CanonicalSource for BytesSource<'_> {
    fn read_exact(&mut self, output: &mut [u8]) -> Result<(), Error> {
        self.peak_read_bytes = self.peak_read_bytes.max(output.len());
        let end = self.position.checked_add(output.len()).ok_or_else(|| {
            Error::new(ErrorKind::Overflow, ROOT_FORMAT_V2, FieldClass::Source, 0)
        })?;
        let Some(input) = self.bytes.get(self.position..end) else {
            return Err(Error::new(
                ErrorKind::UnexpectedEnd,
                ROOT_FORMAT_V2,
                FieldClass::Source,
                u32::try_from(self.position).unwrap_or(u32::MAX),
            ));
        };
        let mut copied = 0;
        for chunk in input.chunks(self.fragment) {
            let next = copied + chunk.len();
            output[copied..next].copy_from_slice(chunk);
            copied = next;
        }
        self.position = end;
        Ok(())
    }

    fn ensure_exhausted(&mut self) -> Result<(), Error> {
        if self.position == self.bytes.len() {
            Ok(())
        } else {
            Err(Error::new(
                ErrorKind::TrailingBytes,
                ROOT_FORMAT_V2,
                FieldClass::Source,
                u32::try_from(self.position).unwrap_or(u32::MAX),
            ))
        }
    }
}

pub struct FailingSource<'a> {
    bytes: &'a [u8],
    position: usize,
    fail_at: usize,
}

impl<'a> FailingSource<'a> {
    pub const fn new(bytes: &'a [u8], fail_at: usize) -> Self {
        Self {
            bytes,
            position: 0,
            fail_at,
        }
    }
}

impl CanonicalSource for FailingSource<'_> {
    fn read_exact(&mut self, output: &mut [u8]) -> Result<(), Error> {
        let end = self.position.checked_add(output.len()).ok_or_else(|| {
            Error::new(ErrorKind::Overflow, ROOT_FORMAT_V2, FieldClass::Source, 0)
        })?;
        if end > self.fail_at {
            return Err(Error::new(
                ErrorKind::SourceFailure,
                ROOT_FORMAT_V2,
                FieldClass::Source,
                u32::try_from(self.position).unwrap_or(u32::MAX),
            ));
        }
        let Some(input) = self.bytes.get(self.position..end) else {
            return Err(Error::new(
                ErrorKind::UnexpectedEnd,
                ROOT_FORMAT_V2,
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
            Err(Error::new(
                ErrorKind::TrailingBytes,
                ROOT_FORMAT_V2,
                FieldClass::Source,
                u32::try_from(self.position).unwrap_or(u32::MAX),
            ))
        }
    }
}

#[derive(Default)]
pub struct RecordSink {
    pub records: Vec<Vec<u8>>,
}

impl CanonicalSink for RecordSink {
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.records.push(bytes.to_vec());
        Ok(())
    }
}

#[derive(Default)]
pub struct LogicalOwnerCounter {
    live: Cell<u64>,
    peak: Cell<u64>,
}

impl LogicalOwnerCounter {
    pub fn enter(&self) -> LogicalOwnerGuard<'_> {
        self.acquire();
        LogicalOwnerGuard { counter: self }
    }

    pub fn track<T>(&self, value: T) -> TrackedOwner<'_, T> {
        self.acquire();
        TrackedOwner {
            counter: self,
            value,
        }
    }

    pub fn live(&self) -> u64 {
        self.live.get()
    }

    pub fn peak(&self) -> u64 {
        self.peak.get()
    }

    fn acquire(&self) {
        let current = self.live.get();
        assert_ne!(current, u64::MAX, "logical owner counter overflow");
        let next = current + 1;
        self.live.set(next);
        self.peak.set(self.peak.get().max(next));
    }

    fn release(&self) {
        let current = self.live.get();
        assert!(current > 0, "logical owner counter underflow");
        self.live.set(current - 1);
    }
}

pub struct LogicalOwnerGuard<'a> {
    counter: &'a LogicalOwnerCounter,
}

impl Drop for LogicalOwnerGuard<'_> {
    fn drop(&mut self) {
        self.counter.release();
    }
}

pub struct TrackedOwner<'a, T> {
    counter: &'a LogicalOwnerCounter,
    value: T,
}

impl<T> Deref for TrackedOwner<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl<T> Drop for TrackedOwner<'_, T> {
    fn drop(&mut self) {
        self.counter.release();
    }
}

#[derive(Default)]
pub struct CaptureDigest {
    pub preimage: Vec<u8>,
    pub invocations: u64,
}

impl TypedDigest for CaptureDigest {
    fn digest(
        &mut self,
        domain: DigestDomain,
        version: sandbox_runtime_layerstack_core::FormatVersion,
        payload_len: u64,
        encode_payload: &mut dyn FnMut(&mut dyn CanonicalSink) -> Result<(), Error>,
    ) -> Result<Digest32, Error> {
        self.invocations = self.invocations.checked_add(1).ok_or_else(|| {
            Error::new(ErrorKind::Overflow, ROOT_FORMAT_V2, FieldClass::Digest, 0)
        })?;
        let mut sink = VecSink::default();
        encode_digest_preimage_header(domain, version, payload_len, &mut sink)?;
        let header_len = sink.bytes.len();
        encode_payload(&mut sink)?;
        let actual_payload = sink.bytes.len().checked_sub(header_len).ok_or_else(|| {
            Error::new(ErrorKind::Overflow, ROOT_FORMAT_V2, FieldClass::Digest, 0)
        })?;
        if u64::try_from(actual_payload).ok() != Some(payload_len) {
            return Err(Error::new(
                ErrorKind::DigestFailure,
                ROOT_FORMAT_V2,
                FieldClass::Digest,
                u32::try_from(actual_payload).unwrap_or(u32::MAX),
            ));
        }
        self.preimage = sink.bytes;
        Ok(folded_digest(&self.preimage))
    }
}

pub struct FailingDigest;

impl TypedDigest for FailingDigest {
    fn digest(
        &mut self,
        _domain: DigestDomain,
        _version: sandbox_runtime_layerstack_core::FormatVersion,
        _payload_len: u64,
        _encode_payload: &mut dyn FnMut(&mut dyn CanonicalSink) -> Result<(), Error>,
    ) -> Result<Digest32, Error> {
        Err(Error::new(
            ErrorKind::DigestFailure,
            ROOT_FORMAT_V2,
            FieldClass::Digest,
            0,
        ))
    }
}

pub struct SkippingDigest;

impl TypedDigest for SkippingDigest {
    fn digest(
        &mut self,
        _domain: DigestDomain,
        _version: sandbox_runtime_layerstack_core::FormatVersion,
        _payload_len: u64,
        _encode_payload: &mut dyn FnMut(&mut dyn CanonicalSink) -> Result<(), Error>,
    ) -> Result<Digest32, Error> {
        Ok(Digest32::new([0; 32]))
    }
}

pub struct RepeatingDigest;

impl TypedDigest for RepeatingDigest {
    fn digest(
        &mut self,
        _domain: DigestDomain,
        _version: sandbox_runtime_layerstack_core::FormatVersion,
        _payload_len: u64,
        encode_payload: &mut dyn FnMut(&mut dyn CanonicalSink) -> Result<(), Error>,
    ) -> Result<Digest32, Error> {
        let mut sink = VecSink::default();
        encode_payload(&mut sink)?;
        encode_payload(&mut sink)?;
        Ok(folded_digest(&sink.bytes))
    }
}

fn folded_digest(bytes: &[u8]) -> Digest32 {
    let mut output = [0_u8; 32];
    let seeds = [
        0xcbf2_9ce4_8422_2325_u64,
        0x8422_2325_cbf2_9ce4_u64,
        0x9e37_79b9_7f4a_7c15_u64,
        0xd6e8_feb8_6659_fd93_u64,
    ];
    for (lane, seed) in seeds.into_iter().enumerate() {
        let mut value = seed;
        for byte in bytes {
            value ^= u64::from(*byte);
            value = value.wrapping_mul(0x0000_0100_0000_01b3);
            value ^= value.rotate_left(u32::try_from(lane + 1).unwrap_or(1));
        }
        let start = lane * 8;
        output[start..start + 8].copy_from_slice(&value.to_be_bytes());
    }
    Digest32::new(output)
}

pub fn metadata(mode: u32, xattrs: Vec<Xattr>) -> Result<NodeMetadata, Error> {
    NodeMetadata::new(mode, 1_000, 1_001, -7, 42, xattrs)
}

pub fn file_segments(byte: u8) -> ObjectId {
    ObjectId::new(ObjectKind::FileSegments, Digest32::new([byte; 32]))
}

pub fn simple_regular(path: &[u8], object_byte: u8) -> Result<TreeEntry, Error> {
    TreeEntry::regular(
        CanonicalPath::from_bytes(path)?,
        metadata(0o644, Vec::new())?,
        8,
        Vec::new(),
        file_segments(object_byte),
        None,
    )
}

pub fn complete_sample_tree() -> Result<Vec<TreeEntry>, Error> {
    let shared_metadata = metadata(0o640, Vec::new())?;
    Ok(vec![
        TreeEntry::directory(
            CanonicalPath::from_bytes(b"bin")?,
            metadata(0o755, vec![Xattr::new(b"user.note".to_vec(), vec![0xff])?])?,
        )?,
        TreeEntry::regular(
            CanonicalPath::from_bytes(b"bin/a")?,
            shared_metadata.clone(),
            10,
            vec![SparseExtent::new(4, 2)?],
            file_segments(0x11),
            Some(HardlinkGroupId::new(1)?),
        )?,
        TreeEntry::regular(
            CanonicalPath::from_bytes(b"bin/b")?,
            shared_metadata,
            10,
            vec![SparseExtent::new(4, 2)?],
            file_segments(0x11),
            Some(HardlinkGroupId::new(1)?),
        )?,
        TreeEntry::symlink(
            CanonicalPath::from_bytes(b"link")?,
            metadata(0o777, Vec::new())?,
            b"bin/a".to_vec(),
        )?,
        TreeEntry::device(
            CanonicalPath::from_bytes(b"null")?,
            metadata(0o600, Vec::new())?,
            1,
            3,
        )?,
        TreeEntry::fifo(
            CanonicalPath::from_bytes(b"pipe")?,
            metadata(0o600, Vec::new())?,
        )?,
    ])
}

pub fn encode_tree(entries: &[TreeEntry]) -> Result<Vec<u8>, Error> {
    let entries_bytes = entries.iter().try_fold(0_u64, |total, entry| {
        total
            .checked_add(u64::from(tree_entry_record_len(entry)?))
            .ok_or_else(|| Error::new(ErrorKind::Overflow, ROOT_FORMAT_V2, FieldClass::Tree, 0))
    })?;
    let mut sink = VecSink::default();
    let mut iterator = entries.iter();
    encode_tree_record(
        u64::try_from(entries.len()).map_err(|_| {
            Error::new(
                ErrorKind::LimitExceeded,
                ROOT_FORMAT_V2,
                FieldClass::Tree,
                0,
            )
        })?,
        entries_bytes,
        &mut iterator,
        &mut sink,
    )?;
    Ok(sink.bytes)
}

fn hardlink_path(record: &[u8]) -> &[u8] {
    let Some(length_bytes) = record.get(20..24) else {
        return &[];
    };
    let Ok(length_bytes) = <[u8; 4]>::try_from(length_bytes) else {
        return &[];
    };
    let Ok(length) = usize::try_from(u32::from_be_bytes(length_bytes)) else {
        return &[];
    };
    let Some(end) = 24_usize.checked_add(length) else {
        return &[];
    };
    record.get(24..end).unwrap_or(&[])
}

fn hardlink_order(left: &[u8], right: &[u8]) -> Ordering {
    let left_group = left.get(..16).unwrap_or(&[]);
    let right_group = right.get(..16).unwrap_or(&[]);
    left_group
        .cmp(right_group)
        .then_with(|| hardlink_path(left).cmp(hardlink_path(right)))
}

fn flatten(records: &[Vec<u8>]) -> Vec<u8> {
    let capacity = records
        .iter()
        .map(Vec::len)
        .fold(0_usize, usize::saturating_add);
    let mut output = Vec::with_capacity(capacity);
    for record in records {
        output.extend_from_slice(record);
    }
    output
}

fn allocated_record_bytes(records: &[Vec<u8>], outer_capacity: usize) -> usize {
    outer_capacity
        .saturating_mul(size_of::<Vec<u8>>())
        .saturating_add(
            records
                .iter()
                .map(Vec::capacity)
                .fold(0_usize, usize::saturating_add),
        )
}

fn observe_scratch(peak: &mut usize, allocations: &[usize]) {
    let current = allocations
        .iter()
        .copied()
        .fold(0_usize, usize::saturating_add);
    *peak = (*peak).max(current);
}

fn maximum_entry_record_len(tree_bytes: &[u8]) -> Result<usize, Error> {
    const TREE_HEADER_BYTES: usize = 35;
    const ENTRY_HEADER_BYTES: usize = 15;
    const ENTRY_PAYLOAD_LEN_OFFSET: usize = 11;

    let count_bytes = tree_bytes.get(19..27).ok_or_else(|| {
        Error::new(
            ErrorKind::UnexpectedEnd,
            ROOT_FORMAT_V2,
            FieldClass::Tree,
            0,
        )
    })?;
    let count = u64::from_be_bytes(
        <[u8; 8]>::try_from(count_bytes)
            .map_err(|_| Error::new(ErrorKind::Malformed, ROOT_FORMAT_V2, FieldClass::Tree, 0))?,
    );
    let mut position = TREE_HEADER_BYTES;
    let mut maximum = 0_usize;
    for ordinal in 0..count {
        let payload_len_start =
            position
                .checked_add(ENTRY_PAYLOAD_LEN_OFFSET)
                .ok_or_else(|| {
                    Error::new(
                        ErrorKind::Overflow,
                        ROOT_FORMAT_V2,
                        FieldClass::Tree,
                        u32::try_from(ordinal).unwrap_or(u32::MAX),
                    )
                })?;
        let payload_len_end = payload_len_start.checked_add(4).ok_or_else(|| {
            Error::new(
                ErrorKind::Overflow,
                ROOT_FORMAT_V2,
                FieldClass::Tree,
                u32::try_from(ordinal).unwrap_or(u32::MAX),
            )
        })?;
        let payload_len_bytes = tree_bytes
            .get(payload_len_start..payload_len_end)
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::UnexpectedEnd,
                    ROOT_FORMAT_V2,
                    FieldClass::Tree,
                    u32::try_from(ordinal).unwrap_or(u32::MAX),
                )
            })?;
        let payload_len = usize::try_from(u32::from_be_bytes(
            <[u8; 4]>::try_from(payload_len_bytes).map_err(|_| {
                Error::new(
                    ErrorKind::Malformed,
                    ROOT_FORMAT_V2,
                    FieldClass::Tree,
                    u32::try_from(ordinal).unwrap_or(u32::MAX),
                )
            })?,
        ))
        .map_err(|_| {
            Error::new(
                ErrorKind::LimitExceeded,
                ROOT_FORMAT_V2,
                FieldClass::Tree,
                u32::try_from(ordinal).unwrap_or(u32::MAX),
            )
        })?;
        let record_len = ENTRY_HEADER_BYTES.checked_add(payload_len).ok_or_else(|| {
            Error::new(
                ErrorKind::Overflow,
                ROOT_FORMAT_V2,
                FieldClass::Tree,
                u32::try_from(ordinal).unwrap_or(u32::MAX),
            )
        })?;
        position = position.checked_add(record_len).ok_or_else(|| {
            Error::new(
                ErrorKind::Overflow,
                ROOT_FORMAT_V2,
                FieldClass::Tree,
                u32::try_from(ordinal).unwrap_or(u32::MAX),
            )
        })?;
        if position > tree_bytes.len() {
            return Err(Error::new(
                ErrorKind::UnexpectedEnd,
                ROOT_FORMAT_V2,
                FieldClass::Tree,
                u32::try_from(ordinal).unwrap_or(u32::MAX),
            ));
        }
        maximum = maximum.max(record_len);
    }
    if position != tree_bytes.len() {
        return Err(Error::new(
            ErrorKind::TrailingBytes,
            ROOT_FORMAT_V2,
            FieldClass::Tree,
            u32::try_from(position).unwrap_or(u32::MAX),
        ));
    }
    Ok(maximum)
}

pub fn validate_tree_bytes_with_peak(
    tree_bytes: &[u8],
    digest: &mut dyn TypedDigest,
) -> Result<(ValidatedTree, u64), Error> {
    // The decoder owns at most one entry plus the previous path. Twice the
    // largest canonical entry record is a conservative bound for those
    // simultaneous value allocations without retaining a decoded tree.
    let entry_decode_bound = maximum_entry_record_len(tree_bytes)?.saturating_mul(2);
    let mut tree_source = BytesSource::new(tree_bytes);
    let mut hardlinks = RecordSink::default();
    let mut references = RecordSink::default();
    let pending = stage_tree_candidate(&mut tree_source, &mut hardlinks, &mut references, digest)?;
    let mut peak_scratch_bytes = tree_source
        .peak_read_bytes()
        .saturating_add(entry_decode_bound);
    observe_scratch(
        &mut peak_scratch_bytes,
        &[
            allocated_record_bytes(&hardlinks.records, hardlinks.records.capacity()),
            allocated_record_bytes(&references.records, references.records.capacity()),
            entry_decode_bound,
        ],
    );

    hardlinks
        .records
        .sort_unstable_by(|left, right| hardlink_order(left, right));
    references.records.sort_unstable();
    let hardlink_bytes = flatten(&hardlinks.records);
    let reference_bytes = flatten(&references.records);
    observe_scratch(
        &mut peak_scratch_bytes,
        &[
            allocated_record_bytes(&hardlinks.records, hardlinks.records.capacity()),
            allocated_record_bytes(&references.records, references.records.capacity()),
            hardlink_bytes.capacity(),
            reference_bytes.capacity(),
        ],
    );

    let mut known_records = references.records;
    known_records.dedup();
    let known_payload_len = known_records
        .iter()
        .map(Vec::len)
        .fold(0_usize, usize::saturating_add);
    let mut known_bytes = Vec::with_capacity(8_usize.saturating_add(known_payload_len));
    known_bytes.extend_from_slice(
        &u64::try_from(known_records.len())
            .map_err(|_| {
                Error::new(
                    ErrorKind::LimitExceeded,
                    ROOT_FORMAT_V2,
                    FieldClass::ObjectReference,
                    0,
                )
            })?
            .to_be_bytes(),
    );
    for record in &known_records {
        known_bytes.extend_from_slice(record);
    }
    let largest_hardlink_claim = hardlinks.records.iter().map(Vec::len).max().unwrap_or(0);
    observe_scratch(
        &mut peak_scratch_bytes,
        &[
            allocated_record_bytes(&hardlinks.records, hardlinks.records.capacity()),
            allocated_record_bytes(&known_records, known_records.capacity()),
            hardlink_bytes.capacity(),
            reference_bytes.capacity(),
            known_bytes.capacity(),
            largest_hardlink_claim.saturating_mul(2),
        ],
    );

    let mut hardlink_source = BytesSource::new(&hardlink_bytes);
    let mut reference_source = BytesSource::new(&reference_bytes);
    let mut known_source = BytesSource::new(&known_bytes);
    let tree = validate_tree_candidate(
        pending,
        &mut hardlink_source,
        &mut reference_source,
        &mut known_source,
    )?;
    let peak_read_bytes = hardlink_source
        .peak_read_bytes()
        .max(reference_source.peak_read_bytes())
        .max(known_source.peak_read_bytes());
    peak_scratch_bytes = peak_scratch_bytes.max(peak_read_bytes);
    Ok((tree, u64::try_from(peak_scratch_bytes).unwrap_or(u64::MAX)))
}

pub fn validate_tree_bytes(
    tree_bytes: &[u8],
    digest: &mut dyn TypedDigest,
) -> Result<ValidatedTree, Error> {
    validate_tree_bytes_with_peak(tree_bytes, digest).map(|(tree, _peak_read_bytes)| tree)
}

pub fn validated_tree(
    entries: &[TreeEntry],
    digest: &mut dyn TypedDigest,
) -> Result<(Vec<u8>, ValidatedTree), Error> {
    let bytes = encode_tree(entries)?;
    let tree = validate_tree_bytes(&bytes, digest)?;
    Ok((bytes, tree))
}

pub fn sample_root(
    tree: &ValidatedTree,
    generation: u64,
    publication_byte: u8,
    parent: Option<RootId>,
    base: Option<RootId>,
) -> Result<RootRecordV2, Error> {
    let publication_id = PublicationId::new([publication_byte; 16])?;
    Ok(RootRecordV2::new(
        tree,
        ChunkProfileId::SEQ_CDC_V1,
        parent,
        base,
        PublicationIdentity::new(generation, publication_id),
    ))
}

pub fn encode_root(record: &RootRecordV2) -> Result<Vec<u8>, Error> {
    let mut sink = VecSink::default();
    encode_root_record(record, &mut sink)?;
    Ok(sink.bytes)
}

pub fn identify_root(record: &RootRecordV2, digest: &mut dyn TypedDigest) -> Result<RootId, Error> {
    root_id(record, digest)
}
