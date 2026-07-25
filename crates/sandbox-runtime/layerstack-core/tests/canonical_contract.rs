mod common;

use sandbox_runtime_layerstack_core::{
    decode_object_record, decode_root_record, decode_tree_record, decode_v3_record,
    encode_object_record, encode_root_record, encode_tree_record, encode_v3_record, root_id,
    stage_tree_candidate, tree_entry_record_len, tree_page_id, v3_record_id,
    validate_tree_candidate, CanonicalPath, CanonicalRecordV3, Capability, Digest32, Error,
    ErrorKind, HardlinkGroupId, NodeMetadata, ObjectKind, ObjectRecord, RawDigest, SparseExtent,
    TreeEntry, Xattr, ROOT_FORMAT_V2,
};

use common::{
    encode_root, encode_tree, file_segments, metadata, sample_root, simple_regular,
    validate_tree_bytes, BytesSource, CaptureDigest, FailingDigest, FailingSink, FailingSource,
    FragmentingSink, RecordSink, RepeatingDigest, SkippingDigest, VecSink,
};

const MAGIC: &[u8; 8] = b"EOS-LS2\0";
const CONTRACT_V3: &str = include_str!("../../layerstack/tests/fixtures/cas/v3/contract-v3.tsv");

struct DeclaredChecksum(Digest32);

impl RawDigest for DeclaredChecksum {
    fn digest_bytes(&mut self, _bytes: &[u8]) -> Result<Digest32, Error> {
        Ok(self.0)
    }
}

fn decode_hex(value: &str) -> Vec<u8> {
    assert_eq!(value.len() % 2, 0);
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = (pair[0] as char).to_digit(16).expect("hex high nibble");
            let low = (pair[1] as char).to_digit(16).expect("hex low nibble");
            u8::try_from((high << 4) | low).expect("hex byte")
        })
        .collect()
}

fn v3_golden_rows() -> impl Iterator<Item = (&'static str, u8, usize, &'static str, Vec<u8>)> {
    CONTRACT_V3
        .lines()
        .filter(|line| !line.starts_with('#') && !line.is_empty())
        .map(|line| {
            let mut columns = line.split('\t');
            let name = columns.next().expect("name");
            let kind = u8::from_str_radix(columns.next().expect("kind"), 16).expect("kind hex");
            let encoded_length = columns
                .next()
                .expect("encoded length")
                .parse::<usize>()
                .expect("encoded length integer");
            let sha256 = columns.next().expect("sha256");
            let bytes = decode_hex(columns.next().expect("record hex"));
            assert!(columns.next().is_none());
            (name, kind, encoded_length, sha256, bytes)
        })
}

fn push_header(output: &mut Vec<u8>, kind: u8, payload_len: u64, tree: bool) {
    output.extend_from_slice(MAGIC);
    output.push(kind);
    output.extend_from_slice(&ROOT_FORMAT_V2.get().to_be_bytes());
    if tree {
        output.extend_from_slice(&payload_len.to_be_bytes());
    } else {
        output.extend_from_slice(&u32::try_from(payload_len).unwrap_or(u32::MAX).to_be_bytes());
    }
}

fn push_tlv(output: &mut Vec<u8>, tag: u8, value: &[u8]) {
    output.push(tag);
    output.extend_from_slice(&u32::try_from(value.len()).unwrap_or(u32::MAX).to_be_bytes());
    output.extend_from_slice(value);
}

fn push_option(output: &mut Vec<u8>, tag: u8, value: Option<&[u8]>) {
    output.push(tag);
    let value_len = value.map_or(0, |bytes| bytes.len());
    output.extend_from_slice(
        &u32::try_from(value_len + 1)
            .unwrap_or(u32::MAX)
            .to_be_bytes(),
    );
    output.push(u8::from(value.is_some()));
    if let Some(bytes) = value {
        output.extend_from_slice(bytes);
    }
}

fn expected_metadata() -> Vec<u8> {
    let mut payload = Vec::new();
    push_tlv(&mut payload, 1, &0o644_u32.to_be_bytes());
    push_tlv(&mut payload, 2, &1_000_u32.to_be_bytes());
    push_tlv(&mut payload, 3, &1_001_u32.to_be_bytes());
    push_tlv(&mut payload, 4, &(-7_i64).to_be_bytes());
    push_tlv(&mut payload, 5, &42_u32.to_be_bytes());
    push_tlv(&mut payload, 6, &0_u32.to_be_bytes());
    let mut record = Vec::new();
    push_header(
        &mut record,
        0x13,
        u64::try_from(payload.len()).unwrap_or(u64::MAX),
        false,
    );
    record.extend_from_slice(&payload);
    record
}

fn expected_reference(object_byte: u8) -> Vec<u8> {
    let mut payload = Vec::new();
    push_tlv(&mut payload, 1, &[ObjectKind::FileSegments as u8]);
    push_tlv(&mut payload, 2, &[object_byte; 32]);
    let mut record = Vec::new();
    push_header(
        &mut record,
        0x14,
        u64::try_from(payload.len()).unwrap_or(u64::MAX),
        false,
    );
    record.extend_from_slice(&payload);
    record
}

fn expected_single_regular_tree(object_byte: u8) -> Vec<u8> {
    let metadata = expected_metadata();
    let reference = expected_reference(object_byte);
    let mut entry_payload = Vec::new();
    push_tlv(&mut entry_payload, 1, &[2]);
    push_tlv(&mut entry_payload, 2, b"a");
    push_tlv(&mut entry_payload, 3, &metadata);
    push_option(&mut entry_payload, 4, None);
    push_option(&mut entry_payload, 5, None);
    push_option(&mut entry_payload, 6, None);
    push_option(&mut entry_payload, 7, Some(&8_u64.to_be_bytes()));
    push_option(&mut entry_payload, 8, Some(&0_u32.to_be_bytes()));
    push_option(&mut entry_payload, 9, None);
    push_option(&mut entry_payload, 10, Some(&reference));

    let mut entry = Vec::new();
    push_header(
        &mut entry,
        0x12,
        u64::try_from(entry_payload.len()).unwrap_or(u64::MAX),
        false,
    );
    entry.extend_from_slice(&entry_payload);

    let mut tree_payload = Vec::new();
    tree_payload.extend_from_slice(&1_u64.to_be_bytes());
    tree_payload.extend_from_slice(&u64::try_from(entry.len()).unwrap_or(u64::MAX).to_be_bytes());
    tree_payload.extend_from_slice(&entry);
    let mut tree = Vec::new();
    push_header(
        &mut tree,
        0x11,
        u64::try_from(tree_payload.len()).unwrap_or(u64::MAX),
        true,
    );
    tree.extend_from_slice(&tree_payload);
    tree
}

fn hostile_tree_with_entry_prefix(entry_payload_len: u32, entry_payload_prefix: &[u8]) -> Vec<u8> {
    let entry_record_len = u64::from(entry_payload_len) + 15;
    let mut tree = Vec::new();
    push_header(&mut tree, 0x11, 16 + entry_record_len, true);
    tree.extend_from_slice(&1_u64.to_be_bytes());
    tree.extend_from_slice(&entry_record_len.to_be_bytes());
    push_header(&mut tree, 0x12, u64::from(entry_payload_len), false);
    tree.extend_from_slice(entry_payload_prefix);
    tree
}

fn validate_one_reference_with_known(tree: &[u8], known: &[u8]) -> Result<(), Error> {
    let mut tree_source = BytesSource::new(tree);
    let mut hardlinks = RecordSink::default();
    let mut references = RecordSink::default();
    let pending = stage_tree_candidate(
        &mut tree_source,
        &mut hardlinks,
        &mut references,
        &mut CaptureDigest::default(),
    )?;
    references.records.sort();
    let hardlink_bytes: Vec<u8> = hardlinks.records.into_iter().flatten().collect();
    let reference_bytes: Vec<u8> = references.records.into_iter().flatten().collect();
    let mut hardlink_source = BytesSource::new(&hardlink_bytes);
    let mut reference_source = BytesSource::new(&reference_bytes);
    let mut known_source = BytesSource::new(known);
    validate_tree_candidate(
        pending,
        &mut hardlink_source,
        &mut reference_source,
        &mut known_source,
    )?;
    Ok(())
}

#[test]
fn canonical_v3_owner_goldens_round_trip() -> Result<(), Error> {
    let mut count = 0_usize;
    for (name, kind, encoded_length, sha256, bytes) in v3_golden_rows() {
        assert_eq!(bytes.len(), encoded_length, "{name}");
        assert_eq!(sha256.len(), 64, "{name}");
        assert_eq!(&bytes[..8], MAGIC, "{name}");
        assert_eq!(bytes[8], kind, "{name}");
        assert_eq!(&bytes[9..11], &3_u16.to_be_bytes(), "{name}");

        let checksum = if matches!(kind, 0x30..=0x33) {
            let mut value = [0_u8; 32];
            value.copy_from_slice(&bytes[bytes.len() - 32..]);
            Digest32::new(value)
        } else {
            Digest32::default()
        };
        let mut source = BytesSource::fragmented(&bytes, 1);
        let mut raw_digest = DeclaredChecksum(checksum);
        let record = decode_v3_record(&mut source, &mut raw_digest)?;
        assert_eq!(record.kind() as u8, kind, "{name}");

        let mut encoded = VecSink::default();
        encode_v3_record(&record, &mut encoded)?;
        assert_eq!(encoded.bytes, bytes, "{name}");

        if !matches!(kind, 0x13 | 0x30..=0x33) {
            let mut digest = CaptureDigest::default();
            let _ = v3_record_id(&record, &mut digest)?;
            assert_eq!(digest.preimage, bytes, "{name}");
            assert_eq!(digest.invocations, 1, "{name}");
        }
        count += 1;
    }
    assert_eq!(count, 16);
    Ok(())
}

#[test]
fn canonical_v3_nominal_domains_and_error_codes_are_exact() -> Result<(), Error> {
    let chunk = CanonicalRecordV3::chunk(vec![1])?;
    let error = tree_page_id(&chunk, &mut CaptureDigest::default()).expect_err("wrong domain");
    assert_eq!(error.kind(), ErrorKind::WrongDomain);
    assert_eq!(error.kind().stage03_code(), Some(3));

    let expected = [
        (ErrorKind::WrongKind, 1),
        (ErrorKind::UnsupportedVersion, 2),
        (ErrorKind::TrailingBytes, 4),
        (ErrorKind::ObjectCollisionOrCorruption, 18),
        (ErrorKind::RequestDeadline, 32),
    ];
    for (kind, code) in expected {
        assert_eq!(kind.stage03_code(), Some(code));
    }
    assert_eq!(ErrorKind::Malformed.stage03_code(), None);
    Ok(())
}

#[test]
fn prc_r01_exact_records_and_round_trip() -> Result<(), Error> {
    let empty = encode_tree(&[])?;
    let mut expected_empty = Vec::new();
    push_header(&mut expected_empty, 0x11, 16, true);
    expected_empty.extend_from_slice(&0_u64.to_be_bytes());
    expected_empty.extend_from_slice(&0_u64.to_be_bytes());
    assert_eq!(empty, expected_empty);
    assert_eq!(empty.len(), 35);

    let entry = simple_regular(b"a", 0x22)?;
    let tree = encode_tree(std::slice::from_ref(&entry))?;
    assert_eq!(tree, expected_single_regular_tree(0x22));
    assert_eq!(tree.len(), 252);

    let mut decoded = Vec::new();
    let mut source = BytesSource::new(&tree);
    let capabilities = decode_tree_record(&mut source, &mut |value| {
        decoded.push(value.clone());
        Ok(())
    })?;
    assert_eq!(decoded, vec![entry]);
    assert_eq!(capabilities.bits(), 0);

    let mut digest = CaptureDigest::default();
    let validated = validate_tree_bytes(&tree, &mut digest)?;
    assert_eq!(digest.invocations, 1);
    let root = sample_root(&validated, 9, 0xa5, None, None)?;
    let root_bytes = encode_root(&root)?;

    let mut root_payload = Vec::new();
    push_tlv(&mut root_payload, 1, &0_u64.to_be_bytes());
    push_tlv(&mut root_payload, 2, &1_u16.to_be_bytes());
    push_tlv(&mut root_payload, 3, validated.id().digest().as_bytes());
    push_option(&mut root_payload, 4, None);
    push_option(&mut root_payload, 5, None);
    push_tlv(&mut root_payload, 6, &9_u64.to_be_bytes());
    push_tlv(&mut root_payload, 7, &[0xa5; 16]);
    let mut expected_root = Vec::new();
    push_header(
        &mut expected_root,
        0x10,
        u64::try_from(root_payload.len()).unwrap_or(u64::MAX),
        false,
    );
    expected_root.extend_from_slice(&root_payload);
    assert_eq!(root_bytes, expected_root);
    assert_eq!(root_bytes.len(), 118);

    let mut root_source = BytesSource::new(&root_bytes);
    assert_eq!(decode_root_record(&mut root_source, &validated)?, root);

    let object = ObjectRecord::new(ObjectKind::ChunkPayload, Vec::new())?;
    let mut object_sink = VecSink::default();
    encode_object_record(&object, &mut object_sink)?;
    let mut expected_object = Vec::new();
    push_header(&mut expected_object, 3, 0, false);
    assert_eq!(object_sink.bytes, expected_object);
    let mut object_source = BytesSource::new(&expected_object);
    assert_eq!(decode_object_record(&mut object_source)?, object);
    Ok(())
}

#[test]
fn prc_r02_portable_raw_paths() -> Result<(), Error> {
    let raw = CanonicalPath::from_bytes(&[b'a', b'/', 0xff, b'\\', b'b'])?;
    assert_eq!(raw.as_bytes(), &[b'a', b'/', 0xff, b'\\', b'b']);

    for invalid in [
        &b""[..],
        &b"/a"[..],
        &b"a/"[..],
        &b"a//b"[..],
        &b"."[..],
        &b".."[..],
        &b"a/./b"[..],
        &b"a/../b"[..],
        &b"a\0b"[..],
    ] {
        assert!(CanonicalPath::from_bytes(invalid).is_err(), "{invalid:?}");
    }
    assert_eq!(CanonicalPath::from_bytes(b"a\\b")?.as_bytes(), b"a\\b",);
    Ok(())
}

#[test]
fn prc_r03_fragmentation_is_deterministic_and_unsorted_input_rejects() -> Result<(), Error> {
    let canonical_entries = vec![
        simple_regular(b"alpha", 1)?,
        simple_regular(b"beta", 2)?,
        simple_regular(b"gamma", 3)?,
    ];
    let canonical = encode_tree(&canonical_entries)?;
    let entries_bytes = canonical_entries.iter().try_fold(0_u64, |total, entry| {
        total
            .checked_add(u64::from(tree_entry_record_len(entry)?))
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::Overflow,
                    ROOT_FORMAT_V2,
                    sandbox_runtime_layerstack_core::FieldClass::Tree,
                    0,
                )
            })
    })?;
    for fragment in 1..=canonical.len() {
        let mut sink = FragmentingSink::new(fragment);
        let mut iterator = canonical_entries.iter();
        encode_tree_record(
            u64::try_from(canonical_entries.len()).unwrap_or(u64::MAX),
            entries_bytes,
            &mut iterator,
            &mut sink,
        )?;
        assert_eq!(sink.bytes, canonical);
    }

    let mut unsorted = canonical_entries.clone();
    unsorted.swap(0, 2);
    assert!(encode_tree(&unsorted).is_err());

    for fragment in 1..=canonical.len() {
        let mut source = BytesSource::fragmented(&canonical, fragment);
        let mut decoded = Vec::new();
        let capabilities = decode_tree_record(&mut source, &mut |entry| {
            decoded.push(entry.clone());
            Ok(())
        })?;
        assert_eq!(decoded.len(), 3);
        assert_eq!(capabilities.bits(), 0);
    }
    Ok(())
}

#[test]
fn prc_r05_dense_xattrs_decode_within_the_compact_bound() -> Result<(), Error> {
    let mut xattrs = Vec::with_capacity(6_000);
    for ordinal in 0..6_000_usize {
        let high = u8::try_from(ordinal / 255 + 1).unwrap_or(u8::MAX);
        let low = u8::try_from(ordinal % 255 + 1).unwrap_or(u8::MAX);
        xattrs.push(Xattr::new(vec![high, low], Vec::new())?);
    }
    let entry = TreeEntry::directory(
        CanonicalPath::from_bytes(b"dense-xattrs")?,
        metadata(0o755, xattrs)?,
    )?;
    let tree = encode_tree(std::slice::from_ref(&entry))?;
    let mut decoded = Vec::new();
    let mut source = BytesSource::new(&tree);
    decode_tree_record(&mut source, &mut |value| {
        decoded.push(value.clone());
        Ok(())
    })?;
    assert_eq!(decoded, vec![entry]);
    assert_eq!(decoded[0].metadata().xattrs().len(), 6_000);
    Ok(())
}

#[test]
fn prc_r05_maximum_valid_hardlink_fingerprint_is_accepted() -> Result<(), Error> {
    let shared_metadata = metadata(0o644, vec![Xattr::new(b"x".to_vec(), vec![0; 65_423])?])?;
    let entries = vec![
        TreeEntry::regular(
            CanonicalPath::from_bytes(b"a")?,
            shared_metadata.clone(),
            0,
            Vec::new(),
            file_segments(1),
            Some(HardlinkGroupId::new(1)?),
        )?,
        TreeEntry::regular(
            CanonicalPath::from_bytes(b"b")?,
            shared_metadata,
            0,
            Vec::new(),
            file_segments(1),
            Some(HardlinkGroupId::new(1)?),
        )?,
    ];
    let tree = encode_tree(&entries)?;
    let validated = validate_tree_bytes(&tree, &mut CaptureDigest::default())?;
    assert_eq!(validated.entry_count(), 2);
    Ok(())
}

#[test]
fn prc_r05_approved_regular_entry_boundaries_are_accepted() -> Result<(), Error> {
    let dense = TreeEntry::regular(
        CanonicalPath::from_bytes(b"dense")?,
        metadata(0o644, vec![Xattr::new(b"x".to_vec(), vec![0; 65_439])?])?,
        0,
        Vec::new(),
        file_segments(1),
        None,
    )?;
    let dense_tree = encode_tree(std::slice::from_ref(&dense))?;
    assert_eq!(
        validate_tree_bytes(&dense_tree, &mut CaptureDigest::default())?.entry_count(),
        1,
    );

    let mut holes = Vec::with_capacity(4_090);
    for offset in 0..4_090_u64 {
        holes.push(SparseExtent::new(offset, 1)?);
    }
    let sparse = TreeEntry::regular(
        CanonicalPath::from_bytes(b"sparse")?,
        metadata(0o644, Vec::new())?,
        4_090,
        holes,
        file_segments(2),
        None,
    )?;
    let sparse_tree = encode_tree(std::slice::from_ref(&sparse))?;
    assert_eq!(
        validate_tree_bytes(&sparse_tree, &mut CaptureDigest::default())?.entry_count(),
        1,
    );
    Ok(())
}

#[test]
fn prc_r05_hostile_inputs_fail_closed() -> Result<(), Error> {
    let oversized_path = vec![b'a'; 4_097];
    assert!(CanonicalPath::from_bytes(&oversized_path).is_err());

    assert!(NodeMetadata::new(0o10_000, 0, 0, 0, 0, Vec::new()).is_err());
    assert!(NodeMetadata::new(0, 0, 0, 0, 1_000_000_000, Vec::new()).is_err());
    assert_eq!(
        Xattr::new(b"trusted.overlay.opaque".to_vec(), b"y".to_vec())?.key(),
        b"trusted.overlay.opaque",
    );
    assert_eq!(
        Xattr::new(b"user.overlay.opaque".to_vec(), b"y".to_vec())?.key(),
        b"user.overlay.opaque",
    );
    assert!(TreeEntry::device(
        CanonicalPath::from_bytes(b"whiteout")?,
        metadata(0, Vec::new())?,
        0,
        0,
    )
    .is_err());

    let duplicate_xattrs = vec![
        Xattr::new(b"user.a".to_vec(), vec![1])?,
        Xattr::new(b"user.a".to_vec(), vec![2])?,
    ];
    assert!(metadata(0, duplicate_xattrs).is_err());
    let unsorted_xattrs = vec![
        Xattr::new(b"user.b".to_vec(), vec![1])?,
        Xattr::new(b"user.a".to_vec(), vec![2])?,
    ];
    assert!(metadata(0, unsorted_xattrs).is_err());

    let oversized_metadata = metadata(0o644, vec![Xattr::new(b"x".to_vec(), vec![0; 65_440])?])?;
    assert!(TreeEntry::regular(
        CanonicalPath::from_bytes(b"dense")?,
        oversized_metadata,
        0,
        Vec::new(),
        file_segments(1),
        None,
    )
    .is_err());

    let mut holes = Vec::new();
    for offset in 0..4_091_u64 {
        holes.push(SparseExtent::new(offset, 1)?);
    }
    assert!(TreeEntry::regular(
        CanonicalPath::from_bytes(b"holes")?,
        metadata(0o644, Vec::new())?,
        4_091,
        holes,
        file_segments(1),
        None,
    )
    .is_err());
    assert!(TreeEntry::regular(
        CanonicalPath::from_bytes(b"overlap")?,
        metadata(0o644, Vec::new())?,
        8,
        vec![SparseExtent::new(1, 4)?, SparseExtent::new(3, 2)?],
        file_segments(1),
        None,
    )
    .is_err());

    let entry = simple_regular(b"a", 0x22)?;
    let tree = encode_tree(std::slice::from_ref(&entry))?;
    let duplicate_paths = vec![entry.clone(), entry.clone()];
    assert!(encode_tree(&duplicate_paths).is_err());
    let mut trailing = tree.clone();
    trailing.push(0);
    let mut trailing_source = BytesSource::new(&trailing);
    assert!(decode_tree_record(&mut trailing_source, &mut |_| Ok(())).is_err());

    let mut unknown_version = tree.clone();
    unknown_version[9..11].copy_from_slice(&3_u16.to_be_bytes());
    let mut version_source = BytesSource::new(&unknown_version);
    assert!(decode_tree_record(&mut version_source, &mut |_| Ok(())).is_err());

    let mut hostile_length = Vec::new();
    hostile_length.extend_from_slice(MAGIC);
    hostile_length.push(0x11);
    hostile_length.extend_from_slice(&ROOT_FORMAT_V2.get().to_be_bytes());
    hostile_length.extend_from_slice(&u64::MAX.to_be_bytes());
    let mut length_source = BytesSource::new(&hostile_length);
    assert!(decode_tree_record(&mut length_source, &mut |_| Ok(())).is_err());

    let mut oversized_path_prefix = Vec::new();
    push_tlv(&mut oversized_path_prefix, 1, &[2]);
    oversized_path_prefix.push(2);
    oversized_path_prefix.extend_from_slice(&4_097_u32.to_be_bytes());
    let oversized_path_tree = hostile_tree_with_entry_prefix(70_000, &oversized_path_prefix);
    let mut oversized_path_source = BytesSource::new(&oversized_path_tree);
    let Err(oversized_path_error) = decode_tree_record(&mut oversized_path_source, &mut |_| Ok(()))
    else {
        panic!("oversized nested path was accepted");
    };
    assert_eq!(oversized_path_error.kind(), ErrorKind::LimitExceeded);

    let mut oversized_metadata_prefix = Vec::new();
    push_tlv(&mut oversized_metadata_prefix, 1, &[2]);
    push_tlv(&mut oversized_metadata_prefix, 2, b"a");
    oversized_metadata_prefix.push(3);
    oversized_metadata_prefix.extend_from_slice(&65_537_u32.to_be_bytes());
    let oversized_metadata_tree =
        hostile_tree_with_entry_prefix(70_000, &oversized_metadata_prefix);
    let mut oversized_metadata_source = BytesSource::new(&oversized_metadata_tree);
    let Err(oversized_metadata_error) =
        decode_tree_record(&mut oversized_metadata_source, &mut |_| Ok(()))
    else {
        panic!("oversized nested metadata was accepted");
    };
    assert_eq!(oversized_metadata_error.kind(), ErrorKind::LimitExceeded);

    let valid_metadata = expected_metadata();
    let mut oversized_target_prefix = Vec::new();
    push_tlv(&mut oversized_target_prefix, 1, &[3]);
    push_tlv(&mut oversized_target_prefix, 2, b"link");
    push_tlv(&mut oversized_target_prefix, 3, &valid_metadata);
    oversized_target_prefix.push(4);
    oversized_target_prefix.extend_from_slice(&4_098_u32.to_be_bytes());
    let oversized_target_tree = hostile_tree_with_entry_prefix(70_000, &oversized_target_prefix);
    let mut oversized_target_source = BytesSource::new(&oversized_target_tree);
    let Err(oversized_target_error) =
        decode_tree_record(&mut oversized_target_source, &mut |_| Ok(()))
    else {
        panic!("oversized nested symlink target was accepted");
    };
    assert_eq!(oversized_target_error.kind(), ErrorKind::LimitExceeded);

    let mut oversized_holes_prefix = Vec::new();
    push_tlv(&mut oversized_holes_prefix, 1, &[2]);
    push_tlv(&mut oversized_holes_prefix, 2, b"a");
    push_tlv(&mut oversized_holes_prefix, 3, &valid_metadata);
    push_option(&mut oversized_holes_prefix, 4, None);
    push_option(&mut oversized_holes_prefix, 5, None);
    push_option(&mut oversized_holes_prefix, 6, None);
    push_option(&mut oversized_holes_prefix, 7, Some(&8_u64.to_be_bytes()));
    oversized_holes_prefix.push(8);
    oversized_holes_prefix.extend_from_slice(&65_537_u32.to_be_bytes());
    let oversized_holes_tree = hostile_tree_with_entry_prefix(70_000, &oversized_holes_prefix);
    let mut oversized_holes_source = BytesSource::new(&oversized_holes_tree);
    let Err(oversized_holes_error) =
        decode_tree_record(&mut oversized_holes_source, &mut |_| Ok(()))
    else {
        panic!("oversized nested sparse-hole vector was accepted");
    };
    assert_eq!(oversized_holes_error.kind(), ErrorKind::LimitExceeded);

    let mut digest = CaptureDigest::default();
    let validated = validate_tree_bytes(&tree, &mut digest)?;
    let root = sample_root(&validated, 1, 1, None, None)?;
    let canonical_root_bytes = encode_root(&root)?;
    for end in 0..canonical_root_bytes.len() {
        let mut short_source = BytesSource::new(&canonical_root_bytes[..end]);
        assert!(decode_root_record(&mut short_source, &validated).is_err());
    }
    let mut failing_source = FailingSource::new(&canonical_root_bytes, 20);
    let Err(source_error) = decode_root_record(&mut failing_source, &validated) else {
        panic!("injected source failure produced a root");
    };
    assert_eq!(source_error.kind(), ErrorKind::SourceFailure);

    let mut failing_sink = FailingSink::new(20);
    let Err(sink_error) = encode_root_record(&root, &mut failing_sink) else {
        panic!("injected sink failure produced root bytes");
    };
    assert_eq!(sink_error.kind(), ErrorKind::SinkFailure);

    let Err(digest_error) = root_id(&root, &mut FailingDigest) else {
        panic!("failing digest adapter produced an ID");
    };
    assert_eq!(digest_error.kind(), ErrorKind::DigestFailure);
    let Err(digest_error) = root_id(&root, &mut SkippingDigest) else {
        panic!("non-invoking digest adapter produced an ID");
    };
    assert_eq!(digest_error.kind(), ErrorKind::DigestFailure);
    let Err(digest_error) = root_id(&root, &mut RepeatingDigest) else {
        panic!("repeating digest adapter produced an ID");
    };
    assert_eq!(digest_error.kind(), ErrorKind::DigestFailure);

    let mut root_bytes = canonical_root_bytes.clone();
    root_bytes[20] = 0x80;
    let mut capability_source = BytesSource::new(&root_bytes);
    let Err(capability_error) = decode_root_record(&mut capability_source, &validated) else {
        panic!("unknown required capability was accepted");
    };
    assert_eq!(capability_error.kind(), ErrorKind::UnknownCapability);

    let mut mismatched_capabilities = canonical_root_bytes.clone();
    mismatched_capabilities[27] = Capability::Xattrs.bit() as u8;
    let mut mismatched_capability_source = BytesSource::new(&mismatched_capabilities);
    let Err(mismatched_capability_error) =
        decode_root_record(&mut mismatched_capability_source, &validated)
    else {
        panic!("non-minimal required capabilities were accepted");
    };
    assert_eq!(mismatched_capability_error.kind(), ErrorKind::NonCanonical,);

    let mut mismatched_tree = canonical_root_bytes.clone();
    mismatched_tree[40] ^= 1;
    let mut mismatched_tree_source = BytesSource::new(&mismatched_tree);
    let Err(mismatched_tree_error) = decode_root_record(&mut mismatched_tree_source, &validated)
    else {
        panic!("root for an unvalidated tree was accepted");
    };
    assert_eq!(mismatched_tree_error.kind(), ErrorKind::MissingReference);

    let mut dangling_tree_source = BytesSource::new(&tree);
    let mut hardlinks = RecordSink::default();
    let mut references = RecordSink::default();
    let pending = stage_tree_candidate(
        &mut dangling_tree_source,
        &mut hardlinks,
        &mut references,
        &mut CaptureDigest::default(),
    )?;
    let reference_bytes: Vec<u8> = references.records.into_iter().flatten().collect();
    let mut hardlink_source = BytesSource::new(&[]);
    let mut reference_source = BytesSource::new(&reference_bytes);
    let no_known_reference_count = 0_u64.to_be_bytes();
    let mut no_known_references = BytesSource::new(&no_known_reference_count);
    let Err(dangling_error) = validate_tree_candidate(
        pending,
        &mut hardlink_source,
        &mut reference_source,
        &mut no_known_references,
    ) else {
        panic!("dangling object reference was accepted");
    };
    assert_eq!(dangling_error.kind(), ErrorKind::MissingReference);

    let oversized_known_count = 257_u64.to_be_bytes();
    let Err(oversized_known_error) =
        validate_one_reference_with_known(&tree, &oversized_known_count)
    else {
        panic!("oversized known-reference source was accepted");
    };
    assert_eq!(oversized_known_error.kind(), ErrorKind::LimitExceeded);

    let mut claimed_reference = [0_u8; 33];
    claimed_reference[0] = ObjectKind::FileSegments as u8;
    claimed_reference[1..].fill(0x22);
    let mut lower_reference = [0_u8; 33];
    lower_reference[0] = ObjectKind::FileSegments as u8;
    lower_reference[1..].fill(0x11);

    let mut unsorted_known = Vec::new();
    unsorted_known.extend_from_slice(&2_u64.to_be_bytes());
    unsorted_known.extend_from_slice(&claimed_reference);
    unsorted_known.extend_from_slice(&lower_reference);
    let Err(unsorted_known_error) = validate_one_reference_with_known(&tree, &unsorted_known)
    else {
        panic!("unsorted trailing known reference was accepted");
    };
    assert_eq!(unsorted_known_error.kind(), ErrorKind::NonCanonical);

    let mut duplicate_known = Vec::new();
    duplicate_known.extend_from_slice(&2_u64.to_be_bytes());
    duplicate_known.extend_from_slice(&claimed_reference);
    duplicate_known.extend_from_slice(&claimed_reference);
    let Err(duplicate_known_error) = validate_one_reference_with_known(&tree, &duplicate_known)
    else {
        panic!("duplicate known reference was accepted");
    };
    assert_eq!(duplicate_known_error.kind(), ErrorKind::NonCanonical);

    let mut wrong_kind_reference = [0_u8; 33];
    wrong_kind_reference[0] = ObjectKind::ChunkPayload as u8;
    wrong_kind_reference[1..].fill(0x22);
    let mut wrong_known = Vec::new();
    wrong_known.extend_from_slice(&1_u64.to_be_bytes());
    wrong_known.extend_from_slice(&wrong_kind_reference);

    let mut wrong_claim_tree_source = BytesSource::new(&tree);
    let mut wrong_claim_hardlinks = RecordSink::default();
    let mut discarded_claims = RecordSink::default();
    let wrong_claim_pending = stage_tree_candidate(
        &mut wrong_claim_tree_source,
        &mut wrong_claim_hardlinks,
        &mut discarded_claims,
        &mut CaptureDigest::default(),
    )?;
    let mut empty_hardlinks = BytesSource::new(&[]);
    let mut wrong_claim_source = BytesSource::new(&wrong_kind_reference);
    let mut wrong_claim_known_source = BytesSource::new(&wrong_known);
    let Err(wrong_claim_error) = validate_tree_candidate(
        wrong_claim_pending,
        &mut empty_hardlinks,
        &mut wrong_claim_source,
        &mut wrong_claim_known_source,
    ) else {
        panic!("matching non-FileSegments claim and known identity were accepted");
    };
    assert_eq!(wrong_claim_error.kind(), ErrorKind::InvalidValue);

    let mut wrong_known_tree_source = BytesSource::new(&tree);
    let mut wrong_known_hardlinks = RecordSink::default();
    let mut valid_claims = RecordSink::default();
    let wrong_known_pending = stage_tree_candidate(
        &mut wrong_known_tree_source,
        &mut wrong_known_hardlinks,
        &mut valid_claims,
        &mut CaptureDigest::default(),
    )?;
    let valid_claim_bytes: Vec<u8> = valid_claims.records.into_iter().flatten().collect();
    let mut empty_hardlinks = BytesSource::new(&[]);
    let mut valid_claim_source = BytesSource::new(&valid_claim_bytes);
    let mut wrong_known_source = BytesSource::new(&wrong_known);
    let Err(wrong_known_error) = validate_tree_candidate(
        wrong_known_pending,
        &mut empty_hardlinks,
        &mut valid_claim_source,
        &mut wrong_known_source,
    ) else {
        panic!("non-FileSegments known identity was accepted");
    };
    assert_eq!(wrong_known_error.kind(), ErrorKind::InvalidValue);

    let inconsistent_hardlinks = vec![
        TreeEntry::regular(
            CanonicalPath::from_bytes(b"a")?,
            metadata(0o644, Vec::new())?,
            8,
            Vec::new(),
            file_segments(1),
            Some(HardlinkGroupId::new(1)?),
        )?,
        TreeEntry::regular(
            CanonicalPath::from_bytes(b"b")?,
            metadata(0o644, Vec::new())?,
            8,
            Vec::new(),
            file_segments(2),
            Some(HardlinkGroupId::new(1)?),
        )?,
    ];
    let inconsistent_bytes = encode_tree(&inconsistent_hardlinks)?;
    let Err(hardlink_error) =
        validate_tree_bytes(&inconsistent_bytes, &mut CaptureDigest::default())
    else {
        panic!("inconsistent hardlink group was accepted");
    };
    assert_eq!(hardlink_error.kind(), ErrorKind::HardlinkMismatch);

    let capabilities = sandbox_runtime_layerstack_core::CapabilitySet::from_bits(
        Capability::Xattrs.bit()
            | Capability::SparseHoles.bit()
            | Capability::Hardlinks.bit()
            | Capability::Symlinks.bit()
            | Capability::Devices.bit()
            | Capability::Fifo.bit(),
    )?;
    assert_eq!(capabilities.bits(), 0x3f);
    Ok(())
}
