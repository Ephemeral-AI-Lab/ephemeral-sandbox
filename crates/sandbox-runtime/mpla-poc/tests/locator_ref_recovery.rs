use std::fs::File;
use std::path::{Path, PathBuf};

use sandbox_runtime_mpla_poc::locator::{
    ForwardLocatorEntry, LocatorDelta, LocatorExtent, LocatorStore, PayloadRootId,
    ReverseLocatorEntry,
};
use sandbox_runtime_mpla_poc::ref_store::{PairedRefStore, RefCommitOutcome};
use sandbox_runtime_mpla_poc::{
    AttributionRootId, CanonicalDurabilityReceipt, CanonicalRootPair, LocatorGeneration,
    LocatorRefCandidate, NamedFaultInjector, NamedFaultPoint, OperationId, PocError, PublicationId,
    RefSequence, RootId, SCHEMA_VERSION,
};
use uuid::Uuid;

struct TestRoot {
    path: PathBuf,
}

impl TestRoot {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!("mpla-m1-{label}-{}", Uuid::new_v4()));
        std::fs::create_dir(&path).expect("create test root");
        Self { path }
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[test]
fn immutable_locator_generations_merge_and_preserve_reverse_accounting() {
    let root = TestRoot::new("locator-generation");
    let store = LocatorStore::open(root.path.join("locators")).expect("open locator store");
    let first = locator_delta(1, "first");
    let first_receipt = store
        .install(&first, &mut NamedFaultInjector::default())
        .expect("install first generation");
    assert_eq!(first_receipt.generation, LocatorGeneration::INITIAL);
    let mut second = locator_delta(2, "second");
    second.expected_parent = Some(first_receipt.generation);
    let second_receipt = store
        .install(&second, &mut NamedFaultInjector::default())
        .expect("install second generation");
    assert_eq!(
        second_receipt.generation,
        LocatorGeneration::INITIAL
            .checked_next()
            .expect("next locator generation")
    );
    let mut stale = locator_delta(3, "stale");
    stale.expected_parent = Some(first_receipt.generation);
    let error = store
        .install(&stale, &mut NamedFaultInjector::default())
        .expect_err("stale locator parent must fail");
    assert!(matches!(error, PocError::OwnerConflict(_)));

    let selected = store.selected().expect("read selected").expect("selector");
    assert_eq!(selected.forward.len(), 2);
    assert_eq!(selected.reverse.len(), 2);
    assert_eq!(
        store
            .resolve(&payload_root(1))
            .expect("resolve first")
            .expect("first locator")
            .allocation_id,
        first.forward[0].allocation_id
    );
    assert_eq!(
        selected
            .reverse
            .iter()
            .map(|entry| entry.accounted_bytes)
            .sum::<u64>(),
        8_192
    );
    store
        .validate_generation_receipt(&first_receipt)
        .expect("old immutable generation remains durable");
}

#[test]
fn locator_fault_replay_never_selects_an_incomplete_generation() {
    for point in [
        NamedFaultPoint::LocatorAfterForward,
        NamedFaultPoint::LocatorAfterReverse,
        NamedFaultPoint::LocatorAfterManifestFsync,
        NamedFaultPoint::LocatorAfterSelectorRename,
        NamedFaultPoint::LocatorAfterSelectorDirFsync,
    ] {
        let root = TestRoot::new(point.as_str());
        let path = root.path.join("locators");
        let store = LocatorStore::open(&path).expect("open locator store");
        let delta = locator_delta(1, point.as_str());
        let mut faults = NamedFaultInjector::armed([(point, 1)]);
        assert!(matches!(
            store.install(&delta, &mut faults),
            Err(PocError::RecoveryRequired(_))
        ));
        if let Some(selected) = LocatorStore::open(&path)
            .expect("reopen locator store")
            .selected()
            .expect("read selector")
        {
            assert_eq!(selected.forward.len(), 1);
            assert_eq!(selected.reverse.len(), 1);
        }

        let reopened = LocatorStore::open(&path).expect("fresh locator process");
        let receipt = reopened
            .install(&delta, &mut NamedFaultInjector::default())
            .expect("durable replay");
        reopened
            .validate_receipt(&receipt)
            .expect("selected complete generation");
        let selected = reopened.selected().expect("read replay").expect("selector");
        assert_eq!(selected.forward.len(), 1);
        assert_eq!(selected.reverse.len(), 1);
    }
}

#[test]
fn paired_ref_faults_replay_to_old_or_complete_new_and_stable_response() {
    for point in [
        NamedFaultPoint::RefBeforeTemp,
        NamedFaultPoint::RefAfterTempFsync,
        NamedFaultPoint::RefAfterReplace,
        NamedFaultPoint::RefAfterParentFsync,
        NamedFaultPoint::ResponseLossPublish,
    ] {
        let root = TestRoot::new(point.as_str());
        let locator_store =
            LocatorStore::open(root.path.join("locators")).expect("open locator store");
        let delta = locator_delta(1, point.as_str());
        let locator = locator_store
            .install(&delta, &mut NamedFaultInjector::default())
            .expect("install locator");
        let refs = PairedRefStore::open(root.path.join("refs")).expect("open refs");
        let canonical = canonical_receipt(&root.path, point.as_str());
        let candidate = ref_candidate(&delta, locator.generation, RefSequence::ZERO);
        let mut faults = NamedFaultInjector::armed([(point, 1)]);
        assert!(matches!(
            refs.commit(
                "main",
                &candidate,
                &canonical,
                &locator,
                &locator_store,
                &mut faults,
            ),
            Err(PocError::RecoveryRequired(_))
        ));

        if let Some(observed) = refs.read("main").expect("read after fault") {
            assert_eq!(observed.roots, candidate.roots);
            assert_eq!(observed.locator_generation, locator.generation);
            assert_eq!(observed.operation_id, candidate.operation_id);
        }

        let reopened_refs =
            PairedRefStore::open(root.path.join("refs")).expect("fresh ref process");
        let result = reopened_refs
            .commit(
                "main",
                &candidate,
                &canonical,
                &locator,
                &locator_store,
                &mut NamedFaultInjector::default(),
            )
            .expect("replay commit");
        let RefCommitOutcome::Committed(receipt) = result else {
            panic!("stable retry did not converge");
        };
        assert_eq!(receipt.value.sequence.get(), 1);
        assert!(receipt.parent_directory_synced);
        let resolved = reopened_refs
            .read_resolved("main", &locator_store)
            .expect("resolve paired ref")
            .expect("paired ref");
        assert_eq!(resolved.value, receipt.value);
        assert_eq!(resolved.canonical, canonical);
        assert_eq!(resolved.locator, locator);

        let stable = reopened_refs
            .commit(
                "main",
                &candidate,
                &canonical,
                &locator,
                &locator_store,
                &mut NamedFaultInjector::default(),
            )
            .expect("stable response replay");
        let RefCommitOutcome::Committed(stable) = stable else {
            panic!("stored result was not returned");
        };
        assert!(stable.idempotent_replay);
        assert_eq!(stable.value, receipt.value);
    }
}

#[test]
fn paired_ref_rejects_incomplete_prerequisites_and_excludes_physical_identity() {
    let root = TestRoot::new("ref-prerequisite");
    let locator_store = LocatorStore::open(root.path.join("locators")).expect("locator store");
    let delta = locator_delta(1, "prerequisite");
    let locator = locator_store
        .install(&delta, &mut NamedFaultInjector::default())
        .expect("install locator");
    let refs = PairedRefStore::open(root.path.join("refs")).expect("ref store");
    let mut canonical = canonical_receipt(&root.path, "prerequisite");
    canonical.manifest_directory_fsynced = false;
    let candidate = ref_candidate(&delta, locator.generation, RefSequence::ZERO);
    assert!(matches!(
        refs.commit(
            "main",
            &candidate,
            &canonical,
            &locator,
            &locator_store,
            &mut NamedFaultInjector::default(),
        ),
        Err(PocError::Integrity(_))
    ));
    assert!(refs.read("main").expect("read empty head").is_none());

    let encoded = serde_json::to_string(&candidate).expect("encode candidate");
    assert!(!encoded.contains(delta.forward[0].allocation_id.as_str()));
    assert!(!encoded.contains(root.path.to_string_lossy().as_ref()));
    assert!(serde_json::to_string(&delta)
        .expect("encode locator")
        .contains(delta.forward[0].allocation_id.as_str()));
}

#[test]
fn sixteen_paired_ref_progressions_all_resolve_complete_durable_state() {
    let root = TestRoot::new("sixteen-progressions");
    let locator_store = LocatorStore::open(root.path.join("locators")).expect("locator store");
    let refs = PairedRefStore::open(root.path.join("refs")).expect("ref store");
    let mut expected = RefSequence::ZERO;
    for index in 1..=16 {
        let delta = locator_delta(index, &format!("progression-{index}"));
        let locator = locator_store
            .install(&delta, &mut NamedFaultInjector::default())
            .expect("install locator progression");
        let canonical = canonical_receipt(&root.path, &format!("progression-{index}"));
        let candidate = ref_candidate(&delta, locator.generation, expected);
        let RefCommitOutcome::Committed(receipt) = refs
            .commit(
                "main",
                &candidate,
                &canonical,
                &locator,
                &locator_store,
                &mut NamedFaultInjector::default(),
            )
            .expect("commit paired ref progression")
        else {
            panic!("sequential paired ref reported stale parent");
        };
        expected = receipt.value.sequence;
        let resolved = refs
            .read_resolved("main", &locator_store)
            .expect("resolve progression")
            .expect("paired ref");
        assert_eq!(resolved.value.sequence, expected);
        assert_eq!(resolved.locator.generation, locator.generation);
    }
    assert_eq!(expected.get(), 16);
    assert_eq!(
        locator_store
            .selected()
            .expect("selected locator")
            .expect("locator")
            .forward
            .len(),
        16
    );
}

fn locator_delta(seed: u8, label: &str) -> LocatorDelta {
    let operation_id = OperationId::from_string(format!("operation-{label}"));
    let publication_id = PublicationId::from_string(format!("publication-{label}"));
    let allocation_id =
        sandbox_runtime_mpla_poc::AllocationId::from_string(Uuid::new_v4().to_string());
    LocatorDelta {
        schema_version: SCHEMA_VERSION,
        operation_id: operation_id.clone(),
        publication_id: publication_id.clone(),
        expected_parent: None,
        forward: vec![ForwardLocatorEntry {
            payload_root: payload_root(seed),
            allocation_id: allocation_id.clone(),
            owner_epoch: 2,
            extents: vec![LocatorExtent {
                relative_path: format!("payload/{seed}"),
                offset: 0,
                length: 4_096,
            }],
        }],
        reverse: vec![ReverseLocatorEntry {
            allocation_id,
            owner_epoch: 2,
            operation_id,
            publication_id,
            payload_roots: vec![payload_root(seed)],
            accounted_bytes: 4_096,
        }],
    }
}

fn ref_candidate(
    delta: &LocatorDelta,
    generation: LocatorGeneration,
    expected_sequence: RefSequence,
) -> LocatorRefCandidate {
    LocatorRefCandidate {
        schema_version: SCHEMA_VERSION,
        operation_id: delta.operation_id.clone(),
        publication_id: delta.publication_id.clone(),
        roots: root_pair(delta.forward[0].payload_root.as_str().as_bytes()[0]),
        locator_generation: generation,
        expected_sequence,
    }
}

fn payload_root(seed: u8) -> PayloadRootId {
    PayloadRootId::parse(format!("{seed:02x}").repeat(32)).expect("payload root")
}

fn root_pair(seed: u8) -> CanonicalRootPair {
    CanonicalRootPair {
        root_id: RootId::parse(format!("{seed:02x}").repeat(32)).expect("root ID"),
        attribution_root_id: AttributionRootId::parse(
            format!("{:02x}", seed.wrapping_add(64)).repeat(32),
        )
        .expect("attribution root ID"),
    }
}

fn canonical_receipt(root: &Path, label: &str) -> CanonicalDurabilityReceipt {
    let canonical_dir = root.join("canonical");
    std::fs::create_dir_all(&canonical_dir).expect("create canonical directory");
    let manifest = canonical_dir.join(format!("{label}.json"));
    let file = File::create(&manifest).expect("create canonical manifest");
    file.sync_all().expect("fsync canonical manifest");
    File::open(&canonical_dir)
        .expect("open canonical directory")
        .sync_all()
        .expect("fsync canonical directory");
    CanonicalDurabilityReceipt {
        root_manifest: manifest,
        immutable_object_count: 2,
        immutable_object_bytes: 8_192,
        object_set_sha256: "ab".repeat(32),
        files_fsynced: true,
        object_directory_fsynced: true,
        manifest_fsynced: true,
        manifest_directory_fsynced: true,
    }
}
