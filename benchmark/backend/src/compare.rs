use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::artifacts::{ArtifactError, ArtifactId, ArtifactStore};
use crate::model::{
    ComparisonPlan, OperationId, PhaseCorrelationRule, PhaseId, PhaseSource, PhaseUnit,
    TreatmentField,
};
use crate::plan::{ExpandedCell, ExpandedPlan, EXPANDED_PLAN_SCHEMA_VERSION};
use crate::report::{
    BenchmarkReport, CellSummary, MetricIdentity, MetricSummary, PhaseSummary, ReportError,
    REPORT_DERIVATION_REVISION, REPORT_SCHEMA_VERSION,
};
use crate::resources::{MetricDirection, MetricUnit};
use crate::scheduler::{
    is_terminal, RunManifest, EXPANDED_PLAN_SCHEMA_NAME, RUN_MANIFEST_SCHEMA_NAME,
    RUN_MANIFEST_SCHEMA_VERSION,
};
use crate::statistics::{
    self, ConfidenceInterval, DistributionProjection, SampleStatistics, StatisticsError,
    STATISTICS_SCHEMA_VERSION,
};

pub const COMPARISON_SCHEMA_VERSION: u32 = 1;
pub const COMPARISON_DERIVATION_REVISION: u32 = 3;
pub const DEFAULT_COMPARISON_PROTOCOL_ID: &str = "same_treatment";
pub const DEFAULT_COMPARISON_PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComparisonRequest {
    pub reference_run_id: String,
    pub candidate_run_id: String,
    pub descriptive_override: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonDeclarationSource {
    Defaulted,
    Explicit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedComparisonPlan {
    pub protocol_id: String,
    pub protocol_version: u32,
    pub treatment_fields: BTreeSet<TreatmentField>,
    pub source: ComparisonDeclarationSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComparisonProtocolDecision {
    pub reference: NormalizedComparisonPlan,
    pub candidate: NormalizedComparisonPlan,
    pub declarations_compatible: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompatibilityScope {
    CoreInvariant,
    Treatment,
    Metric,
    Correctness,
    Phase,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompatibilityCheck {
    pub check_id: String,
    pub label: String,
    pub compatible: bool,
    pub consequence: String,
    pub scope: CompatibilityScope,
    pub blocks_aggregate: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TreatmentDifference {
    pub field: TreatmentField,
    pub identity_component: String,
    pub reference: Option<String>,
    pub candidate: Option<String>,
    pub declared: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MatchedCell {
    pub match_id: String,
    pub comparison_key_sha256: String,
    pub operation_id: OperationId,
    pub reference_cell_id: String,
    pub candidate_cell_id: String,
    pub effective_protocol_compatible: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorrectnessDifference {
    pub reference_correctness_failed: u64,
    pub candidate_correctness_failed: u64,
    pub reference_cleanup_invalid: u64,
    pub candidate_cleanup_invalid: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComparisonDelta {
    pub comparison_id: String,
    pub match_id: String,
    pub reference_cell_id: String,
    pub candidate_cell_id: String,
    pub metric_id: String,
    pub unit: MetricUnit,
    pub reference_unit: Option<MetricUnit>,
    pub candidate_unit: Option<MetricUnit>,
    pub reference_value: Option<f64>,
    pub candidate_value: Option<f64>,
    pub reference_n: u64,
    pub candidate_n: u64,
    pub reference_unavailable_n: u64,
    pub candidate_unavailable_n: u64,
    pub reference_statistics: Option<SampleStatistics>,
    pub candidate_statistics: Option<SampleStatistics>,
    pub absolute_change: Option<f64>,
    pub percent_change: Option<f64>,
    pub median_difference_confidence_interval: Option<ConfidenceInterval>,
    pub confidence_interval_omission_reason: Option<String>,
    pub unavailable_reason: Option<String>,
    pub direction: MetricDirection,
    pub descriptive_only: bool,
    pub correctness: CorrectnessDifference,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhaseComparison {
    pub comparison_id: String,
    pub match_id: String,
    pub reference_cell_id: String,
    pub candidate_cell_id: String,
    pub phase_id: PhaseId,
    pub unit: PhaseUnit,
    pub reference_summary: Option<PhaseSummary>,
    pub candidate_summary: Option<PhaseSummary>,
    pub identity_compatible: bool,
    pub reference_value: Option<f64>,
    pub candidate_value: Option<f64>,
    pub absolute_change: Option<f64>,
    pub percent_change: Option<f64>,
    pub median_difference_confidence_interval: Option<ConfidenceInterval>,
    pub confidence_interval_omission_reason: Option<String>,
    pub unavailable_reason: Option<String>,
    pub descriptive_only: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComparisonResponse {
    pub schema_version: u32,
    pub comparison_derivation_revision: u32,
    pub reference_run_id: String,
    pub candidate_run_id: String,
    pub protocol: ComparisonProtocolDecision,
    pub compatible: bool,
    pub descriptive_only: bool,
    pub treatment_differences: Vec<String>,
    pub typed_treatment_differences: Vec<TreatmentDifference>,
    pub checks: Vec<CompatibilityCheck>,
    pub matched_cell_ids: Vec<String>,
    pub matched_cells: Vec<MatchedCell>,
    pub deltas: Vec<ComparisonDelta>,
    pub phase_comparisons: Vec<PhaseComparison>,
    pub performance_verdict: Option<String>,
}

#[derive(Debug, Error)]
pub enum CompareError {
    #[error(transparent)]
    Artifact(#[from] ArtifactError),
    #[error(transparent)]
    Report(#[from] ReportError),
    #[error(transparent)]
    Statistics(#[from] StatisticsError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("reference and candidate run ids must differ")]
    SameRun,
    #[error("artifact inconsistency for run {run_id}: {message}")]
    ArtifactInconsistency { run_id: String, message: String },
}

struct RunArtifacts {
    manifest: RunManifest,
    expanded: ExpandedPlan,
    report: BenchmarkReport,
}

struct IndexedCell<'a> {
    cell: &'a ExpandedCell,
    canonical_key: Vec<u8>,
}

struct CellPair<'a> {
    matched: MatchedCell,
    reference_expanded: &'a ExpandedCell,
    candidate_expanded: &'a ExpandedCell,
    reference_report: &'a CellSummary,
    candidate_report: &'a CellSummary,
}

pub fn compare(
    store: &ArtifactStore,
    request: &ComparisonRequest,
) -> Result<ComparisonResponse, CompareError> {
    if request.reference_run_id == request.candidate_run_id {
        return Err(CompareError::SameRun);
    }

    let reference = load_run(store, &request.reference_run_id)?;
    let candidate = load_run(store, &request.candidate_run_id)?;
    let protocol = comparison_protocol_decision(
        reference.expanded.canonical_plan.comparison.as_ref(),
        candidate.expanded.canonical_plan.comparison.as_ref(),
    );
    let declared_treatment_fields = protocol
        .declarations_compatible
        .then_some(&protocol.reference.treatment_fields);

    let mut checks = Vec::new();
    checks.push(check(
        "comparison_declaration",
        "Versioned comparison declaration",
        protocol.declarations_compatible,
        if protocol.declarations_compatible {
            "Both runs use the same declaration source, protocol id, version, and treatment allowlist."
        } else {
            "Declarations differ, or one run is absent while the other is explicit; treatment allowlists are never unioned."
        },
        CompatibilityScope::CoreInvariant,
        true,
    ));
    checks.push(check(
        "terminal_reports",
        "Terminal report evidence",
        is_terminal(reference.manifest.state)
            && is_terminal(candidate.manifest.state)
            && !reference.report.provisional
            && !candidate.report.provisional,
        "Aggregate comparison requires two terminal, non-provisional reports.",
        CompatibilityScope::CoreInvariant,
        true,
    ));

    let mut typed_treatment_differences = Vec::new();
    treatment_checks(
        &reference.manifest,
        &candidate.manifest,
        declared_treatment_fields,
        &mut checks,
        &mut typed_treatment_differences,
    );
    invariant_checks(&reference, &candidate, &mut checks);
    operation_revision_checks(&reference, &candidate, &mut checks);

    let (pairs, duplicate_projection) = match_cells(&reference, &candidate)?;
    checks.push(check(
        "operation_comparison_projection_uniqueness",
        "Operation comparison projection uniqueness",
        !duplicate_projection,
        if duplicate_projection {
            "A run contains duplicate persisted comparison keys, so cell pairing is ambiguous."
        } else {
            "Every persisted operation comparison key identifies at most one cell per run."
        },
        CompatibilityScope::CoreInvariant,
        true,
    ));
    let exact_cell_scope = !pairs.is_empty()
        && pairs.len() == reference.expanded.cells.len()
        && pairs.len() == candidate.expanded.cells.len();
    checks.push(check(
        "matched_cell_scope",
        "Matched operation and factor scope",
        exact_cell_scope,
        &format!(
            "Reference has {} cell(s), candidate has {} cell(s), and {} exact persisted typed operation comparison key(s) match; every cell must match exactly for aggregate comparison.",
            reference.expanded.cells.len(),
            candidate.expanded.cells.len(),
            pairs.len(),
        ),
        CompatibilityScope::CoreInvariant,
        true,
    ));
    let effective_protocols_compatible = pairs
        .iter()
        .all(|pair| pair.matched.effective_protocol_compatible);
    checks.push(check(
        "effective_cell_protocol",
        "Effective per-cell protocol",
        effective_protocols_compatible,
        if effective_protocols_compatible {
            "Warmups, measured trials, timeout, destructive boundary, and cleanup policy match for every paired cell."
        } else {
            "At least one paired cell differs in warmups, measured trials, timeout, destructive boundary, or cleanup policy."
        },
        CompatibilityScope::CoreInvariant,
        true,
    ));

    comparison_view_checks(&pairs, &mut checks);
    let compatible = checks
        .iter()
        .filter(|result| result.blocks_aggregate)
        .all(|result| result.compatible);
    let descriptive_only = request.descriptive_override;
    let derived_deltas = build_deltas(
        reference.expanded.canonical_plan.seed,
        &request.reference_run_id,
        &request.candidate_run_id,
        &pairs,
        compatible,
        descriptive_only,
        &mut checks,
    )?;
    let deltas = if compatible || descriptive_only {
        derived_deltas
    } else {
        Vec::new()
    };
    let derived_phase_comparisons = build_phase_comparisons(
        reference.expanded.canonical_plan.seed,
        &request.reference_run_id,
        &request.candidate_run_id,
        &pairs,
        compatible,
        descriptive_only,
        &mut checks,
    )?;
    let phase_comparisons = if compatible || descriptive_only {
        derived_phase_comparisons
    } else {
        Vec::new()
    };

    let matched_cells = pairs
        .iter()
        .map(|pair| pair.matched.clone())
        .collect::<Vec<_>>();
    let matched_cell_ids = matched_cells
        .iter()
        .map(|cell| cell.match_id.clone())
        .collect();
    let treatment_differences = typed_treatment_differences
        .iter()
        .map(treatment_difference_label)
        .collect();

    Ok(ComparisonResponse {
        schema_version: COMPARISON_SCHEMA_VERSION,
        comparison_derivation_revision: COMPARISON_DERIVATION_REVISION,
        reference_run_id: request.reference_run_id.clone(),
        candidate_run_id: request.candidate_run_id.clone(),
        protocol,
        compatible,
        descriptive_only,
        treatment_differences,
        typed_treatment_differences,
        checks,
        matched_cell_ids,
        matched_cells,
        deltas,
        phase_comparisons,
        performance_verdict: None,
    })
}

fn load_run(store: &ArtifactStore, run_id: &str) -> Result<RunArtifacts, CompareError> {
    let manifest: RunManifest = store.read_envelope(
        run_id,
        ArtifactId::RunManifest,
        RUN_MANIFEST_SCHEMA_NAME,
        RUN_MANIFEST_SCHEMA_VERSION,
    )?;
    let expanded: ExpandedPlan = store.read_envelope(
        run_id,
        ArtifactId::ExpandedPlan,
        EXPANDED_PLAN_SCHEMA_NAME,
        EXPANDED_PLAN_SCHEMA_VERSION,
    )?;
    let report = crate::report::regenerate(store, run_id, false)?;

    require_artifact(
        run_id,
        manifest.schema_version == RUN_MANIFEST_SCHEMA_VERSION
            && expanded.schema_version == EXPANDED_PLAN_SCHEMA_VERSION
            && report.schema_version == REPORT_SCHEMA_VERSION,
        "embedded manifest, expanded-plan, or report schema version is unsupported",
    )?;
    require_artifact(
        run_id,
        manifest.run_id == run_id,
        "manifest run id does not match its artifact directory",
    )?;
    require_artifact(
        run_id,
        report.run_id == run_id,
        "report run id does not match its artifact directory",
    )?;
    require_artifact(
        run_id,
        manifest.plan_hash == expanded.plan_hash && report.plan_hash == expanded.plan_hash,
        "manifest, expanded plan, and report plan hashes do not match",
    )?;
    require_artifact(
        run_id,
        manifest.state == report.state,
        "manifest and report states do not match",
    )?;
    require_artifact(
        run_id,
        manifest.treatment == manifest.environment.treatment,
        "manifest treatment identity copies do not match",
    )?;
    require_artifact(
        run_id,
        manifest.fixed_lifecycle_policy == expanded.fixed_lifecycle_policy,
        "manifest and expanded-plan lifecycle policies do not match",
    )?;
    require_artifact(
        run_id,
        manifest.environment.client_cohort == expanded.canonical_plan.environment.client_cohort
            && manifest.environment.client_cohort == expanded.effective_environment.client_cohort,
        "client cohort differs across manifest, intent, and effective environment",
    )?;
    require_artifact(
        run_id,
        manifest.environment.image_reference == expanded.canonical_plan.environment.image.0,
        "image reference differs across manifest and expanded plan",
    )?;
    require_artifact(
        run_id,
        manifest.environment.image_digest == expanded.effective_environment.image_digest,
        "image digest differs across manifest and effective environment",
    )?;
    require_artifact(
        run_id,
        manifest.environment.host.filesystem == expanded.effective_environment.filesystem,
        "filesystem identity differs across manifest and effective environment",
    )?;
    require_artifact(
        run_id,
        manifest.environment.host.free_space_bytes
            == expanded.effective_environment.free_space_bytes,
        "free-space snapshot differs across manifest and effective environment",
    )?;
    require_artifact(
        run_id,
        manifest.environment.workspace_root_identity
            == expanded.effective_environment.workspace_root_identity,
        "workspace-root identity differs across manifest and expanded plan",
    )?;
    require_artifact(
        run_id,
        report.definition_snapshot_version == manifest.definition_snapshot.schema_version,
        "report and manifest definition snapshot versions do not match",
    )?;
    validate_report_cells(run_id, &expanded, &report)?;

    Ok(RunArtifacts {
        manifest,
        expanded,
        report,
    })
}

fn require_artifact(run_id: &str, condition: bool, message: &str) -> Result<(), CompareError> {
    if condition {
        Ok(())
    } else {
        Err(CompareError::ArtifactInconsistency {
            run_id: run_id.to_owned(),
            message: message.to_owned(),
        })
    }
}

fn validate_report_cells(
    run_id: &str,
    expanded: &ExpandedPlan,
    report: &BenchmarkReport,
) -> Result<(), CompareError> {
    let mut report_cells = BTreeMap::new();
    for cell in &report.cells {
        if report_cells.insert(cell.cell_id.as_str(), cell).is_some() {
            return Err(CompareError::ArtifactInconsistency {
                run_id: run_id.to_owned(),
                message: format!("report repeats cell {}", cell.cell_id),
            });
        }
    }
    if report_cells.len() != expanded.cells.len() {
        return Err(CompareError::ArtifactInconsistency {
            run_id: run_id.to_owned(),
            message: "report and expanded plan contain different cell counts".to_owned(),
        });
    }
    let mut operation_revisions = BTreeMap::new();
    for cell in &expanded.cells {
        if cell.comparison_key.operation != cell.operation_id
            || cell.comparison_key.semantic_revision != cell.operation_semantic_revision
            || cell.comparison_key.factor_schema_revision != cell.factor_schema_revision
        {
            return Err(CompareError::ArtifactInconsistency {
                run_id: run_id.to_owned(),
                message: format!(
                    "expanded cell {} has inconsistent operation comparison revisions",
                    cell.cell_id
                ),
            });
        }
        let revisions = (
            cell.operation_semantic_revision,
            cell.factor_schema_revision,
            cell.comparison_key.comparison_projection_revision,
        );
        if operation_revisions
            .insert(cell.operation_id, revisions)
            .is_some_and(|existing| existing != revisions)
        {
            return Err(CompareError::ArtifactInconsistency {
                run_id: run_id.to_owned(),
                message: format!(
                    "expanded plan contains multiple definition revisions for {}",
                    operation_id_slug(cell.operation_id)
                ),
            });
        }
        let Some(report_cell) = report_cells.get(cell.cell_id.as_str()) else {
            return Err(CompareError::ArtifactInconsistency {
                run_id: run_id.to_owned(),
                message: format!("report is missing expanded cell {}", cell.cell_id),
            });
        };
        if report_cell.operation_id != cell.operation_id
            || report_cell.family_id != cell.family_id
            || report_cell.comparison_key != cell.comparison_key
        {
            return Err(CompareError::ArtifactInconsistency {
                run_id: run_id.to_owned(),
                message: format!(
                    "report scientific identity does not match expanded cell {}",
                    cell.cell_id
                ),
            });
        }
    }
    Ok(())
}

fn comparison_protocol_decision(
    reference: Option<&ComparisonPlan>,
    candidate: Option<&ComparisonPlan>,
) -> ComparisonProtocolDecision {
    let reference_normalized = normalize_comparison_plan(reference);
    let candidate_normalized = normalize_comparison_plan(candidate);
    let declarations_compatible = match (reference, candidate) {
        (None, None) => true,
        (Some(reference), Some(candidate)) => reference == candidate,
        (None, Some(_)) | (Some(_), None) => false,
    };
    ComparisonProtocolDecision {
        reference: reference_normalized,
        candidate: candidate_normalized,
        declarations_compatible,
    }
}

fn normalize_comparison_plan(plan: Option<&ComparisonPlan>) -> NormalizedComparisonPlan {
    match plan {
        Some(plan) => NormalizedComparisonPlan {
            protocol_id: plan.protocol_id.clone(),
            protocol_version: plan.protocol_version,
            treatment_fields: plan.treatment_fields.clone(),
            source: ComparisonDeclarationSource::Explicit,
        },
        None => NormalizedComparisonPlan {
            protocol_id: DEFAULT_COMPARISON_PROTOCOL_ID.to_owned(),
            protocol_version: DEFAULT_COMPARISON_PROTOCOL_VERSION,
            treatment_fields: BTreeSet::new(),
            source: ComparisonDeclarationSource::Defaulted,
        },
    }
}

fn treatment_checks(
    reference: &RunManifest,
    candidate: &RunManifest,
    declared_fields: Option<&BTreeSet<TreatmentField>>,
    checks: &mut Vec<CompatibilityCheck>,
    differences: &mut Vec<TreatmentDifference>,
) {
    treatment_required_string(
        "treatment.source_commit",
        "Source commit",
        TreatmentField::SourceCommit,
        "source_commit",
        &reference.treatment.source_commit,
        &candidate.treatment.source_commit,
        declared_fields,
        checks,
        differences,
    );

    let dirty_hashes_valid = (!reference.treatment.source_dirty
        || nonempty(reference.treatment.source_diff_hash.as_deref()))
        && (!candidate.treatment.source_dirty
            || nonempty(candidate.treatment.source_diff_hash.as_deref()));
    checks.push(check(
        "treatment.dirty_source_evidence",
        "Dirty source evidence",
        dirty_hashes_valid,
        if dirty_hashes_valid {
            "Every dirty source tree carries its diff hash in treatment identity."
        } else {
            "A dirty source tree lacks the required diff hash, so its treatment cannot be identified."
        },
        CompatibilityScope::Treatment,
        true,
    ));
    treatment_component(
        "treatment.source_dirty",
        "Source dirty state",
        TreatmentField::SourceDiffHash,
        "source_dirty",
        Some(reference.treatment.source_dirty.to_string()),
        Some(candidate.treatment.source_dirty.to_string()),
        declared_fields,
        checks,
        differences,
        false,
    );
    treatment_component(
        "treatment.source_diff_hash",
        "Source diff hash",
        TreatmentField::SourceDiffHash,
        "source_diff_hash",
        reference.treatment.source_diff_hash.clone(),
        candidate.treatment.source_diff_hash.clone(),
        declared_fields,
        checks,
        differences,
        false,
    );
    treatment_component(
        "treatment.daemon_binary_hash",
        "Daemon binary hash",
        TreatmentField::DaemonBinaryHash,
        "daemon_binary_hash",
        reference.treatment.daemon_binary_hash.clone(),
        candidate.treatment.daemon_binary_hash.clone(),
        declared_fields,
        checks,
        differences,
        true,
    );
    treatment_component(
        "treatment.gateway_binary_hash",
        "Gateway binary hash",
        TreatmentField::GatewayBinaryHash,
        "gateway_binary_hash",
        reference.treatment.gateway_binary_hash.clone(),
        candidate.treatment.gateway_binary_hash.clone(),
        declared_fields,
        checks,
        differences,
        true,
    );
}

#[allow(clippy::too_many_arguments)]
fn treatment_required_string(
    check_id: &str,
    label: &str,
    field: TreatmentField,
    component: &str,
    reference: &str,
    candidate: &str,
    declared_fields: Option<&BTreeSet<TreatmentField>>,
    checks: &mut Vec<CompatibilityCheck>,
    differences: &mut Vec<TreatmentDifference>,
) {
    treatment_component(
        check_id,
        label,
        field,
        component,
        (!reference.is_empty()).then(|| reference.to_owned()),
        (!candidate.is_empty()).then(|| candidate.to_owned()),
        declared_fields,
        checks,
        differences,
        true,
    );
}

#[allow(clippy::too_many_arguments)]
fn treatment_component(
    check_id: &str,
    label: &str,
    field: TreatmentField,
    component: &str,
    reference: Option<String>,
    candidate: Option<String>,
    declared_fields: Option<&BTreeSet<TreatmentField>>,
    checks: &mut Vec<CompatibilityCheck>,
    differences: &mut Vec<TreatmentDifference>,
    required: bool,
) {
    let declared = declared_fields.is_some_and(|fields| fields.contains(&field));
    let present = !required || (nonempty(reference.as_deref()) && nonempty(candidate.as_deref()));
    let equal = reference == candidate;
    let compatible = present && (equal || declared);
    let consequence = if !present {
        format!("{label} is unavailable for one or both runs, so treatment identity is incomplete.")
    } else if equal {
        format!("{label} matches as required by the comparison protocol.")
    } else if declared {
        format!("{label} differs exactly as declared treatment and is not treated as an invariant.")
    } else {
        format!("{label} differs but is not in the identical predeclared treatment allowlist.")
    };
    checks.push(check(
        check_id,
        label,
        compatible,
        &consequence,
        CompatibilityScope::Treatment,
        true,
    ));
    if !equal {
        differences.push(TreatmentDifference {
            field,
            identity_component: component.to_owned(),
            reference,
            candidate,
            declared,
        });
    }
}

fn invariant_checks(
    reference: &RunArtifacts,
    candidate: &RunArtifacts,
    checks: &mut Vec<CompatibilityCheck>,
) {
    checks.push(equality_check(
        "client_cohort",
        "Client cohort",
        reference.manifest.environment.client_cohort,
        candidate.manifest.environment.client_cohort,
        "Direct client, CLI E2E, and future remote-product cohorts are distinct scientific populations.",
    ));
    checks.push(equality_check(
        "sandbox_image_reference",
        "Sandbox image reference",
        &reference.manifest.environment.image_reference,
        &candidate.manifest.environment.image_reference,
        "The non-secret requested image reference must match.",
    ));
    checks.push(required_optional_equality_check(
        "sandbox_image_digest",
        "Sandbox image digest",
        reference.manifest.environment.image_digest.as_deref(),
        candidate.manifest.environment.image_digest.as_deref(),
        "The resolved sandbox image digest is an environment invariant.",
    ));
    checks.push(equality_check(
        "workspace_root_identity",
        "Workspace binding identity",
        &reference.manifest.environment.workspace_root_identity,
        &candidate.manifest.environment.workspace_root_identity,
        "The canonical machine-local workspace binding must match for aggregate comparison.",
    ));
    checks.push(equality_check(
        "gateway_mode",
        "Fixed isolated gateway mode",
        reference.expanded.effective_environment.gateway_mode,
        candidate.expanded.effective_environment.gateway_mode,
        "Both runs must use the same fixed product-access topology.",
    ));
    checks.push(equality_check(
        "host_operating_system",
        "Host operating system",
        &reference.manifest.environment.host.operating_system,
        &candidate.manifest.environment.host.operating_system,
        "The host operating-system family must match.",
    ));
    checks.push(equality_check(
        "host_architecture",
        "Host architecture",
        &reference.manifest.environment.host.architecture,
        &candidate.manifest.environment.host.architecture,
        "Host architecture is a compatibility invariant.",
    ));
    checks.push(required_optional_equality_check(
        "kernel_major_minor",
        "Kernel major/minor",
        normalized_major_minor(
            reference
                .manifest
                .environment
                .host
                .kernel_release
                .as_deref(),
        )
        .as_deref(),
        normalized_major_minor(
            candidate
                .manifest
                .environment
                .host
                .kernel_release
                .as_deref(),
        )
        .as_deref(),
        "Kernel major/minor must be available and equal; patch differences are intentionally ignored.",
    ));
    checks.push(required_optional_equality_check(
        "docker_engine_major",
        "Docker engine major version",
        normalized_major(
            reference
                .manifest
                .environment
                .host
                .docker_engine_version
                .as_deref(),
        )
        .as_deref(),
        normalized_major(
            candidate
                .manifest
                .environment
                .host
                .docker_engine_version
                .as_deref(),
        )
        .as_deref(),
        "Docker engine major must be available and equal; minor and patch differences are outside the v1 invariant.",
    ));
    checks.push(required_optional_equality_check(
        "filesystem_identity",
        "Filesystem identity",
        reference.manifest.environment.host.filesystem.as_deref(),
        candidate.manifest.environment.host.filesystem.as_deref(),
        "Filesystem identity/type materially affects allocation and I/O evidence.",
    ));
    checks.push(equality_check(
        "monotonic_clock",
        "Monotonic clock source",
        &reference.manifest.environment.host.monotonic_clock,
        &candidate.manifest.environment.host.monotonic_clock,
        "The measurement clock source must match.",
    ));
    checks.push(equality_check(
        "resource_sampling_interval",
        "Resource sampling interval",
        reference
            .expanded
            .canonical_plan
            .protocol
            .resource_interval_ms,
        candidate
            .expanded
            .canonical_plan
            .protocol
            .resource_interval_ms,
        "Resource and correlation views require the same explicit millisecond interval.",
    ));
    checks.push(equality_check(
        "cell_order_protocol",
        "Cell-order protocol",
        reference.expanded.canonical_plan.protocol.order,
        candidate.expanded.canonical_plan.protocol.order,
        "The versioned cell-order protocol must match.",
    ));
    checks.push(equality_check(
        "protocol_seed",
        "Protocol seed",
        reference.expanded.canonical_plan.seed,
        candidate.expanded.canonical_plan.seed,
        "The seed controls deterministic order and fixture generation.",
    ));
    checks.push(equality_check(
        "fixed_lifecycle_policy",
        "Fixed lifecycle and stabilization policy",
        reference.manifest.fixed_lifecycle_policy,
        candidate.manifest.fixed_lifecycle_policy,
        "Lifecycle, failure, stabilization, retry, campaign, and family-order revisions must match.",
    ));
    checks.push(equality_check(
        "effective_failure_policy",
        "Effective failure policy",
        reference.manifest.failure_policy,
        candidate.manifest.failure_policy,
        "The complete effective failure actions, retry count, and semantic revision must match.",
    ));
    checks.push(equality_check(
        "effective_timeout_policy",
        "Effective timeout policy",
        reference.manifest.effective_timeouts,
        candidate.manifest.effective_timeouts,
        "Every effective lifecycle, gateway, observation, and cleanup timeout must match.",
    ));
    checks.push(equality_check(
        "effective_gateway_policy",
        "Effective gateway policy",
        &reference.manifest.gateway_policy,
        &candidate.manifest.gateway_policy,
        "The complete isolated-gateway mode, limits, remount widths, and readiness policy must match.",
    ));
    checks.push(equality_check(
        "effective_safety_policy",
        "Effective safety policy",
        &reference.manifest.safety_policy,
        &candidate.manifest.safety_policy,
        "The complete campaign, product, and fixed gateway safety-cap identity must match.",
    ));
    checks.push(equality_check(
        "fixture_generator_revision",
        "Fixture generator revision",
        reference.manifest.fixture_generator_revision,
        candidate.manifest.fixture_generator_revision,
        "The persisted fixture generator semantic revision must match.",
    ));

    let fixture_identity_available = !reference.manifest.fixture_hashes.is_empty()
        && !candidate.manifest.fixture_hashes.is_empty()
        && reference
            .manifest
            .fixture_hashes
            .iter()
            .all(|(key, value)| !key.is_empty() && !value.is_empty())
        && candidate
            .manifest
            .fixture_hashes
            .iter()
            .all(|(key, value)| !key.is_empty() && !value.is_empty());
    checks.push(check(
        "fixture_identity",
        "Fixture identity",
        fixture_identity_available
            && reference.manifest.fixture_hashes == candidate.manifest.fixture_hashes,
        if !fixture_identity_available {
            "One or both runs lack the required fixture hashes."
        } else if reference.manifest.fixture_hashes == candidate.manifest.fixture_hashes {
            "Persisted fixture hashes match exactly."
        } else {
            "Fixture hashes differ; changed input state cannot be aggregated."
        },
        CompatibilityScope::CoreInvariant,
        true,
    ));
    checks.push(check(
        "report_derivation",
        "Statistics and report derivation",
        reference.report.schema_version == candidate.report.schema_version
            && reference.report.schema_version == REPORT_SCHEMA_VERSION
            && reference.report.report_derivation_revision
                == candidate.report.report_derivation_revision
            && reference.report.report_derivation_revision == REPORT_DERIVATION_REVISION,
        "Report schema and derivation revisions must match; display-only definition labels are not compared.",
        CompatibilityScope::CoreInvariant,
        true,
    ));
}

fn operation_revision_checks(
    reference: &RunArtifacts,
    candidate: &RunArtifacts,
    checks: &mut Vec<CompatibilityCheck>,
) {
    let reference_revisions = operation_revisions(&reference.expanded.cells);
    let candidate_revisions = operation_revisions(&candidate.expanded.cells);

    for operation_id in OperationId::ALL {
        let (reference_revision, candidate_revision) = (
            reference_revisions.get(&operation_id),
            candidate_revisions.get(&operation_id),
        );
        if reference_revision.is_none() && candidate_revision.is_none() {
            continue;
        }
        let compatible = reference_revision.is_some() && reference_revision == candidate_revision;
        let consequence = match (reference_revision, candidate_revision) {
            (Some(_), Some(_)) if compatible => {
                "Operation semantic, factor-schema, and comparison-projection revisions match."
                    .to_owned()
            }
            (Some(reference), Some(candidate)) => format!(
                "Operation revisions differ: reference={reference:?}, candidate={candidate:?}. Changed semantics cannot be aggregated."
            ),
            (Some(reference), None) => format!(
                "The operation exists only in the reference run with revisions {reference:?}; operation sets must match exactly."
            ),
            (None, Some(candidate)) => format!(
                "The operation exists only in the candidate run with revisions {candidate:?}; operation sets must match exactly."
            ),
            (None, None) => unreachable!("two absent operations are skipped"),
        };
        checks.push(check(
            &format!(
                "operation_definition_revision.{}",
                operation_id_slug(operation_id)
            ),
            "Operation definition revisions",
            compatible,
            &consequence,
            CompatibilityScope::CoreInvariant,
            true,
        ));
    }
}

fn operation_revisions(cells: &[ExpandedCell]) -> BTreeMap<OperationId, (u32, u32, u32)> {
    cells
        .iter()
        .map(|cell| {
            (
                cell.operation_id,
                (
                    cell.operation_semantic_revision,
                    cell.factor_schema_revision,
                    cell.comparison_key.comparison_projection_revision,
                ),
            )
        })
        .collect()
}

const fn operation_id_slug(operation_id: OperationId) -> &'static str {
    match operation_id {
        OperationId::ExecCommand => "exec_command",
        OperationId::FileRead => "file_read",
        OperationId::FileWrite => "file_write",
        OperationId::FileEdit => "file_edit",
        OperationId::FileBlame => "file_blame",
        OperationId::CreateWorkspace => "create_workspace",
        OperationId::SquashLayerstack => "squash_layerstack",
    }
}

fn match_cells<'a>(
    reference: &'a RunArtifacts,
    candidate: &'a RunArtifacts,
) -> Result<(Vec<CellPair<'a>>, bool), CompareError> {
    let reference_index = index_cells(&reference.expanded.cells)?;
    let candidate_index = index_cells(&candidate.expanded.cells)?;
    let reference_reports = reference
        .report
        .cells
        .iter()
        .map(|cell| (cell.cell_id.as_str(), cell))
        .collect::<BTreeMap<_, _>>();
    let candidate_reports = candidate
        .report
        .cells
        .iter()
        .map(|cell| (cell.cell_id.as_str(), cell))
        .collect::<BTreeMap<_, _>>();

    let duplicate_projection = reference_index.values().any(|cells| cells.len() != 1)
        || candidate_index.values().any(|cells| cells.len() != 1);
    let mut pairs = Vec::new();
    for (key_sha256, reference_cells) in &reference_index {
        let Some(candidate_cells) = candidate_index.get(key_sha256) else {
            continue;
        };
        if reference_cells.len() != 1 || candidate_cells.len() != 1 {
            continue;
        }
        let reference_cell = &reference_cells[0];
        let candidate_cell = &candidate_cells[0];
        if reference_cell.canonical_key != candidate_cell.canonical_key {
            return Err(CompareError::ArtifactInconsistency {
                run_id: format!(
                    "{} and {}",
                    reference.manifest.run_id, candidate.manifest.run_id
                ),
                message: format!("comparison-key SHA-256 collision at {key_sha256}"),
            });
        }
        let reference_report = reference_reports
            .get(reference_cell.cell.cell_id.as_str())
            .copied()
            .ok_or_else(|| CompareError::ArtifactInconsistency {
                run_id: reference.manifest.run_id.clone(),
                message: format!(
                    "report is missing matched cell {}",
                    reference_cell.cell.cell_id
                ),
            })?;
        let candidate_report = candidate_reports
            .get(candidate_cell.cell.cell_id.as_str())
            .copied()
            .ok_or_else(|| CompareError::ArtifactInconsistency {
                run_id: candidate.manifest.run_id.clone(),
                message: format!(
                    "report is missing matched cell {}",
                    candidate_cell.cell.cell_id
                ),
            })?;
        let match_id = sha256_json(&(
            COMPARISON_SCHEMA_VERSION,
            key_sha256,
            reference_cell.cell.operation_id,
        ))?;
        pairs.push(CellPair {
            matched: MatchedCell {
                match_id,
                comparison_key_sha256: key_sha256.clone(),
                operation_id: reference_cell.cell.operation_id,
                reference_cell_id: reference_cell.cell.cell_id.clone(),
                candidate_cell_id: candidate_cell.cell.cell_id.clone(),
                effective_protocol_compatible: reference_cell.cell.protocol
                    == candidate_cell.cell.protocol,
            },
            reference_expanded: reference_cell.cell,
            candidate_expanded: candidate_cell.cell,
            reference_report,
            candidate_report,
        });
    }
    pairs.sort_by(|left, right| left.matched.match_id.cmp(&right.matched.match_id));
    Ok((pairs, duplicate_projection))
}

fn index_cells(
    cells: &[ExpandedCell],
) -> Result<BTreeMap<String, Vec<IndexedCell<'_>>>, CompareError> {
    let mut index = BTreeMap::<String, Vec<IndexedCell<'_>>>::new();
    for cell in cells {
        let canonical_key = serde_json::to_vec(&cell.comparison_key)?;
        let digest = sha256_bytes(&canonical_key);
        index.entry(digest).or_default().push(IndexedCell {
            cell,
            canonical_key,
        });
    }
    Ok(index)
}

fn comparison_view_checks(pairs: &[CellPair<'_>], checks: &mut Vec<CompatibilityCheck>) {
    for pair in pairs {
        let reference_checks = pair
            .reference_report
            .checks
            .iter()
            .map(|summary| (summary.id, summary.semantic_revision))
            .collect::<BTreeSet<_>>();
        let candidate_checks = pair
            .candidate_report
            .checks
            .iter()
            .map(|summary| (summary.id, summary.semantic_revision))
            .collect::<BTreeSet<_>>();
        checks.push(check(
            &format!("correctness_definitions.{}", pair.matched.match_id),
            "Observed correctness definitions",
            reference_checks == candidate_checks,
            if reference_checks == candidate_checks {
                "Observed correctness check ids and semantic revisions match for this cell pair."
            } else {
                "Correctness evidence uses different check ids/revisions or is absent on one side; correctness change is descriptive only."
            },
            CompatibilityScope::Correctness,
            false,
        ));

        let reference_phases = phase_identities(&pair.reference_report.phases);
        let candidate_phases = phase_identities(&pair.candidate_report.phases);
        checks.push(check(
            &format!("phase_definitions.{}", pair.matched.match_id),
            "Observed phase definitions",
            reference_phases == candidate_phases,
            if reference_phases == candidate_phases {
                "Observed phase ids, semantic revisions, units, sources, correlation rules, and exact trace span names match for this cell pair."
            } else {
                "Phase evidence differs; phase-specific comparison is unavailable without weakening latency compatibility."
            },
            CompatibilityScope::Phase,
            false,
        ));

        debug_assert_eq!(
            pair.reference_expanded.comparison_key,
            pair.candidate_expanded.comparison_key
        );
    }
}

type PhaseIdentity<'a> = (
    PhaseId,
    u32,
    PhaseUnit,
    PhaseSource,
    PhaseCorrelationRule,
    &'a str,
);

fn phase_identities(phases: &[PhaseSummary]) -> BTreeSet<PhaseIdentity<'_>> {
    phases
        .iter()
        .map(|summary| {
            (
                summary.id,
                summary.semantic_revision,
                summary.unit,
                summary.source,
                summary.correlation,
                summary.trace_span_name.as_str(),
            )
        })
        .collect()
}

fn build_deltas(
    run_seed: u64,
    reference_run_id: &str,
    candidate_run_id: &str,
    pairs: &[CellPair<'_>],
    compatible: bool,
    descriptive_only: bool,
    checks: &mut Vec<CompatibilityCheck>,
) -> Result<Vec<ComparisonDelta>, CompareError> {
    let mut deltas = Vec::new();
    for pair in pairs {
        let (reference_metrics, reference_duplicates) =
            metric_index(&pair.reference_report.metrics);
        let (candidate_metrics, candidate_duplicates) =
            metric_index(&pair.candidate_report.metrics);
        let mut metric_ids = reference_metrics.keys().cloned().collect::<BTreeSet<_>>();
        metric_ids.extend(candidate_metrics.keys().cloned());
        metric_ids.extend(reference_duplicates.iter().cloned());
        metric_ids.extend(candidate_duplicates.iter().cloned());

        for metric_id in metric_ids {
            let check_id = format!("metric_identity.{}.{}", pair.matched.match_id, metric_id);
            if reference_duplicates.contains(&metric_id)
                || candidate_duplicates.contains(&metric_id)
            {
                checks.push(check(
                    &check_id,
                    "Metric identity",
                    false,
                    "The metric id occurs more than once in a cell report, so its view is ambiguous.",
                    CompatibilityScope::Metric,
                    false,
                ));
                continue;
            }
            let reference_metric = reference_metrics.get(&metric_id).copied();
            let candidate_metric = candidate_metrics.get(&metric_id).copied();
            let identities_compatible = match (reference_metric, candidate_metric) {
                (Some(reference), Some(candidate)) => {
                    metric_identities_compatible(&reference.identity, &candidate.identity)
                        && reference.statistics.schema_version
                            == candidate.statistics.schema_version
                        && reference.statistics.schema_version == STATISTICS_SCHEMA_VERSION
                }
                (None, _) | (_, None) => false,
            };
            checks.push(check(
                &check_id,
                "Metric definition and availability semantics",
                identities_compatible,
                if identities_compatible {
                    "Metric id/revision, scope, unit, kind, source, availability, aggregation, ratio scale, sampling, and derivation match."
                } else {
                    "The metric is absent on one side or its scientific identity differs; only this metric view is unavailable."
                },
                CompatibilityScope::Metric,
                false,
            ));

            let evidence_available = reference_metric
                .and_then(|metric| metric.statistics.median)
                .is_some()
                && candidate_metric
                    .and_then(|metric| metric.statistics.median)
                    .is_some();
            checks.push(check(
                &format!("metric_evidence.{}.{}", pair.matched.match_id, metric_id),
                "Metric evidence availability",
                evidence_available,
                if evidence_available {
                    "Both runs contain a successful measured median for this metric."
                } else {
                    "One or both runs have no eligible available median; unavailable counters remain unavailable, never zero."
                },
                CompatibilityScope::Metric,
                false,
            ));

            deltas.push(build_delta(
                run_seed,
                reference_run_id,
                candidate_run_id,
                pair,
                &metric_id,
                reference_metric,
                candidate_metric,
                compatible,
                identities_compatible,
                descriptive_only,
            )?);
        }
    }
    deltas.sort_by(|left, right| left.comparison_id.cmp(&right.comparison_id));
    Ok(deltas)
}

fn build_phase_comparisons(
    run_seed: u64,
    reference_run_id: &str,
    candidate_run_id: &str,
    pairs: &[CellPair<'_>],
    compatible: bool,
    descriptive_only: bool,
    checks: &mut Vec<CompatibilityCheck>,
) -> Result<Vec<PhaseComparison>, CompareError> {
    let mut comparisons = Vec::new();
    for pair in pairs {
        let (reference_phases, reference_duplicates) = phase_index(&pair.reference_report.phases);
        let (candidate_phases, candidate_duplicates) = phase_index(&pair.candidate_report.phases);
        let mut phase_ids = reference_phases.keys().copied().collect::<BTreeSet<_>>();
        phase_ids.extend(candidate_phases.keys().copied());
        phase_ids.extend(reference_duplicates.iter().copied());
        phase_ids.extend(candidate_duplicates.iter().copied());

        for phase_id in phase_ids {
            let phase_slug = phase_id_slug(phase_id);
            let check_id = format!("phase_identity.{}.{}", pair.matched.match_id, phase_slug);
            if reference_duplicates.contains(&phase_id) || candidate_duplicates.contains(&phase_id)
            {
                checks.push(check(
                    &check_id,
                    "Phase identity",
                    false,
                    "The phase id occurs more than once in a cell report, so its view is ambiguous.",
                    CompatibilityScope::Phase,
                    false,
                ));
                continue;
            }

            let reference_phase = reference_phases.get(&phase_id).copied();
            let candidate_phase = candidate_phases.get(&phase_id).copied();
            let identities_compatible = match (reference_phase, candidate_phase) {
                (Some(reference), Some(candidate)) => {
                    phase_summaries_compatible(reference, candidate)
                }
                (None, _) | (_, None) => false,
            };
            checks.push(check(
                &check_id,
                "Phase definition and correlation semantics",
                identities_compatible,
                if identities_compatible {
                    "Phase id/revision, unit, source, correlation rule, trace span, and statistics schema match."
                } else {
                    "The phase is absent on one side or its scientific identity differs; only this phase view is unavailable."
                },
                CompatibilityScope::Phase,
                false,
            ));

            let evidence_available = reference_phase
                .and_then(|phase| phase.duration.median)
                .is_some()
                && candidate_phase
                    .and_then(|phase| phase.duration.median)
                    .is_some();
            checks.push(check(
                &format!("phase_evidence.{}.{}", pair.matched.match_id, phase_slug),
                "Phase evidence availability",
                evidence_available,
                if evidence_available {
                    "Both runs contain a successful measured phase-duration median."
                } else {
                    "One or both runs have no eligible phase-duration median."
                },
                CompatibilityScope::Phase,
                false,
            ));

            comparisons.push(build_phase_comparison(
                run_seed,
                reference_run_id,
                candidate_run_id,
                pair,
                phase_id,
                reference_phase,
                candidate_phase,
                compatible,
                identities_compatible,
                descriptive_only,
            )?);
        }
    }
    comparisons.sort_by(|left, right| left.comparison_id.cmp(&right.comparison_id));
    Ok(comparisons)
}

#[allow(clippy::too_many_arguments)]
fn build_phase_comparison(
    run_seed: u64,
    reference_run_id: &str,
    candidate_run_id: &str,
    pair: &CellPair<'_>,
    phase_id: PhaseId,
    reference: Option<&PhaseSummary>,
    candidate: Option<&PhaseSummary>,
    compatible: bool,
    identity_compatible: bool,
    descriptive_only: bool,
) -> Result<PhaseComparison, CompareError> {
    let reference_value = reference.and_then(|phase| phase.duration.median);
    let candidate_value = candidate.and_then(|phase| phase.duration.median);
    let unit = reference
        .map(|phase| phase.unit)
        .or_else(|| candidate.map(|phase| phase.unit))
        .unwrap_or(PhaseUnit::Nanoseconds);
    let aggregate_allowed = compatible && identity_compatible && !descriptive_only;
    let absolute_change = aggregate_allowed
        .then(|| Some(candidate_value? - reference_value?))
        .flatten();
    let percent_change = aggregate_allowed
        .then(|| percent_change(reference_value?, candidate_value?, true))
        .flatten();

    let (median_difference_confidence_interval, confidence_interval_omission_reason) =
        if aggregate_allowed && reference_value.is_some() && candidate_value.is_some() {
            let reference = reference.expect("phase median checked above");
            let candidate = candidate.expect("phase median checked above");
            let reference_samples = samples_from_statistics(
                reference_run_id,
                &format!("phase {}", phase_id_slug(phase_id)),
                &reference.duration,
                eligible_phase_count(reference, reference_run_id)?,
            )?;
            let candidate_samples = samples_from_statistics(
                candidate_run_id,
                &format!("phase {}", phase_id_slug(phase_id)),
                &candidate.duration,
                eligible_phase_count(candidate, candidate_run_id)?,
            )?;
            let seed = comparison_seed(
                run_seed,
                &pair.matched.match_id,
                &format!("phase.{}", phase_id_slug(phase_id)),
            );
            let interval = statistics::bootstrap_median_difference_interval(
                &reference_samples,
                &candidate_samples,
                seed,
            )?;
            let omission = interval.is_none().then(|| "insufficient_n".to_owned());
            (interval, omission)
        } else {
            (None, None)
        };

    let unavailable_reason = if descriptive_only {
        Some("descriptive_override_suppresses_aggregate_change".to_owned())
    } else if !compatible {
        Some("core_compatibility_failed".to_owned())
    } else if !identity_compatible {
        Some("phase_identity_or_correlation_semantics_mismatch".to_owned())
    } else if reference_value.is_none() || candidate_value.is_none() {
        Some("eligible_phase_median_unavailable".to_owned())
    } else if reference_value.is_some_and(|value| value <= 0.0) {
        Some("percent_change_requires_positive_reference".to_owned())
    } else {
        None
    };
    let comparison_id = sha256_json(&(
        COMPARISON_SCHEMA_VERSION,
        "phase",
        &pair.matched.match_id,
        phase_id,
    ))?;

    Ok(PhaseComparison {
        comparison_id,
        match_id: pair.matched.match_id.clone(),
        reference_cell_id: pair.matched.reference_cell_id.clone(),
        candidate_cell_id: pair.matched.candidate_cell_id.clone(),
        phase_id,
        unit,
        reference_summary: reference.cloned(),
        candidate_summary: candidate.cloned(),
        identity_compatible,
        reference_value,
        candidate_value,
        absolute_change,
        percent_change,
        median_difference_confidence_interval,
        confidence_interval_omission_reason,
        unavailable_reason,
        descriptive_only,
    })
}

fn phase_index(phases: &[PhaseSummary]) -> (BTreeMap<PhaseId, &PhaseSummary>, BTreeSet<PhaseId>) {
    let mut index = BTreeMap::new();
    let mut duplicates = BTreeSet::new();
    for phase in phases {
        if index.insert(phase.id, phase).is_some() {
            duplicates.insert(phase.id);
        }
    }
    (index, duplicates)
}

fn phase_summaries_compatible(reference: &PhaseSummary, candidate: &PhaseSummary) -> bool {
    reference.id == candidate.id
        && reference.semantic_revision == candidate.semantic_revision
        && reference.unit == candidate.unit
        && reference.source == candidate.source
        && reference.correlation == candidate.correlation
        && reference.trace_span_name == candidate.trace_span_name
        && reference.duration.schema_version == candidate.duration.schema_version
        && reference.duration.schema_version == STATISTICS_SCHEMA_VERSION
}

fn eligible_phase_count(summary: &PhaseSummary, run_id: &str) -> Result<u64, CompareError> {
    let successful = summary
        .attempted
        .checked_sub(summary.failed)
        .ok_or_else(|| CompareError::ArtifactInconsistency {
            run_id: run_id.to_owned(),
            message: format!(
                "phase {} declares {} failures for {} attempts",
                phase_id_slug(summary.id),
                summary.failed,
                summary.attempted
            ),
        })?;
    let eligible =
        u64::try_from(summary.duration.count).map_err(|_| CompareError::ArtifactInconsistency {
            run_id: run_id.to_owned(),
            message: format!(
                "phase {} duration count does not fit the persisted count domain",
                phase_id_slug(summary.id)
            ),
        })?;
    if eligible > successful {
        return Err(CompareError::ArtifactInconsistency {
            run_id: run_id.to_owned(),
            message: format!(
                "phase {} contains {} eligible durations but only {} successful attempts",
                phase_id_slug(summary.id),
                eligible,
                successful
            ),
        });
    }
    Ok(eligible)
}

const fn phase_id_slug(phase_id: PhaseId) -> &'static str {
    match phase_id {
        PhaseId::LayerstackSquash => "layerstack_squash",
        PhaseId::LayerstackStoragePlan => "layerstack_storage_plan",
        PhaseId::LayerstackFlatten => "layerstack_flatten",
        PhaseId::LayerstackCommit => "layerstack_commit",
        PhaseId::LayerstackRemountSweep => "layerstack_remount_sweep",
        PhaseId::WorkspaceSessionRemount => "workspace_session_remount",
    }
}

#[allow(clippy::too_many_arguments)]
fn build_delta(
    run_seed: u64,
    reference_run_id: &str,
    candidate_run_id: &str,
    pair: &CellPair<'_>,
    metric_id: &str,
    reference: Option<&MetricSummary>,
    candidate: Option<&MetricSummary>,
    compatible: bool,
    metric_compatible: bool,
    descriptive_only: bool,
) -> Result<ComparisonDelta, CompareError> {
    let reference_value = reference.and_then(|metric| metric.statistics.median);
    let candidate_value = candidate.and_then(|metric| metric.statistics.median);
    let reference_unit = reference.map(|metric| metric.identity.unit);
    let candidate_unit = candidate.map(|metric| metric.identity.unit);
    let unit = reference_unit
        .or(candidate_unit)
        .unwrap_or(MetricUnit::Count);
    let direction = match (reference, candidate) {
        (Some(reference), Some(candidate))
            if reference.identity.direction == candidate.identity.direction =>
        {
            reference.identity.direction
        }
        (Some(reference), None) => reference.identity.direction,
        (None, Some(candidate)) => candidate.identity.direction,
        (Some(_), Some(_)) | (None, None) => MetricDirection::DescriptiveOnly,
    };
    let aggregate_allowed = compatible && metric_compatible && !descriptive_only;
    let absolute_change = aggregate_allowed
        .then(|| Some(candidate_value? - reference_value?))
        .flatten();
    let ratio_scale = reference.is_some_and(|metric| metric.identity.ratio_scale)
        && candidate.is_some_and(|metric| metric.identity.ratio_scale);
    let percent_change = aggregate_allowed
        .then(|| percent_change(reference_value?, candidate_value?, ratio_scale))
        .flatten();

    let (median_difference_confidence_interval, confidence_interval_omission_reason) =
        if aggregate_allowed && reference_value.is_some() && candidate_value.is_some() {
            let reference_samples =
                samples_from_summary(reference_run_id, reference.expect("checked above"))?;
            let candidate_samples =
                samples_from_summary(candidate_run_id, candidate.expect("checked above"))?;
            let seed = comparison_seed(run_seed, &pair.matched.match_id, metric_id);
            let interval = statistics::bootstrap_median_difference_interval(
                &reference_samples,
                &candidate_samples,
                seed,
            )?;
            let omission = interval.is_none().then(|| "insufficient_n".to_owned());
            (interval, omission)
        } else {
            (None, None)
        };

    let unavailable_reason = if descriptive_only {
        Some("descriptive_override_suppresses_aggregate_change".to_owned())
    } else if !compatible {
        Some("core_compatibility_failed".to_owned())
    } else if !metric_compatible {
        Some("metric_identity_or_availability_semantics_mismatch".to_owned())
    } else if reference_value.is_none() || candidate_value.is_none() {
        Some("eligible_metric_median_unavailable".to_owned())
    } else if !ratio_scale {
        Some("percent_change_requires_ratio_scale_metric".to_owned())
    } else if reference_value.is_some_and(|value| value <= 0.0) {
        Some("percent_change_requires_positive_reference".to_owned())
    } else {
        None
    };
    let comparison_id =
        sha256_json(&(COMPARISON_SCHEMA_VERSION, &pair.matched.match_id, metric_id))?;

    Ok(ComparisonDelta {
        comparison_id,
        match_id: pair.matched.match_id.clone(),
        reference_cell_id: pair.matched.reference_cell_id.clone(),
        candidate_cell_id: pair.matched.candidate_cell_id.clone(),
        metric_id: metric_id.to_owned(),
        unit,
        reference_unit,
        candidate_unit,
        reference_value,
        candidate_value,
        reference_n: reference.map_or(0, |metric| metric.available_n),
        candidate_n: candidate.map_or(0, |metric| metric.available_n),
        reference_unavailable_n: reference.map_or(0, |metric| metric.unavailable.count),
        candidate_unavailable_n: candidate.map_or(0, |metric| metric.unavailable.count),
        reference_statistics: reference.map(|metric| metric.statistics.clone()),
        candidate_statistics: candidate.map(|metric| metric.statistics.clone()),
        absolute_change,
        percent_change,
        median_difference_confidence_interval,
        confidence_interval_omission_reason,
        unavailable_reason,
        direction,
        descriptive_only,
        correctness: CorrectnessDifference {
            reference_correctness_failed: pair.reference_report.counts.correctness_failed,
            candidate_correctness_failed: pair.candidate_report.counts.correctness_failed,
            reference_cleanup_invalid: pair.reference_report.counts.cleanup_invalid,
            candidate_cleanup_invalid: pair.candidate_report.counts.cleanup_invalid,
        },
    })
}

fn metric_index(metrics: &[MetricSummary]) -> (BTreeMap<String, &MetricSummary>, BTreeSet<String>) {
    let mut index = BTreeMap::new();
    let mut duplicates = BTreeSet::new();
    for metric in metrics {
        if index.insert(metric.identity.id.clone(), metric).is_some() {
            duplicates.insert(metric.identity.id.clone());
        }
    }
    (index, duplicates)
}

fn metric_identities_compatible(reference: &MetricIdentity, candidate: &MetricIdentity) -> bool {
    reference.id == candidate.id
        && reference.semantic_revision == candidate.semantic_revision
        && reference.unit == candidate.unit
        && reference.scope == candidate.scope
        && reference.kind == candidate.kind
        && reference.availability == candidate.availability
        && reference.aggregation == candidate.aggregation
        && reference.direction == candidate.direction
        && reference.source == candidate.source
        && reference.ratio_scale == candidate.ratio_scale
        && reference.report_derivation_revision == candidate.report_derivation_revision
}

fn samples_from_summary(run_id: &str, summary: &MetricSummary) -> Result<Vec<f64>, CompareError> {
    samples_from_statistics(
        run_id,
        &format!("metric {}", summary.identity.id),
        &summary.statistics,
        summary.available_n,
    )
}

fn samples_from_statistics(
    run_id: &str,
    identity: &str,
    statistics: &SampleStatistics,
    expected_count: u64,
) -> Result<Vec<f64>, CompareError> {
    let samples = match &statistics.distribution {
        DistributionProjection::Empty => Vec::new(),
        DistributionProjection::RawPoints { values } => values.clone(),
        DistributionProjection::HistogramEcdf { ecdf, .. } => {
            ecdf.iter().map(|point| point.value).collect()
        }
    };
    if samples.len() != statistics.count
        || u64::try_from(samples.len()).ok() != Some(expected_count)
    {
        return Err(CompareError::ArtifactInconsistency {
            run_id: run_id.to_owned(),
            message: format!(
                "{identity} distribution contains {} values but declares statistics count {} and expected count {}",
                samples.len(),
                statistics.count,
                expected_count
            ),
        });
    }
    Ok(samples)
}

fn percent_change(reference: f64, candidate: f64, ratio_scale: bool) -> Option<f64> {
    (ratio_scale && reference > 0.0).then(|| (candidate - reference) / reference * 100.0)
}

fn check(
    check_id: &str,
    label: &str,
    compatible: bool,
    consequence: &str,
    scope: CompatibilityScope,
    blocks_aggregate: bool,
) -> CompatibilityCheck {
    CompatibilityCheck {
        check_id: check_id.to_owned(),
        label: label.to_owned(),
        compatible,
        consequence: consequence.to_owned(),
        scope,
        blocks_aggregate,
    }
}

fn equality_check<T: PartialEq>(
    check_id: &str,
    label: &str,
    reference: T,
    candidate: T,
    consequence: &str,
) -> CompatibilityCheck {
    check(
        check_id,
        label,
        reference == candidate,
        consequence,
        CompatibilityScope::CoreInvariant,
        true,
    )
}

fn required_optional_equality_check(
    check_id: &str,
    label: &str,
    reference: Option<&str>,
    candidate: Option<&str>,
    consequence: &str,
) -> CompatibilityCheck {
    check(
        check_id,
        label,
        nonempty(reference) && nonempty(candidate) && reference == candidate,
        consequence,
        CompatibilityScope::CoreInvariant,
        true,
    )
}

fn nonempty(value: Option<&str>) -> bool {
    value.is_some_and(|value| !value.is_empty())
}

fn normalized_major_minor(value: Option<&str>) -> Option<String> {
    let mut components = numeric_components(value?);
    let major = components.next()?;
    let minor = components.next()?;
    Some(format!("{major}.{minor}"))
}

fn normalized_major(value: Option<&str>) -> Option<String> {
    numeric_components(value?)
        .next()
        .map(|value| value.to_owned())
}

fn numeric_components(value: &str) -> impl Iterator<Item = &str> {
    value
        .split(|character: char| !character.is_ascii_digit())
        .filter(|component| !component.is_empty())
}

fn treatment_difference_label(difference: &TreatmentDifference) -> String {
    let declared = if difference.declared {
        "declared"
    } else {
        "undeclared"
    };
    format!("{} differs ({declared})", difference.identity_component)
}

fn comparison_seed(run_seed: u64, match_id: &str, metric_id: &str) -> u64 {
    let mut hasher = Sha256::new();
    hasher.update(COMPARISON_SCHEMA_VERSION.to_le_bytes());
    hasher.update(COMPARISON_DERIVATION_REVISION.to_le_bytes());
    hasher.update(run_seed.to_le_bytes());
    hasher.update([0]);
    hasher.update(match_id.as_bytes());
    hasher.update([0]);
    hasher.update(metric_id.as_bytes());
    let digest = hasher.finalize();
    u64::from_le_bytes(digest[..8].try_into().unwrap_or([0; 8]))
}

fn sha256_json<T: Serialize>(value: &T) -> Result<String, serde_json::Error> {
    serde_json::to_vec(value).map(|bytes| sha256_bytes(&bytes))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(71);
    output.push_str("sha256-");
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_absent_declarations_use_the_default_protocol() {
        let decision = comparison_protocol_decision(None, None);
        assert!(decision.declarations_compatible);
        assert_eq!(
            decision.reference.protocol_id,
            DEFAULT_COMPARISON_PROTOCOL_ID
        );
        assert_eq!(
            decision.reference.source,
            ComparisonDeclarationSource::Defaulted
        );
        assert!(decision.reference.treatment_fields.is_empty());
    }

    #[test]
    fn absent_and_explicit_default_are_still_incompatible() {
        let explicit = ComparisonPlan {
            protocol_id: DEFAULT_COMPARISON_PROTOCOL_ID.to_owned(),
            protocol_version: DEFAULT_COMPARISON_PROTOCOL_VERSION,
            treatment_fields: BTreeSet::new(),
        };
        let decision = comparison_protocol_decision(None, Some(&explicit));
        assert!(!decision.declarations_compatible);
        assert_eq!(
            decision.candidate.source,
            ComparisonDeclarationSource::Explicit
        );
    }

    #[test]
    fn declarations_must_match_exactly() {
        let reference = ComparisonPlan {
            protocol_id: "release_comparison".to_owned(),
            protocol_version: 1,
            treatment_fields: BTreeSet::from([TreatmentField::SourceCommit]),
        };
        let mut candidate = reference.clone();
        assert!(
            comparison_protocol_decision(Some(&reference), Some(&candidate))
                .declarations_compatible
        );
        candidate
            .treatment_fields
            .insert(TreatmentField::GatewayBinaryHash);
        assert!(
            !comparison_protocol_decision(Some(&reference), Some(&candidate))
                .declarations_compatible
        );
    }

    #[test]
    fn percentage_change_requires_ratio_scale_and_positive_reference() {
        assert_eq!(percent_change(10.0, 12.5, true), Some(25.0));
        assert_eq!(percent_change(0.0, 12.5, true), None);
        assert_eq!(percent_change(-10.0, -5.0, true), None);
        assert_eq!(percent_change(10.0, 12.5, false), None);
    }

    #[test]
    fn declared_diff_hash_treatment_does_not_require_a_clean_side_hash() {
        let declarations = BTreeSet::from([TreatmentField::SourceDiffHash]);
        let mut checks = Vec::new();
        let mut differences = Vec::new();
        treatment_component(
            "treatment.source_diff_hash",
            "Source diff hash",
            TreatmentField::SourceDiffHash,
            "source_diff_hash",
            Some("sha256-dirty".to_owned()),
            None,
            Some(&declarations),
            &mut checks,
            &mut differences,
            false,
        );

        assert!(checks[0].compatible);
        assert!(differences[0].declared);
    }

    #[test]
    fn comparison_bootstrap_seed_uses_the_run_seed_and_scientific_cell_identity() {
        let seed = comparison_seed(17, "match-a", "operation.latency");
        assert_eq!(seed, comparison_seed(17, "match-a", "operation.latency"));
        assert_ne!(seed, comparison_seed(18, "match-a", "operation.latency"));
        assert_ne!(seed, comparison_seed(17, "match-b", "operation.latency"));
        assert_ne!(seed, comparison_seed(17, "match-a", "sandbox.cpu.time"));
    }

    #[test]
    fn host_versions_use_the_documented_compatibility_components() {
        assert_eq!(
            normalized_major_minor(Some("6.10.14-linuxkit")),
            Some("6.10".to_owned())
        );
        assert_eq!(
            normalized_major(Some("Docker version 27.4.1, build abc")),
            Some("27".to_owned())
        );
        assert_eq!(normalized_major_minor(None), None);
    }

    #[test]
    fn phase_compatibility_uses_the_complete_semantic_identity() {
        let reference = PhaseSummary {
            id: PhaseId::LayerstackSquash,
            label: "Squash".to_owned(),
            help: "Squash the selected layer stack.".to_owned(),
            semantic_revision: 1,
            unit: PhaseUnit::Nanoseconds,
            source: PhaseSource::ProductTrace,
            correlation: PhaseCorrelationRule::ExactRequestTraceSpan,
            trace_span_name: "layerstack.squash".to_owned(),
            attempted: 1,
            failed: 0,
            duration: statistics::summarize(&[10.0], 7).expect("phase statistics"),
        };
        let exact_match = reference.clone();
        assert_eq!(
            phase_identities(std::slice::from_ref(&reference)),
            phase_identities(std::slice::from_ref(&exact_match))
        );

        let mut different_span = exact_match.clone();
        different_span.trace_span_name = "layerstack.squash.commit".to_owned();
        assert_ne!(
            phase_identities(std::slice::from_ref(&reference)),
            phase_identities(std::slice::from_ref(&different_span))
        );

        let mut different_revision = exact_match;
        different_revision.semantic_revision = 2;
        assert_ne!(
            phase_identities(std::slice::from_ref(&reference)),
            phase_identities(std::slice::from_ref(&different_revision))
        );
    }
}
