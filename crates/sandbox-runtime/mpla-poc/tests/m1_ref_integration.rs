#[cfg(target_os = "linux")]
use std::fs;
use std::fs::File;
use std::path::{Path, PathBuf};

use sandbox_runtime_mpla_poc::allocation::create_allocation;
use sandbox_runtime_mpla_poc::lease::issue_workspace_lease;
use sandbox_runtime_mpla_poc::locator::{
    ForwardLocatorEntry, LocatorDelta, LocatorExtent, LocatorStore, PayloadRootId,
    ReverseLocatorEntry,
};
use sandbox_runtime_mpla_poc::occ::{
    BranchOcc, ChangedPathSet, ConflictAllocation, OccPublication, OccPublishOutcome,
    RebasedCanonical,
};
use sandbox_runtime_mpla_poc::owner::{compare_and_adopt, current_owner};
use sandbox_runtime_mpla_poc::recovery::{
    capture_recovery_allocation_identity, DurableRecoveryPhase, PublicationRecovery,
    RecoveryOutcome, RecoveryRequest,
};
use sandbox_runtime_mpla_poc::ref_store::PairedRefStore;
use sandbox_runtime_mpla_poc::{
    AllocationHandle, AllocationId, AttributionInput, AttributionRootId,
    CanonicalDurabilityReceipt, CanonicalRootPair, InodeWitness, LocatorGeneration,
    LocatorRefCandidate, NamedFaultInjector, NamedFaultPoint, OperationId, OwnerSubject,
    OwnerTransitionRequest, PhysicalSnapshot, PocError, PublicationId, RefSequence, RootId,
    SessionId, StableAllocationReceipt, SCHEMA_VERSION,
};
use sha2::{Digest, Sha256};
use uuid::Uuid;

struct TestRoot {
    path: PathBuf,
}

impl TestRoot {
    fn new(label: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("mpla-m1-integration-{label}-{}", Uuid::new_v4()));
        std::fs::create_dir(&path).expect("create integration root");
        Self { path }
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

struct OwnedAllocation {
    allocation: AllocationHandle,
    operation_id: OperationId,
    publication_id: PublicationId,
    owner_epoch: u64,
}

#[cfg(not(target_os = "linux"))]
#[test]
fn recovery_identity_capture_fails_closed_without_descriptor_authority() {
    let allocation_id = AllocationId::from_string("allocation-id");
    assert!(matches!(
        capture_recovery_allocation_identity(
            Path::new("/unsupported/al/allocation-id"),
            &allocation_id,
        ),
        Err(PocError::Unsupported(message))
            if message.contains("Linux descriptor authority")
    ));
}

#[test]
fn changed_path_overlap_is_exact_and_ancestor_aware() {
    let incoming = ChangedPathSet::new([
        "same/key".to_owned(),
        "tree/branch/leaf".to_owned(),
        "separate".to_owned(),
    ])
    .expect("incoming paths");
    let committed = ChangedPathSet::new(["same/key".to_owned(), "tree/branch".to_owned()])
        .expect("committed paths");
    let overlaps = incoming.overlaps(&committed);
    assert_eq!(overlaps.len(), 2);
    assert!(overlaps
        .iter()
        .any(|overlap| overlap.incoming == "same/key" && overlap.committed == "same/key"));
    assert!(overlaps.iter().any(|overlap| {
        overlap.incoming == "tree/branch/leaf" && overlap.committed == "tree/branch"
    }));
}

#[test]
fn four_disjoint_publishers_converge_and_overlap_retains_exact_owner() {
    let root = TestRoot::new("occ");
    let locator_store = LocatorStore::open(root.path.join("locators")).expect("locator store");
    let ref_store = PairedRefStore::open(root.path.join("refs")).expect("ref store");
    let occ = BranchOcc::open(root.path.join("occ")).expect("OCC");
    let mut publishers = Vec::new();

    for index in 0..5 {
        let owned = adopt_allocation(&root.path, &format!("publisher-{index}"));
        let delta = locator_delta(&owned, &format!("payload-{index}"));
        locator_store
            .install(&delta, &mut NamedFaultInjector::default())
            .expect("install publisher locator");
        publishers.push((owned, delta));
    }

    for (index, (owned, _)) in publishers.iter().take(4).enumerate() {
        let canonical = canonical_receipt(&root.path, &format!("canonical-{index}"));
        let publication = occ_publication(
            owned,
            RefSequence::ZERO,
            format!("directory/{index}"),
            canonical,
        );
        let result = occ
            .publish(
                "main",
                &publication,
                &locator_store,
                &ref_store,
                &mut NamedFaultInjector::default(),
                |candidate, head, _| {
                    Ok(RebasedCanonical {
                        roots: root_pair(&format!(
                            "rebase-{}-{}",
                            candidate.operation_id, head.sequence
                        )),
                        durability: canonical_receipt(
                            &root.path,
                            &format!("rebase-{}-{}", candidate.operation_id, head.sequence),
                        ),
                    })
                },
            )
            .expect("publish disjoint candidate");
        let OccPublishOutcome::Committed {
            receipt,
            rebase_count,
        } = result
        else {
            panic!("disjoint publisher conflicted");
        };
        assert_eq!(receipt.value.sequence.get(), (index + 1) as u64);
        assert_eq!(rebase_count, u32::from(index != 0));
    }

    let (loser, _) = &publishers[4];
    let overlap = occ_publication(
        loser,
        RefSequence::ZERO,
        "directory/2/child".to_owned(),
        canonical_receipt(&root.path, "overlap"),
    );
    let result = occ
        .publish(
            "main",
            &overlap,
            &locator_store,
            &ref_store,
            &mut NamedFaultInjector::default(),
            |_, _, _| panic!("overlap must not invoke rebase"),
        )
        .expect("typed overlap");
    let OccPublishOutcome::Conflict(conflict) = result else {
        panic!("ancestor overlap unexpectedly committed");
    };
    assert_eq!(conflict.expected_sequence, RefSequence::ZERO);
    assert_eq!(conflict.observed_sequence.get(), 4);
    assert_eq!(
        conflict.allocation_id,
        loser.allocation.descriptor.allocation_id
    );
    assert_eq!(conflict.owner_epoch, loser.owner_epoch);
    assert_eq!(conflict.accounted_bytes, 4_096);
    assert_eq!(conflict.overlaps.len(), 1);
    assert_eq!(conflict.overlaps[0].incoming, "directory/2/child");
    assert_eq!(conflict.overlaps[0].committed, "directory/2");
    assert_eq!(
        current_owner(&loser.allocation.allocation_root)
            .expect("retained owner")
            .subject,
        OwnerSubject::PayloadOwned {
            publication_id: loser.publication_id.clone()
        }
    );
    assert_eq!(
        ref_store
            .read("main")
            .expect("read converged head")
            .expect("head")
            .sequence
            .get(),
        4
    );

    let replay = occ
        .publish(
            "main",
            &overlap,
            &locator_store,
            &ref_store,
            &mut NamedFaultInjector::default(),
            |_, _, _| panic!("retained conflict replay must not rebase"),
        )
        .expect("replay typed overlap");
    assert_eq!(replay, OccPublishOutcome::Conflict(conflict));
}

#[cfg(target_os = "linux")]
#[test]
fn fresh_process_recovery_repairs_every_ref_edge_from_durable_state() {
    for point in [
        NamedFaultPoint::RefBeforeTemp,
        NamedFaultPoint::RefAfterTempFsync,
        NamedFaultPoint::RefAfterReplace,
        NamedFaultPoint::RefAfterParentFsync,
        NamedFaultPoint::ResponseLossPublish,
    ] {
        let root = TestRoot::new(point.as_str());
        let owned = adopt_allocation(&root.path, point.as_str());
        let locator_path = root.path.join("locators");
        let refs_path = root.path.join("refs");
        let occ_path = root.path.join("occ");
        let recovery_path = root.path.join("recovery");
        let locator_store = LocatorStore::open(&locator_path).expect("locator store");
        let ref_store = PairedRefStore::open(&refs_path).expect("ref store");
        let occ = BranchOcc::open(&occ_path).expect("OCC");
        let recovery = PublicationRecovery::open(&recovery_path).expect("recovery");
        let request = recovery_request(&root.path, &owned, point.as_str(), RefSequence::ZERO);
        let prepared = recovery.prepare(&request).expect("prepare recovery");
        assert_eq!(prepared.phase, DurableRecoveryPhase::Sealing);

        let mut faults = NamedFaultInjector::armed([(point, 1)]);
        assert!(matches!(
            recovery.replay(
                &owned.operation_id,
                &locator_store,
                &ref_store,
                &occ,
                &mut faults,
                |_, _, _| panic!("initial publication does not rebase"),
            ),
            Err(PocError::RecoveryRequired(_))
        ));
        if let Some(head) = ref_store.read("main").expect("read faulted head") {
            assert_eq!(head.operation_id, owned.operation_id);
        }

        drop(recovery);
        drop(occ);
        drop(ref_store);
        drop(locator_store);
        let fresh_locator = LocatorStore::open(&locator_path).expect("fresh locator");
        let fresh_refs = PairedRefStore::open(&refs_path).expect("fresh refs");
        let fresh_occ = BranchOcc::open(&occ_path).expect("fresh OCC");
        let fresh_recovery = PublicationRecovery::open(&recovery_path).expect("fresh recovery");
        let outcome = fresh_recovery
            .replay(
                &owned.operation_id,
                &fresh_locator,
                &fresh_refs,
                &fresh_occ,
                &mut NamedFaultInjector::default(),
                |_, _, _| panic!("single publication must not rebase"),
            )
            .expect("fresh replay");
        let RecoveryOutcome::Committed(receipt) = outcome else {
            panic!("faulted ref edge did not recover as committed");
        };
        assert!(receipt.parent_directory_synced);
        assert_eq!(receipt.value.operation_id, owned.operation_id);
        assert_eq!(
            fresh_recovery
                .inspect(&owned.operation_id)
                .expect("inspect terminal recovery")
                .phase,
            DurableRecoveryPhase::PublicationCommitted
        );
        let resolved = fresh_refs
            .read_resolved("main", &fresh_locator)
            .expect("resolve recovered ref")
            .expect("paired ref");
        assert_eq!(resolved.value, receipt.value);
        let owner = current_owner(&owned.allocation.allocation_root).expect("exact owner");
        assert_eq!(owner.owner_epoch, owned.owner_epoch);
        assert_eq!(
            owner.subject,
            OwnerSubject::PayloadOwned {
                publication_id: owned.publication_id
            }
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn recovery_waits_on_durable_owner_outcome_without_reopening_workspace() {
    let root = TestRoot::new("await-owner");
    let allocation =
        create_allocation(&root.path.join("arena"), &OperationId::new()).expect("allocation");
    let lease = issue_workspace_lease(&allocation, SessionId::new(), &OperationId::new())
        .expect("workspace lease");
    let owned = OwnedAllocation {
        allocation,
        operation_id: OperationId::from_string("operation-await-owner"),
        publication_id: PublicationId::from_string("publication-await-owner"),
        owner_epoch: lease.owner_epoch + 1,
    };
    let locator_store = LocatorStore::open(root.path.join("locators")).expect("locator");
    let ref_store = PairedRefStore::open(root.path.join("refs")).expect("refs");
    let occ = BranchOcc::open(root.path.join("occ")).expect("OCC");
    let recovery = PublicationRecovery::open(root.path.join("recovery")).expect("recovery");
    let request = recovery_request(&root.path, &owned, "await-owner", RefSequence::ZERO);
    recovery.prepare(&request).expect("prepare");
    let outcome = recovery
        .replay(
            &owned.operation_id,
            &locator_store,
            &ref_store,
            &occ,
            &mut NamedFaultInjector::default(),
            |_, _, _| panic!("owner transition is not complete"),
        )
        .expect("owner wait decision");
    assert!(matches!(
        outcome,
        RecoveryOutcome::AwaitingOwnerTransition {
            phase: DurableRecoveryPhase::Sealing,
            observed: OwnerSubject::WorkspaceOwned { .. }
        }
    ));
    assert_eq!(
        recovery
            .inspect(&owned.operation_id)
            .expect("inspect sealing")
            .phase,
        DurableRecoveryPhase::Sealing
    );
}

#[cfg(target_os = "linux")]
#[test]
fn recovery_rejects_reverse_locator_accounting_that_disagrees_with_owner() {
    let root = TestRoot::new("reverse-accounting");
    let owned = adopt_allocation(&root.path, "reverse-accounting");
    let recovery = PublicationRecovery::open(root.path.join("recovery")).expect("recovery");
    let mut request = recovery_request(&root.path, &owned, "reverse-accounting", RefSequence::ZERO);
    request.locator_delta.reverse[0].accounted_bytes += 1;
    assert!(matches!(
        recovery.prepare(&request),
        Err(PocError::Integrity(message))
            if message.contains("reverse locator ownership/accounting disagrees")
    ));
}

#[cfg(target_os = "linux")]
#[test]
fn recovery_prepare_rejects_forged_allocation_and_owner_identity() {
    let root = TestRoot::new("forged-recovery-identity");
    let owned = adopt_allocation(&root.path, "forged-recovery-identity");
    let recovery = PublicationRecovery::open(root.path.join("recovery")).expect("recovery");
    let mut request = recovery_request(
        &root.path,
        &owned,
        "forged-recovery-identity",
        RefSequence::ZERO,
    );
    let exact = request.allocation_identity;

    request.allocation_identity.allocation_inode = exact
        .allocation_inode
        .checked_add(1)
        .expect("forge allocation inode");
    assert!(matches!(
        recovery.prepare(&request),
        Err(PocError::RecoveryRequired(_))
    ));

    request.allocation_identity = exact;
    request.allocation_identity.owner_inode =
        exact.owner_inode.checked_add(1).expect("forge owner inode");
    assert!(matches!(
        recovery.prepare(&request),
        Err(PocError::RecoveryRequired(_))
    ));
}

#[cfg(target_os = "linux")]
#[test]
fn recovery_replay_rejects_semantically_identical_allocation_replacement_before_publication() {
    let root = TestRoot::new("replaced-recovery-allocation");
    let owned = adopt_allocation(&root.path, "replaced-recovery-allocation");
    let locator_store = LocatorStore::open(root.path.join("locators")).expect("locator");
    let ref_store = PairedRefStore::open(root.path.join("refs")).expect("refs");
    let occ_root = root.path.join("occ");
    let occ = BranchOcc::open(&occ_root).expect("OCC");
    let recovery = PublicationRecovery::open(root.path.join("recovery")).expect("recovery");
    let request = recovery_request(
        &root.path,
        &owned,
        "replaced-recovery-allocation",
        RefSequence::ZERO,
    );
    recovery.prepare(&request).expect("prepare recovery");

    let allocation_root = owned.allocation.allocation_root.clone();
    let displaced = allocation_root.with_extension("pinned-original");
    fs::rename(&allocation_root, &displaced).expect("displace recovery allocation");
    copy_recovery_tree(&displaced, &allocation_root);

    assert!(matches!(
        recovery.replay(
            &owned.operation_id,
            &locator_store,
            &ref_store,
            &occ,
            &mut NamedFaultInjector::default(),
            |_, _, _| panic!("replaced recovery authority must not rebase"),
        ),
        Err(PocError::RecoveryRequired(_))
    ));
    assert_recovery_publication_stores_untouched(
        &recovery,
        &owned.operation_id,
        &locator_store,
        &ref_store,
        &occ_root,
    );
}

#[cfg(target_os = "linux")]
#[test]
fn recovery_replay_rejects_semantically_identical_owner_replacement_before_publication() {
    let root = TestRoot::new("replaced-recovery-owner");
    let owned = adopt_allocation(&root.path, "replaced-recovery-owner");
    let locator_store = LocatorStore::open(root.path.join("locators")).expect("locator");
    let ref_store = PairedRefStore::open(root.path.join("refs")).expect("refs");
    let occ_root = root.path.join("occ");
    let occ = BranchOcc::open(&occ_root).expect("OCC");
    let recovery = PublicationRecovery::open(root.path.join("recovery")).expect("recovery");
    let request = recovery_request(
        &root.path,
        &owned,
        "replaced-recovery-owner",
        RefSequence::ZERO,
    );
    recovery.prepare(&request).expect("prepare recovery");

    let owner_dir = owned.allocation.owner_dir.clone();
    let displaced = owner_dir.with_extension("pinned-original");
    fs::rename(&owner_dir, &displaced).expect("displace recovery owner");
    copy_recovery_tree(&displaced, &owner_dir);

    assert!(matches!(
        recovery.replay(
            &owned.operation_id,
            &locator_store,
            &ref_store,
            &occ,
            &mut NamedFaultInjector::default(),
            |_, _, _| panic!("replaced recovery owner must not rebase"),
        ),
        Err(PocError::RecoveryRequired(_))
    ));
    assert_recovery_publication_stores_untouched(
        &recovery,
        &owned.operation_id,
        &locator_store,
        &ref_store,
        &occ_root,
    );
}

#[cfg(target_os = "linux")]
fn copy_recovery_tree(source: &Path, destination: &Path) {
    fs::create_dir(destination).expect("create recovery replacement directory");
    for entry in fs::read_dir(source).expect("read recovery source directory") {
        let entry = entry.expect("read recovery source entry");
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let file_type = entry.file_type().expect("read recovery source entry type");
        if file_type.is_dir() {
            copy_recovery_tree(&source_path, &destination_path);
        } else {
            assert!(
                file_type.is_file(),
                "recovery replacement contains a special entry"
            );
            fs::copy(&source_path, &destination_path).expect("copy recovery replacement file");
        }
    }
}

#[cfg(target_os = "linux")]
fn assert_recovery_publication_stores_untouched(
    recovery: &PublicationRecovery,
    operation_id: &OperationId,
    locator_store: &LocatorStore,
    ref_store: &PairedRefStore,
    occ_root: &Path,
) {
    assert_eq!(
        recovery
            .inspect(operation_id)
            .expect("inspect rejected recovery")
            .phase,
        DurableRecoveryPhase::Sealing
    );
    assert!(locator_store
        .selected()
        .expect("read untouched locator")
        .is_none());
    assert!(ref_store
        .read("main")
        .expect("read untouched ref")
        .is_none());
    assert_eq!(
        fs::read_dir(occ_root.join("branches"))
            .expect("read untouched OCC branches")
            .count(),
        0
    );
}

fn adopt_allocation(root: &Path, label: &str) -> OwnedAllocation {
    let allocation =
        create_allocation(&root.join("arena"), &OperationId::new()).expect("allocation");
    let lease = issue_workspace_lease(&allocation, SessionId::new(), &OperationId::new())
        .expect("workspace lease");
    let operation_id = OperationId::from_string(format!("operation-{label}"));
    let publication_id = PublicationId::from_string(format!("publication-{label}"));
    let physical = physical_snapshot(&allocation);
    let stable = StableAllocationReceipt {
        schema_version: SCHEMA_VERSION,
        operation_id: operation_id.clone(),
        allocation: allocation.descriptor.clone(),
        expected_owner_epoch: lease.owner_epoch,
        before: physical.clone(),
        after: physical,
        sync_completed: true,
    };
    let request = OwnerTransitionRequest {
        schema_version: SCHEMA_VERSION,
        operation_id: operation_id.clone(),
        publication_id: publication_id.clone(),
        session_id: lease.session_id,
        allocation_id: allocation.descriptor.allocation_id.clone(),
        expected_lease_epoch: lease.lease_epoch,
        expected_owner_epoch: lease.owner_epoch,
    };
    let adoption =
        compare_and_adopt(&allocation.allocation_root, &stable, &request).expect("adopt owner");
    OwnedAllocation {
        allocation,
        operation_id,
        publication_id,
        owner_epoch: adoption.new_owner.owner_epoch,
    }
}

fn occ_publication(
    owned: &OwnedAllocation,
    expected_sequence: RefSequence,
    path: String,
    canonical: CanonicalDurabilityReceipt,
) -> OccPublication {
    OccPublication {
        candidate: LocatorRefCandidate {
            schema_version: SCHEMA_VERSION,
            operation_id: owned.operation_id.clone(),
            publication_id: owned.publication_id.clone(),
            roots: root_pair(owned.operation_id.as_str()),
            locator_generation: LocatorGeneration::INITIAL,
            expected_sequence,
        },
        canonical,
        changed_paths: ChangedPathSet::new([path]).expect("changed paths"),
        conflict_allocation: ConflictAllocation {
            allocation_root: owned.allocation.allocation_root.clone(),
            allocation_id: owned.allocation.descriptor.allocation_id.clone(),
            owner_epoch: owned.owner_epoch,
            accounted_bytes: 4_096,
        },
    }
}

fn recovery_request(
    root: &Path,
    owned: &OwnedAllocation,
    label: &str,
    expected_sequence: RefSequence,
) -> RecoveryRequest {
    RecoveryRequest {
        schema_version: SCHEMA_VERSION,
        operation_id: owned.operation_id.clone(),
        publication_id: owned.publication_id.clone(),
        branch: "main".to_owned(),
        allocation_root: owned.allocation.allocation_root.clone(),
        allocation_identity: capture_recovery_allocation_identity(
            &owned.allocation.allocation_root,
            &owned.allocation.descriptor.allocation_id,
        )
        .expect("capture recovery allocation identity"),
        allocation_id: owned.allocation.descriptor.allocation_id.clone(),
        owner_epoch: owned.owner_epoch,
        accounted_bytes: 4_096,
        locator_delta: locator_delta(owned, label),
        candidate: LocatorRefCandidate {
            schema_version: SCHEMA_VERSION,
            operation_id: owned.operation_id.clone(),
            publication_id: owned.publication_id.clone(),
            roots: root_pair(label),
            locator_generation: LocatorGeneration::INITIAL,
            expected_sequence,
        },
        canonical: canonical_receipt(root, label),
        changed_paths: ChangedPathSet::new([format!("workspace/{label}")]).expect("changed paths"),
    }
}

fn locator_delta(owned: &OwnedAllocation, label: &str) -> LocatorDelta {
    let payload_root = PayloadRootId::parse(digest_hex(label)).expect("payload root");
    LocatorDelta {
        schema_version: SCHEMA_VERSION,
        operation_id: owned.operation_id.clone(),
        publication_id: owned.publication_id.clone(),
        expected_parent: None,
        forward: vec![ForwardLocatorEntry {
            payload_root: payload_root.clone(),
            allocation_id: owned.allocation.descriptor.allocation_id.clone(),
            owner_epoch: owned.owner_epoch,
            extents: vec![LocatorExtent {
                relative_path: format!("payload/{label}"),
                offset: 0,
                length: 4_096,
            }],
        }],
        reverse: vec![ReverseLocatorEntry {
            allocation_id: owned.allocation.descriptor.allocation_id.clone(),
            owner_epoch: owned.owner_epoch,
            operation_id: owned.operation_id.clone(),
            publication_id: owned.publication_id.clone(),
            payload_roots: vec![payload_root],
            accounted_bytes: 4_096,
        }],
    }
}

fn root_pair(label: &str) -> CanonicalRootPair {
    CanonicalRootPair {
        root_id: RootId::parse(digest_hex(&format!("root-{label}"))).expect("root"),
        attribution_root_id: AttributionRootId::parse(digest_hex(&format!("attribution-{label}")))
            .expect("attribution root"),
    }
}

fn canonical_receipt(root: &Path, label: &str) -> CanonicalDurabilityReceipt {
    let canonical_dir = root.join("canonical");
    std::fs::create_dir_all(&canonical_dir).expect("create canonical directory");
    let manifest = canonical_dir.join(format!("{}.json", digest_hex(label)));
    let file = File::create(&manifest).expect("canonical manifest");
    file.sync_all().expect("fsync canonical manifest");
    File::open(&canonical_dir)
        .expect("canonical directory")
        .sync_all()
        .expect("fsync canonical directory");
    CanonicalDurabilityReceipt {
        root_manifest: manifest,
        semantic_attribution: AttributionInput {
            actor_id: "test-actor".to_owned(),
            semantic_operation_id: label.to_owned(),
        },
        immutable_object_count: 2,
        immutable_object_bytes: 8_192,
        object_set_sha256: digest_hex(&format!("objects-{label}")),
        files_fsynced: true,
        object_directory_fsynced: true,
        manifest_fsynced: true,
        manifest_directory_fsynced: true,
    }
}

fn digest_hex(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn physical_snapshot(allocation: &AllocationHandle) -> PhysicalSnapshot {
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;

    let metadata = std::fs::metadata(&allocation.upper_dir).expect("stat upper");
    #[cfg(unix)]
    let (device, inode) = (metadata.dev(), metadata.ino());
    #[cfg(not(unix))]
    let (device, inode) = (0, 0);
    PhysicalSnapshot {
        allocation_id: allocation.descriptor.allocation_id.clone(),
        allocation_path: allocation.allocation_root.clone(),
        device,
        representative_inodes: vec![InodeWitness {
            relative_path: PathBuf::from("."),
            device,
            inode,
        }],
        logical_bytes: 0,
        allocated_bytes: 0,
        inode_count: 1,
        file_count: 0,
        directory_count: 1,
    }
}
