#[path = "../../layerstack-core/tests/common/mod.rs"]
mod common;

use std::error::Error as StdError;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use sandbox_runtime_layerstack::{
    canonical_root_json, canonical_tree_json, parse_canonical_root_json, parse_canonical_tree_json,
    prepare_tiny_portable_tree, PortableBackendMarker, PortablePreparationError,
    PortablePreparationInput, PortablePreparationStats, Sha256Digest,
};
use sandbox_runtime_layerstack_core::{
    decode_root_record, decode_tree_record, decode_v3_record, encode_v3_record, v3_record_id,
    CanonicalPath, Digest32, Error, ErrorKind, FieldClass, HardlinkGroupId, ObjectId, ObjectKind,
    RecordKindV3, RootId, RootRecordV2, SparseExtent, TreeEntry, Xattr, ROOT_FORMAT_V2,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use common::{
    complete_sample_tree, encode_root, encode_tree, file_segments, identify_root, metadata,
    sample_root, validate_tree_bytes_with_peak, BytesSource, LogicalOwnerCounter, VecSink,
};

const CONTRACT_JSON: &str = include_str!("fixtures/cas/v2/contract-v2.json");
const CONTRACT_BIN: &[u8] = include_bytes!("fixtures/cas/v2/contract-v2.bin");
const CONTRACT_V3: &str = include_str!("fixtures/cas/v3/contract-v3.tsv");
const PORTABLE_ROOT_R08_ORACLE: &[u8] = include_bytes!("fixtures/cas/v2/portable-root-r08-v1.bin");
const V1_GOLDEN: &str = include_str!("fixtures/v1/baseline.json");
const DECISION_ID: &str = "PRC-STAGE02-OWNER-DECISION-D2.5";
const SEED: u64 = 23_042;
const CONTRACT_PUBLICATION_BYTE: u8 = 0x52;
const STAGE00_CORPUS_SEED: u64 = 20_260_724;
const STAGE00_MANIFEST_SHA256: &str =
    "sha256:535afb81bb63809b953d5ba969850e5fafe64174e858501ad2cdcfef324e4a6b";
const STAGE00_WORKLOAD_IDS: [&str; 7] = [
    "empty_no_op",
    "localized_edit_1k_in_1m",
    "incompressible_1m",
    "small_files_256_total_1m",
    "overwrite_history_1",
    "overwrite_history_8",
    "overwrite_history_32",
];
const PORTABLE_ROOT_R08_ORACLE_FORMAT: &str = "stage02-canonical-record-concatenation-v1";
const PORTABLE_ROOT_R08_ORACLE_SHA256: &str =
    "sha256:dbdca20a50da366b037a9adeecc688bdce32cd4e0840630b31babb6018055c60";
static PORTABLE_SPOOL_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContractFixture {
    schema_version: u64,
    format: String,
    decision_id: String,
    provenance: ContractProvenance,
    canonical_input: serde_json::Value,
    binary: ContractBinary,
    identities: ContractIdentities,
    rejections: Vec<RejectionVector>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContractProvenance {
    source: String,
    seed: u64,
    authority: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContractBinary {
    layout: Vec<String>,
    tree_bytes_len: usize,
    root_bytes_len: usize,
    canonical_bytes_len: usize,
    canonical_bytes_sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContractIdentities {
    tree_manifest_id: String,
    root_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RejectionVector {
    case: String,
    error_kind: String,
    field: String,
}

#[derive(Clone, Copy)]
enum VectorKind {
    Empty,
    Localized,
    Incompressible,
    SmallFiles,
    MixedMetadata,
    History1,
    History8,
    History32,
}

#[derive(Clone, Copy)]
struct VectorSpec {
    name: &'static str,
    kind: VectorKind,
    generation: u64,
    publication_byte: u8,
    parent_byte: Option<u8>,
    base_byte: Option<u8>,
}

const SPECS: [VectorSpec; 8] = [
    VectorSpec {
        name: "empty_no_op",
        kind: VectorKind::Empty,
        generation: 1,
        publication_byte: 1,
        parent_byte: None,
        base_byte: None,
    },
    VectorSpec {
        name: "localized_edit_1k_in_1m",
        kind: VectorKind::Localized,
        generation: 2,
        publication_byte: 2,
        parent_byte: None,
        base_byte: None,
    },
    VectorSpec {
        name: "incompressible_1m",
        kind: VectorKind::Incompressible,
        generation: 3,
        publication_byte: 3,
        parent_byte: None,
        base_byte: None,
    },
    VectorSpec {
        name: "small_files_256_total_1m",
        kind: VectorKind::SmallFiles,
        generation: 4,
        publication_byte: 4,
        parent_byte: None,
        base_byte: None,
    },
    VectorSpec {
        name: "raw_bytes_mixed_metadata",
        kind: VectorKind::MixedMetadata,
        generation: 5,
        publication_byte: 5,
        parent_byte: None,
        base_byte: None,
    },
    VectorSpec {
        name: "overwrite_history_1",
        kind: VectorKind::History1,
        generation: 6,
        publication_byte: 6,
        parent_byte: Some(1),
        base_byte: None,
    },
    VectorSpec {
        name: "overwrite_history_8",
        kind: VectorKind::History8,
        generation: 7,
        publication_byte: 7,
        parent_byte: Some(8),
        base_byte: Some(1),
    },
    VectorSpec {
        name: "overwrite_history_32",
        kind: VectorKind::History32,
        generation: 8,
        publication_byte: 8,
        parent_byte: Some(32),
        base_byte: Some(1),
    },
];

#[derive(Clone, Copy)]
struct ExpectedBenchmarkVector {
    name: &'static str,
    canonical_bytes_offset: usize,
    tree_bytes_len: usize,
    canonical_bytes_len: usize,
    canonical_bytes_sha256: &'static str,
    root_id: &'static str,
}

const EXPECTED_BENCHMARK_VECTORS: [ExpectedBenchmarkVector; 8] = [
    ExpectedBenchmarkVector {
        name: "empty_no_op",
        canonical_bytes_offset: 0,
        tree_bytes_len: 35,
        canonical_bytes_len: 153,
        canonical_bytes_sha256:
            "sha256:7a7690f8d398ef54d6ac53b3e4b4d84041712caa78ba3ac37bdaf4c04045ca5c",
        root_id: "sha256:35e9926e4da8aa4d64df14dc638939fdfa7e98564e2e761876b06734dc9cf5ba",
    },
    ExpectedBenchmarkVector {
        name: "localized_edit_1k_in_1m",
        canonical_bytes_offset: 153,
        tree_bytes_len: 270,
        canonical_bytes_len: 388,
        canonical_bytes_sha256:
            "sha256:661a5719b737008affdf3807c200660ed1ca76a32a5bf79e0f36c5cddf1c2006",
        root_id: "sha256:ea7234e6bd953ec4a232b9ae9d0bdbbe6970c8ef45d3029e93c1993cdd472832",
    },
    ExpectedBenchmarkVector {
        name: "incompressible_1m",
        canonical_bytes_offset: 541,
        tree_bytes_len: 275,
        canonical_bytes_len: 393,
        canonical_bytes_sha256:
            "sha256:a2d57274ab87ee331e273b15839ecaa4d08c8a1135c258d893ccb463744a4a78",
        root_id: "sha256:ec3457073a84cbf07aa53375a46734702c583a1405fdc3ea23d239f7093a455e",
    },
    ExpectedBenchmarkVector {
        name: "small_files_256_total_1m",
        canonical_bytes_offset: 934,
        tree_bytes_len: 57_635,
        canonical_bytes_len: 57_753,
        canonical_bytes_sha256:
            "sha256:02312edae69d00f2de098b4ff2c396a036cfaf35a946286d963cfcec143d08d2",
        root_id: "sha256:3d818a82514b3d25847ff282a25c8a583ff88d6d182f6b0cae50e8f5888b6f80",
    },
    ExpectedBenchmarkVector {
        name: "raw_bytes_mixed_metadata",
        canonical_bytes_offset: 58_687,
        tree_bytes_len: 1_178,
        canonical_bytes_len: 1_296,
        canonical_bytes_sha256:
            "sha256:5ae19c484d3dce2da5766fd3a15d98074c592557820845aedab18d720bf00c1f",
        root_id: "sha256:2136d2305f90f0cd512861bb0ab9bcb6701ad13a7c2fc72afba10422c2ba5d14",
    },
    ExpectedBenchmarkVector {
        name: "overwrite_history_1",
        canonical_bytes_offset: 59_983,
        tree_bytes_len: 264,
        canonical_bytes_len: 414,
        canonical_bytes_sha256:
            "sha256:bec119c465c6a93c3fc521bd2399eed3940870547a90d0c21bd053586e110c5f",
        root_id: "sha256:96332ded8ef5458e46b205a82ca9561986e9336267cbe9e83ea3097473b1e664",
    },
    ExpectedBenchmarkVector {
        name: "overwrite_history_8",
        canonical_bytes_offset: 60_397,
        tree_bytes_len: 264,
        canonical_bytes_len: 446,
        canonical_bytes_sha256:
            "sha256:0075707d5baf0841d0e528c8e58ed805eb7ceb78ed67b27f4bcd2d3fca474db5",
        root_id: "sha256:58f05dac1035235d0a540612560098e1b7d8a89b6ec4f54d688bfe9212c99f9a",
    },
    ExpectedBenchmarkVector {
        name: "overwrite_history_32",
        canonical_bytes_offset: 60_843,
        tree_bytes_len: 264,
        canonical_bytes_len: 446,
        canonical_bytes_sha256:
            "sha256:d571502b88826bde5b03a4f1ea1f326ba4adf841e70af0f244622846a0257873",
        root_id: "sha256:0f112554fd1a2f5644112c711c8828ed31000f19ae5762f6686bd27ea598f7d2",
    },
];

struct ComputedVector {
    tree_bytes_len: usize,
    canonical_bytes: Vec<u8>,
    root: RootRecordV2,
    root_id: RootId,
    peak_codec_scratch_bytes: u64,
}

struct BenchmarkVector {
    spec: VectorSpec,
    tree_bytes_len: usize,
    canonical_bytes: &'static [u8],
    root_id: RootId,
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest: [u8; 32] = Sha256::digest(bytes).into();
    digest_value_hex(Digest32::new(digest))
}

fn decode_v3_hex(value: &str) -> Vec<u8> {
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

#[test]
fn canonical_v3_sha256_adapter_matches_owner_goldens() -> Result<(), Error> {
    let mut count = 0_usize;
    for line in CONTRACT_V3
        .lines()
        .filter(|line| !line.starts_with('#') && !line.is_empty())
    {
        let mut columns = line.split('\t');
        let name = columns.next().expect("name");
        let kind = u8::from_str_radix(columns.next().expect("kind"), 16).expect("kind hex");
        let encoded_length = columns
            .next()
            .expect("encoded length")
            .parse::<usize>()
            .expect("encoded length integer");
        let expected_sha256 = columns.next().expect("sha256");
        let bytes = decode_v3_hex(columns.next().expect("record hex"));
        assert!(columns.next().is_none(), "{name}");

        assert_eq!(bytes.len(), encoded_length, "{name}");
        assert_eq!(
            sha256_hex(&bytes),
            format!("sha256:{expected_sha256}"),
            "{name}"
        );

        let mut source = BytesSource::fragmented(&bytes, 1);
        let record = decode_v3_record(&mut source, &mut Sha256Digest)?;
        assert_eq!(record.kind() as u8, kind, "{name}");

        let mut encoded = VecSink::default();
        encode_v3_record(&record, &mut encoded)?;
        assert_eq!(encoded.bytes, bytes, "{name}");

        if !matches!(
            record.kind(),
            RecordKindV3::Metadata
                | RecordKindV3::Head
                | RecordKindV3::OperationState
                | RecordKindV3::Locator
                | RecordKindV3::SourceLease
        ) {
            assert_eq!(
                digest_value_hex(v3_record_id(&record, &mut Sha256Digest)?),
                format!("sha256:{expected_sha256}"),
                "{name}"
            );
        }
        count += 1;
    }
    assert_eq!(count, 16);
    Ok(())
}

fn digest_value_hex(digest: Digest32) -> String {
    let mut output = String::with_capacity(71);
    output.push_str("sha256:");
    for byte in digest.as_bytes() {
        let _ = write!(&mut output, "{byte:02x}");
    }
    output
}

fn root_hex(root: RootId) -> String {
    digest_value_hex(root.digest())
}

fn regular(path: Vec<u8>, logical_len: u64, object: ObjectId) -> Result<TreeEntry, Error> {
    TreeEntry::regular(
        CanonicalPath::new(path)?,
        metadata(0o644, Vec::new())?,
        logical_len,
        Vec::new(),
        object,
        None,
    )
}

fn vector_entries(kind: VectorKind) -> Result<Vec<TreeEntry>, Error> {
    match kind {
        VectorKind::Empty => Ok(Vec::new()),
        VectorKind::Localized => Ok(vec![regular(
            b"large/localized.bin".to_vec(),
            1_048_576,
            file_segments(0x21),
        )?]),
        VectorKind::Incompressible => Ok(vec![regular(
            b"large/incompressible.bin".to_vec(),
            1_048_576,
            file_segments(0x22),
        )?]),
        VectorKind::SmallFiles => {
            let mut entries = Vec::with_capacity(256);
            for index in 0_u16..256 {
                entries.push(regular(
                    format!("small/{index:03}").into_bytes(),
                    4_096,
                    ObjectId::new(
                        ObjectKind::FileSegments,
                        Digest32::new([u8::try_from(index).unwrap_or(0); 32]),
                    ),
                )?);
            }
            Ok(entries)
        }
        VectorKind::MixedMetadata => {
            let shared_metadata = metadata(0o640, Vec::new())?;
            Ok(vec![
                TreeEntry::directory(
                    CanonicalPath::from_bytes(b"bin")?,
                    metadata(
                        0o755,
                        vec![Xattr::new(b"user.note".to_vec(), vec![0, 0xff, b'\\'])?],
                    )?,
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
                TreeEntry::fifo(
                    CanonicalPath::from_bytes(b"raw/pipe")?,
                    metadata(0o600, Vec::new())?,
                )?,
                TreeEntry::device(
                    CanonicalPath::from_bytes(b"raw/\xff")?,
                    metadata(0o600, Vec::new())?,
                    1,
                    3,
                )?,
            ])
        }
        VectorKind::History1 => Ok(vec![regular(
            b"history/final".to_vec(),
            1_048_576,
            file_segments(0x31),
        )?]),
        VectorKind::History8 => Ok(vec![regular(
            b"history/final".to_vec(),
            1_048_576,
            file_segments(0x38),
        )?]),
        VectorKind::History32 => Ok(vec![regular(
            b"history/final".to_vec(),
            1_048_576,
            file_segments(0x40),
        )?]),
    }
}

fn provenance(byte: Option<u8>) -> Option<RootId> {
    byte.map(|value| RootId::new(Digest32::new([value; 32])))
}

fn compute_entries(
    entries: &[TreeEntry],
    generation: u64,
    publication_byte: u8,
    parent: Option<RootId>,
    base: Option<RootId>,
) -> Result<ComputedVector, Error> {
    let tree_bytes = encode_tree(entries)?;
    let (tree, peak_codec_scratch_bytes) =
        validate_tree_bytes_with_peak(&tree_bytes, &mut Sha256Digest)?;
    let root = sample_root(&tree, generation, publication_byte, parent, base)?;
    let root_bytes = encode_root(&root)?;
    let root_id = identify_root(&root, &mut Sha256Digest)?;
    let tree_bytes_len = tree_bytes.len();
    let mut canonical_bytes = Vec::with_capacity(tree_bytes.len() + root_bytes.len());
    canonical_bytes.extend_from_slice(&tree_bytes);
    canonical_bytes.extend_from_slice(&root_bytes);
    Ok(ComputedVector {
        tree_bytes_len,
        canonical_bytes,
        root,
        root_id,
        peak_codec_scratch_bytes,
    })
}

fn compute_contract() -> Result<ComputedVector, Error> {
    compute_entries(
        &complete_sample_tree()?,
        SEED,
        CONTRACT_PUBLICATION_BYTE,
        None,
        None,
    )
}

fn compute_vector(spec: VectorSpec) -> Result<ComputedVector, Error> {
    compute_entries(
        &vector_entries(spec.kind)?,
        spec.generation,
        spec.publication_byte,
        provenance(spec.parent_byte),
        provenance(spec.base_byte),
    )
}

fn contract_fixture() -> ContractFixture {
    match serde_json::from_str(CONTRACT_JSON) {
        Ok(value) => value,
        Err(error) => panic!("invalid portable-root fixture inventory: {error}"),
    }
}

fn metadata_json(mode: u32, xattrs: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "mode": mode,
        "uid": 1000,
        "gid": 1001,
        "mtime_seconds": -7,
        "mtime_nanoseconds": 42,
        "xattrs": xattrs
    })
}

fn expected_contract_input() -> serde_json::Value {
    let empty_xattrs = serde_json::json!([]);
    let file_segments_digest = "11".repeat(32);
    serde_json::json!({
        "root": {
            "required_capabilities": 63,
            "chunk_profile": 1,
            "parent": null,
            "base": null,
            "publication": {
                "generation": 23042,
                "id_hex": "52525252525252525252525252525252"
            }
        },
        "tree": {
            "entry_count": 6,
            "entries": [
                {
                    "kind": "directory",
                    "path_hex": "62696e",
                    "metadata": metadata_json(
                        493,
                        serde_json::json!([{
                            "key_hex": "757365722e6e6f7465",
                            "value_hex": "ff"
                        }])
                    )
                },
                {
                    "kind": "regular",
                    "path_hex": "62696e2f61",
                    "metadata": metadata_json(416, empty_xattrs.clone()),
                    "logical_length": 10,
                    "sparse_holes": [{"offset": 4, "length": 2}],
                    "hardlink_group": 1,
                    "file_segments_digest_hex": file_segments_digest.clone()
                },
                {
                    "kind": "regular",
                    "path_hex": "62696e2f62",
                    "metadata": metadata_json(416, empty_xattrs.clone()),
                    "logical_length": 10,
                    "sparse_holes": [{"offset": 4, "length": 2}],
                    "hardlink_group": 1,
                    "file_segments_digest_hex": file_segments_digest
                },
                {
                    "kind": "symlink",
                    "path_hex": "6c696e6b",
                    "metadata": metadata_json(511, empty_xattrs.clone()),
                    "target_hex": "62696e2f61"
                },
                {
                    "kind": "device",
                    "path_hex": "6e756c6c",
                    "metadata": metadata_json(384, empty_xattrs.clone()),
                    "major": 1,
                    "minor": 3
                },
                {
                    "kind": "fifo",
                    "path_hex": "70697065",
                    "metadata": metadata_json(384, empty_xattrs)
                }
            ]
        }
    })
}

fn assert_rejection_inventory(rejections: &[RejectionVector]) {
    let expected = [
        ("empty-path", "InvalidValue", "Path"),
        ("path-traversal", "InvalidValue", "Path"),
        (
            "unknown-required-capability",
            "UnknownCapability",
            "Capability",
        ),
        ("unsorted-tree-entries", "NonCanonical", "Path"),
        (
            "dangling-file-segments-reference",
            "MissingReference",
            "ObjectReference",
        ),
        ("trailing-root-bytes", "TrailingBytes", "Source"),
    ];
    assert_eq!(rejections.len(), expected.len());
    for (actual, (case, error_kind, field)) in rejections.iter().zip(expected) {
        assert_eq!(actual.case, case);
        assert_eq!(actual.error_kind, error_kind);
        assert_eq!(actual.field, field);
    }
}

fn assert_contract_fixture() -> Result<ComputedVector, Error> {
    let fixture = contract_fixture();
    assert_eq!(fixture.schema_version, 1);
    assert_eq!(fixture.format, "portable-root-contract-v2");
    assert_eq!(fixture.decision_id, DECISION_ID);
    assert_eq!(
        fixture.provenance.source,
        "sandbox-runtime-layerstack-core/tests/common::complete_sample_tree",
    );
    assert_eq!(fixture.provenance.seed, SEED);
    assert_eq!(
        fixture.provenance.authority,
        "test-only; legacy-v1-remains-sole-runtime-authority",
    );
    assert_eq!(fixture.canonical_input, expected_contract_input());
    assert_eq!(
        fixture.binary.layout,
        vec!["tree-record".to_owned(), "root-record".to_owned()],
    );

    let computed = compute_contract()?;
    let root_bytes_len = computed.canonical_bytes.len() - computed.tree_bytes_len;
    assert_eq!(fixture.binary.tree_bytes_len, computed.tree_bytes_len);
    assert_eq!(fixture.binary.root_bytes_len, root_bytes_len);
    assert_eq!(
        fixture.binary.canonical_bytes_len,
        computed.canonical_bytes.len(),
    );
    assert_eq!(
        fixture.binary.canonical_bytes_sha256,
        sha256_hex(&computed.canonical_bytes),
    );
    assert_eq!(computed.canonical_bytes, CONTRACT_BIN);
    assert_eq!(
        fixture.identities.tree_manifest_id,
        digest_value_hex(computed.root.tree_manifest().digest()),
    );
    assert_eq!(fixture.identities.root_id, root_hex(computed.root_id),);
    assert_rejection_inventory(&fixture.rejections);
    Ok(computed)
}

#[test]
fn portable_root_golden_contract() -> Result<(), Box<dyn StdError>> {
    let computed = assert_contract_fixture()?;
    let tree_bytes = &computed.canonical_bytes[..computed.tree_bytes_len];
    let (tree, _peak_codec_scratch_bytes) =
        validate_tree_bytes_with_peak(tree_bytes, &mut Sha256Digest)?;

    let tree_json = canonical_tree_json(tree)?;
    let parsed_tree = parse_canonical_tree_json(&tree_json)?;
    assert_eq!(
        parsed_tree.tree_manifest_id,
        digest_value_hex(tree.id().digest()),
    );

    let root_json = canonical_root_json(&computed.root, computed.root_id)?;
    let parsed_root = parse_canonical_root_json(&root_json)?;
    assert_eq!(parsed_root.root_id, root_hex(computed.root_id));
    assert_ne!(parsed_tree.schema, parsed_root.schema);

    let v1: serde_json::Value = serde_json::from_str(V1_GOLDEN)?;
    assert_eq!(v1["schema_version"], 1);
    assert_eq!(
        v1["artifacts"]["base_root_hash"],
        "541721df8cd996a3d2294d81f1132b62fa15ca89fe034feab24d6270a7e40458",
    );
    assert_eq!(
        v1["artifacts"]["manifest_sha256"],
        "2766e1a6da54dfd017515b674361bcf32e4d697ca0cbc17244d20b6b087ee85b",
    );
    Ok(())
}

fn portable_spool_path(label: &str) -> PathBuf {
    let sequence = PORTABLE_SPOOL_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "sandbox-layerstack-prc-r03-{}-{sequence}-{label}",
        std::process::id(),
    ))
}

fn seeded_permutation(
    inputs: &[PortablePreparationInput],
    mut seed: u64,
) -> Vec<PortablePreparationInput> {
    let mut permuted = inputs.to_vec();
    for upper in (1..permuted.len()).rev() {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let index = usize::try_from(seed % u64::try_from(upper + 1).unwrap_or(1)).unwrap_or(0);
        permuted.swap(upper, index);
    }
    permuted
}

#[test]
fn prc_r03_layerstack_preparation_is_insertion_order_independent() -> Result<(), Box<dyn StdError>>
{
    let mut canonical_entries = Vec::with_capacity(17);
    for index in 0_u8..17 {
        canonical_entries.push(regular(
            format!("prepared/entry-{index:02}").into_bytes(),
            4_096,
            file_segments(index.saturating_add(1)),
        )?);
    }
    let expected_bytes = encode_tree(&canonical_entries)?;

    let mut inputs = Vec::with_capacity(20);
    inputs.push(PortablePreparationInput::BackendMarker(
        PortableBackendMarker::LinuxWhiteout,
    ));
    inputs.extend(
        canonical_entries
            .iter()
            .cloned()
            .map(Box::new)
            .map(PortablePreparationInput::Entry),
    );
    inputs.push(PortablePreparationInput::Entry(Box::new(
        canonical_entries[8].clone(),
    )));
    inputs.push(PortablePreparationInput::BackendMarker(
        PortableBackendMarker::OpaqueDirectory,
    ));

    let mut reversed = inputs.clone();
    reversed.reverse();
    let variants = [
        ("forward", inputs.clone()),
        ("reverse", reversed),
        ("seeded", seeded_permutation(&inputs, SEED)),
    ];
    let expected_stats = PortablePreparationStats {
        input_items: 20,
        linux_whiteouts_filtered: 1,
        opaque_directories_filtered: 1,
        initial_runs: 18,
        merge_passes: 2,
        max_merge_fan_in: 8,
        coalesced_duplicates: 1,
        output_entries: 17,
    };
    for (label, variant) in variants {
        let spool = portable_spool_path(label);
        assert!(!spool.exists());
        let prepared = prepare_tiny_portable_tree(&variant, &spool)?;
        assert_eq!(prepared.stats(), expected_stats);
        assert_eq!(prepared.entry_count(), 17);
        let mut sink = VecSink::default();
        let _required_capabilities = prepared.encode(&mut sink)?;
        assert_eq!(sink.bytes, expected_bytes);
        assert!(!spool.exists());
    }

    let conflicting = vec![
        PortablePreparationInput::Entry(Box::new(regular(
            b"prepared/conflict".to_vec(),
            1,
            file_segments(0xa1),
        )?)),
        PortablePreparationInput::Entry(Box::new(regular(
            b"prepared/conflict".to_vec(),
            1,
            file_segments(0xa2),
        )?)),
    ];
    let conflict_spool = portable_spool_path("conflict");
    assert!(!conflict_spool.exists());
    let conflict = prepare_tiny_portable_tree(&conflicting, &conflict_spool);
    assert!(matches!(
        conflict,
        Err(PortablePreparationError::ConflictingDuplicate)
    ));
    assert!(!conflict_spool.exists());
    Ok(())
}

fn prepare_benchmark_corpus() -> Result<Vec<BenchmarkVector>, Error> {
    assert_eq!(
        PORTABLE_ROOT_R08_ORACLE.len(),
        EXPECTED_BENCHMARK_VECTORS
            .iter()
            .map(|vector| vector.canonical_bytes_len)
            .sum::<usize>(),
    );
    assert_eq!(
        sha256_hex(PORTABLE_ROOT_R08_ORACLE),
        PORTABLE_ROOT_R08_ORACLE_SHA256,
    );

    let mut corpus = Vec::with_capacity(SPECS.len());
    let owners = LogicalOwnerCounter::default();
    let mut next_offset = 0_usize;
    for ((spec, expected), index) in SPECS
        .into_iter()
        .zip(EXPECTED_BENCHMARK_VECTORS)
        .zip(0_u32..)
    {
        assert_eq!(spec.name, expected.name);
        assert_eq!(expected.canonical_bytes_offset, next_offset);
        let end = expected
            .canonical_bytes_offset
            .checked_add(expected.canonical_bytes_len)
            .ok_or_else(|| benchmark_error(FieldClass::Length, index))?;
        let canonical_bytes = PORTABLE_ROOT_R08_ORACLE
            .get(expected.canonical_bytes_offset..end)
            .ok_or_else(|| benchmark_error(FieldClass::Record, index))?;
        assert!(expected.tree_bytes_len < canonical_bytes.len());
        assert_eq!(sha256_hex(canonical_bytes), expected.canonical_bytes_sha256,);
        let (root_id, _peak_codec_scratch_bytes) =
            verify_encoded_vector(canonical_bytes, expected.tree_bytes_len, &owners)?;
        assert_eq!(owners.live(), 0);
        assert_eq!(root_hex(root_id), expected.root_id);
        if canonical_bytes.len() > 262_144 {
            return Err(Error::new(
                ErrorKind::LimitExceeded,
                ROOT_FORMAT_V2,
                FieldClass::Record,
                index,
            ));
        }
        corpus.push(BenchmarkVector {
            spec,
            tree_bytes_len: expected.tree_bytes_len,
            canonical_bytes,
            root_id,
        });
        next_offset = end;
    }
    assert_eq!(next_offset, PORTABLE_ROOT_R08_ORACLE.len());
    Ok(corpus)
}

fn benchmark_error(field: FieldClass, ordinal: u32) -> Error {
    Error::new(ErrorKind::Malformed, ROOT_FORMAT_V2, field, ordinal)
}

fn verify_encoded_vector(
    canonical_bytes: &[u8],
    tree_bytes_len: usize,
    owners: &LogicalOwnerCounter,
) -> Result<(RootId, u64), Error> {
    let owner_floor = owners.live();
    let tree_bytes = canonical_bytes
        .get(..tree_bytes_len)
        .ok_or_else(|| benchmark_error(FieldClass::Tree, 0))?;
    let root_bytes = canonical_bytes
        .get(tree_bytes_len..)
        .ok_or_else(|| benchmark_error(FieldClass::Record, 0))?;

    let mut decoded_entry_count = 0_u64;
    let mut tree_source = BytesSource::new(tree_bytes);
    decode_tree_record(&mut tree_source, &mut |_entry| {
        let _entry_owner = owners.enter();
        decoded_entry_count = decoded_entry_count
            .checked_add(1)
            .ok_or_else(|| benchmark_error(FieldClass::Tree, u32::MAX))?;
        Ok(())
    })?;
    let mut peak_codec_scratch_bytes =
        u64::try_from(tree_source.peak_read_bytes()).unwrap_or(u64::MAX);
    assert_eq!(owners.live(), owner_floor);

    let (tree_value, validation_peak) =
        validate_tree_bytes_with_peak(tree_bytes, &mut Sha256Digest)?;
    let tree = owners.track(tree_value);
    assert_eq!(decoded_entry_count, tree.entry_count());
    peak_codec_scratch_bytes = peak_codec_scratch_bytes.max(validation_peak);

    let mut root_source = BytesSource::new(root_bytes);
    let root = owners.track(decode_root_record(&mut root_source, &tree)?);
    peak_codec_scratch_bytes = peak_codec_scratch_bytes
        .max(u64::try_from(root_source.peak_read_bytes()).unwrap_or(u64::MAX));
    assert_eq!(root.tree_manifest(), tree.id());
    let root_id = identify_root(&root, &mut Sha256Digest)?;

    drop(root);
    drop(tree);
    assert_eq!(owners.live(), owner_floor);
    Ok((root_id, peak_codec_scratch_bytes))
}

fn require_id(value: Option<RootId>, ordinal: u32) -> Result<RootId, Error> {
    value.ok_or_else(|| benchmark_error(FieldClass::Digest, ordinal))
}

fn control_iteration(corpus: &[BenchmarkVector]) -> Result<(RootId, RootId, u64, u64, u64), Error> {
    let owners = LogicalOwnerCounter::default();
    let mut first = None;
    let mut last = None;
    let mut bytes = 0_u64;
    let mut peak_codec_scratch_bytes = 0_u64;
    for vector in corpus {
        let (root_id, peak) =
            verify_encoded_vector(vector.canonical_bytes, vector.tree_bytes_len, &owners)?;
        assert_eq!(root_id, vector.root_id);
        peak_codec_scratch_bytes = peak_codec_scratch_bytes.max(peak);
        assert_eq!(owners.live(), 0);
        first.get_or_insert(vector.root_id);
        last = Some(vector.root_id);
        bytes =
            bytes.saturating_add(u64::try_from(vector.canonical_bytes.len()).unwrap_or(u64::MAX));
    }
    Ok((
        require_id(first, 1)?,
        require_id(last, 2)?,
        bytes,
        peak_codec_scratch_bytes,
        owners.live(),
    ))
}

fn candidate_iteration(
    corpus: &[BenchmarkVector],
) -> Result<(RootId, RootId, u64, u64, u64), Error> {
    let owners = LogicalOwnerCounter::default();
    let mut first = None;
    let mut last = None;
    let mut bytes = 0_u64;
    let mut peak_codec_scratch_bytes = 0_u64;
    for vector in corpus {
        let computed = owners.track(compute_vector(vector.spec)?);
        assert_eq!(computed.canonical_bytes.as_slice(), vector.canonical_bytes);
        assert_eq!(computed.tree_bytes_len, vector.tree_bytes_len);
        assert_eq!(computed.root_id, vector.root_id);
        let (decoded_root_id, decode_peak) =
            verify_encoded_vector(&computed.canonical_bytes, computed.tree_bytes_len, &owners)?;
        assert_eq!(decoded_root_id, vector.root_id);
        peak_codec_scratch_bytes = peak_codec_scratch_bytes
            .max(computed.peak_codec_scratch_bytes)
            .max(decode_peak);
        first.get_or_insert(computed.root_id);
        last = Some(computed.root_id);
        bytes =
            bytes.saturating_add(u64::try_from(computed.canonical_bytes.len()).unwrap_or(u64::MAX));
        drop(computed);
        assert_eq!(owners.live(), 0);
    }
    Ok((
        require_id(first, 3)?,
        require_id(last, 4)?,
        bytes,
        peak_codec_scratch_bytes,
        owners.live(),
    ))
}

#[derive(Clone, Copy)]
enum ArmMode {
    Control,
    Candidate,
}

impl ArmMode {
    const fn name(self) -> &'static str {
        match self {
            Self::Control => "control",
            Self::Candidate => "candidate",
        }
    }
}

fn execute_iteration(
    mode: ArmMode,
    corpus: &[BenchmarkVector],
) -> Result<(RootId, RootId, u64, u64, u64), Error> {
    match mode {
        ArmMode::Control => control_iteration(corpus),
        ArmMode::Candidate => candidate_iteration(corpus),
    }
}

fn calibration_ns(mode: ArmMode, corpus: &[BenchmarkVector]) -> Result<(u64, u64), Error> {
    let mut iterations = 1_u64;
    loop {
        let start = Instant::now();
        for _ in 0..iterations {
            let _ = execute_iteration(mode, corpus)?;
        }
        let elapsed = u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX);
        if elapsed >= 100_000_000 || iterations >= 1_048_576 {
            return Ok((elapsed.max(1), iterations));
        }
        iterations = iterations.saturating_mul(2);
    }
}

#[derive(Serialize)]
struct Arm {
    mode: &'static str,
    iterations: u64,
    encoded_bytes: u64,
    elapsed_ns: u64,
    throughput_bytes_per_second: f64,
    first_root_id: String,
    last_root_id: String,
    errors: u64,
    peak_codec_scratch_bytes: u64,
    retained_logical_owners: u64,
    canonical_bytes_match: bool,
    identifiers_match: bool,
}

fn run_arm(mode: ArmMode, iterations: u64, corpus: &[BenchmarkVector]) -> Result<Arm, Error> {
    let start = Instant::now();
    let mut first = None;
    let mut last = None;
    let mut bytes_per_iteration = 0_u64;
    let mut peak_codec_scratch_bytes = 0_u64;
    let mut retained_logical_owners = 0_u64;
    for _ in 0..iterations {
        let (iteration_first, iteration_last, bytes, peak, retained) =
            execute_iteration(mode, corpus)?;
        first.get_or_insert(iteration_first);
        last = Some(iteration_last);
        bytes_per_iteration = bytes;
        peak_codec_scratch_bytes = peak_codec_scratch_bytes.max(peak);
        retained_logical_owners = retained_logical_owners.max(retained);
    }
    assert_eq!(retained_logical_owners, 0);
    let elapsed_ns = u64::try_from(start.elapsed().as_nanos())
        .unwrap_or(u64::MAX)
        .max(1);
    let encoded_bytes = bytes_per_iteration.saturating_mul(iterations);
    Ok(Arm {
        mode: mode.name(),
        iterations,
        encoded_bytes,
        elapsed_ns,
        throughput_bytes_per_second: encoded_bytes as f64 * 1_000_000_000_f64 / elapsed_ns as f64,
        first_root_id: root_hex(require_id(first, 5)?),
        last_root_id: root_hex(require_id(last, 6)?),
        errors: 0,
        peak_codec_scratch_bytes,
        retained_logical_owners,
        canonical_bytes_match: true,
        identifiers_match: true,
    })
}

#[derive(Serialize)]
struct Pair {
    pair_index: u64,
    warmup: bool,
    order: [&'static str; 2],
    control: Arm,
    candidate: Arm,
    candidate_to_control_elapsed_ratio: f64,
    candidate_to_control_throughput_ratio: f64,
}

#[derive(Serialize)]
struct CorpusRow {
    name: &'static str,
    canonical_bytes_offset: usize,
    tree_bytes_len: usize,
    canonical_bytes_len: usize,
    canonical_bytes_sha256: &'static str,
    root_id: &'static str,
}

#[test]
#[ignore = "PRC-R08 bounded 30-60 second diagnostic benchmark"]
fn portable_root_tiny_loop() -> Result<(), Box<dyn StdError>> {
    let corpus = prepare_benchmark_corpus()?;
    let (control_elapsed, control_iterations) = calibration_ns(ArmMode::Control, &corpus)?;
    let (candidate_elapsed, candidate_iterations) = calibration_ns(ArmMode::Candidate, &corpus)?;
    let control_per_iteration = control_elapsed as f64 / control_iterations as f64;
    let candidate_per_iteration = candidate_elapsed as f64 / candidate_iterations as f64;
    let iterations = (6_000_000_000_f64 / (control_per_iteration + candidate_per_iteration))
        .ceil()
        .max(1_f64) as u64;

    let mut pairs = Vec::with_capacity(6);
    for pair_index in 0_u64..6 {
        let even = pair_index % 2 == 0;
        let (control, candidate) = if even {
            (
                run_arm(ArmMode::Control, iterations, &corpus)?,
                run_arm(ArmMode::Candidate, iterations, &corpus)?,
            )
        } else {
            let candidate = run_arm(ArmMode::Candidate, iterations, &corpus)?;
            let control = run_arm(ArmMode::Control, iterations, &corpus)?;
            (control, candidate)
        };
        let elapsed_ratio = candidate.elapsed_ns as f64 / control.elapsed_ns as f64;
        let throughput_ratio =
            candidate.throughput_bytes_per_second / control.throughput_bytes_per_second;
        pairs.push(Pair {
            pair_index,
            warmup: pair_index == 0,
            order: if even {
                ["control", "candidate"]
            } else {
                ["candidate", "control"]
            },
            control,
            candidate,
            candidate_to_control_elapsed_ratio: elapsed_ratio,
            candidate_to_control_throughput_ratio: throughput_ratio,
        });
    }

    let aggregate_elapsed_ns = pairs
        .iter()
        .map(|pair| pair.control.elapsed_ns + pair.candidate.elapsed_ns)
        .sum::<u64>();
    assert!((30_000_000_000..=60_000_000_000).contains(&aggregate_elapsed_ns));
    let all_operations_under_60s = pairs.iter().all(|pair| {
        pair.control.elapsed_ns < 60_000_000_000 && pair.candidate.elapsed_ns < 60_000_000_000
    });
    assert!(all_operations_under_60s);
    let total_iterations = pairs
        .iter()
        .map(|pair| pair.control.iterations + pair.candidate.iterations)
        .sum::<u64>();
    let total_encoded_bytes = pairs
        .iter()
        .map(|pair| pair.control.encoded_bytes + pair.candidate.encoded_bytes)
        .sum::<u64>();
    let peak_codec_scratch_bytes = pairs
        .iter()
        .flat_map(|pair| {
            [
                pair.control.peak_codec_scratch_bytes,
                pair.candidate.peak_codec_scratch_bytes,
            ]
        })
        .max()
        .unwrap_or(0);
    assert!(peak_codec_scratch_bytes <= 262_144);
    let retained_logical_owners = pairs
        .iter()
        .flat_map(|pair| {
            [
                pair.control.retained_logical_owners,
                pair.candidate.retained_logical_owners,
            ]
        })
        .sum::<u64>();
    assert_eq!(retained_logical_owners, 0);

    let vectors = EXPECTED_BENCHMARK_VECTORS.map(|row| CorpusRow {
        name: row.name,
        canonical_bytes_offset: row.canonical_bytes_offset,
        tree_bytes_len: row.tree_bytes_len,
        canonical_bytes_len: row.canonical_bytes_len,
        canonical_bytes_sha256: row.canonical_bytes_sha256,
        root_id: row.root_id,
    });
    let payload = serde_json::json!({
        "schema_version": 1,
        "case_id": "PRC-R08",
        "corpus": {
            "version": "v2",
            "seed": SEED,
            "stage00": {
                "schema_version": 1,
                "corpus_version": "v1",
                "seed": STAGE00_CORPUS_SEED,
                "manifest_sha256": STAGE00_MANIFEST_SHA256,
                "workload_ids": STAGE00_WORKLOAD_IDS
            },
            "vectors": vectors
        },
        "canonical_oracle": {
            "format": PORTABLE_ROOT_R08_ORACLE_FORMAT,
            "decision_id": DECISION_ID,
            "byte_count": PORTABLE_ROOT_R08_ORACLE.len(),
            "sha256": PORTABLE_ROOT_R08_ORACLE_SHA256
        },
        "protocol": {
            "warmup_pairs": 1,
            "measured_pairs": 5,
            "counterbalanced": true,
            "operation_timeout_ms": 60_000,
            "aggregate_min_ms": 30_000,
            "aggregate_max_ms": 60_000,
            "scratch_limit_bytes": 262_144
        },
        "pairs": pairs,
        "summary": {
            "measured_pair_count": 5,
            "total_iterations": total_iterations,
            "total_encoded_bytes": total_encoded_bytes,
            "aggregate_elapsed_ns": aggregate_elapsed_ns,
            "all_operations_under_60s": all_operations_under_60s,
            "all_canonical_bytes_match": true,
            "all_identifiers_match": true,
            "total_errors": 0,
            "peak_codec_scratch_bytes": peak_codec_scratch_bytes,
            "retained_logical_owners": retained_logical_owners
        },
        "environment": {
            "clock": "std::time::Instant",
            "thread_count": 1
        }
    });
    let json = serde_json::to_string(&payload)?;
    println!("\nPRC_R08_JSON:{json}");
    Ok(())
}
