#![forbid(unsafe_code)]
#![cfg(target_os = "linux")]

use std::ffi::OsStr;
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::time::Duration;

use sandbox_runtime_layerstack_core::{Digest32, RecordKindV3, RootId};

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
    GenerationManifest, GenerationSelection, GenerationStore, MaterializationKey,
};
use stack::candidate::materialization::{
    GenerationRetentionReason, GenerationRetirementOutcome, MaterializationCoordinator,
    MaterializationDisposition, MaterializationError, MaterializationRequest, MaterializationStage,
};
use stack::candidate::native_backend::{
    NativeBackend, CAP_FIFO, CAP_HARDLINK, CAP_SPARSE, CAP_SYMLINK, CAP_XATTR,
};
use stack::candidate::object_store::LooseObjectStore;
use stack::candidate::tree::{
    FileKindV3, FileNodeV3, MetadataV3, PersistentPages, SegmentDescriptor, SegmentKind,
    TreeEntryV3,
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
        Ok(Self {
            root,
            key: MaterializationKey::linux_overlayfs(logical_root),
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
    let key = MaterializationKey::linux_overlayfs(root_id);
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
    let required_capabilities = backend.preflight(&mut pages, root_id)?;
    assert_eq!(required_capabilities.feature_bits, expected_capabilities);
    let carrier = root.path().join("cold-reconstruction-carrier");

    benchmark_phase_boundary("setup_complete", true)?;
    let built = backend.reconstruct(&mut pages, root_id, &carrier, || Ok(()))?;
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

fn assert_no_materialization_mutation(root: &Path) {
    assert!(!root.join("operations").exists());
    assert!(!root.join("materializations").exists());
    assert!(!root.join("refs/leases").exists());
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
    let generations = GenerationStore::new(fixture.root.path().to_path_buf())?;
    let id = fixture.key.id()?;
    let (generation, fence) = generations.next_generation(id)?;
    assert_eq!((generation, fence), (2, 2));
    let work = fixture.root.path().join("manual-generation-two");
    let store = LooseObjectStore::new(fixture.root.path().to_path_buf())?;
    let mut pages = PersistentPages::new(&store);
    let backend = NativeBackend::new();
    let build = backend.reconstruct(&mut pages, fixture.key.root, &work, || Ok(()))?;
    let verified = backend.verify(&mut pages, fixture.key.root, &work)?;
    assert_eq!(verified.native_tree_sha256, build.native_tree_sha256);
    assert_eq!(verified.entry_count, build.entry_count);
    assert_eq!(verified.logical_bytes, build.logical_bytes);
    assert_eq!(verified.allocated_bytes, build.allocated_bytes);
    let mut manifest: GenerationManifest = first.manifest.clone();
    manifest.generation = generation;
    manifest.fence = fence;
    manifest.native_tree_sha256 = build.native_tree_sha256.clone();
    manifest.carriers[0].native_tree_sha256 = build.native_tree_sha256;
    manifest.entry_count = build.entry_count;
    manifest.logical_bytes = build.logical_bytes;
    manifest.allocated_bytes = build.allocated_bytes;
    manifest.build_operation_id = "22".repeat(32);
    manifest.completed_unix_seconds = manifest.completed_unix_seconds.saturating_add(1);
    mutate(&mut manifest);
    let _guard = writer.exclusive()?;
    generations.install_carrier(id, generation, &work)?;
    generations.publish_manifest(&fixture.key, &manifest)?;
    Ok(generations.promote_generation(&fixture.key, generation)?)
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

    let (_, resources) = state.observe(0);
    assert_eq!(resources.active_buffers, 0);
    assert_eq!(resources.active_tasks, 0);
    assert_eq!(resources.active_workers, 0);
    assert_eq!(resources.queued_items, 0);
    assert_eq!(resources.queued_bytes, 0);
    assert_eq!(resources.byte_permits_in_use, 0);
    assert!((1..=4).contains(&resources.high_water_active_workers));
    assert!((1..=4).contains(&resources.high_water_active_tasks));
    assert!((1..=64 * 1024).contains(&resources.high_water_queued_bytes));
    assert!((1..=64 * 1024).contains(&resources.high_water_byte_permits_in_use));
    assert!(resources.logical_cleanup_complete);
    assert!(!resources.counter_saturated);
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
    assert_no_materialization_mutation(fixture.root.path());
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
    assert_no_materialization_mutation(fixture.root.path());
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
fn exact_generation_lease_renews_releases_and_blocks_grace_deletion(
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

    let started = coordinator.begin_generation_retirement(&fixture.key, 1, 100, 5, &writer)?;
    let GenerationRetirementOutcome::GraceStarted(ticket) = started else {
        panic!("old generation should enter grace");
    };
    assert_eq!(
        coordinator.finish_generation_retirement(&ticket, 104, &writer)?,
        GenerationRetirementOutcome::GracePending(ticket.clone())
    );

    let generations = GenerationStore::new(fixture.root.path().to_path_buf())?;
    let lease =
        generations.acquire_lease(&fixture.key, &first, "test-owner", "session-a", 101, 120)?;
    assert_eq!(lease.generation, 1);
    assert_eq!(lease.fence, 1);
    let renewed = generations.renew_lease(&lease, 102, 130)?;
    assert_eq!(renewed.expires_unix_seconds, 130);
    assert_eq!(
        coordinator.finish_generation_retirement(&ticket, 105, &writer)?,
        GenerationRetirementOutcome::Protected(GenerationRetentionReason::ExactGenerationLease)
    );
    assert!(generations.release_lease(&renewed)?);
    assert!(!generations.release_lease(&renewed)?);

    let GenerationRetirementOutcome::GraceStarted(ticket) =
        coordinator.begin_generation_retirement(&fixture.key, 1, 200, 5, &writer)?
    else {
        panic!("released generation should re-enter grace");
    };
    assert_eq!(
        coordinator.finish_generation_retirement(&ticket, 205, &writer)?,
        GenerationRetirementOutcome::Deleted
    );
    assert_eq!(generations.generation_numbers(fixture.key.id()?)?, vec![2]);
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
