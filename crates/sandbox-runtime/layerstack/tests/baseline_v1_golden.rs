#![cfg(unix)]

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use sandbox_runtime_layerstack::{
    build_workspace_base, emit_delta_stream, layer_digest, manifest_root_hash, DeltaWinner,
    LayerChange, LayerPath, LayerRef, LayerStack, Manifest, ACTIVE_MANIFEST_FILE, LAYERS_DIR,
    LAYER_METADATA_DIR, STAGING_DIR, WORKSPACE_BASE_LAYER_ID,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

const GOLDEN: &str = include_str!("fixtures/v1/baseline.json");
const TEST_FAILPOINTS_ENV: &str = "SANDBOX_LAYERSTACK_ENABLE_TEST_FAILPOINTS";
const TEST_FAILPOINT_STAGE_ENV: &str = "SANDBOX_LAYERSTACK_TEST_FAILPOINT_STAGE";

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

struct Fixture {
    base: PathBuf,
    root: PathBuf,
    workspace: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> TestResult<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let base = std::env::temp_dir().join(format!(
            "layerstack-v1-golden-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("layer-stack");
        let workspace = base.join("workspace");
        std::fs::create_dir_all(&workspace)?;
        Ok(Self {
            base,
            root,
            workspace,
        })
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

struct EnvGuard {
    enable: Option<std::ffi::OsString>,
    stage: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn enable() -> Self {
        let guard = Self {
            enable: std::env::var_os(TEST_FAILPOINTS_ENV),
            stage: std::env::var_os(TEST_FAILPOINT_STAGE_ENV),
        };
        std::env::set_var(TEST_FAILPOINTS_ENV, "1");
        std::env::remove_var(TEST_FAILPOINT_STAGE_ENV);
        guard
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        restore_env(TEST_FAILPOINTS_ENV, self.enable.take());
        restore_env(TEST_FAILPOINT_STAGE_ENV, self.stage.take());
    }
}

fn restore_env(name: &str, value: Option<std::ffi::OsString>) {
    if let Some(value) = value {
        std::env::set_var(name, value);
    } else {
        std::env::remove_var(name);
    }
}

fn golden() -> Value {
    serde_json::from_str(GOLDEN).expect("valid v1 golden fixture")
}

fn expected(group: &str, name: &str) -> String {
    golden()[group][name]
        .as_str()
        .unwrap_or_else(|| panic!("missing {group}.{name}"))
        .to_owned()
}

fn assert_golden(group: &str, name: &str, actual: impl AsRef<str>) {
    assert_eq!(
        actual.as_ref(),
        expected(group, name),
        "frozen v1 mismatch for {group}.{name}"
    );
}

fn path(value: &str) -> LayerPath {
    LayerPath::parse(value).expect("valid fixture path")
}

fn write(path_value: &str, content: Vec<u8>) -> LayerChange {
    LayerChange::Write {
        path: path(path_value),
        content,
    }
}

fn deterministic_bytes(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed;
    let mut bytes = Vec::with_capacity(len);
    for _ in 0..len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        bytes.push((state >> 24) as u8);
    }
    bytes
}

fn sha256(bytes: impl AsRef<[u8]>) -> String {
    let mut digest = Sha256::new();
    digest.update(bytes.as_ref());
    format!("{:x}", digest.finalize())
}

fn sorted_names(path: &Path) -> TestResult<Vec<String>> {
    let mut names = std::fs::read_dir(path)?
        .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
        .collect::<Result<Vec<_>, std::io::Error>>()?;
    names.sort();
    Ok(names)
}

fn canonical_tree(root: &Path) -> TestResult<(String, String)> {
    fn walk(root: &Path, current: &Path, tree: &mut String, content: &mut Sha256) -> TestResult {
        let mut entries = std::fs::read_dir(current)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let entry_path = entry.path();
            let rel = entry_path
                .strip_prefix(root)?
                .to_string_lossy()
                .replace('\\', "/");
            let metadata = std::fs::symlink_metadata(&entry_path)?;
            if metadata.file_type().is_symlink() {
                let target = std::fs::read_link(&entry_path)?;
                tree.push_str(&format!("L\0{rel}\0{}\n", target.to_string_lossy()));
            } else if metadata.is_dir() {
                tree.push_str(&format!("D\0{rel}\n"));
                walk(root, &entry_path, tree, content)?;
            } else if metadata.is_file() {
                let bytes = std::fs::read(&entry_path)?;
                let digest = sha256(&bytes);
                tree.push_str(&format!("F\0{rel}\0{}\0{digest}\n", bytes.len()));
                content.update(rel.as_bytes());
                content.update(b"\0");
                content.update(&bytes);
                content.update(b"\0");
            }
        }
        Ok(())
    }

    let mut tree = String::new();
    let mut content = Sha256::new();
    walk(root, root, &mut tree, &mut content)?;
    Ok((sha256(tree.as_bytes()), format!("{:x}", content.finalize())))
}

fn set_mtime(path: &Path, seconds: u64) -> TestResult {
    let stamp = SystemTime::UNIX_EPOCH + Duration::from_secs(seconds);
    let handle = std::fs::File::open(path)?;
    handle.set_times(std::fs::FileTimes::new().set_modified(stamp))?;
    Ok(())
}

fn create_base_corpus(workspace: &Path) -> TestResult<bool> {
    std::fs::create_dir_all(workspace.join("bin"))?;
    std::fs::create_dir_all(workspace.join("data"))?;
    std::fs::create_dir_all(workspace.join("links"))?;
    std::fs::write(workspace.join("regular.txt"), b"legacy-v1\n")?;
    std::fs::write(workspace.join("bin/tool.sh"), b"#!/bin/sh\nexit 0\n")?;
    std::fs::set_permissions(
        workspace.join("bin/tool.sh"),
        std::fs::Permissions::from_mode(0o751),
    )?;
    std::fs::write(
        workspace.join("data/hard-a.bin"),
        deterministic_bytes(4096, 0x4841_5244),
    )?;
    std::fs::hard_link(
        workspace.join("data/hard-a.bin"),
        workspace.join("data/hard-b.bin"),
    )?;
    let mut sparse = std::fs::File::create(workspace.join("data/sparse.bin"))?;
    sparse.set_len(65_537)?;
    sparse.seek(SeekFrom::Start(65_536))?;
    sparse.write_all(&[0x7f])?;
    std::os::unix::fs::symlink("../regular.txt", workspace.join("links/regular"))?;
    let xattr_set = rustix::fs::setxattr(
        workspace.join("regular.txt"),
        "user.layerstack_phase1",
        b"v1",
        rustix::fs::XattrFlags::empty(),
    )
    .is_ok();
    Ok(xattr_set)
}

#[test]
fn deterministic_v1_hash_corpus_matches_frozen_values() {
    let mut localized = vec![b'a'; 1024 * 1024];
    localized[524_288..525_312].copy_from_slice(&deterministic_bytes(1024, 0x4c4f_4341));

    let mut small_files = Vec::with_capacity(256);
    for index in 0..256 {
        small_files.push(write(
            &format!("small/{index:03}.bin"),
            deterministic_bytes(4096, 0x534d_414c + index),
        ));
    }

    let cases = [
        ("empty_no_op", layer_digest(&[])),
        ("one_byte", layer_digest(&[write("one.bin", vec![0xa5])])),
        (
            "boundary_64_kib",
            layer_digest(&[write(
                "boundary.bin",
                deterministic_bytes(64 * 1024, 0x424f_554e),
            )]),
        ),
        (
            "regular_file",
            layer_digest(&[write("regular.txt", b"legacy-v1\n".to_vec())]),
        ),
        (
            "localized_edit",
            layer_digest(&[write("localized.bin", localized)]),
        ),
        (
            "incompressible_1_mib",
            layer_digest(&[write(
                "incompressible.bin",
                deterministic_bytes(1024 * 1024, 0x494e_434f),
            )]),
        ),
        ("small_files_256", layer_digest(&small_files)),
        (
            "symlink",
            layer_digest(&[LayerChange::Symlink {
                path: path("links/current"),
                source_path: "../regular.txt".to_owned(),
            }]),
        ),
        (
            "whiteout_opaque",
            layer_digest(&[
                LayerChange::Delete {
                    path: path("gone.txt"),
                },
                LayerChange::OpaqueDir {
                    path: path("cleared"),
                },
            ]),
        ),
        (
            "ordered_multilayer_root",
            manifest_root_hash(
                &Manifest::new(
                    3,
                    vec![
                        LayerRef {
                            layer_id: "L000003-cccc3333".to_owned(),
                            path: "layers/L000003-cccc3333".to_owned(),
                        },
                        LayerRef {
                            layer_id: "L000002-bbbb2222".to_owned(),
                            path: "layers/L000002-bbbb2222".to_owned(),
                        },
                        LayerRef {
                            layer_id: WORKSPACE_BASE_LAYER_ID.to_owned(),
                            path: format!("layers/{WORKSPACE_BASE_LAYER_ID}"),
                        },
                    ],
                    1,
                )
                .expect("v1 manifest"),
            ),
        ),
    ];

    for (name, actual) in cases {
        assert_golden("corpus", name, actual);
    }
}

#[test]
fn base_tree_manifest_content_and_metadata_match_frozen_values() -> TestResult {
    let fixture = Fixture::new("base")?;
    let xattr_set = create_base_corpus(&fixture.workspace)?;
    let source_hard = std::fs::metadata(fixture.workspace.join("data/hard-a.bin"))?;
    let binding = build_workspace_base(&fixture.root, &fixture.workspace, false)?;
    let layer = fixture.root.join(LAYERS_DIR).join(WORKSPACE_BASE_LAYER_ID);

    assert_golden("artifacts", "base_root_hash", &binding.base_root_hash);
    assert_golden(
        "artifacts",
        "manifest_sha256",
        sha256(std::fs::read(fixture.root.join(ACTIVE_MANIFEST_FILE))?),
    );
    let (tree_digest, content_digest) = canonical_tree(&layer)?;
    assert_golden("artifacts", "tree_sha256", tree_digest);
    assert_golden("artifacts", "content_sha256", content_digest);

    assert_eq!(
        std::fs::metadata(layer.join("bin/tool.sh"))?
            .permissions()
            .mode()
            & 0o7777,
        0o751
    );
    let target_hard_a = std::fs::metadata(layer.join("data/hard-a.bin"))?;
    let target_hard_b = std::fs::metadata(layer.join("data/hard-b.bin"))?;
    assert_eq!(source_hard.nlink(), 2);
    assert_ne!(target_hard_a.ino(), target_hard_b.ino());
    assert_eq!(
        std::fs::read(layer.join("data/sparse.bin"))?,
        std::fs::read(fixture.workspace.join("data/sparse.bin"))?
    );
    if xattr_set {
        let mut value = [0_u8; 16];
        assert!(rustix::fs::lgetxattr(
            layer.join("regular.txt"),
            "user.layerstack_phase1",
            &mut value
        )
        .is_err());
    }
    for rel in [
        "regular.txt",
        "bin/tool.sh",
        "data/hard-a.bin",
        "data/hard-b.bin",
        "data/sparse.bin",
    ] {
        assert_eq!(
            std::fs::metadata(fixture.workspace.join(rel))?.uid(),
            std::fs::metadata(layer.join(rel))?.uid()
        );
    }

    let metadata_facts = concat!(
        "file_mode=preserved\n",
        "directory_mode=not_explicitly_carried\n",
        "owner=not_explicitly_carried\n",
        "hardlink=lossy_independent_files\n",
        "sparse=lossy_byte_stream\n",
        "xattr=lossy_not_carried\n",
        "export_uid_gid=unset\n",
        "export_hardlink=lossy_regular_entries\n"
    );
    assert_golden("artifacts", "metadata_sha256", sha256(metadata_facts));
    Ok(())
}

#[test]
fn export_bytes_and_lossy_metadata_match_frozen_values() -> TestResult {
    let fixture = Fixture::new("export")?;
    let source = fixture.base.join("source");
    let opaque = source.join("opaque");
    std::fs::create_dir_all(&opaque)?;
    std::fs::set_permissions(&opaque, std::fs::Permissions::from_mode(0o701))?;

    let payload = source.join("payload.bin");
    std::fs::write(&payload, deterministic_bytes(64 * 1024, 0x4558_504f))?;
    std::fs::set_permissions(&payload, std::fs::Permissions::from_mode(0o640))?;
    set_mtime(&payload, 1_700_000_123)?;
    let _ = rustix::fs::setxattr(
        &payload,
        "user.layerstack_phase1",
        b"not-exported",
        rustix::fs::XattrFlags::empty(),
    );

    let sparse = source.join("sparse.bin");
    let mut sparse_file = std::fs::File::create(&sparse)?;
    sparse_file.set_len(65_537)?;
    sparse_file.seek(SeekFrom::Start(65_536))?;
    sparse_file.write_all(&[0x7f])?;
    drop(sparse_file);
    std::fs::set_permissions(&sparse, std::fs::Permissions::from_mode(0o600))?;
    set_mtime(&sparse, 1_700_000_124)?;
    set_mtime(&opaque, 1_700_000_125)?;

    let mut winners = BTreeMap::new();
    winners.insert(
        path("hard-a.bin"),
        DeltaWinner::File {
            source: payload.clone(),
        },
    );
    winners.insert(path("hard-b.bin"), DeltaWinner::File { source: payload });
    winners.insert(path("gone.txt"), DeltaWinner::Delete);
    winners.insert(path("opaque"), DeltaWinner::OpaqueDir { source: opaque });
    winners.insert(path("sparse.bin"), DeltaWinner::File { source: sparse });

    let spool = fixture.base.join("baseline.tar.zst");
    let stats = emit_delta_stream(&winners, &spool, 3)?;
    assert_eq!(stats.files, 3);
    assert_eq!(stats.whiteouts, 1);
    assert_eq!(stats.opaques, 1);
    assert_golden(
        "artifacts",
        "export_tar_zst_sha256",
        sha256(std::fs::read(&spool)?),
    );

    let decoder = zstd::stream::read::Decoder::new(std::fs::File::open(&spool)?)?;
    let mut archive = tar::Archive::new(decoder);
    let mut entries = Vec::new();
    for entry in archive.entries()? {
        let mut entry = entry?;
        let header = entry.header().clone();
        let path = entry.path()?.to_string_lossy().into_owned();
        let mut content = Vec::new();
        entry.read_to_end(&mut content)?;
        entries.push((
            path,
            header.entry_type(),
            header.uid().ok(),
            header.gid().ok(),
            header.size()?,
            content,
        ));
    }
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.0.as_str())
            .collect::<Vec<_>>(),
        vec![
            ".wh.gone.txt",
            "hard-a.bin",
            "hard-b.bin",
            "opaque/",
            "opaque/.wh..wh..opq",
            "sparse.bin"
        ]
    );
    assert!(entries
        .iter()
        .filter(|entry| entry.0 == "hard-a.bin" || entry.0 == "hard-b.bin")
        .all(|entry| entry.1 == tar::EntryType::Regular));
    assert!(entries
        .iter()
        .all(|entry| entry.2.is_none() && entry.3.is_none()));
    assert_eq!(
        entries
            .iter()
            .find(|entry| entry.0 == "sparse.bin")
            .expect("sparse entry")
            .4,
        65_537
    );
    Ok(())
}

#[test]
fn publication_failpoints_never_expose_partial_manifests_and_recover() -> TestResult {
    let _env = EnvGuard::enable();
    for stage in [
        "staging_fsync",
        "layer_rename",
        "metadata",
        "occ_reread",
        "manifest_replace",
    ] {
        let fixture = Fixture::new(stage)?;
        std::fs::write(fixture.workspace.join("base.txt"), b"base\n")?;
        build_workspace_base(&fixture.root, &fixture.workspace, false)?;
        let manifest_before = std::fs::read(fixture.root.join(ACTIVE_MANIFEST_FILE))?;
        let layers_before = sorted_names(&fixture.root.join(LAYERS_DIR))?;
        let metadata_before = sorted_names(&fixture.root.join(LAYER_METADATA_DIR))?;
        let mut stack = LayerStack::open(fixture.root.clone())?;
        let change = LayerChange::Write {
            path: path("published.txt"),
            content: b"committed\n".to_vec(),
        };

        std::env::set_var(TEST_FAILPOINT_STAGE_ENV, stage);
        let error = stack
            .publish_layer(std::slice::from_ref(&change))
            .expect_err("injected publish failure");
        std::env::remove_var(TEST_FAILPOINT_STAGE_ENV);
        assert!(error.to_string().contains(stage));
        assert_eq!(
            std::fs::read(fixture.root.join(ACTIVE_MANIFEST_FILE))?,
            manifest_before
        );
        assert_eq!(sorted_names(&fixture.root.join(LAYERS_DIR))?, layers_before);
        assert_eq!(
            sorted_names(&fixture.root.join(LAYER_METADATA_DIR))?,
            metadata_before
        );
        assert!(sorted_names(&fixture.root.join(STAGING_DIR))?.is_empty());

        if stage == "layer_rename" {
            std::fs::create_dir_all(
                fixture
                    .root
                    .join(STAGING_DIR)
                    .join("owned-recovery.staging"),
            )?;
            let sweep = stack.sweep_storage()?;
            assert_eq!(sweep.removed_staging_entries, 1);
            assert_eq!(sweep.skipped_reason, None);
        }

        let committed = stack.publish_layer(std::slice::from_ref(&change))?;
        assert_eq!(
            stack.read_bytes("published.txt")?,
            (Some(b"committed\n".to_vec()), true)
        );
        let deduped = stack.publish_layer(&[change])?;
        assert_eq!(deduped, committed);
        assert!(sorted_names(&fixture.root.join(STAGING_DIR))?.is_empty());
    }
    Ok(())
}
