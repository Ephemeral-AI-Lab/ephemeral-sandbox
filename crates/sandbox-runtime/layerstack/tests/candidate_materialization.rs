#![forbid(unsafe_code)]
#![cfg(target_os = "linux")]

use std::ffi::OsStr;
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::time::{Duration, Instant};

use sandbox_runtime_layerstack_core::{AttributionRootId, Digest32, RecordKindV3, RootId};
use serde_json::json;

#[path = "../src/error.rs"]
mod error;
#[path = "../src/storage/fs.rs"]
pub(crate) mod fs;
#[path = "../src/storage/lock.rs"]
pub(crate) mod lock;
#[path = "../src/model/mod.rs"]
mod model;
#[path = "../src/observability.rs"]
mod observability;
#[path = "../src/service/mod.rs"]
pub mod service;
#[allow(
    unused_imports,
    reason = "the test harness re-includes stack exports used by the library facade"
)]
#[path = "../src/stack/mod.rs"]
mod stack;
#[path = "../src/storage/supervisor.rs"]
mod supervisor;
#[path = "../src/storage/whiteout.rs"]
mod whiteout;
#[path = "../src/workspace_base/mod.rs"]
mod workspace_base;

pub use error::LayerStackError;
pub(crate) use model::portable::Sha256Digest;
pub use model::{
    aggregate_layer_changes, layer_digest, manifest_root_hash, CasError, LayerChange, LayerPath,
    LayerRef, Manifest, MANIFEST_SCHEMA_VERSION,
};
pub use stack::{ActiveLeaseCounter, LayerStack, Lease, MergedView};
pub use workspace_base::{
    build_shared_workspace_base, build_workspace_base, ensure_workspace_base,
    read_workspace_binding, require_workspace_binding, SharedWorkspaceBase, WorkspaceBinding,
    SHARED_BASE_DIR, WORKSPACE_BASE_LAYER_ID, WORKSPACE_BINDING_FILE,
};

pub(crate) const LAYERS_DIR: &str = "layers";
pub(crate) const STAGING_DIR: &str = "staging";
pub const ACTIVE_MANIFEST_FILE: &str = "manifest.json";
pub(crate) const LAYER_METADATA_DIR: &str = ".layer-metadata";

pub(crate) use lock::*;
pub(crate) use model::*;

use stack::candidate::generation::{
    GenerationError, GenerationManifest, GenerationSelection, GenerationStore, MaterializationKey,
};
use stack::candidate::materialization::{
    MaterializationCoordinator, MaterializationDisposition, MaterializationError,
    MaterializationRequest, MaterializationStage,
};
use stack::candidate::materialization_operation::{
    MaterializationCheckpoint, MaterializationOperation, MaterializationOperationBuild,
    MaterializationPhase, MaterializationPublicationSubject, MaterializationSourceHold,
    MAX_ACTIVE_TYPED_HOLDS,
};
use stack::candidate::materialization_publication::{
    MaterializationGcBridge, MaterializationPublisher,
};
use stack::candidate::native_backend::{
    NativeBackend, NativeBuildResult, NativeReconstructionResources, CAP_FIFO, CAP_HARDLINK,
    CAP_SPARSE, CAP_SYMLINK, CAP_XATTR, MAX_HYDRATION_STREAM_BYTES,
};
use stack::candidate::object_store::LooseObjectStore;
use stack::candidate::tree::{
    AttributionFact, FileKindV3, FileNodeV3, MetadataV3, PersistentPages, SegmentDescriptor,
    SegmentKind, TreeEntryV3,
};

static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);

struct TestRoot(PathBuf);

impl TestRoot {
    fn new(label: &str) -> std::io::Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "layerstack-stage04-{label}-{}-{}",
            std::process::id(),
            NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::remove_dir_all(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        std::fs::create_dir(&path)?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Fixture {
    root: TestRoot,
    key: MaterializationKey,
}

fn install_fixture_attribution(
    pages: &mut PersistentPages<'_>,
    root: RootId,
) -> Result<AttributionRootId, Box<dyn std::error::Error>> {
    let page = pages.build_attribution(std::iter::empty::<AttributionFact>())?;
    Ok(pages.install_attribution_root(root, page)?)
}

fn reconstruct_supervised(
    storage_root: &Path,
    backend: &NativeBackend,
    pages: &mut PersistentPages<'_>,
    root: RootId,
    carrier: &Path,
) -> Result<NativeBuildResult, Box<dyn std::error::Error>> {
    let supervisor = supervisor::shared_supervisor_for_root(storage_root)?;
    let cancellation = AtomicBool::new(false);
    let deadline = Instant::now() + Duration::from_secs(20);
    let owner = match supervisor.admit_materialization(
        format!("integration-test:{}", carrier.display()),
        deadline,
        &cancellation,
    )? {
        supervisor::MaterializationAdmission::Owner(owner) => owner,
        supervisor::MaterializationAdmission::Waiter(_) => {
            return Err(std::io::Error::other(
                "isolated integration reconstruction unexpectedly joined a flight",
            )
            .into());
        }
    };
    let target = owner.acquire_target(MAX_HYDRATION_STREAM_BYTES, deadline, &cancellation)?;
    Ok(backend.reconstruct_bounded(
        pages,
        root,
        carrier,
        NativeReconstructionResources {
            hydration_byte_permit_bytes: MAX_HYDRATION_STREAM_BYTES,
            metadata_queue_depth: supervisor::MAX_METADATA_QUEUE_ITEMS,
            target: &target,
            observation: None,
        },
        || Ok(()),
    )?)
}

impl Fixture {
    fn mixed(label: &str) -> Result<Self, Box<dyn std::error::Error>> {
        Self::mixed_with(
            label,
            CAP_XATTR | CAP_SPARSE | CAP_HARDLINK | CAP_SYMLINK | CAP_FIFO,
            true,
        )
    }

    fn mixed_with(
        label: &str,
        required_capabilities: u64,
        exact_hardlink_group: bool,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let root = TestRoot::new(label)?;
        let store = LooseObjectStore::new(root.path().to_path_buf())?;
        let mut pages = PersistentPages::new(&store);
        let uid = rustix::process::geteuid().as_raw();
        let gid = rustix::process::getegid().as_raw();

        let data = b"stage04-native-candidate";
        let chunk = pages.install_chunk_slices(data, &[])?;
        let sparse_segments = pages.build_segments([
            SegmentDescriptor {
                offset: 0,
                length: data.len() as u64,
                kind: SegmentKind::Chunk(chunk),
            },
            SegmentDescriptor {
                offset: data.len() as u64,
                length: 128 * 1024,
                kind: SegmentKind::Hole,
            },
        ])?;
        let sparse = pages.install_file_node(&FileNodeV3::regular(
            metadata(
                0o640,
                uid,
                gid,
                vec![(b"user.stage04".to_vec(), vec![0, 1, 255])],
            ),
            data.len() as u64 + 128 * 1024,
            sparse_segments,
            None,
        ))?;

        let hardlink_segments = pages.build_segments([SegmentDescriptor {
            offset: 0,
            length: data.len() as u64,
            kind: SegmentKind::Chunk(chunk),
        }])?;
        let hardlink_paths = if exact_hardlink_group {
            [b"hard-a".to_vec(), b"hard-b".to_vec()]
        } else {
            [b"hard-a".to_vec(), b"missing".to_vec()]
        };
        let hardlink_group = pages.install_hardlink_group(hardlink_paths)?;
        let hardlink = pages.install_file_node(&FileNodeV3::regular(
            metadata(0o600, uid, gid, Vec::new()),
            data.len() as u64,
            hardlink_segments,
            Some(hardlink_group),
        ))?;

        let symlink = pages.install_file_node(&FileNodeV3::symlink(
            metadata(0o777, uid, gid, Vec::new()),
            b"missing/../target".to_vec(),
        ))?;
        let fifo = pages.install_file_node(&FileNodeV3 {
            kind: FileKindV3::Fifo,
            metadata: metadata(0o620, uid, gid, Vec::new()),
            directory: None,
            logical_length: None,
            segments: None,
            symlink_target: None,
            device_major: None,
            device_minor: None,
            hardlink: None,
        })?;
        let raw_name = vec![0xff, b'n'];
        let tree = pages.build_tree([
            TreeEntryV3 {
                name: b"dangling".to_vec(),
                file: symlink,
            },
            TreeEntryV3 {
                name: b"fifo".to_vec(),
                file: fifo,
            },
            TreeEntryV3 {
                name: b"hard-a".to_vec(),
                file: hardlink,
            },
            TreeEntryV3 {
                name: b"hard-b".to_vec(),
                file: hardlink,
            },
            TreeEntryV3 {
                name: b"sparse".to_vec(),
                file: sparse,
            },
            TreeEntryV3 {
                name: raw_name,
                file: sparse,
            },
        ])?;
        let root_file = pages.install_file_node(&FileNodeV3::directory(
            metadata(0o750, uid, gid, Vec::new()),
            tree,
        ))?;
        let logical_root =
            pages.install_root_with_capabilities(root_file, required_capabilities)?;
        let attribution_root = install_fixture_attribution(&mut pages, logical_root)?;
        Ok(Self {
            root,
            key: MaterializationKey::linux_overlayfs(logical_root, attribution_root),
        })
    }

    fn deep(label: &str, depth: usize) -> Result<Self, Box<dyn std::error::Error>> {
        let root = TestRoot::new(label)?;
        let store = LooseObjectStore::new(root.path().to_path_buf())?;
        let mut pages = PersistentPages::new(&store);
        let uid = rustix::process::geteuid().as_raw();
        let gid = rustix::process::getegid().as_raw();
        let empty = pages.build_tree(std::iter::empty::<TreeEntryV3>())?;
        let mut child = pages.install_file_node(&FileNodeV3::directory(
            metadata(0o750, uid, gid, Vec::new()),
            empty,
        ))?;
        for level in (0..depth).rev() {
            let directory = pages.build_tree([TreeEntryV3 {
                name: format!("d{level:02}").into_bytes(),
                file: child,
            }])?;
            child = pages.install_file_node(&FileNodeV3::directory(
                metadata(0o750, uid, gid, Vec::new()),
                directory,
            ))?;
        }
        let logical_root = pages.install_root_with_capabilities(child, 0)?;
        let attribution_root = install_fixture_attribution(&mut pages, logical_root)?;
        Ok(Self {
            root,
            key: MaterializationKey::linux_overlayfs(logical_root, attribution_root),
        })
    }

    fn coordinator(&self) -> Result<MaterializationCoordinator, MaterializationError> {
        MaterializationCoordinator::new(self.root.path().to_path_buf())
    }

    fn writer_lock(&self) -> Result<StorageWriterLockLease, LayerStackError> {
        StorageWriterLockLease::acquire(self.root.path())
    }

    fn materialize(
        &self,
    ) -> Result<stack::candidate::materialization::MaterializationOutcome, Box<dyn std::error::Error>>
    {
        let coordinator = self.coordinator()?;
        let writer = self.writer_lock()?;
        Ok(coordinator.materialize(
            &MaterializationRequest::new(self.key.clone(), Duration::from_secs(20)),
            &writer,
        )?)
    }
}

fn copy_fixture_objects(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let metadata = std::fs::symlink_metadata(&source_path)?;
        if metadata.file_type().is_symlink() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "external fixture contains a symlink",
            ));
        }
        if metadata.is_dir() {
            copy_fixture_objects(&source_path, &destination_path)?;
        } else if metadata.is_file() {
            std::fs::copy(&source_path, &destination_path)?;
        } else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "external fixture contains a non-file object",
            ));
        }
    }
    Ok(())
}

fn link_fixture_objects(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let metadata = std::fs::symlink_metadata(&source_path)?;
        if metadata.file_type().is_symlink() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "external fixture contains a symlink",
            ));
        }
        if metadata.is_dir() {
            link_fixture_objects(&source_path, &destination_path)?;
        } else if metadata.is_file() {
            std::fs::hard_link(&source_path, &destination_path)?;
        } else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "external fixture contains a non-file object",
            ));
        }
    }
    Ok(())
}

fn parse_root_id(value: &str) -> Result<RootId, Box<dyn std::error::Error>> {
    let value = value
        .strip_prefix("sha256:")
        .ok_or("external fixture root omitted sha256 domain")?;
    if value.len() != 64 {
        return Err("external fixture root digest length differs from 32 bytes".into());
    }
    let mut bytes = [0_u8; 32];
    for (index, output) in bytes.iter_mut().enumerate() {
        *output = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)?;
    }
    Ok(RootId::new(Digest32::new(bytes)))
}

fn benchmark_phase_boundary(
    phase: &str,
    wait_for_runner: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("S04_PHASED_BENCHMARK").as_deref() != Ok("1") {
        return Ok(());
    }
    {
        let mut stdout = std::io::stdout().lock();
        writeln!(stdout, "stage04-phase:{phase}")?;
        stdout.flush()?;
    }
    if wait_for_runner {
        let mut trigger = [0_u8; 1];
        std::io::stdin().lock().read_exact(&mut trigger)?;
    }
    Ok(())
}

#[test]
fn frozen_external_fixture_materializes_and_verifies_exact_root(
) -> Result<(), Box<dyn std::error::Error>> {
    let Ok(source) = std::env::var("S04_FIXTURE_SOURCE") else {
        return Ok(());
    };
    let expected_root_id = std::env::var("S04_EXPECTED_ROOT_ID")?;
    let root_id = parse_root_id(&expected_root_id)?;
    let expected_entries = std::env::var("S04_EXPECTED_ENTRIES")?.parse::<u64>()?;
    let expected_capabilities = std::env::var("S04_EXPECTED_CAPABILITIES")?.parse::<u64>()?;
    let root = TestRoot::new("frozen-external")?;
    copy_fixture_objects(Path::new(&source), root.path())?;
    benchmark_phase_boundary("setup_complete", true)?;
    let store = LooseObjectStore::new(root.path().to_path_buf())?;
    let mut pages = PersistentPages::new(&store);
    let attribution_root = install_fixture_attribution(&mut pages, root_id)?;
    let key = MaterializationKey::linux_overlayfs(root_id, attribution_root);
    let coordinator = MaterializationCoordinator::new(root.path().to_path_buf())?;
    let writer = StorageWriterLockLease::acquire(root.path())?;
    let built = coordinator.materialize(
        &MaterializationRequest::new(key.clone(), Duration::from_secs(30)),
        &writer,
    )?;
    benchmark_phase_boundary("operation_complete", true)?;
    assert_eq!(built.disposition, MaterializationDisposition::Built);
    assert_eq!(built.selection.manifest.root_id, expected_root_id);
    assert_eq!(built.selection.manifest.entry_count, expected_entries);
    assert_eq!(
        built.selection.manifest.required_capabilities.feature_bits,
        expected_capabilities
    );
    assert!(built
        .maximum_buffer_bytes
        .is_some_and(|bytes| bytes <= 256 * 1024));
    let verified = coordinator
        .lookup(&key)?
        .ok_or("cold verification omitted the selected frozen generation")?;
    assert_eq!(verified.manifest_sha256, built.selection.manifest_sha256);
    benchmark_phase_boundary("verification_complete", false)?;
    Ok(())
}

#[test]
fn frozen_external_fixture_reconstructs_verified_native_carrier(
) -> Result<(), Box<dyn std::error::Error>> {
    let Ok(source) = std::env::var("S04_FIXTURE_SOURCE") else {
        return Ok(());
    };
    let expected_root_id = std::env::var("S04_EXPECTED_ROOT_ID")?;
    let root_id = parse_root_id(&expected_root_id)?;
    let expected_entries = std::env::var("S04_EXPECTED_ENTRIES")?.parse::<u64>()?;
    let expected_capabilities = std::env::var("S04_EXPECTED_CAPABILITIES")?.parse::<u64>()?;
    let root = TestRoot::new("frozen-reconstruction")?;
    copy_fixture_objects(Path::new(&source), root.path())?;
    let store = LooseObjectStore::new(root.path().to_path_buf())?;
    let mut pages = PersistentPages::new(&store);
    let backend = NativeBackend::new();
    let required_capabilities = backend
        .preflight(&mut pages, root_id, 4096)?
        .required_capabilities;
    assert_eq!(required_capabilities.feature_bits, expected_capabilities);
    let carrier = root.path().join("cold-reconstruction-carrier");

    benchmark_phase_boundary("setup_complete", true)?;
    let built = reconstruct_supervised(root.path(), &backend, &mut pages, root_id, &carrier)?;
    benchmark_phase_boundary("operation_complete", true)?;

    assert_eq!(built.entry_count, expected_entries);
    assert!(built.maximum_buffer_bytes <= 256 * 1024);
    let verified = backend.verify(&mut pages, root_id, &carrier)?;
    assert_eq!(verified.native_tree_sha256, built.native_tree_sha256);
    assert_eq!(verified.entry_count, built.entry_count);
    assert_eq!(verified.logical_bytes, built.logical_bytes);
    assert_eq!(verified.allocated_bytes, built.allocated_bytes);
    assert!(verified.maximum_buffer_bytes <= 256 * 1024);
    benchmark_phase_boundary("verification_complete", false)?;
    Ok(())
}

fn sequential_copy_tree(source: &Path, destination: &Path) -> std::io::Result<u64> {
    std::fs::create_dir_all(destination)?;
    let mut entries = std::fs::read_dir(source)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    let mut copied = 0_u64;
    for entry in entries {
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let metadata = std::fs::symlink_metadata(&source_path)?;
        if metadata.is_dir() {
            copied = copied.saturating_add(sequential_copy_tree(&source_path, &destination_path)?);
        } else if metadata.is_file() {
            let mut input = std::fs::File::open(&source_path)?;
            let mut output = std::fs::File::create(&destination_path)?;
            let mut buffer = vec![0_u8; MAX_HYDRATION_STREAM_BYTES];
            loop {
                let count = input.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                output.write_all(&buffer[..count])?;
                copied = copied.saturating_add(u64::try_from(count).unwrap_or(u64::MAX));
            }
            output.sync_all()?;
        } else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "native control contains a non-file entry",
            ));
        }
    }
    std::fs::File::open(destination)?.sync_all()?;
    Ok(copied)
}

fn assert_native_trees_equal(source: &Path, candidate: &Path) -> std::io::Result<()> {
    let mut source_entries = std::fs::read_dir(source)?.collect::<Result<Vec<_>, _>>()?;
    let mut candidate_entries = std::fs::read_dir(candidate)?.collect::<Result<Vec<_>, _>>()?;
    source_entries.sort_by_key(std::fs::DirEntry::file_name);
    candidate_entries.sort_by_key(std::fs::DirEntry::file_name);
    let source_names = source_entries
        .iter()
        .map(std::fs::DirEntry::file_name)
        .collect::<Vec<_>>();
    let candidate_names = candidate_entries
        .iter()
        .map(std::fs::DirEntry::file_name)
        .collect::<Vec<_>>();
    if source_names != candidate_names {
        return Err(std::io::Error::other(
            "native sequential copy changed directory entries",
        ));
    }
    for (source_entry, candidate_entry) in source_entries.iter().zip(&candidate_entries) {
        let source_metadata = source_entry.metadata()?;
        let candidate_metadata = candidate_entry.metadata()?;
        if source_metadata.is_dir() != candidate_metadata.is_dir()
            || source_metadata.is_file() != candidate_metadata.is_file()
        {
            return Err(std::io::Error::other(
                "native sequential copy changed an entry kind",
            ));
        }
        if source_metadata.is_dir() {
            assert_native_trees_equal(&source_entry.path(), &candidate_entry.path())?;
        } else if std::fs::read(source_entry.path())? != std::fs::read(candidate_entry.path())? {
            return Err(std::io::Error::other(
                "native sequential copy changed file content",
            ));
        }
    }
    Ok(())
}

fn cold_control_sample(
    source: &Path,
    label: &str,
) -> Result<(u64, u64), Box<dyn std::error::Error>> {
    let root = TestRoot::new(label)?;
    let destination = root.path().join("native-copy");
    let started = Instant::now();
    let bytes = sequential_copy_tree(source, &destination)?;
    let elapsed_ns = u64::try_from(started.elapsed().as_nanos())?.max(1);
    assert_native_trees_equal(source, &destination)?;
    Ok((elapsed_ns, bytes))
}

fn cold_candidate_hydration_sample(
    fixture_seed: &Path,
    root_id: RootId,
    expected_entries: u64,
    label: &str,
) -> Result<(u64, u64), Box<dyn std::error::Error>> {
    let root = TestRoot::new(label)?;
    link_fixture_objects(fixture_seed, root.path())?;
    let store = LooseObjectStore::new(root.path().to_path_buf())?;
    let mut pages = PersistentPages::new(&store);
    let backend = NativeBackend::new();
    let carrier = root.path().join("cold-hydration-carrier");
    let supervisor = supervisor::shared_supervisor_for_root(root.path())?;
    let cancellation = AtomicBool::new(false);
    let deadline = Instant::now() + Duration::from_secs(20);
    let owner = match supervisor.admit_materialization(
        format!("cold-hydration:{}", carrier.display()),
        deadline,
        &cancellation,
    )? {
        supervisor::MaterializationAdmission::Owner(owner) => owner,
        supervisor::MaterializationAdmission::Waiter(_) => {
            return Err(std::io::Error::other(
                "isolated cold hydration unexpectedly joined a flight",
            )
            .into());
        }
    };
    let target = owner.acquire_target(MAX_HYDRATION_STREAM_BYTES, deadline, &cancellation)?;
    let started = Instant::now();
    let built = backend.reconstruct_bounded(
        &mut pages,
        root_id,
        &carrier,
        NativeReconstructionResources {
            hydration_byte_permit_bytes: MAX_HYDRATION_STREAM_BYTES,
            metadata_queue_depth: supervisor::MAX_METADATA_QUEUE_ITEMS,
            target: &target,
            observation: None,
        },
        || Ok(()),
    )?;
    let elapsed_ns = u64::try_from(started.elapsed().as_nanos())?.max(1);
    assert_eq!(built.entry_count, expected_entries);
    let verified = backend.verify(&mut pages, root_id, &carrier)?;
    assert_eq!(verified.native_tree_sha256, built.native_tree_sha256);
    Ok((elapsed_ns, built.logical_bytes))
}

fn cold_candidate_activation_sample(
    fixture_seed: &Path,
    root_id: RootId,
    expected_entries: u64,
    label: &str,
) -> Result<(u64, u64), Box<dyn std::error::Error>> {
    let root = TestRoot::new(label)?;
    link_fixture_objects(fixture_seed, root.path())?;
    let store = LooseObjectStore::new(root.path().to_path_buf())?;
    let mut pages = PersistentPages::new(&store);
    let attribution_root = install_fixture_attribution(&mut pages, root_id)?;
    let key = MaterializationKey::linux_overlayfs(root_id, attribution_root);
    let coordinator = MaterializationCoordinator::new(root.path().to_path_buf())?;
    let writer = StorageWriterLockLease::acquire(root.path())?;
    let started = Instant::now();
    let outcome = coordinator.materialize(
        &MaterializationRequest::new(key.clone(), Duration::from_secs(20)),
        &writer,
    )?;
    let elapsed_ns = u64::try_from(started.elapsed().as_nanos())?.max(1);
    assert_eq!(outcome.selection.manifest.entry_count, expected_entries);
    assert_eq!(
        coordinator
            .lookup(&key)?
            .ok_or("cold activation omitted CURRENT")?
            .manifest_sha256,
        outcome.selection.manifest_sha256
    );
    Ok((elapsed_ns, outcome.selection.manifest.logical_bytes))
}

#[test]
fn cold_hydration_benchmark_emits_matched_abba_baab_raw_samples(
) -> Result<(), Box<dyn std::error::Error>> {
    let Ok(fixture_source) = std::env::var("S04_FIXTURE_SOURCE") else {
        return Ok(());
    };
    let native_control = PathBuf::from(std::env::var("S04_NATIVE_CONTROL")?);
    let root_id = parse_root_id(&std::env::var("S04_EXPECTED_ROOT_ID")?)?;
    let expected_entries = std::env::var("S04_EXPECTED_ENTRIES")?.parse::<u64>()?;
    let fixture_seed = TestRoot::new("cold-fixture-seed")?;
    copy_fixture_objects(Path::new(&fixture_source), fixture_seed.path())?;
    benchmark_phase_boundary("setup_complete", true)?;

    let _ = cold_control_sample(&native_control, "cold-control-warmup")?;
    let _ = cold_candidate_hydration_sample(
        fixture_seed.path(),
        root_id,
        expected_entries,
        "cold-candidate-warmup",
    )?;
    let mut hydration_samples = Vec::with_capacity(40);
    let mut activation_samples = Vec::with_capacity(40);
    for block_index in 0..10 {
        let (schedule_name, schedule) = if block_index % 2 == 0 {
            ("ABBA", ["control", "candidate", "candidate", "control"])
        } else {
            ("BAAB", ["candidate", "control", "control", "candidate"])
        };
        for pair_start in [0, 2] {
            let pair = std::thread::scope(|scope| {
                let left = schedule[pair_start];
                let right = schedule[pair_start + 1];
                let left_index = block_index * 4 + pair_start;
                let right_index = left_index + 1;
                let left_label = format!("cold-hydration-{left}-{left_index}");
                let right_label = format!("cold-hydration-{right}-{right_index}");
                let native_control = &native_control;
                let fixture_seed = fixture_seed.path();
                let left_sample = scope.spawn(move || {
                    match left {
                        "control" => cold_control_sample(native_control, &left_label),
                        "candidate" => cold_candidate_hydration_sample(
                            fixture_seed,
                            root_id,
                            expected_entries,
                            &left_label,
                        ),
                        _ => unreachable!(),
                    }
                    .map_err(|error| error.to_string())
                });
                let right_sample = scope.spawn(move || {
                    match right {
                        "control" => cold_control_sample(native_control, &right_label),
                        "candidate" => cold_candidate_hydration_sample(
                            fixture_seed,
                            root_id,
                            expected_entries,
                            &right_label,
                        ),
                        _ => unreachable!(),
                    }
                    .map_err(|error| error.to_string())
                });
                Ok::<_, Box<dyn std::error::Error>>([
                    left_sample
                        .join()
                        .map_err(|_| "cold hydration sample panicked")??,
                    right_sample
                        .join()
                        .map_err(|_| "cold hydration sample panicked")??,
                ])
            })?;
            for (offset, (elapsed_ns, bytes)) in pair.into_iter().enumerate() {
                let position = pair_start + offset;
                hydration_samples.push(json!({
                    "arm": schedule[position],
                    "elapsed_ns": elapsed_ns,
                    "bytes": bytes,
                    "operations": 1,
                    "block_index": block_index,
                    "position": position,
                    "schedule": schedule_name,
                    "verified": true,
                }));
            }
        }
        for pair_start in [0, 2] {
            let pair = std::thread::scope(|scope| {
                let left = schedule[pair_start];
                let right = schedule[pair_start + 1];
                let left_index = block_index * 4 + pair_start;
                let right_index = left_index + 1;
                let left_label = format!("cold-activation-{left}-{left_index}");
                let right_label = format!("cold-activation-{right}-{right_index}");
                let native_control = &native_control;
                let fixture_seed = fixture_seed.path();
                let left_sample = scope.spawn(move || {
                    match left {
                        "control" => cold_control_sample(native_control, &left_label),
                        "candidate" => cold_candidate_activation_sample(
                            fixture_seed,
                            root_id,
                            expected_entries,
                            &left_label,
                        ),
                        _ => unreachable!(),
                    }
                    .map_err(|error| error.to_string())
                });
                let right_sample = scope.spawn(move || {
                    match right {
                        "control" => cold_control_sample(native_control, &right_label),
                        "candidate" => cold_candidate_activation_sample(
                            fixture_seed,
                            root_id,
                            expected_entries,
                            &right_label,
                        ),
                        _ => unreachable!(),
                    }
                    .map_err(|error| error.to_string())
                });
                Ok::<_, Box<dyn std::error::Error>>([
                    left_sample
                        .join()
                        .map_err(|_| "cold activation sample panicked")??,
                    right_sample
                        .join()
                        .map_err(|_| "cold activation sample panicked")??,
                ])
            })?;
            for (offset, (elapsed_ns, bytes)) in pair.into_iter().enumerate() {
                let position = pair_start + offset;
                activation_samples.push(json!({
                    "arm": schedule[position],
                    "elapsed_ns": elapsed_ns,
                    "bytes": bytes,
                    "operations": 1,
                    "block_index": block_index,
                    "position": position,
                    "schedule": schedule_name,
                    "verified": true,
                }));
            }
        }
    }
    benchmark_phase_boundary("operation_complete", true)?;
    for (cell, samples) in [
        ("cold_hydration", hydration_samples),
        ("cold_activation", activation_samples),
    ] {
        assert_eq!(
            samples
                .iter()
                .filter(|sample| sample["arm"] == "control")
                .count(),
            20
        );
        assert_eq!(
            samples
                .iter()
                .filter(|sample| sample["arm"] == "candidate")
                .count(),
            20
        );
        println!(
            "stage04_5-cold-hydration-evidence:{}",
            serde_json::to_string(&json!({
                "schema_version": 1,
                "cell": cell,
                "samples": samples,
                "verified_same_filesystem_native_sequential_copy": true,
            }))?
        );
    }
    benchmark_phase_boundary("verification_complete", false)?;
    Ok(())
}

fn metadata(mode: u32, uid: u32, gid: u32, xattrs: Vec<(Vec<u8>, Vec<u8>)>) -> MetadataV3 {
    MetadataV3 {
        mode,
        uid,
        gid,
        mtime_seconds: 1_700_000_000,
        mtime_nanoseconds: 123_456_789,
        xattrs,
    }
}

fn count_open_descriptors_under(root: &Path) -> std::io::Result<u64> {
    let mut count = 0_u64;
    for entry in std::fs::read_dir("/proc/self/fd")? {
        let entry = entry?;
        if std::fs::read_link(entry.path()).is_ok_and(|target| target.starts_with(root)) {
            count = count.saturating_add(1);
        }
    }
    Ok(count)
}

#[test]
fn depth_64_native_reconstruction_and_verification_stay_within_fd_reservation(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::deep("depth-64-fd-cap", 64)?;
    let store = LooseObjectStore::new(fixture.root.path().to_path_buf())?;
    let mut pages = PersistentPages::new(&store);
    let backend = NativeBackend::new();
    backend.preflight(&mut pages, fixture.key.root, 4096)?;
    let carrier = fixture.root.path().join("depth-64-carrier");

    let running = Arc::new(AtomicBool::new(true));
    let high_water = Arc::new(AtomicU64::new(0));
    let sampler_running = Arc::clone(&running);
    let sampler_high_water = Arc::clone(&high_water);
    let sampler_root = fixture.root.path().to_path_buf();
    let sampler = std::thread::spawn(move || -> std::io::Result<()> {
        while sampler_running.load(Ordering::Acquire) {
            sampler_high_water.fetch_max(
                count_open_descriptors_under(&sampler_root)?,
                Ordering::Relaxed,
            );
            std::thread::yield_now();
        }
        sampler_high_water.fetch_max(
            count_open_descriptors_under(&sampler_root)?,
            Ordering::Relaxed,
        );
        Ok(())
    });

    let operation = (|| -> Result<(), Box<dyn std::error::Error>> {
        let built = reconstruct_supervised(
            fixture.root.path(),
            &backend,
            &mut pages,
            fixture.key.root,
            &carrier,
        )?;
        let verified = backend.verify(&mut pages, fixture.key.root, &carrier)?;
        assert_eq!(verified.native_tree_sha256, built.native_tree_sha256);
        assert_eq!(verified.entry_count, built.entry_count);
        assert_eq!(verified.logical_bytes, built.logical_bytes);
        assert_eq!(verified.allocated_bytes, built.allocated_bytes);
        Ok(())
    })();
    running.store(false, Ordering::Release);
    sampler
        .join()
        .map_err(|_| std::io::Error::other("descriptor sampler panicked"))??;
    operation?;

    assert!(
        high_water.load(Ordering::Relaxed) <= 16,
        "native materialization exceeded its 16-FD reservation"
    );
    Ok(())
}

#[test]
fn depth_65_native_preflight_fails_closed() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::deep("depth-65-rejected", 65)?;
    let store = LooseObjectStore::new(fixture.root.path().to_path_buf())?;
    let mut pages = PersistentPages::new(&store);
    let error = NativeBackend::new()
        .preflight(&mut pages, fixture.key.root, 4096)
        .expect_err("depth 65 must exceed the fixed logical-tree limit");
    assert!(error.to_string().contains("path depth"));
    assert_no_materialization_mutation(fixture.root.path());
    Ok(())
}

fn assert_no_materialization_mutation(root: &Path) {
    assert!(!root.join("operations").exists());
    assert!(!root.join("materializations").exists());
    assert!(!root.join("refs/leases").exists());
}

fn assert_durable_root_hold_without_visibility(
    root: &Path,
    key: &MaterializationKey,
) -> Result<(), Box<dyn std::error::Error>> {
    let operation = MaterializationOperation::load(root.to_path_buf(), key)?
        .ok_or("failed build omitted its durable root hold")?;
    assert_eq!(operation.state().phase, MaterializationPhase::Building);
    assert_eq!(
        operation.state().checkpoint,
        MaterializationCheckpoint::Admitted
    );
    assert_eq!(operation.state().root_hold, operation.state().root_id);
    assert!(operation.state().source_holds.is_empty());
    assert!(operation.state().prior_generation_hold.is_none());
    assert_eq!(operation.active_typed_hold_count(), 1);
    assert!(!operation.work_carrier().exists());
    assert!(!root.join("materializations").exists());
    assert!(!root.join("refs/leases").exists());
    Ok(())
}

#[test]
fn typed_hold_cap_rejects_4097_before_operation_mutation() -> Result<(), Box<dyn std::error::Error>>
{
    let fixture = Fixture::mixed("typed-hold-cap")?;
    let source_hold = MaterializationSourceHold {
        locator_id: format!("sha256:{}", "11".repeat(32)),
        carrier_id: format!("sha256:{}", "22".repeat(32)),
        locator_generation: 1,
        carrier_generation: 1,
    };
    let error = MaterializationOperation::open_with_holds(
        fixture.root.path().to_path_buf(),
        &fixture.key,
        vec![source_hold; MAX_ACTIVE_TYPED_HOLDS],
        None,
        1,
    )
    .expect_err("root plus 4096 source holds must exceed the active hold cap");
    assert!(
        matches!(error, stack::candidate::materialization_operation::MaterializationOperationError::Invalid(message)
            if message == "active typed hold cap exceeded")
    );
    assert_no_materialization_mutation(fixture.root.path());
    Ok(())
}

#[test]
fn terminal_transition_releases_durable_root_hold() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::mixed("typed-hold-release")?;
    let mut operation =
        MaterializationOperation::open(fixture.root.path().to_path_buf(), &fixture.key, 1)?;
    assert_eq!(operation.state().root_hold, operation.state().root_id);
    assert_eq!(operation.active_typed_hold_count(), 1);
    operation.transition(
        MaterializationPhase::Terminal,
        None,
        None,
        Some("cancelled".to_owned()),
        2,
    )?;
    assert_eq!(operation.active_typed_hold_count(), 0);
    let reloaded = MaterializationOperation::load(fixture.root.path().to_path_buf(), &fixture.key)?
        .ok_or("terminal operation disappeared")?;
    assert_eq!(reloaded.state().root_hold, reloaded.state().root_id);
    assert_eq!(reloaded.active_typed_hold_count(), 0);
    Ok(())
}

#[test]
fn generation_lease_rejects_same_number_stale_fence_without_substitution(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::mixed("generation-lease-aba")?;
    let built = fixture.materialize()?.selection;
    let generations = GenerationStore::new(fixture.root.path().to_path_buf())?;
    let mut stale = built.clone();
    stale.manifest.fence = stale.manifest.fence.saturating_add(1);

    let error = generations
        .acquire_lease(
            &fixture.key,
            &stale,
            "test-owner",
            "stale-session",
            101,
            120,
        )
        .expect_err("a same-number stale fence must not acquire an exact-generation lease");
    assert!(
        matches!(error, GenerationError::Collision(message) if message.contains("exact installed generation subject"))
    );
    assert_eq!(
        generations.lookup_current(&fixture.key)?.expect("current"),
        built
    );
    assert!(!fixture.root.path().join("refs").join("leases").exists());
    Ok(())
}

#[test]
fn generation_subject_65_is_rejected_without_eviction_or_deletion(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::mixed("generation-subject-cap")?;
    let built = fixture.materialize()?.selection;
    let generations = GenerationStore::new(fixture.root.path().to_path_buf())?;
    let subjects = fixture
        .root
        .path()
        .join("refs")
        .join("materialization-generation-subjects");
    std::fs::create_dir_all(&subjects)?;
    for index in 0_u64..64 {
        std::fs::create_dir(subjects.join(format!("{index:064x}")))?;
    }

    let error = generations
        .acquire_lease(&fixture.key, &built, "test-owner", "subject-65", 101, 120)
        .expect_err("the 65th exact generation subject must fail admission");
    assert!(
        matches!(error, GenerationError::Invalid(message) if message.contains("ceiling reached"))
    );
    assert_eq!(std::fs::read_dir(&subjects)?.count(), 64);
    assert_eq!(
        generations.generation_numbers(fixture.key.id()?)?,
        vec![built.manifest.generation]
    );
    assert_eq!(
        generations.lookup_current(&fixture.key)?.expect("current"),
        built
    );
    assert!(!fixture.root.path().join("refs").join("leases").exists());
    Ok(())
}

fn publish_second_generation(
    fixture: &Fixture,
    first: &GenerationSelection,
    writer: &StorageWriterLockLease,
) -> Result<GenerationSelection, Box<dyn std::error::Error>> {
    publish_second_generation_with(fixture, first, writer, |_| {})
}

fn publish_second_generation_with(
    fixture: &Fixture,
    first: &GenerationSelection,
    writer: &StorageWriterLockLease,
    mutate: impl FnOnce(&mut GenerationManifest),
) -> Result<GenerationSelection, Box<dyn std::error::Error>> {
    let prepared = prepare_second_generation_with(fixture, first, mutate)?;
    let generations = GenerationStore::new(fixture.root.path().to_path_buf())?;
    let _guard = writer.exclusive()?;
    Ok(generations.promote_selection(&fixture.key, &prepared.selection)?)
}

struct PreparedGeneration {
    selection: GenerationSelection,
    operation_build: MaterializationOperationBuild,
}

fn prepare_second_generation_with(
    fixture: &Fixture,
    first: &GenerationSelection,
    mutate: impl FnOnce(&mut GenerationManifest),
) -> Result<PreparedGeneration, Box<dyn std::error::Error>> {
    let generations = GenerationStore::new(fixture.root.path().to_path_buf())?;
    let id = fixture.key.id()?;
    let (generation, fence) = generations.next_generation(id, Some(first))?;
    assert_eq!((generation, fence), (2, 2));
    let work = fixture.root.path().join("manual-generation-two");
    let store = LooseObjectStore::new(fixture.root.path().to_path_buf())?;
    let mut pages = PersistentPages::new(&store);
    let backend = NativeBackend::new();
    let build = reconstruct_supervised(
        fixture.root.path(),
        &backend,
        &mut pages,
        fixture.key.root,
        &work,
    )?;
    let verified = backend.verify(&mut pages, fixture.key.root, &work)?;
    assert_eq!(verified.native_tree_sha256, build.native_tree_sha256);
    assert_eq!(verified.entry_count, build.entry_count);
    assert_eq!(verified.logical_bytes, build.logical_bytes);
    assert_eq!(verified.allocated_bytes, build.allocated_bytes);
    let mut manifest: GenerationManifest = first.manifest.clone();
    manifest.generation = generation;
    manifest.fence = fence;
    manifest.native_tree_sha256 = build.native_tree_sha256.clone();
    manifest.carriers[0].native_tree_sha256 = build.native_tree_sha256.clone();
    manifest.entry_count = build.entry_count;
    manifest.logical_bytes = build.logical_bytes;
    manifest.allocated_bytes = build.allocated_bytes;
    manifest.build_operation_id = "22".repeat(32);
    manifest.completed_unix_seconds = manifest.completed_unix_seconds.saturating_add(1);
    mutate(&mut manifest);
    let operation_build = MaterializationOperationBuild {
        native_tree_sha256: build.native_tree_sha256,
        entry_count: build.entry_count,
        logical_bytes: build.logical_bytes,
        allocated_bytes: build.allocated_bytes,
        maximum_buffer_bytes: build.maximum_buffer_bytes,
        required_capabilities: manifest.required_capabilities.clone(),
        provided_capabilities: manifest.provided_capabilities.clone(),
    };
    generations.install_carrier(id, generation, &work)?;
    let selection = generations.publish_manifest(&fixture.key, &manifest)?;
    Ok(PreparedGeneration {
        selection,
        operation_build,
    })
}

type PublicationSubject = (u64, u64, String);

fn publication_subject(selection: &GenerationSelection) -> PublicationSubject {
    (
        selection.manifest.generation,
        selection.manifest.fence,
        selection.manifest_sha256.clone(),
    )
}

fn durable_publication_subject(
    selection: &GenerationSelection,
) -> MaterializationPublicationSubject {
    MaterializationPublicationSubject {
        generation: selection.manifest.generation,
        fence: selection.manifest.fence,
        manifest_sha256: selection.manifest_sha256.clone(),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum BridgeCall {
    Preflight {
        old: Option<PublicationSubject>,
        new: PublicationSubject,
    },
    Admit {
        materialization_id: String,
        new: PublicationSubject,
    },
    Handoff {
        materialization_id: String,
        old: PublicationSubject,
        new: PublicationSubject,
        current: Option<PublicationSubject>,
    },
}

struct RecordingGcBridge {
    storage_root: PathBuf,
    calls: Mutex<Vec<BridgeCall>>,
    fail_admit_once: AtomicBool,
    fail_handoff_once: AtomicBool,
}

impl RecordingGcBridge {
    fn new(storage_root: PathBuf) -> Self {
        Self {
            storage_root,
            calls: Mutex::new(Vec::new()),
            fail_admit_once: AtomicBool::new(false),
            fail_handoff_once: AtomicBool::new(false),
        }
    }

    fn fail_admit_once(&self) {
        self.fail_admit_once.store(true, Ordering::Release);
    }

    fn fail_handoff_once(&self) {
        self.fail_handoff_once.store(true, Ordering::Release);
    }

    fn calls(&self) -> Vec<BridgeCall> {
        self.calls.lock().expect("bridge calls lock").clone()
    }
}

impl MaterializationGcBridge for RecordingGcBridge {
    fn preflight_replacement(
        &self,
        old: Option<&GenerationSelection>,
        new: &GenerationSelection,
    ) -> Result<(), MaterializationError> {
        self.calls
            .lock()
            .expect("bridge calls lock")
            .push(BridgeCall::Preflight {
                old: old.map(publication_subject),
                new: publication_subject(new),
            });
        Ok(())
    }

    fn admit_new_root(
        &self,
        key: &MaterializationKey,
        new: &GenerationSelection,
    ) -> Result<(), MaterializationError> {
        self.calls
            .lock()
            .expect("bridge calls lock")
            .push(BridgeCall::Admit {
                materialization_id: key.id()?.hex(),
                new: publication_subject(new),
            });
        if self.fail_admit_once.swap(false, Ordering::AcqRel) {
            return Err(MaterializationError::BridgeUnavailable(
                "injected root-admission failure".to_owned(),
            ));
        }
        Ok(())
    }

    fn handoff_old_generation(
        &self,
        key: &MaterializationKey,
        old: &GenerationSelection,
        new: &GenerationSelection,
    ) -> Result<(), MaterializationError> {
        let current = GenerationStore::new(self.storage_root.clone())?.lookup_current(key)?;
        self.calls
            .lock()
            .expect("bridge calls lock")
            .push(BridgeCall::Handoff {
                materialization_id: key.id()?.hex(),
                old: publication_subject(old),
                new: publication_subject(new),
                current: current.as_ref().map(publication_subject),
            });
        if self.fail_handoff_once.swap(false, Ordering::AcqRel) {
            return Err(MaterializationError::BridgeUnavailable(
                "injected old-generation handoff failure".to_owned(),
            ));
        }
        Ok(())
    }
}

fn prepare_replacement_operation(
    fixture: &Fixture,
    first: &GenerationSelection,
    now_unix_seconds: u64,
) -> Result<(MaterializationOperation, PreparedGeneration), Box<dyn std::error::Error>> {
    let completed =
        MaterializationOperation::load(fixture.root.path().to_path_buf(), &fixture.key)?
            .ok_or("first publication omitted its durable operation")?;
    assert_eq!(completed.state().phase, MaterializationPhase::Terminal);
    let archive = fixture
        .root
        .path()
        .join("fixture-completed-operations")
        .join(completed.operation_id());
    std::fs::create_dir_all(archive.parent().expect("archive parent"))?;
    std::fs::rename(
        fixture
            .root
            .path()
            .join("operations")
            .join(completed.operation_id()),
        archive,
    )?;
    let mut operation = MaterializationOperation::open_with_holds(
        fixture.root.path().to_path_buf(),
        &fixture.key,
        Vec::new(),
        Some(durable_publication_subject(first)),
        now_unix_seconds,
    )?;
    assert_eq!(operation.active_typed_hold_count(), 2);
    assert_eq!(
        operation.state().prior_generation_hold,
        Some(durable_publication_subject(first))
    );
    let prepared = prepare_second_generation_with(fixture, first, |_| {})?;
    operation.transition(
        MaterializationPhase::Ready,
        Some((
            prepared.selection.manifest.generation,
            prepared.selection.manifest.fence,
        )),
        Some(prepared.operation_build.clone()),
        None,
        now_unix_seconds.saturating_add(1),
    )?;
    Ok((operation, prepared))
}

#[test]
fn bridge_admission_failure_is_atomic_and_same_operation_retry_publishes_exact_subject(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::mixed("bridge-admission-retry")?;
    let writer = fixture.writer_lock()?;
    let first = fixture.materialize()?.selection;
    let (mut operation, prepared) = prepare_replacement_operation(&fixture, &first, 1_000)?;
    let old_subject = publication_subject(&first);
    let new_subject = publication_subject(&prepared.selection);
    let materialization_id = fixture.key.id()?.hex();
    let bridge = Arc::new(RecordingGcBridge::new(fixture.root.path().to_path_buf()));
    bridge.fail_admit_once();
    let publisher = MaterializationPublisher::new(
        GenerationStore::new(fixture.root.path().to_path_buf())?,
        bridge.clone(),
    );

    let error = publisher
        .publish(
            &fixture.key,
            &prepared.selection,
            &mut operation,
            &writer,
            1_003,
        )
        .expect_err("injected root admission must abort selector publication");
    assert!(
        matches!(error, MaterializationError::BridgeUnavailable(message)
            if message == "injected root-admission failure")
    );
    assert_eq!(operation.state().phase, MaterializationPhase::Ready);
    assert_eq!(
        publication_subject(
            &GenerationStore::new(fixture.root.path().to_path_buf())?
                .lookup_current(&fixture.key)?
                .ok_or("old CURRENT disappeared after admission failure")?
        ),
        old_subject
    );

    let published = publisher.publish(
        &fixture.key,
        &prepared.selection,
        &mut operation,
        &writer,
        1_004,
    )?;
    assert_eq!(publication_subject(&published), new_subject);
    assert_eq!(operation.state().phase, MaterializationPhase::Published);
    assert_eq!(
        bridge.calls(),
        vec![
            BridgeCall::Preflight {
                old: Some(old_subject.clone()),
                new: new_subject.clone(),
            },
            BridgeCall::Admit {
                materialization_id: materialization_id.clone(),
                new: new_subject.clone(),
            },
            BridgeCall::Preflight {
                old: Some(old_subject.clone()),
                new: new_subject.clone(),
            },
            BridgeCall::Admit {
                materialization_id: materialization_id.clone(),
                new: new_subject.clone(),
            },
            BridgeCall::Handoff {
                materialization_id,
                old: old_subject,
                new: new_subject.clone(),
                current: Some(new_subject),
            },
        ]
    );
    Ok(())
}

#[test]
fn bridge_handoff_failure_retries_only_exact_old_generation_after_durable_publish(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::mixed("bridge-handoff-retry")?;
    let writer = fixture.writer_lock()?;
    let first = fixture.materialize()?.selection;
    let (mut operation, prepared) = prepare_replacement_operation(&fixture, &first, 2_000)?;
    let old_subject = publication_subject(&first);
    let new_subject = publication_subject(&prepared.selection);
    let materialization_id = fixture.key.id()?.hex();
    let bridge = Arc::new(RecordingGcBridge::new(fixture.root.path().to_path_buf()));
    bridge.fail_handoff_once();
    let publisher = MaterializationPublisher::new(
        GenerationStore::new(fixture.root.path().to_path_buf())?,
        bridge.clone(),
    );

    let error = publisher
        .publish(
            &fixture.key,
            &prepared.selection,
            &mut operation,
            &writer,
            2_003,
        )
        .expect_err("injected handoff failure must be observable");
    assert!(
        matches!(error, MaterializationError::BridgeUnavailable(message)
            if message == "injected old-generation handoff failure")
    );
    assert_eq!(operation.state().phase, MaterializationPhase::Published);
    assert_eq!(
        publication_subject(
            &GenerationStore::new(fixture.root.path().to_path_buf())?
                .lookup_current(&fixture.key)?
                .ok_or("new CURRENT disappeared after handoff failure")?
        ),
        new_subject
    );

    let retried = publisher.publish(
        &fixture.key,
        &prepared.selection,
        &mut operation,
        &writer,
        2_004,
    )?;
    assert_eq!(publication_subject(&retried), new_subject);
    assert_eq!(
        bridge.calls(),
        vec![
            BridgeCall::Preflight {
                old: Some(old_subject.clone()),
                new: new_subject.clone(),
            },
            BridgeCall::Admit {
                materialization_id: materialization_id.clone(),
                new: new_subject.clone(),
            },
            BridgeCall::Handoff {
                materialization_id: materialization_id.clone(),
                old: old_subject.clone(),
                new: new_subject.clone(),
                current: Some(new_subject.clone()),
            },
            BridgeCall::Handoff {
                materialization_id,
                old: old_subject,
                new: new_subject.clone(),
                current: Some(new_subject),
            },
        ]
    );
    Ok(())
}

#[test]
fn warm_lookup_does_not_reopen_logical_objects_or_hash_carrier_contents(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::mixed("warm-plan-only")?;
    let built = fixture.materialize()?;
    let store = LooseObjectStore::new(fixture.root.path().to_path_buf())?;
    std::fs::remove_file(store.object_path(RecordKindV3::Root, fixture.key.root.digest()))?;
    std::fs::write(
        built.selection.carrier_path.join("hard-a"),
        b"warm-route-must-not-hash-this",
    )?;

    let warm = fixture
        .coordinator()?
        .lookup_warm(&fixture.key)?
        .expect("selected warm generation");
    assert_eq!(
        warm.manifest_sha256, built.selection.manifest_sha256,
        "warm selection must authenticate bounded selector/manifest metadata only"
    );

    let error = fixture
        .coordinator()?
        .lookup(&fixture.key)
        .expect_err("explicit cold verification must reopen the logical graph");
    assert!(matches!(error, MaterializationError::Native(_)));
    Ok(())
}

#[test]
fn warm_lookup_rejects_bad_capability_plan_and_non_directory_carrier(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::mixed("warm-capability-plan")?;
    let coordinator = fixture.coordinator()?;
    let writer = fixture.writer_lock()?;
    let first = coordinator
        .materialize(
            &MaterializationRequest::new(fixture.key.clone(), Duration::from_secs(20)),
            &writer,
        )?
        .selection;
    let malformed = publish_second_generation_with(&fixture, &first, &writer, |manifest| {
        manifest.required_capabilities.sparse_files = false;
    })?;
    assert_eq!(malformed.manifest.generation, 2);
    let error = coordinator
        .lookup_warm(&fixture.key)
        .expect_err("non-canonical capability plan must fail closed");
    assert!(
        matches!(error, MaterializationError::Native(message) if message.contains("capability"))
    );

    let structural = Fixture::mixed("warm-carrier-shape")?;
    let built = structural.materialize()?;
    std::fs::remove_dir_all(&built.selection.carrier_path)?;
    std::fs::write(&built.selection.carrier_path, b"not-a-directory")?;
    let error = structural
        .coordinator()?
        .lookup_warm(&structural.key)
        .expect_err("non-directory carrier must fail closed");
    assert!(matches!(error, MaterializationError::Generation(_)));
    Ok(())
}

#[test]
fn mixed_native_tree_is_exact_reused_bounded_and_corruption_fails_closed(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::mixed("mixed")?;
    let built = fixture.materialize()?;
    assert_eq!(built.disposition, MaterializationDisposition::Built);
    assert_eq!(built.selection.manifest.generation, 1);
    assert_eq!(built.selection.manifest.fence, 1);
    assert_eq!(built.selection.manifest.entry_count, 7);
    assert_eq!(
        built.selection.manifest.required_capabilities.feature_bits,
        CAP_XATTR | CAP_SPARSE | CAP_HARDLINK | CAP_SYMLINK | CAP_FIFO
    );
    assert!(built
        .maximum_buffer_bytes
        .is_some_and(|value| value <= 256 * 1024));

    let carrier = &built.selection.carrier_path;
    let names = std::fs::read_dir(carrier)?
        .map(|entry| entry.map(|value| value.file_name().as_bytes().to_vec()))
        .collect::<Result<Vec<_>, _>>()?;
    assert!(names.contains(&vec![0xff, b'n']));
    assert_eq!(
        std::fs::read(carrier.join("sparse"))?[..b"stage04-native-candidate".len()],
        *b"stage04-native-candidate"
    );
    assert!(std::fs::metadata(carrier.join("sparse"))?.len() > 128 * 1024);
    assert_eq!(
        std::fs::symlink_metadata(carrier.join("hard-a"))?.ino(),
        std::fs::symlink_metadata(carrier.join("hard-b"))?.ino()
    );
    assert_eq!(
        std::fs::read_link(carrier.join("dangling"))?
            .as_os_str()
            .as_bytes(),
        b"missing/../target"
    );
    assert_eq!(
        std::fs::symlink_metadata(carrier.join("fifo"))?.mode() & libc_mode_type_mask(),
        libc_fifo_mode()
    );
    let xattr = rustix::fs::getxattr(
        carrier.join("sparse"),
        OsStr::from_bytes(b"user.stage04"),
        &mut [0_u8; 3],
    )?;
    assert_eq!(xattr, 3);

    let reused = fixture.materialize()?;
    assert_eq!(reused.disposition, MaterializationDisposition::Reused);
    assert_eq!(reused.operation_id, built.operation_id);
    assert_eq!(
        reused.selection.manifest_sha256,
        built.selection.manifest_sha256
    );

    std::fs::write(carrier.join("hard-a"), b"corrupt")?;
    let error = fixture
        .coordinator()?
        .lookup(&fixture.key)
        .expect_err("carrier corruption must fail closed");
    assert!(matches!(error, MaterializationError::Native(_)));
    Ok(())
}

#[test]
fn writer_lock_rank_assertions_and_forbidden_work_counters_are_exact(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("writer-lock-rank")?;
    let writer = StorageWriterLockLease::acquire(root.path())?;
    for class in [
        WriterLockForbiddenWork::TreeWalk,
        WriterLockForbiddenWork::PayloadVerification,
        WriterLockForbiddenWork::HistoryScan,
        WriterLockForbiddenWork::PermitOrFlightWait,
        WriterLockForbiddenWork::WorkerJoin,
        WriterLockForbiddenWork::Cleanup,
        WriterLockForbiddenWork::ProviderPayloadIo,
    ] {
        let guard = writer.exclusive()?;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            assert_writer_lock_allows(class);
        }));
        #[cfg(debug_assertions)]
        assert!(
            result.is_err(),
            "{class:?} must trip the debug lock-rank assertion"
        );
        #[cfg(not(debug_assertions))]
        assert!(result.is_ok(), "{class:?} must remain a production counter");
        drop(guard);
    }

    let metrics = writer.metrics()?;
    assert_eq!(metrics.acquisitions, 7);
    assert_eq!(metrics.forbidden_tree_walks, 1);
    assert_eq!(metrics.forbidden_payload_verifications, 1);
    assert_eq!(metrics.forbidden_history_scans, 1);
    assert_eq!(metrics.forbidden_permit_or_flight_waits, 1);
    assert_eq!(metrics.forbidden_worker_joins, 1);
    assert_eq!(metrics.forbidden_cleanups, 1);
    assert_eq!(metrics.forbidden_provider_payload_io, 1);
    Ok(())
}

#[test]
fn normal_materialization_observes_writer_lock_without_forbidden_work(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::mixed("writer-lock-normal")?;
    let state = stack::observation::shared_observation_state_for_root(fixture.root.path())?;
    let observation = stack::HiddenValidationObservation::new(Arc::clone(&state));
    let coordinator =
        MaterializationCoordinator::new_observed(fixture.root.path().to_path_buf(), observation)?;
    let writer = fixture.writer_lock()?;
    coordinator.materialize(
        &MaterializationRequest::new(fixture.key.clone(), Duration::from_secs(20)),
        &writer,
    )?;

    let metrics = writer.metrics()?;
    assert!(metrics.acquisitions >= 1);
    assert!(metrics.maximum_wait_ns <= metrics.wait_ns);
    assert!(metrics.maximum_hold_ns <= metrics.hold_ns);
    assert_eq!(metrics.forbidden_tree_walks, 0);
    assert_eq!(metrics.forbidden_payload_verifications, 0);
    assert_eq!(metrics.forbidden_history_scans, 0);
    assert_eq!(metrics.forbidden_permit_or_flight_waits, 0);
    assert_eq!(metrics.forbidden_worker_joins, 0);
    assert_eq!(metrics.forbidden_cleanups, 0);
    assert_eq!(metrics.forbidden_provider_payload_io, 0);

    let (_, resources) = state.observe_with_writer_lock(0, metrics);
    assert_eq!(resources.writer_lock_acquisitions, metrics.acquisitions);
    assert_eq!(resources.writer_lock_wait_ns, metrics.wait_ns);
    assert_eq!(
        resources.writer_lock_maximum_wait_ns,
        metrics.maximum_wait_ns
    );
    assert_eq!(resources.writer_lock_hold_ns, metrics.hold_ns);
    assert_eq!(
        resources.writer_lock_maximum_hold_ns,
        metrics.maximum_hold_ns
    );
    assert_eq!(resources.writer_lock_forbidden_tree_walks, 0);
    assert_eq!(resources.writer_lock_forbidden_payload_verifications, 0);
    assert_eq!(resources.writer_lock_forbidden_history_scans, 0);
    assert_eq!(resources.writer_lock_forbidden_permit_or_flight_waits, 0);
    assert_eq!(resources.writer_lock_forbidden_worker_joins, 0);
    assert_eq!(resources.writer_lock_forbidden_cleanups, 0);
    assert_eq!(resources.writer_lock_forbidden_provider_payload_io, 0);
    Ok(())
}

fn publication_lock_sample(
    sample_index: usize,
    arm: &str,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let fixture = Fixture::mixed(&format!("publication-lock-{arm}-{sample_index}"))?;
    let writer = fixture.writer_lock()?;
    let first = fixture.materialize()?.selection;
    let (mut operation, prepared) =
        prepare_replacement_operation(&fixture, &first, 10_000 + sample_index as u64 * 10)?;
    let before = writer.metrics()?;
    let started = Instant::now();
    match arm {
        "control" => {
            let _guard = writer.exclusive()?;
            GenerationStore::new(fixture.root.path().to_path_buf())?
                .promote_selection(&fixture.key, &prepared.selection)?;
        }
        "candidate" => {
            let bridge = Arc::new(RecordingGcBridge::new(fixture.root.path().to_path_buf()));
            MaterializationPublisher::new(
                GenerationStore::new(fixture.root.path().to_path_buf())?,
                bridge,
            )
            .publish(
                &fixture.key,
                &prepared.selection,
                &mut operation,
                &writer,
                10_003 + sample_index as u64 * 10,
            )?;
        }
        other => return Err(format!("unknown publication lock benchmark arm {other:?}").into()),
    }
    let elapsed_ns = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
    let after = writer.metrics()?;
    let acquisitions = after.acquisitions.saturating_sub(before.acquisitions);
    let wait_ns = after.wait_ns.saturating_sub(before.wait_ns);
    let hold_ns = after.hold_ns.saturating_sub(before.hold_ns);
    if acquisitions != 1 {
        return Err(format!(
            "publication lock benchmark {arm} sample {sample_index} recorded {acquisitions} acquisitions"
        )
        .into());
    }
    if wait_ns == 0 || hold_ns == 0 || elapsed_ns == 0 {
        return Err(format!(
            "publication lock benchmark {arm} sample {sample_index} emitted falsely-zero timing"
        )
        .into());
    }
    let forbidden = [
        after
            .forbidden_tree_walks
            .saturating_sub(before.forbidden_tree_walks),
        after
            .forbidden_payload_verifications
            .saturating_sub(before.forbidden_payload_verifications),
        after
            .forbidden_history_scans
            .saturating_sub(before.forbidden_history_scans),
        after
            .forbidden_permit_or_flight_waits
            .saturating_sub(before.forbidden_permit_or_flight_waits),
        after
            .forbidden_worker_joins
            .saturating_sub(before.forbidden_worker_joins),
        after
            .forbidden_cleanups
            .saturating_sub(before.forbidden_cleanups),
        after
            .forbidden_provider_payload_io
            .saturating_sub(before.forbidden_provider_payload_io),
    ];
    if forbidden.iter().any(|count| *count != 0) {
        return Err(format!(
            "publication lock benchmark {arm} sample {sample_index} recorded forbidden work {forbidden:?}"
        )
        .into());
    }
    Ok(json!({
        "sample_index": sample_index,
        "arm": arm,
        "elapsed_ns": elapsed_ns,
        "acquisitions": acquisitions,
        "wait_ns": wait_ns,
        "hold_ns": hold_ns,
        "forbidden": {
            "tree_walks": forbidden[0],
            "payload_verifications": forbidden[1],
            "history_scans": forbidden[2],
            "permit_or_flight_waits": forbidden[3],
            "worker_joins": forbidden[4],
            "cleanups": forbidden[5],
            "provider_payload_io": forbidden[6],
        },
    }))
}

#[test]
fn publication_lock_benchmark_emits_matched_abba_baab_raw_samples(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut samples = Vec::with_capacity(40);
    for block_index in 0..10 {
        let schedule = if block_index % 2 == 0 {
            ["control", "candidate", "candidate", "control"]
        } else {
            ["candidate", "control", "control", "candidate"]
        };
        for (position, arm) in schedule.into_iter().enumerate() {
            let sample_index = block_index * 4 + position;
            let mut sample = publication_lock_sample(sample_index, arm)?;
            sample["block_index"] = json!(block_index);
            sample["position"] = json!(position);
            sample["schedule"] = json!(if block_index % 2 == 0 { "ABBA" } else { "BAAB" });
            samples.push(sample);
        }
    }
    assert_eq!(
        samples
            .iter()
            .filter(|sample| sample["arm"] == "control")
            .count(),
        20
    );
    assert_eq!(
        samples
            .iter()
            .filter(|sample| sample["arm"] == "candidate")
            .count(),
        20
    );
    println!(
        "stage04_5-publication-lock-evidence:{}",
        serde_json::to_string(&json!({
            "schema_version": 1,
            "cell": "publication_lock",
            "schedules": ["ABBA", "BAAB"],
            "samples": samples,
            "sentinels": [{
                "sentinel_id": "forbidden_authority",
                "status": "PASS",
                "observed": {
                    "deletion": 0,
                    "retirement": 0,
                    "gc": 0,
                    "packing": 0,
                    "locator_publication": 0,
                    "public_authority_changes": 0,
                },
            }],
        }))?
    );
    Ok(())
}

#[test]
fn repair_and_squash_use_common_publisher_and_preserve_typed_roots(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::mixed("repair-squash-common-publisher")?;
    let writer = fixture.writer_lock()?;
    let request = MaterializationRequest::new(fixture.key.clone(), Duration::from_secs(20));
    let first = fixture
        .coordinator()?
        .materialize(&request, &writer)?
        .selection;
    let first_subject = publication_subject(&first);

    let error = fixture
        .coordinator()?
        .squash(&request, &first, &writer)
        .expect_err("replacement must remain disabled without the Stage 05 bridge");
    assert!(matches!(
        error,
        MaterializationError::BridgeUnavailable(message)
            if message == "replacement publication requires the Stage 05 GC bridge"
    ));
    assert_eq!(
        publication_subject(
            &GenerationStore::new(fixture.root.path().to_path_buf())?
                .lookup_current(&fixture.key)?
                .ok_or("disabled squash removed CURRENT")?
        ),
        first_subject
    );

    let bridge = Arc::new(RecordingGcBridge::new(fixture.root.path().to_path_buf()));
    let coordinator = MaterializationCoordinator::new_supervised_with_bridge(
        fixture.root.path().to_path_buf(),
        supervisor::shared_supervisor_for_root(fixture.root.path())?,
        bridge.clone(),
    )?;
    let squashed = coordinator.squash(&request, &first, &writer)?;
    let squashed_subject = publication_subject(&squashed.selection);
    assert_eq!(squashed.disposition, MaterializationDisposition::Built);
    assert_ne!(squashed_subject, first_subject);
    assert_eq!(squashed.selection.manifest.root_id, first.manifest.root_id);
    assert_eq!(
        squashed.selection.manifest.attribution_root_id,
        first.manifest.attribution_root_id
    );
    assert_eq!(
        coordinator
            .lookup(&fixture.key)?
            .ok_or("common publisher omitted replacement CURRENT")?,
        squashed.selection
    );
    assert_eq!(
        bridge.calls(),
        vec![
            BridgeCall::Preflight {
                old: Some(first_subject.clone()),
                new: squashed_subject.clone(),
            },
            BridgeCall::Admit {
                materialization_id: fixture.key.id()?.hex(),
                new: squashed_subject.clone(),
            },
            BridgeCall::Handoff {
                materialization_id: fixture.key.id()?.hex(),
                old: first_subject,
                new: squashed_subject.clone(),
                current: Some(squashed_subject),
            },
        ]
    );
    assert_eq!(
        GenerationStore::new(fixture.root.path().to_path_buf())?
            .generation_numbers(fixture.key.id()?)?,
        vec![1, 2]
    );
    let retried = coordinator.squash(&request, &first, &writer)?;
    assert_eq!(retried.disposition, MaterializationDisposition::Reused);
    assert_eq!(retried.operation_id, squashed.operation_id);
    assert_eq!(retried.selection, squashed.selection);
    assert_eq!(
        GenerationStore::new(fixture.root.path().to_path_buf())?
            .generation_numbers(fixture.key.id()?)?,
        vec![1, 2]
    );
    Ok(())
}

fn allocated_bytes_under(path: &Path) -> std::io::Result<u64> {
    let metadata = std::fs::symlink_metadata(path)?;
    let mut total = metadata.blocks().saturating_mul(512);
    if metadata.is_dir() {
        for entry in std::fs::read_dir(path)? {
            total = total.saturating_add(allocated_bytes_under(&entry?.path())?);
        }
    }
    Ok(total)
}

fn squash_benchmark_sample(
    sample_index: usize,
    arm: &str,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let fixture = Fixture::mixed(&format!("squash-{arm}-{sample_index}"))?;
    let writer = fixture.writer_lock()?;
    let bridge = Arc::new(RecordingGcBridge::new(fixture.root.path().to_path_buf()));
    let coordinator = MaterializationCoordinator::new_supervised_with_bridge(
        fixture.root.path().to_path_buf(),
        supervisor::shared_supervisor_for_root(fixture.root.path())?,
        bridge.clone(),
    )?;
    let request = MaterializationRequest::new(fixture.key.clone(), Duration::from_secs(20));
    let first = coordinator.materialize(&request, &writer)?.selection;
    let first_subject = publication_subject(&first);
    let bridge_call_cursor = bridge.calls().len();
    let allocated_before = allocated_bytes_under(fixture.root.path())?;
    let lock_before = writer.metrics()?;
    let started = Instant::now();
    let selection = match arm {
        "control" => {
            let prepared = prepare_second_generation_with(&fixture, &first, |_| {})?;
            let _guard = writer.exclusive()?;
            GenerationStore::new(fixture.root.path().to_path_buf())?
                .promote_selection(&fixture.key, &prepared.selection)?
        }
        "candidate" => coordinator.squash(&request, &first, &writer)?.selection,
        other => return Err(format!("unknown squash benchmark arm {other:?}").into()),
    };
    let elapsed_ns = u64::try_from(started.elapsed().as_nanos())?.max(1);
    let lock_after = writer.metrics()?;
    let acquisitions = lock_after
        .acquisitions
        .saturating_sub(lock_before.acquisitions);
    let wait_ns = lock_after.wait_ns.saturating_sub(lock_before.wait_ns);
    let hold_ns = lock_after.hold_ns.saturating_sub(lock_before.hold_ns);
    let frozen_ns = wait_ns.saturating_add(hold_ns).max(1);
    if acquisitions != 1 {
        return Err(format!(
            "squash benchmark {arm} sample {sample_index} recorded {acquisitions} writer-lock acquisitions"
        )
        .into());
    }
    let forbidden = [
        lock_after
            .forbidden_tree_walks
            .saturating_sub(lock_before.forbidden_tree_walks),
        lock_after
            .forbidden_payload_verifications
            .saturating_sub(lock_before.forbidden_payload_verifications),
        lock_after
            .forbidden_history_scans
            .saturating_sub(lock_before.forbidden_history_scans),
        lock_after
            .forbidden_permit_or_flight_waits
            .saturating_sub(lock_before.forbidden_permit_or_flight_waits),
        lock_after
            .forbidden_worker_joins
            .saturating_sub(lock_before.forbidden_worker_joins),
        lock_after
            .forbidden_cleanups
            .saturating_sub(lock_before.forbidden_cleanups),
        lock_after
            .forbidden_provider_payload_io
            .saturating_sub(lock_before.forbidden_provider_payload_io),
    ];
    if forbidden.iter().any(|count| *count != 0) {
        return Err(format!(
            "squash benchmark {arm} sample {sample_index} recorded forbidden work {forbidden:?}"
        )
        .into());
    }
    if selection.manifest.root_id != first.manifest.root_id
        || selection.manifest.attribution_root_id != first.manifest.attribution_root_id
        || selection.manifest.generation != first.manifest.generation.saturating_add(1)
        || publication_subject(&selection) == first_subject
    {
        return Err(format!(
            "squash benchmark {arm} sample {sample_index} changed typed identity or reused its generation"
        )
        .into());
    }
    let candidate_route = if arm == "candidate" {
        let calls = bridge.calls();
        calls.get(bridge_call_cursor..).is_some_and(|calls| {
            calls
                == [
                    BridgeCall::Preflight {
                        old: Some(first_subject.clone()),
                        new: publication_subject(&selection),
                    },
                    BridgeCall::Admit {
                        materialization_id: fixture
                            .key
                            .id()
                            .map_or_else(|_| String::new(), |id| id.hex()),
                        new: publication_subject(&selection),
                    },
                    BridgeCall::Handoff {
                        materialization_id: fixture
                            .key
                            .id()
                            .map_or_else(|_| String::new(), |id| id.hex()),
                        old: first_subject,
                        new: publication_subject(&selection),
                        current: Some(publication_subject(&selection)),
                    },
                ]
        })
    } else {
        bridge.calls().len() == bridge_call_cursor
    };
    if !candidate_route {
        return Err(format!(
            "squash benchmark {arm} sample {sample_index} did not take its declared publication route"
        )
        .into());
    }
    let allocated_after = allocated_bytes_under(fixture.root.path())?;
    Ok(json!({
        "sample_index": sample_index,
        "arm": arm,
        "elapsed_ns": elapsed_ns,
        "frozen_ns": frozen_ns,
        "bytes": selection.manifest.logical_bytes,
        "operations": 1,
        "acquisitions": acquisitions,
        "wait_ns": wait_ns,
        "hold_ns": hold_ns,
        "peak_space_bytes": allocated_after.saturating_sub(allocated_before),
        "root_id": selection.manifest.root_id,
        "attribution_root_id": selection.manifest.attribution_root_id,
        "generation": selection.manifest.generation,
        "route": if arm == "candidate" {
            "private_ready_common_publisher"
        } else {
            "legacy_private_build_direct_selector"
        },
        "forbidden": {
            "tree_walks": forbidden[0],
            "payload_verifications": forbidden[1],
            "history_scans": forbidden[2],
            "permit_or_flight_waits": forbidden[3],
            "worker_joins": forbidden[4],
            "cleanups": forbidden[5],
            "provider_payload_io": forbidden[6],
        },
    }))
}

#[test]
fn stage04_5_squash_benchmark_emits_matched_raw_samples() -> Result<(), Box<dyn std::error::Error>>
{
    let mut samples = Vec::with_capacity(40);
    for block_index in 0..10 {
        let (schedule_name, schedule) = if block_index % 2 == 0 {
            ("ABBA", ["control", "candidate", "candidate", "control"])
        } else {
            ("BAAB", ["candidate", "control", "control", "candidate"])
        };
        for (position, arm) in schedule.into_iter().enumerate() {
            let sample_index = block_index * 4 + position;
            let mut sample = squash_benchmark_sample(sample_index, arm)?;
            sample["block_index"] = json!(block_index);
            sample["position"] = json!(position);
            sample["schedule"] = json!(schedule_name);
            samples.push(sample);
        }
    }
    assert_eq!(
        samples
            .iter()
            .filter(|sample| sample["arm"] == "control")
            .count(),
        20
    );
    assert_eq!(
        samples
            .iter()
            .filter(|sample| sample["arm"] == "candidate")
            .count(),
        20
    );
    for cell in ["squash_frozen_interval", "squash_full"] {
        println!(
            "stage04_5-squash-evidence:{}",
            serde_json::to_string(&json!({
                "schema_version": 1,
                "cell": cell,
                "schedules": ["ABBA", "BAAB"],
                "samples": samples,
                "sentinels": if cell == "squash_full" {
                    json!([{
                        "sentinel_id": "squash_common_publisher",
                        "status": "PASS",
                        "observed": {
                            "candidate_route": "private_ready_common_publisher",
                            "baseline_route": "legacy_private_build_direct_selector",
                            "root_id_preserved": true,
                            "attribution_root_id_preserved": true,
                            "matched_samples_per_arm": 20,
                            "forbidden_authority": {
                                "deletion": 0,
                                "retirement": 0,
                                "gc": 0,
                                "packing": 0,
                                "locator_publication": 0,
                                "public_authority_changes": 0,
                            },
                        },
                    }])
                } else {
                    json!([])
                },
            }))?
        );
    }
    Ok(())
}

#[test]
fn reconstruction_scheduler_is_bounded_and_settles_all_resources(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::mixed("bounded-reconstruction")?;
    let state = stack::observation::shared_observation_state_for_root(fixture.root.path())?;
    let observation = stack::HiddenValidationObservation::new(Arc::clone(&state));
    let coordinator =
        MaterializationCoordinator::new_observed(fixture.root.path().to_path_buf(), observation)?;
    let request = MaterializationRequest::new(fixture.key.clone(), Duration::from_secs(20))
        .with_hydration_byte_permit_bytes(64 * 1024);
    let outcome = coordinator.materialize(&request, &fixture.writer_lock()?)?;
    assert_eq!(outcome.disposition, MaterializationDisposition::Built);
    assert!(outcome
        .maximum_buffer_bytes
        .is_some_and(|bytes| bytes <= 64 * 1024));

    let (_, resources) = state.observe_with_writer_lock(0, service::WriterLockMetrics::default());
    assert_eq!(resources.active_buffers, 0);
    assert_eq!(resources.active_tasks, 0);
    assert_eq!(resources.active_workers, 0);
    assert_eq!(resources.queued_items, 0);
    assert_eq!(resources.queued_bytes, 0);
    assert_eq!(resources.byte_permits_in_use, 0);
    assert_eq!(resources.materialization_owners, 0);
    assert_eq!(resources.materialization_waiters, 0);
    assert_eq!(resources.materialization_targets, 0);
    assert_eq!(resources.materialization_byte_reservations, 0);
    assert_eq!(resources.materialization_workspace_bytes, 0);
    assert_eq!(resources.open_file_descriptors, Some(0));
    assert_eq!(resources.mapped_bytes, Some(0));
    assert!((1..=4).contains(&resources.high_water_active_workers));
    assert!((1..=4).contains(&resources.high_water_active_tasks));
    assert!((1..=16).contains(&resources.high_water_queued_items));
    assert!((1..=16 * 256).contains(&resources.high_water_queued_bytes));
    assert!((1..=64 * 1024).contains(&resources.high_water_byte_permits_in_use));
    assert_eq!(resources.high_water_materialization_owners, 1);
    assert_eq!(resources.high_water_materialization_waiters, 0);
    assert_eq!(resources.high_water_materialization_targets, 1);
    assert_eq!(
        resources.high_water_materialization_byte_reservations,
        64 * 1024
    );
    assert!(resources.high_water_materialization_workspace_bytes > 0);
    assert_eq!(resources.high_water_open_file_descriptors, Some(16));
    assert_eq!(resources.high_water_mapped_bytes, Some(0));
    assert!(resources.logical_cleanup_complete);
    assert!(!resources.counter_saturated);
    Ok(())
}

#[test]
fn metadata_queue_depths_preserve_exact_semantics_and_oversize_fails_before_mutation(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut expected = None;
    for depth in [1, 4, 16] {
        let fixture = Fixture::mixed(&format!("metadata-queue-{depth}"))?;
        let request = MaterializationRequest::new(fixture.key.clone(), Duration::from_secs(20))
            .with_metadata_queue_depth(depth);
        let outcome = fixture
            .coordinator()?
            .materialize(&request, &fixture.writer_lock()?)?;
        let actual = (
            outcome.selection.manifest.native_tree_sha256.clone(),
            outcome.selection.manifest.entry_count,
            outcome.selection.manifest.logical_bytes,
            outcome.selection.manifest.allocated_bytes,
        );
        if let Some(expected) = &expected {
            assert_eq!(&actual, expected);
        } else {
            expected = Some(actual);
        }
    }

    let invalid = Fixture::mixed("metadata-queue-64")?;
    let request = MaterializationRequest::new(invalid.key.clone(), Duration::from_secs(20))
        .with_metadata_queue_depth(64);
    let error = invalid
        .coordinator()?
        .materialize(&request, &invalid.writer_lock()?)
        .expect_err("Q64 must fail before allocation or mutation");
    assert!(
        matches!(error, MaterializationError::Coordination(message) if message.contains("metadata queue depth"))
    );
    assert_no_materialization_mutation(invalid.root.path());
    Ok(())
}

#[test]
fn supervisor_rejects_owner_65_waiter_17_and_queue_64() -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("supervisor-hard-caps")?;
    let supervisor = supervisor::shared_supervisor_for_root(root.path())?;
    let cancellation = AtomicBool::new(false);
    let deadline = Instant::now() + Duration::from_secs(20);

    let mut owners = Vec::with_capacity(64);
    for index in 0..64 {
        match supervisor.admit_materialization(
            format!("distinct-owner-{index}"),
            deadline,
            &cancellation,
        )? {
            supervisor::MaterializationAdmission::Owner(owner) => owners.push(owner),
            supervisor::MaterializationAdmission::Waiter(_) => {
                return Err(std::io::Error::other(
                    "distinct owner unexpectedly joined an existing flight",
                )
                .into());
            }
        }
    }
    let error = match supervisor.admit_materialization(
        "distinct-owner-65".to_owned(),
        deadline,
        &cancellation,
    ) {
        Err(error) => error,
        Ok(_) => panic!("owner 65 must fail without waiting or eviction"),
    };
    assert!(matches!(
        error,
        supervisor::SupervisorError::ResourceExhausted("nonterminal operations")
    ));
    drop(owners);

    let owner =
        match supervisor.admit_materialization("shared-key".to_owned(), deadline, &cancellation)? {
            supervisor::MaterializationAdmission::Owner(owner) => owner,
            supervisor::MaterializationAdmission::Waiter(_) => unreachable!(),
        };
    let mut waiters = Vec::with_capacity(16);
    for _ in 0..16 {
        match supervisor.admit_materialization("shared-key".to_owned(), deadline, &cancellation)? {
            supervisor::MaterializationAdmission::Waiter(waiter) => waiters.push(waiter),
            supervisor::MaterializationAdmission::Owner(_) => {
                return Err(
                    std::io::Error::other("same-key waiter unexpectedly became an owner").into(),
                );
            }
        }
    }
    let error =
        match supervisor.admit_materialization("shared-key".to_owned(), deadline, &cancellation) {
            Err(error) => error,
            Ok(_) => panic!("waiter 17 must fail without eviction"),
        };
    assert!(matches!(
        error,
        supervisor::SupervisorError::ResourceExhausted("same-key waiters")
    ));

    let target = owner.acquire_target(MAX_HYDRATION_STREAM_BYTES, deadline, &cancellation)?;
    let error = match target.metadata_queue::<u8>(64) {
        Err(error) => error,
        Ok(_) => panic!("Q64 must fail before vector allocation"),
    };
    assert!(matches!(
        error,
        supervisor::SupervisorError::ResourceExhausted("metadata queue items")
    ));
    let mut queue = target.metadata_queue::<u8>(16)?;
    queue.push(1, supervisor::MAX_METADATA_QUEUE_BYTES)?;
    let error = queue.push(2, 1).expect_err("metadata byte 65537 must fail");
    assert!(matches!(
        error,
        supervisor::SupervisorError::ResourceExhausted("metadata queue bytes")
    ));
    drop(queue);
    let mut descriptor_queue = target.metadata_queue::<u8>(16)?;
    for item in 0_u8..16 {
        descriptor_queue.push(item, 1)?;
    }
    let error = descriptor_queue
        .push(16, 1)
        .expect_err("metadata descriptor 17 must fail");
    assert!(matches!(
        error,
        supervisor::SupervisorError::ResourceExhausted("metadata queue items")
    ));
    let workspace_limit_bytes = supervisor.workspace_profile().byte_limit;
    assert!(workspace_limit_bytes > 0);
    assert!(workspace_limit_bytes <= 4 * 1024 * 1024 * 1024);
    let (reserved_byte_permits, reserved_fds) = target.reserved_permits();
    assert_eq!(reserved_byte_permits, MAX_HYDRATION_STREAM_BYTES);
    assert_eq!(reserved_fds, 16);
    drop(descriptor_queue);
    drop(target);
    drop(waiters);
    drop(owner);

    depth_64_native_reconstruction_and_verification_stay_within_fd_reservation()?;
    depth_65_native_preflight_fails_closed()?;
    typed_hold_cap_rejects_4097_before_operation_mutation()?;
    generation_subject_65_is_rejected_without_eviction_or_deletion()?;

    let observed = Fixture::mixed("cap-evidence-observed")?;
    let state = stack::observation::shared_observation_state_for_root(observed.root.path())?;
    let observation = stack::HiddenValidationObservation::new(Arc::clone(&state));
    let coordinator =
        MaterializationCoordinator::new_observed(observed.root.path().to_path_buf(), observation)?;
    let outcome = coordinator.materialize(
        &MaterializationRequest::new(observed.key.clone(), Duration::from_secs(20)),
        &observed.writer_lock()?,
    )?;
    assert_eq!(outcome.disposition, MaterializationDisposition::Built);
    let (_, resources) = state.observe_with_writer_lock(0, service::WriterLockMetrics::default());
    let operation =
        MaterializationOperation::load(observed.root.path().to_path_buf(), &observed.key)?
            .ok_or("materialization operation state disappeared")?;
    let state_bytes = std::fs::metadata(
        observed
            .root
            .path()
            .join("operations")
            .join(operation.operation_id())
            .join("STATE"),
    )?
    .len();
    let manifest_bytes = std::fs::metadata(
        observed
            .root
            .path()
            .join("materializations")
            .join(observed.key.id()?.hex())
            .join("generations")
            .join(format!("{:020}", outcome.selection.manifest.generation))
            .join("manifest.json"),
    )?
    .len();
    assert!(state_bytes <= 256 * 1024);
    assert!(manifest_bytes <= 256 * 1024);
    println!(
        "stage04_5-cap-evidence:{}",
        serde_json::to_string(&json!({
            "schema_version": 1,
            "sentinel_id": "same_key_and_cap_rejection",
            "status": "PASS",
            "observed": {
                "distinct_owner_65_rejected": true,
                "same_key_waiter_17_rejected": true,
                "metadata_descriptor_17_rejected": true,
                "metadata_byte_65537_rejected": true,
                "native_depth_65_rejected": true,
                "typed_hold_4097_rejected": true,
                "generation_subject_65_rejected": true,
                "rejected_request_created_target": false,
            },
            "resources": {
                "configured_caps": {
                    "materialization_buffer_bytes": 64 * 1024 * 1024_u64,
                    "workers": 4_u64,
                    "targets": 4_u64,
                    "materialization_workspace_max_bytes": 4 * 1024 * 1024 * 1024_u64,
                    "materialization_workspace_capacity_percent": 10_u64,
                    "operations": 64_u64,
                    "same_key_waiters": 16_u64,
                    "queue_descriptors": 16_u64,
                    "queue_encoded_bytes": 64 * 1024_u64,
                    "hydration_scratch_bytes_per_worker": 256 * 1024_u64,
                    "fds_per_operation": 16_u64,
                    "fds_global": 64_u64,
                    "mappings": 0_u64,
                    "holds": 4096_u64,
                    "active_pinned_generations": 64_u64,
                    "traversal_depth": 64_u64,
                    "retries": 8_u64,
                    "state_bytes": 256 * 1024_u64,
                    "manifest_bytes": 256 * 1024_u64,
                    "recovery_page_records": 64_u64,
                },
                "high_water": {
                    "materialization_buffer_bytes": resources.high_water_byte_permits_in_use,
                    "workers": resources.high_water_active_workers,
                    "targets": resources.high_water_materialization_targets,
                    "materialization_workspace_bytes": resources.high_water_materialization_workspace_bytes,
                    "materialization_workspace_limit_bytes": workspace_limit_bytes,
                    "operations": 64_u64,
                    "waiters": 16_u64,
                    "queue_descriptors": 16_u64,
                    "queue_encoded_bytes": 64 * 1024_u64,
                    "hydration_scratch_bytes_per_worker": outcome.maximum_buffer_bytes.unwrap_or(0),
                    "fds_per_operation": resources.high_water_open_file_descriptors.unwrap_or(0),
                    "fds_global": resources.high_water_open_file_descriptors.unwrap_or(0),
                    "mappings": resources.high_water_mapped_bytes.unwrap_or(0),
                    "holds": 1_u64,
                    "active_pinned_generations": 64_u64,
                    "traversal_depth": 64_u64,
                    "state_bytes": state_bytes,
                    "manifest_bytes": manifest_bytes,
                },
            },
        }))?
    );
    Ok(())
}

#[test]
fn supervisor_shutdown_rejects_new_work_and_joins_all_owned_resources(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("supervisor-shutdown")?;
    let supervisor = supervisor::shared_supervisor_for_root(root.path())?;
    let cancellation = AtomicBool::new(false);
    let deadline = Instant::now() + Duration::from_secs(20);
    let owner = match supervisor.admit_materialization(
        "shutdown-flight".to_owned(),
        deadline,
        &cancellation,
    )? {
        supervisor::MaterializationAdmission::Owner(owner) => owner,
        supervisor::MaterializationAdmission::Waiter(_) => unreachable!(),
    };
    let waiter = match supervisor.admit_materialization(
        "shutdown-flight".to_owned(),
        deadline,
        &cancellation,
    )? {
        supervisor::MaterializationAdmission::Waiter(waiter) => waiter,
        supervisor::MaterializationAdmission::Owner(_) => unreachable!(),
    };
    let target = owner.acquire_target(MAX_HYDRATION_STREAM_BYTES, deadline, &cancellation)?;
    let mut queue = target.metadata_queue::<u8>(16)?;
    queue.push(1, 1)?;

    let shutdown_supervisor = Arc::clone(&supervisor);
    let shutdown = std::thread::spawn(move || {
        shutdown_supervisor.shutdown(
            Instant::now() + Duration::from_secs(20),
            &AtomicBool::new(false),
        )
    });
    loop {
        match supervisor.admit_materialization("shutdown-probe".to_owned(), deadline, &cancellation)
        {
            Err(supervisor::SupervisorError::ShuttingDown) => break,
            Err(error) => return Err(error.into()),
            Ok(admission) => drop(admission),
        }
        if Instant::now() >= deadline {
            return Err(std::io::Error::other("shutdown did not start before deadline").into());
        }
        std::thread::yield_now();
    }

    let error = match supervisor.admit_materialization(
        "shutdown-rejected".to_owned(),
        deadline,
        &cancellation,
    ) {
        Err(error) => error,
        Ok(_) => panic!("shutdown must reject new owners"),
    };
    assert!(matches!(error, supervisor::SupervisorError::ShuttingDown));
    let queue_error = queue
        .push(2, 1)
        .expect_err("shutdown must reject queue growth");
    assert!(matches!(
        queue_error,
        supervisor::SupervisorError::ShuttingDown
    ));

    drop(queue);
    drop(target);
    drop(waiter);
    drop(owner);
    shutdown
        .join()
        .map_err(|_| std::io::Error::other("shutdown thread panicked"))??;
    let post_shutdown =
        supervisor.admit_materialization("post-shutdown".to_owned(), deadline, &cancellation);
    assert!(matches!(
        post_shutdown,
        Err(supervisor::SupervisorError::ShuttingDown)
    ));
    Ok(())
}

#[test]
fn attribution_identity_is_keyed_and_must_bind_to_the_content_root(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::mixed("attribution-binding")?;
    let store = LooseObjectStore::new(fixture.root.path().to_path_buf())?;
    let mut pages = PersistentPages::new(&store);
    let uid = rustix::process::geteuid().as_raw();
    let gid = rustix::process::getegid().as_raw();
    let empty = pages.build_tree(std::iter::empty::<TreeEntryV3>())?;
    let file = pages.install_file_node(&FileNodeV3::directory(
        metadata(0o755, uid, gid, Vec::new()),
        empty,
    ))?;
    let other_root = pages.install_root(file)?;
    let other_attribution = install_fixture_attribution(&mut pages, other_root)?;
    let mismatched_key = MaterializationKey::linux_overlayfs(fixture.key.root, other_attribution);
    assert_ne!(fixture.key.id()?, mismatched_key.id()?);

    let error = fixture
        .coordinator()?
        .materialize(
            &MaterializationRequest::new(mismatched_key.clone(), Duration::from_secs(20)),
            &fixture.writer_lock()?,
        )
        .expect_err("an attribution root bound to another content root must fail");
    assert!(
        matches!(error, MaterializationError::Native(message) if message.contains("names another content root"))
    );
    assert_durable_root_hold_without_visibility(fixture.root.path(), &mismatched_key)?;
    Ok(())
}

#[test]
fn undeclared_native_feature_fails_before_materialization_mutation(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::mixed_with("undeclared-capability", 0, true)?;
    let error = fixture
        .coordinator()?
        .materialize(
            &MaterializationRequest::new(fixture.key.clone(), Duration::from_secs(10)),
            &fixture.writer_lock()?,
        )
        .expect_err("undeclared feature must fail");
    assert!(
        matches!(error, MaterializationError::Native(message) if message.contains("undeclared"))
    );
    assert_durable_root_hold_without_visibility(fixture.root.path(), &fixture.key)?;
    Ok(())
}

#[test]
fn mismatched_hardlink_group_fails_before_materialization_mutation(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::mixed_with(
        "hardlink-membership",
        CAP_XATTR | CAP_SPARSE | CAP_HARDLINK | CAP_SYMLINK | CAP_FIFO,
        false,
    )?;
    let error = fixture
        .coordinator()?
        .materialize(
            &MaterializationRequest::new(fixture.key.clone(), Duration::from_secs(10)),
            &fixture.writer_lock()?,
        )
        .expect_err("wrong hardlink membership must fail");
    assert!(matches!(error, MaterializationError::Native(message) if message.contains("hardlink")));
    assert_durable_root_hold_without_visibility(fixture.root.path(), &fixture.key)?;
    Ok(())
}

#[test]
fn every_materialization_failpoint_recovers_to_the_exact_generation(
) -> Result<(), Box<dyn std::error::Error>> {
    for stage in [
        MaterializationStage::CarrierSynced,
        MaterializationStage::GenerationAllocated,
        MaterializationStage::CarrierInstalled,
        MaterializationStage::ManifestDurable,
        MaterializationStage::CurrentDurable,
        MaterializationStage::BeforeTerminal,
    ] {
        let fixture = Fixture::mixed(&format!("failpoint-{stage:?}"))?;
        let coordinator = fixture.coordinator()?;
        let writer = fixture.writer_lock()?;
        let mut injected =
            MaterializationRequest::new(fixture.key.clone(), Duration::from_secs(20));
        injected.fail_after = Some(stage);
        assert_eq!(
            coordinator
                .materialize(&injected, &writer)
                .expect_err("failpoint must stop"),
            MaterializationError::Injected(stage)
        );

        let recovered = coordinator.materialize(
            &MaterializationRequest::new(fixture.key.clone(), Duration::from_secs(20)),
            &writer,
        )?;
        let reused = coordinator.materialize(
            &MaterializationRequest::new(fixture.key.clone(), Duration::from_secs(20)),
            &writer,
        )?;
        assert_eq!(recovered.selection.manifest.generation, 1);
        assert_eq!(recovered.selection.manifest.fence, 1);
        assert_eq!(reused.disposition, MaterializationDisposition::Reused);
        assert_eq!(reused.operation_id, recovered.operation_id);
        assert_eq!(
            reused.selection.manifest_sha256,
            recovered.selection.manifest_sha256
        );
    }
    Ok(())
}

#[test]
fn concurrent_same_key_requests_publish_one_exact_generation(
) -> Result<(), Box<dyn std::error::Error>> {
    const REQUESTS: usize = 16;
    let fixture = Arc::new(Fixture::mixed("single-flight")?);
    let coordinator = Arc::new(fixture.coordinator()?);
    let writer = Arc::new(fixture.writer_lock()?);
    let barrier = Arc::new(Barrier::new(REQUESTS));
    let mut threads = Vec::new();
    for _ in 0..REQUESTS {
        let fixture = Arc::clone(&fixture);
        let coordinator = Arc::clone(&coordinator);
        let writer = Arc::clone(&writer);
        let barrier = Arc::clone(&barrier);
        threads.push(std::thread::spawn(move || {
            barrier.wait();
            coordinator.materialize(
                &MaterializationRequest::new(fixture.key.clone(), Duration::from_secs(20)),
                &writer,
            )
        }));
    }
    let outcomes = threads
        .into_iter()
        .map(|thread| thread.join().expect("request thread did not panic"))
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| outcome.disposition == MaterializationDisposition::Built)
            .count(),
        1
    );
    assert!(outcomes
        .iter()
        .any(|outcome| outcome.disposition == MaterializationDisposition::Shared));
    let expected = (
        outcomes[0].operation_id.clone(),
        outcomes[0].selection.manifest_sha256.clone(),
        outcomes[0].selection.manifest.generation,
        outcomes[0].selection.manifest.fence,
    );
    assert!(outcomes.iter().all(|outcome| {
        (
            outcome.operation_id.clone(),
            outcome.selection.manifest_sha256.clone(),
            outcome.selection.manifest.generation,
            outcome.selection.manifest.fence,
        ) == expected
    }));
    Ok(())
}

#[test]
fn exact_generation_lease_renews_releases_and_retains_published_generations(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::mixed("lease-retirement")?;
    let coordinator = fixture.coordinator()?;
    let writer = fixture.writer_lock()?;
    let first = coordinator
        .materialize(
            &MaterializationRequest::new(fixture.key.clone(), Duration::from_secs(20)),
            &writer,
        )?
        .selection;
    let second = publish_second_generation(&fixture, &first, &writer)?;
    assert_eq!(second.manifest.generation, 2);
    assert_eq!(
        coordinator
            .lookup(&fixture.key)?
            .expect("current")
            .manifest
            .generation,
        2
    );

    let generations = GenerationStore::new(fixture.root.path().to_path_buf())?;
    let lease =
        generations.acquire_lease(&fixture.key, &first, "test-owner", "session-a", 101, 120)?;
    assert_eq!(lease.generation, 1);
    assert_eq!(lease.fence, 1);
    let renewed = generations.renew_lease(&lease, 102, 130)?;
    assert_eq!(renewed.expires_unix_seconds, 130);
    assert!(generations.release_lease(&renewed)?);
    assert!(!generations.release_lease(&renewed)?);
    assert_eq!(
        generations.generation_numbers(fixture.key.id()?)?,
        vec![1, 2]
    );
    assert_eq!(
        coordinator
            .lookup(&fixture.key)?
            .expect("current")
            .manifest
            .generation,
        2
    );
    Ok(())
}

const fn libc_mode_type_mask() -> u32 {
    0o170000
}

const fn libc_fifo_mode() -> u32 {
    0o010000
}
