use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use sandbox_runtime_mpla_poc::evacuation::{
    EvacuationPhase, EvacuationRequest, EvacuationStore, StageFiveRetirementAuthorization,
};
use sandbox_runtime_mpla_poc::locator::{
    ForwardLocatorEntry, LocatorDelta, LocatorExtent, LocatorReplacement, LocatorStore,
    PayloadRootId, ReverseLocatorEntry,
};
use sandbox_runtime_mpla_poc::reconcile::{reconcile, LeakCounts, StorageCategoryRoot};
use sandbox_runtime_mpla_poc::{
    AdmissionController, AdmissionTier, AllocationId, NamedFaultInjector, OperationId, PocError,
    PublicationId, SCHEMA_VERSION,
};
use sha2::{Digest, Sha256};
use uuid::Uuid;

const HOST_EVACUATION_BYTES: usize = 4 * 1024 * 1024;

struct TestRoot {
    path: PathBuf,
}

impl TestRoot {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!("mpla-m2-heavy-{label}-{}", Uuid::new_v4()));
        std::fs::create_dir(&path).expect("create heavy test root");
        Self { path }
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[test]
fn hv06_rejects_job_33_before_allocation_and_bounds_all_queued_state() {
    let controller = AdmissionController::new();
    let mut guards = Vec::new();
    for _ in 0..32 {
        guards.push(Some(controller.submit(4_096).expect("admit bounded work")));
    }

    let active = guards
        .iter()
        .flatten()
        .filter(|guard| guard.receipt().tier == AdmissionTier::ActiveData)
        .collect::<Vec<_>>();
    let queued_coordinators = guards
        .iter()
        .flatten()
        .filter(|guard| guard.receipt().tier == AdmissionTier::Coordinator)
        .collect::<Vec<_>>();
    let pending = guards
        .iter()
        .flatten()
        .filter(|guard| guard.receipt().tier == AdmissionTier::PendingDescriptor)
        .collect::<Vec<_>>();
    assert_eq!(active.len(), 4);
    assert_eq!(queued_coordinators.len(), 12);
    assert_eq!(pending.len(), 16);
    assert!(queued_coordinators
        .iter()
        .chain(pending.iter())
        .all(|guard| {
            let receipt = guard.receipt();
            !receipt.owns_payload_allocation
                && !receipt.owns_workspace_mount
                && !receipt.owns_staging_allocation
        }));

    let saturated = controller.snapshot().expect("saturated resource snapshot");
    assert_eq!(saturated.submitted_jobs, 32);
    assert_eq!(saturated.active_data_workers, 4);
    assert_eq!(saturated.coordinators, 16);
    assert_eq!(saturated.pending_descriptors, 16);
    assert_eq!(saturated.pending_descriptor_bytes, 65_536);
    assert_eq!(saturated.private_allocations, 4);
    assert_eq!(saturated.active_mounts, 4);
    assert_eq!(saturated.staging_allocations, 0);

    assert!(matches!(
        controller.submit(1),
        Err(PocError::Overloaded(message))
            if message.contains("job 33 rejected before resource ownership")
    ));
    assert_eq!(
        controller.snapshot().expect("unchanged overload snapshot"),
        saturated
    );

    assert!(guards[5]
        .as_mut()
        .expect("job 6")
        .try_promote()
        .expect("full-capacity promotion check")
        .is_none());
    let mut published_jobs = Vec::new();
    for (index, guard) in guards.iter_mut().take(4).enumerate() {
        let receipt = guard.as_ref().expect("initial active receipt").receipt();
        assert_eq!(
            receipt.job_ordinal,
            u32::try_from(index + 1).expect("job ordinal")
        );
        assert_eq!(receipt.tier, AdmissionTier::ActiveData);
        published_jobs.push(receipt.job_ordinal);
        drop(guard.take());
    }
    assert!(guards[5]
        .as_mut()
        .expect("job 6")
        .try_promote()
        .expect("FIFO promotion check")
        .is_none());

    for (index, guard_slot) in guards.iter_mut().enumerate().skip(4) {
        let expected_ordinal = u32::try_from(index + 1).expect("queued ordinal");
        {
            let guard = guard_slot.as_mut().expect("queued admission");
            loop {
                let promoted = guard
                    .try_promote()
                    .expect("promote admitted work")
                    .expect("FIFO head must make progress");
                assert_eq!(promoted.job_ordinal, expected_ordinal);
                if promoted.tier == AdmissionTier::ActiveData {
                    assert!(promoted.owns_payload_allocation);
                    assert!(promoted.owns_workspace_mount);
                    assert!(!promoted.owns_staging_allocation);
                    published_jobs.push(promoted.job_ordinal);
                    break;
                }
                assert_eq!(promoted.tier, AdmissionTier::Coordinator);
                assert!(!promoted.owns_payload_allocation);
                assert!(!promoted.owns_workspace_mount);
                assert!(!promoted.owns_staging_allocation);
            }
        }
        drop(guard_slot.take());
    }
    assert_eq!(published_jobs, (1_u32..=32).collect::<Vec<_>>());

    let drained = controller.snapshot().expect("drained resource snapshot");
    assert_eq!(drained.submitted_jobs, 32);
    assert_eq!(drained.active_data_workers, 0);
    assert_eq!(drained.coordinators, 0);
    assert_eq!(drained.pending_descriptors, 0);
    assert_eq!(drained.pending_descriptor_bytes, 0);
    assert_eq!(drained.private_allocations, 0);
    assert_eq!(drained.active_mounts, 0);
    assert_eq!(drained.staging_allocations, 0);
}

#[test]
fn hv09_pack_retains_pinned_source_until_authorized_retirement() {
    let root = TestRoot::new("evacuation");
    let source_dir = root.path.join("source");
    std::fs::create_dir(&source_dir).expect("create source directory");
    let source_path = source_dir.join("payload.bin");
    let expected_digest = write_fixture(&source_path, HOST_EVACUATION_BYTES);
    sync_directory(&source_dir);
    let source_metadata = source_path.metadata().expect("source metadata");

    let store = EvacuationStore::open(root.path.join("evacuation")).expect("open evacuation");
    let locator_store = LocatorStore::open(root.path.join("locators")).expect("open locator");
    let operation_id = OperationId::from_string("operation-hv09");
    let publication_id = PublicationId::from_string("publication-hv09");
    let payload_root =
        PayloadRootId::parse(expected_digest.clone()).expect("payload root from fixture digest");
    let source_allocation_id = AllocationId::from_string("allocation-source");
    let target_allocation_id = AllocationId::from_string("allocation-target");
    let source_operation_id = OperationId::from_string("operation-hv09-source");
    let source_publication_id = PublicationId::from_string("publication-hv09-source");
    let source_locator = locator_store
        .install(
            &LocatorDelta {
                schema_version: SCHEMA_VERSION,
                operation_id: source_operation_id.clone(),
                publication_id: source_publication_id.clone(),
                expected_parent: None,
                forward: vec![ForwardLocatorEntry {
                    payload_root: payload_root.clone(),
                    allocation_id: source_allocation_id.clone(),
                    owner_epoch: 7,
                    extents: vec![LocatorExtent {
                        relative_path: "source/payload.bin".to_owned(),
                        offset: 0,
                        length: source_metadata.len(),
                    }],
                }],
                reverse: vec![ReverseLocatorEntry {
                    allocation_id: source_allocation_id.clone(),
                    owner_epoch: 7,
                    operation_id: source_operation_id,
                    publication_id: source_publication_id,
                    payload_roots: vec![payload_root.clone()],
                    accounted_bytes: source_metadata.blocks() * 512,
                }],
            },
            &mut NamedFaultInjector::default(),
        )
        .expect("install source locator");
    let source_generation = source_locator.generation;
    let target_generation = source_generation.checked_next().expect("target generation");
    let request = EvacuationRequest {
        schema_version: SCHEMA_VERSION,
        operation_id: operation_id.clone(),
        publication_id: publication_id.clone(),
        payload_root: payload_root.clone(),
        source_allocation_id: source_allocation_id.clone(),
        source_owner_epoch: 7,
        source_generation,
        source_payload_path: source_path.clone(),
        source_logical_bytes: source_metadata.len(),
        source_allocated_bytes: source_metadata.blocks() * 512,
        target_allocation_id: target_allocation_id.clone(),
        target_owner_epoch: 11,
        target_payload_path: store.pack_path(&operation_id),
    };

    let prepared = store.prepare(&request).expect("prepare evacuation");
    assert_eq!(prepared.phase, EvacuationPhase::Building);
    let mut pinned_source = store
        .pin_selected(&operation_id)
        .expect("pin source reader");
    assert_eq!(pinned_source.generation(), source_generation);
    assert_eq!(pinned_source.allocation_id(), &source_allocation_id);
    assert_eq!(pinned_source.owner_epoch(), 7);

    let ready = store.build_pack(&operation_id).expect("build pack");
    assert_eq!(ready.phase, EvacuationPhase::Ready);
    assert_eq!(
        ready.payload_sha256.as_deref(),
        Some(expected_digest.as_str())
    );
    assert_eq!(
        ready.honest_old_plus_new_peak_bytes,
        ready.source_allocated_bytes + ready.target_allocated_bytes
    );

    let replacement = LocatorReplacement {
        schema_version: SCHEMA_VERSION,
        operation_id: operation_id.clone(),
        publication_id: publication_id.clone(),
        payload_root: payload_root.clone(),
        expected_parent: source_generation,
        expected_source_allocation_id: source_allocation_id.clone(),
        expected_source_owner_epoch: 7,
        target: ForwardLocatorEntry {
            payload_root: payload_root.clone(),
            allocation_id: target_allocation_id.clone(),
            owner_epoch: 11,
            extents: vec![LocatorExtent {
                relative_path: "packs/operation-hv09/payload.pack".to_owned(),
                offset: 0,
                length: ready.target_logical_bytes,
            }],
        },
        target_reverse: ReverseLocatorEntry {
            allocation_id: target_allocation_id.clone(),
            owner_epoch: 11,
            operation_id: operation_id.clone(),
            publication_id,
            payload_roots: vec![payload_root.clone()],
            accounted_bytes: ready.target_allocated_bytes,
        },
    };
    let selected = store
        .replace_locator(
            &operation_id,
            &locator_store,
            &replacement,
            &mut NamedFaultInjector::default(),
        )
        .expect("replace locator with durable pack");
    assert_eq!(selected.phase, EvacuationPhase::LocatorPublished);
    assert_eq!(selected.selected_generation, target_generation);
    assert_eq!(selected.active_reader_pins, 1);
    assert_eq!(selected.retirement_debt_objects, 1);
    assert_eq!(
        selected.retirement_debt_bytes,
        selected.source_allocated_bytes
    );
    assert!(selected.source_present);
    assert!(selected.target_present);
    assert_eq!(
        locator_store
            .resolve(&payload_root)
            .expect("resolve selected payload")
            .expect("selected target"),
        replacement.target
    );
    assert_eq!(
        locator_store
            .selected()
            .expect("selected locator generation")
            .expect("locator generation")
            .reverse,
        vec![replacement.target_reverse.clone()]
    );

    pinned_source
        .seek(SeekFrom::Start(0))
        .expect("rewind pinned source");
    assert_eq!(digest_reader(&mut pinned_source), expected_digest);
    let mut target_reader = store
        .pin_selected(&operation_id)
        .expect("pin selected target");
    assert_eq!(target_reader.generation(), target_generation);
    assert_eq!(target_reader.allocation_id(), &target_allocation_id);
    assert_eq!(digest_reader(&mut target_reader), expected_digest);
    drop(target_reader);

    let authorization = StageFiveRetirementAuthorization {
        schema_version: SCHEMA_VERSION,
        authorization_id: OperationId::from_string("stage-five-hv09"),
        evacuation_operation_id: operation_id.clone(),
        payload_root,
        source_allocation_id,
        source_owner_epoch: 7,
        selected_generation: target_generation,
        deletion_authorized: true,
    };
    assert!(matches!(
        store.retire_source(&operation_id, &authorization),
        Err(PocError::RecoveryRequired(message))
            if message.contains("active reader pins")
    ));
    assert!(source_path.is_file());

    drop(pinned_source);
    let unpinned = store.inspect(&operation_id).expect("inspect released pin");
    assert_eq!(unpinned.active_reader_pins, 0);
    assert_eq!(unpinned.retirement_debt_objects, 1);
    let terminal = store
        .retire_source(&operation_id, &authorization)
        .expect("retire unpinned source");
    assert_eq!(terminal.phase, EvacuationPhase::Terminal);
    assert!(!terminal.source_present);
    assert!(terminal.target_present);
    assert_eq!(terminal.active_reader_pins, 0);
    assert_eq!(terminal.retirement_debt_objects, 0);
    assert_eq!(terminal.retirement_debt_bytes, 0);
    assert_eq!(
        store
            .retire_source(&operation_id, &authorization)
            .expect("idempotent retirement"),
        terminal
    );

    let reconciliation = reconcile(
        &root.path,
        &[
            StorageCategoryRoot {
                category: "scope".to_owned(),
                root: root.path.clone(),
                recursive: false,
            },
            StorageCategoryRoot {
                category: "evacuation".to_owned(),
                root: root.path.join("evacuation"),
                recursive: true,
            },
            StorageCategoryRoot {
                category: "locators".to_owned(),
                root: root.path.join("locators"),
                recursive: true,
            },
            StorageCategoryRoot {
                category: "retired-source-directory".to_owned(),
                root: source_dir,
                recursive: true,
            },
        ],
        LeakCounts::default(),
    )
    .expect("final reconciliation");
    assert!(reconciliation.balanced, "{reconciliation:#?}");
    assert_eq!(reconciliation.unexplained_allocated_bytes, 0);
    assert_eq!(reconciliation.unexplained_inodes, 0);
    assert_eq!(reconciliation.leaks, LeakCounts::default());
}

fn write_fixture(path: &Path, bytes: usize) -> String {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .expect("create host evacuation fixture");
    let mut hasher = Sha256::new();
    let mut remaining = bytes;
    let mut ordinal = 0_u64;
    while remaining != 0 {
        let length = remaining.min(64 * 1024);
        let mut block = vec![0_u8; length];
        for (index, byte) in block.iter_mut().enumerate() {
            *byte = ordinal
                .wrapping_add(u64::try_from(index).expect("fixture index"))
                .to_le_bytes()[0];
        }
        file.write_all(&block).expect("write fixture block");
        hasher.update(&block);
        remaining -= length;
        ordinal = ordinal.wrapping_add(u64::try_from(length).expect("fixture length"));
    }
    file.sync_all().expect("fsync fixture");
    format!("{:x}", hasher.finalize())
}

fn digest_reader(reader: &mut impl Read) -> String {
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer).expect("read pinned payload");
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    format!("{:x}", hasher.finalize())
}

fn sync_directory(path: &Path) {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .expect("fsync fixture directory");
}
