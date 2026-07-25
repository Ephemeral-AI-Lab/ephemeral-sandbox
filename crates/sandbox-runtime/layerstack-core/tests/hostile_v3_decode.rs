mod common;

use sandbox_runtime_layerstack_core::{
    decode_v3_record, decode_v3_record_as, tree_page_id, validate_v3_references, BranchId,
    CanonicalRecordV3, Digest32, Error, ErrorKind, RawDigest, RecordKindV3, TlvV3,
    V3ReferenceLookup,
};

use common::{BytesSource, CaptureDigest};

const CONTRACT_V3: &str = include_str!("../../layerstack/tests/fixtures/cas/v3/contract-v3.tsv");

struct FixedDigest(Digest32);

impl RawDigest for FixedDigest {
    fn digest_bytes(&mut self, _bytes: &[u8]) -> Result<Digest32, Error> {
        Ok(self.0)
    }
}

struct Missing;

impl V3ReferenceLookup for Missing {
    fn contains(&mut self, _kind: RecordKindV3, _digest: Digest32) -> Result<bool, Error> {
        Ok(false)
    }
}

fn hex(value: &str) -> Vec<u8> {
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = (pair[0] as char).to_digit(16).expect("hex high");
            let low = (pair[1] as char).to_digit(16).expect("hex low");
            u8::try_from((high << 4) | low).expect("hex byte")
        })
        .collect()
}

fn golden(name: &str) -> Vec<u8> {
    let row = CONTRACT_V3
        .lines()
        .find(|line| line.starts_with(name) && line.as_bytes().get(name.len()) == Some(&b'\t'))
        .expect("golden row");
    hex(row.split('\t').nth(4).expect("golden hex"))
}

fn field(tag: u8, value: Vec<u8>) -> TlvV3 {
    TlvV3::new(tag, value)
}

fn tree_entries(names: &[&[u8]]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for (ordinal, name) in names.iter().enumerate() {
        bytes.extend_from_slice(
            &u16::try_from(name.len())
                .expect("name length")
                .to_be_bytes(),
        );
        bytes.extend_from_slice(name);
        bytes.extend_from_slice(&[u8::try_from(ordinal + 1).expect("ordinal"); 32]);
    }
    bytes
}

fn assert_kind<T>(result: Result<T, Error>, expected: ErrorKind) {
    let error = result.err().expect("hostile vector must reject");
    assert_eq!(error.kind(), expected);
}

#[test]
fn hostile_v3_decode_rejects_kind_version_domain_and_suffix() -> Result<(), Error> {
    let tree = golden("tree_page_hello_leaf");
    assert_kind(
        decode_v3_record_as(
            &mut BytesSource::new(&tree),
            RecordKindV3::Root,
            &mut FixedDigest(Digest32::default()),
        ),
        ErrorKind::WrongKind,
    );

    let mut wrong_version = golden("root_record_v3");
    wrong_version[9..11].copy_from_slice(&4_u16.to_be_bytes());
    assert_kind(
        decode_v3_record(
            &mut BytesSource::new(&wrong_version),
            &mut FixedDigest(Digest32::default()),
        ),
        ErrorKind::UnsupportedVersion,
    );

    let chunk = CanonicalRecordV3::chunk(vec![1])?;
    assert_kind(
        tree_page_id(&chunk, &mut CaptureDigest::default()),
        ErrorKind::WrongDomain,
    );

    let mut trailing = golden("root_record_v3");
    trailing.push(0);
    assert_kind(
        decode_v3_record(
            &mut BytesSource::new(&trailing),
            &mut FixedDigest(Digest32::default()),
        ),
        ErrorKind::TrailingBytes,
    );
    Ok(())
}

#[test]
fn hostile_v3_decode_rejects_order_count_page_depth_and_dangling() -> Result<(), Error> {
    for (names, expected) in [
        (&[&b"b"[..], &b"a"[..]][..], ErrorKind::NonCanonicalOrder),
        (&[&b"a"[..], &b"a"[..]][..], ErrorKind::DuplicateEntry),
    ] {
        assert_kind(
            CanonicalRecordV3::immutable(
                RecordKindV3::TreePage,
                vec![
                    field(1, vec![1]),
                    field(2, vec![0]),
                    field(3, 2_u16.to_be_bytes().to_vec()),
                    field(4, tree_entries(names)),
                ],
            ),
            expected,
        );
    }

    assert_kind(
        CanonicalRecordV3::immutable(
            RecordKindV3::TreePage,
            vec![
                field(1, vec![1]),
                field(2, vec![0]),
                field(3, 193_u16.to_be_bytes().to_vec()),
                field(4, Vec::new()),
            ],
        ),
        ErrorKind::CountLimit,
    );

    let mut oversized = b"EOS-LS2\0".to_vec();
    oversized.push(RecordKindV3::TreePage as u8);
    oversized.extend_from_slice(&3_u16.to_be_bytes());
    oversized.extend_from_slice(&65_522_u32.to_be_bytes());
    assert_kind(
        decode_v3_record(
            &mut BytesSource::new(&oversized),
            &mut FixedDigest(Digest32::default()),
        ),
        ErrorKind::PageLimit,
    );

    assert_kind(
        CanonicalRecordV3::immutable(
            RecordKindV3::TreePage,
            vec![
                field(1, vec![2]),
                field(2, vec![17]),
                field(3, 2_u16.to_be_bytes().to_vec()),
                field(4, tree_entries(&[b"a", b"b"])),
            ],
        ),
        ErrorKind::DepthLimit,
    );

    let record = decode_v3_record(
        &mut BytesSource::new(&golden("tree_page_hello_leaf")),
        &mut FixedDigest(Digest32::default()),
    )?;
    assert_kind(
        validate_v3_references(&record, &mut Missing),
        ErrorKind::DanglingEdge,
    );
    Ok(())
}

#[test]
fn hostile_v3_decode_rejects_sparse_capability_checksum_identifier_and_overflow(
) -> Result<(), Error> {
    for (descriptor, expected) in [
        (
            {
                let mut value = vec![1];
                value.extend_from_slice(&1_u64.to_be_bytes());
                value.extend_from_slice(&4_u64.to_be_bytes());
                value.extend_from_slice(&[7; 32]);
                value
            },
            ErrorKind::SparseInvalid,
        ),
        (
            {
                let mut value = vec![3];
                value.extend_from_slice(&0_u64.to_be_bytes());
                value.extend_from_slice(&0_u64.to_be_bytes());
                value
            },
            ErrorKind::SparseInvalid,
        ),
    ] {
        assert_kind(
            CanonicalRecordV3::immutable(
                RecordKindV3::SegmentPage,
                vec![
                    field(1, vec![1]),
                    field(2, vec![0]),
                    field(3, 1_u16.to_be_bytes().to_vec()),
                    field(4, 5_u64.to_be_bytes().to_vec()),
                    field(5, descriptor),
                ],
            ),
            expected,
        );
    }

    assert_kind(
        CanonicalRecordV3::immutable(
            RecordKindV3::Root,
            vec![
                field(1, (1_u64 << 6).to_be_bytes().to_vec()),
                field(2, 1_u16.to_be_bytes().to_vec()),
                field(3, vec![1; 32]),
            ],
        ),
        ErrorKind::UnknownRequiredCapability,
    );

    let mut head = golden("head_main_generation_1");
    let mut checksum = [0_u8; 32];
    checksum.copy_from_slice(&head[head.len() - 32..]);
    *head.last_mut().expect("checksum byte") ^= 1;
    assert_kind(
        decode_v3_record(
            &mut BytesSource::new(&head),
            &mut FixedDigest(Digest32::new(checksum)),
        ),
        ErrorKind::ChecksumMismatch,
    );

    assert_kind(
        BranchId::new(b"Uppercase".to_vec()),
        ErrorKind::InvalidIdentifier,
    );

    let locator = vec![
        field(1, vec![RecordKindV3::Chunk as u8]),
        field(2, vec![1; 32]),
        field(3, 1_u64.to_be_bytes().to_vec()),
        field(4, vec![1]),
        field(5, vec![2; 32]),
        field(6, u64::MAX.to_be_bytes().to_vec()),
        field(7, 1_u64.to_be_bytes().to_vec()),
        field(8, vec![3; 32]),
    ];
    assert_kind(
        CanonicalRecordV3::mutable(
            RecordKindV3::Locator,
            locator,
            &mut FixedDigest(Digest32::new([4; 32])),
        ),
        ErrorKind::ArithmeticOverflow,
    );
    Ok(())
}
