use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::time::Duration;

use sandbox_runtime_mpla_poc::{
    bind_product_catalog, collect_control_changes, run_current_i2_closing, CatalogBinding,
    ControlBoundary, ControlCacheMatch, ControlCollectionLimits, ControlVerdict,
    CurrentI2ClosingRequest,
};
#[cfg(target_os = "linux")]
use sandbox_runtime_mpla_poc::{
    run_current_i2_materialization, ControlApiCoverage, ControlCacheExpectation, ControlIntent,
    CurrentI2MaterializationRequest, ExternalReadinessReceipt, PocError,
};
use serde_json::json;
use uuid::Uuid;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(label: &str) -> TestResult<Self> {
        let path = std::env::temp_dir().join(format!("mpla-control-{label}-{}", Uuid::new_v4()));
        fs::create_dir(&path)?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write_catalog(root: &Path) -> TestResult<(PathBuf, PathBuf)> {
    let exporter = root.join("sandbox-catalog-export");
    let catalog = root.join("catalog.json");
    fs::write(&exporter, b"deterministic-exporter")?;
    fs::write(
        &catalog,
        serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
            "kind": "ephemeral_sandbox_product_catalog",
            "domains": {
                "manager": {
                    "operations": [
                        {"name": "squash_layerstacks"}
                    ]
                },
                "runtime": {
                    "operations": [
                        {"name": "exec_command"},
                        {"name": "publish_workspace_session"}
                    ]
                },
                "observability": {
                    "operations": []
                }
            }
        }))?,
    )?;
    Ok((exporter, catalog))
}

fn binding(root: &Path) -> TestResult<CatalogBinding> {
    let (exporter, catalog) = write_catalog(root)?;
    Ok(bind_product_catalog(
        &exporter,
        &catalog,
        "650ef4422bb694714f53a035ff30c8306dbcb312",
    )?)
}

fn matched_boundary(cache_state: ControlCacheMatch) -> ControlBoundary {
    ControlBoundary {
        candidate_start: "externally_closed".to_owned(),
        candidate_stop: "externally_usable".to_owned(),
        current_i2_start: "externally_closed".to_owned(),
        current_i2_stop: "externally_usable".to_owned(),
        same_fixture: true,
        same_intent: true,
        same_durability: true,
        same_readiness: true,
        cache_state,
        unknown_reason: None,
    }
}

#[test]
fn catalog_binding_records_public_and_programmatic_boundaries() -> TestResult {
    let root = TestDirectory::new("catalog")?;
    let binding = binding(root.path())?;
    assert_eq!(binding.schema_version, 1);
    assert!(binding.facts.publish_workspace_session);
    assert!(binding.facts.squash_layerstacks);
    assert!(!binding.facts.activate_workspace_session);
    assert!(!binding.facts.fork_workspace_session);
    assert!(!binding.facts.rollback_workspace_session);
    assert_eq!(binding.binding_id.len(), 64);
    Ok(())
}

#[test]
fn source_collection_is_deterministic_streamed_and_bounded() -> TestResult {
    let root = TestDirectory::new("source")?;
    fs::create_dir(root.path().join("nested"))?;
    fs::write(root.path().join("z.txt"), b"zeta")?;
    fs::write(root.path().join("nested/a.txt"), b"alpha")?;
    #[cfg(unix)]
    std::os::unix::fs::symlink("nested/a.txt", root.path().join("link"))?;

    let limits = ControlCollectionLimits {
        max_entries: 4,
        max_logical_bytes: 9,
        max_path_bytes: 64,
    };
    let first = collect_control_changes(root.path(), &limits)?;
    let second = collect_control_changes(root.path(), &limits)?;
    assert_eq!(first.profile, second.profile);
    assert_eq!(first.profile.logical_bytes, 9);
    assert_eq!(first.profile.regular_files, 2);
    assert_eq!(first.profile.directories, 2);
    #[cfg(unix)]
    assert_eq!(first.profile.symlinks, 1);
    assert_eq!(first.changes.len(), usize::try_from(first.profile.entries)?);

    let error = collect_control_changes(
        root.path(),
        &ControlCollectionLimits {
            max_entries: 1,
            ..limits
        },
    )
    .expect_err("entry bound must fail closed");
    assert!(error.to_string().contains("entry limit"));
    Ok(())
}

#[test]
fn incompatible_boundary_is_unknown_only_with_an_explicit_reason() -> TestResult {
    let mut boundary = matched_boundary(ControlCacheMatch::Mismatched);
    let error = boundary
        .verdict()
        .expect_err("unexplained mismatch must fail");
    assert!(error.to_string().contains("explicit unknown reason"));

    boundary.unknown_reason = Some("current API cannot select the same historical root".to_owned());
    assert_eq!(boundary.verdict()?, ControlVerdict::Unknown);

    boundary.cache_state = ControlCacheMatch::Matched;
    assert_eq!(boundary.verdict()?, ControlVerdict::Unknown);
    Ok(())
}

#[test]
fn closing_control_rejects_overlapping_trees_and_tampered_catalog_binding() -> TestResult {
    let root = TestDirectory::new("binding-integrity")?;
    let source = root.path().join("source");
    let state = root.path().join("state");
    fs::create_dir(&source)?;
    fs::create_dir(&state)?;
    fs::write(source.join("delta"), b"payload")?;
    let changes = collect_control_changes(
        &source,
        &ControlCollectionLimits {
            max_entries: 1,
            max_logical_bytes: 7,
            max_path_bytes: 32,
        },
    )?;
    let binding = binding(root.path())?;

    let overlap = run_current_i2_closing(
        &CurrentI2ClosingRequest {
            state_root: source.clone(),
            publication_id: [1_u8; 16],
            public_root_hash: "root".to_owned(),
            catalog_binding: binding.clone(),
            boundary: matched_boundary(ControlCacheMatch::NotApplicable),
        },
        &changes,
    )
    .expect_err("overlapping state and source roots must fail");
    assert!(overlap.to_string().contains("overlap"));

    let mut tampered = binding;
    tampered.facts.fork_workspace_session = true;
    let error = run_current_i2_closing(
        &CurrentI2ClosingRequest {
            state_root: state,
            publication_id: [2_u8; 16],
            public_root_hash: "root".to_owned(),
            catalog_binding: tampered,
            boundary: matched_boundary(ControlCacheMatch::NotApplicable),
        },
        &changes,
    )
    .expect_err("tampered catalog binding must fail before publication");
    assert!(error.to_string().contains("binding ID"));
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires the qualified Linux native LayerStack profile"]
fn real_current_i2_closing_cold_and_same_key_controls() -> TestResult {
    sandbox_runtime_layerstack::reset_process_state_for_tests();
    let root = TestDirectory::new("real-i2")?;
    let source = root.path().join("source");
    let state = root.path().join("state");
    fs::create_dir(&source)?;
    fs::create_dir(&state)?;
    fs::create_dir(source.join("delta"))?;
    for index in 0..10_u8 {
        fs::write(
            source.join(format!("delta/{index:02}.bin")),
            vec![index; 1024],
        )?;
    }
    let binding = binding(root.path())?;
    let changes = collect_control_changes(
        &source,
        &ControlCollectionLimits {
            max_entries: 16,
            max_logical_bytes: 16 * 1024,
            max_path_bytes: 128,
        },
    )?;
    let closing = run_current_i2_closing(
        &CurrentI2ClosingRequest {
            state_root: state.clone(),
            publication_id: [7_u8; 16],
            public_root_hash: changes.profile.source_manifest_sha256.clone(),
            catalog_binding: binding.clone(),
            boundary: matched_boundary(ControlCacheMatch::NotApplicable),
        },
        &changes,
    )?;
    assert_eq!(closing.verdict, ControlVerdict::Matched);
    assert_eq!(
        closing.coverage.classification,
        ControlApiCoverage::PublicIntentProgrammaticCurrentI2
    );

    let cold = run_current_i2_materialization(
        &CurrentI2MaterializationRequest {
            state_root: state.clone(),
            intent: ControlIntent::ColdActivation,
            timeout: Duration::from_secs(30),
            cache_expectation: ControlCacheExpectation::ColdBuilt,
            expected_selection: None,
            catalog_binding: binding.clone(),
            boundary: matched_boundary(ControlCacheMatch::Matched),
        },
        |carrier| {
            let path = carrier.join("delta/00.bin");
            let bytes = fs::read(&path)
                .map_err(|source| PocError::io("read external readiness carrier", &path, source))?;
            Ok(ExternalReadinessReceipt {
                probe: "external_carrier_read".to_owned(),
                passed: bytes == vec![0_u8; 1024],
                observed: format!("{} bytes", bytes.len()),
            })
        },
    )?;
    let selection = cold
        .materialization
        .as_ref()
        .expect("cold receipt contains materialization")
        .selection_key();

    let reused = run_current_i2_materialization(
        &CurrentI2MaterializationRequest {
            state_root: state,
            intent: ControlIntent::SameKeyActivation,
            timeout: Duration::from_secs(30),
            cache_expectation: ControlCacheExpectation::SameKeyReused,
            expected_selection: Some(selection),
            catalog_binding: binding,
            boundary: matched_boundary(ControlCacheMatch::Matched),
        },
        |carrier| {
            Ok(ExternalReadinessReceipt {
                probe: "external_carrier_metadata".to_owned(),
                passed: carrier.is_dir(),
                observed: carrier.display().to_string(),
            })
        },
    )?;
    assert_eq!(
        reused
            .materialization
            .expect("same-key receipt contains materialization")
            .disposition,
        "reused"
    );
    sandbox_runtime_layerstack::reset_process_state_for_tests();
    Ok(())
}
