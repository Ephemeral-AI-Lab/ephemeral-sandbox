use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::config::BenchmarkPaths;
use crate::definitions::{
    definition, operation_comparison_key, OperationComparisonKey, DEFINITION_SCHEMA_VERSION,
};
use crate::executors::{
    command::CommandSessionMode, expand_operation_plan, files::MutationDestination,
    validate_operation_plan,
};
use crate::fixtures::{
    default_workspace_profile_directory, load_workspace_profiles, FixtureError,
    WorkspaceProfileCatalog, WorkspaceProfileEnvelope,
};
use crate::model::{
    CleanupPolicy, ClientCohort, ConfigurationScope, ExpandedOperationCell, ExperimentPlan, Factor,
    FamilyId, OperationId, OperationPlan, OperationValidationError, ResolvedIsolationPolicy,
    TrialCountPlan, WorkspaceProfileId,
};

pub const PLAN_SCHEMA_VERSION: u32 = 1;
pub const EXPANDED_PLAN_SCHEMA_VERSION: u32 = 1;
/// Bump when the canonical material used to author `plan_hash` changes without
/// changing the expanded-plan wire schema.
///
/// Free space is a sampled admission resource, not an execution identity: it
/// can move while a user is reading the review dialog. The sample remains in
/// the expanded plan for the free-space preflight and in the run manifest for
/// reproducibility, but is deliberately excluded from this revision's hash
/// material so an otherwise identical reviewed run can start.
const PLAN_HASH_REVISION: u32 = 2;
pub const LIFECYCLE_POLICY_REVISION: u32 = 1;
pub const FAILURE_POLICY_REVISION: u32 = 1;
pub const STABILIZATION_POLICY_REVISION: u32 = 1;
const MIN_RESOURCE_INTERVAL_MS: u64 = 20;
const MAX_RESOURCE_INTERVAL_MS: u64 = 1_000;
const MAX_PLAN_NAME_BYTES: usize = 128;
const MAX_CONFIGURATION_ID_BYTES: usize = 128;
const MAX_IMAGE_REFERENCE_BYTES: usize = 512;
const MAX_WARMUPS: u32 = 100;
const MAX_MEASURED_TRIALS: u32 = 1_000;
const MAX_TIMEOUT_MS: u64 = 3_600_000;
pub const MAX_FACTOR_VALUES: usize = 256;
pub const MAX_EXPANDED_CELLS: u64 = 10_000;
pub const MAX_TRIAL_BATCH_COUNT: u64 = 1_000_000;
pub const MAX_ISSUED_OPERATION_REQUEST_COUNT: u64 = 10_000_000;
const LARGE_DESIGN_CELL_THRESHOLD: u64 = 1_000;
const LARGE_DESIGN_TRIAL_BATCH_THRESHOLD: u64 = 10_000;
const LARGE_DESIGN_REQUEST_THRESHOLD: u64 = 100_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresetRef {
    pub id: String,
    pub version: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresetFile {
    pub schema_version: u32,
    pub id: String,
    pub version: u32,
    pub plan: ExperimentPlan,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanValidationRequest {
    pub plan: ExperimentPlan,
    pub starting_preset: Option<PresetRef>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunCreateRequest {
    pub plan: ExperimentPlan,
    pub plan_hash: String,
    pub client_request_id: String,
    pub starting_preset: Option<PresetRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingSeverity {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ValidationFinding {
    pub severity: FindingSeverity,
    pub code: String,
    pub message: String,
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectiveEnvironment {
    pub test_workspace_root: String,
    pub workspace_root_identity: String,
    pub client_cohort: ClientCohort,
    pub image_digest: Option<String>,
    pub filesystem: Option<String>,
    pub free_space_bytes: Option<u64>,
    pub gateway_mode: GatewayMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeEnvironmentSnapshot {
    pub image_digest: String,
    pub filesystem: Option<String>,
    pub free_space_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayMode {
    Isolated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixedLifecyclePolicy {
    pub lifecycle_revision: u32,
    pub failure_revision: u32,
    pub stabilization_revision: u32,
    pub automatic_retries: u32,
    pub one_active_campaign: bool,
    pub sequential_families: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectiveCellProtocol {
    pub destructive: bool,
    pub warmups: u32,
    pub measured_trials: u32,
    pub timeout_ms: u64,
    pub cleanup: CleanupPolicy,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpandedCell {
    pub cell_id: String,
    pub family_id: FamilyId,
    pub operation_id: OperationId,
    pub operation_semantic_revision: u32,
    pub factor_schema_revision: u32,
    pub protocol: EffectiveCellProtocol,
    pub comparison_key: OperationComparisonKey,
    pub operation: ExpandedOperationCell,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionBlock {
    pub block_id: String,
    pub family_id: FamilyId,
    pub cell_ids: Vec<String>,
    pub restart_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DurationRange {
    pub minimum_ns: u64,
    pub maximum_ns: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanEstimates {
    pub cell_count: u64,
    pub trial_batch_count: u64,
    pub issued_operation_request_count: u64,
    pub duration_range: DurationRange,
    pub estimated_peak_disk_bytes: Option<u64>,
    pub required_free_space_bytes: Option<u64>,
    pub gateway_restart_count: u64,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpandedPlan {
    pub schema_version: u32,
    pub runnable: bool,
    pub is_customized: bool,
    pub plan_hash: String,
    pub canonical_plan: ExperimentPlan,
    pub effective_environment: EffectiveEnvironment,
    pub fixed_lifecycle_policy: FixedLifecyclePolicy,
    pub selected_workspace_profiles: Vec<WorkspaceProfileEnvelope>,
    pub cells: Vec<ExpandedCell>,
    pub execution_blocks: Vec<ExecutionBlock>,
    pub estimates: PlanEstimates,
    pub validation: Vec<ValidationFinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct DefinitionRevision {
    operation_id: OperationId,
    semantic_revision: u32,
    factor_schema_revision: u32,
    comparison_projection_revision: u32,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct PlanHashMaterial<'a> {
    schema_version: u32,
    plan_hash_revision: u32,
    definition_schema_version: u32,
    canonical_plan: &'a ExperimentPlan,
    effective_environment: PlanHashEffectiveEnvironment<'a>,
    fixed_lifecycle_policy: FixedLifecyclePolicy,
    definition_revisions: &'a [DefinitionRevision],
    selected_workspace_profiles: &'a [WorkspaceProfileEnvelope],
    cells: &'a [ExpandedCell],
    execution_blocks: &'a [ExecutionBlock],
}

/// The effective-environment identity that participates in review-to-start
/// hash matching. `free_space_bytes` is intentionally not present: it is a
/// volatile resource observation that is independently checked during both
/// review and admission, rather than a machine-local setting or product
/// identity. Its exact value is still retained in `EffectiveEnvironment`.
#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct PlanHashEffectiveEnvironment<'a> {
    test_workspace_root: &'a str,
    workspace_root_identity: &'a str,
    client_cohort: ClientCohort,
    image_digest: Option<&'a str>,
    filesystem: Option<&'a str>,
    gateway_mode: GatewayMode,
}

impl<'a> From<&'a EffectiveEnvironment> for PlanHashEffectiveEnvironment<'a> {
    fn from(environment: &'a EffectiveEnvironment) -> Self {
        Self {
            test_workspace_root: &environment.test_workspace_root,
            workspace_root_identity: &environment.workspace_root_identity,
            client_cohort: environment.client_cohort,
            image_digest: environment.image_digest.as_deref(),
            filesystem: environment.filesystem.as_deref(),
            gateway_mode: environment.gateway_mode,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct DesignCounts {
    cell_count: u64,
    trial_batch_count: u64,
    issued_operation_request_count: u64,
    duration_maximum_ns: u64,
}

impl DesignCounts {
    fn add(&mut self, other: Self) {
        self.cell_count = self.cell_count.saturating_add(other.cell_count);
        self.trial_batch_count = self
            .trial_batch_count
            .saturating_add(other.trial_batch_count);
        self.issued_operation_request_count = self
            .issued_operation_request_count
            .saturating_add(other.issued_operation_request_count);
        self.duration_maximum_ns = self
            .duration_maximum_ns
            .saturating_add(other.duration_maximum_ns);
    }
}

#[derive(Debug, Default)]
struct ExpansionPreflight {
    counts: DesignCounts,
    findings: Vec<ValidationFinding>,
}

impl ExpansionPreflight {
    fn allows_expansion(&self) -> bool {
        !self
            .findings
            .iter()
            .any(|finding| finding.severity == FindingSeverity::Error)
    }
}

#[derive(Debug, Error)]
pub enum PlanError {
    #[error(transparent)]
    Config(#[from] sandbox_config::ConfigError),
    #[error(transparent)]
    Fixture(#[from] FixtureError),
    #[error("plan data at {path} failed validation: {message}")]
    InvalidData { path: PathBuf, message: String },
    #[error("plan serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("plan directory operation failed for {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

pub fn load_plan(path: &Path) -> Result<ExperimentPlan, PlanError> {
    sandbox_config::load_path(path)?
        .document()
        .map_err(Into::into)
}

pub fn load_presets(directory: &Path) -> Result<Vec<PresetFile>, PlanError> {
    let mut paths = fs::read_dir(directory)
        .map_err(|source| PlanError::Io {
            path: directory.to_path_buf(),
            source,
        })?
        .map(|entry| {
            entry
                .map(|value| value.path())
                .map_err(|source| PlanError::Io {
                    path: directory.to_path_buf(),
                    source,
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    paths.retain(|path| {
        matches!(
            path.extension().and_then(|value| value.to_str()),
            Some("yml" | "yaml")
        )
    });
    paths.sort();

    paths
        .into_iter()
        .map(|path| {
            let preset: PresetFile = sandbox_config::load_path(&path)?.document()?;
            if preset.schema_version != PLAN_SCHEMA_VERSION
                || preset.version == 0
                || preset.id.is_empty()
                || preset.id.len() > MAX_CONFIGURATION_ID_BYTES
            {
                return Err(PlanError::InvalidData {
                    path,
                    message: "preset envelope identity or version is invalid".to_owned(),
                });
            }
            Ok(preset)
        })
        .collect()
}

#[must_use]
pub fn slice_default(plan: &ExperimentPlan, scope: ConfigurationScope) -> ExperimentPlan {
    let mut sliced = plan.clone();
    sliced.configuration_base.scope = scope;
    sliced
        .operations
        .retain(|operation| scope_includes_operation(scope, definition(operation.id()).family));
    sliced
}

pub fn validate_and_expand(
    plan: &ExperimentPlan,
    paths: &BenchmarkPaths,
    declared_default: Option<&ExperimentPlan>,
) -> Result<ExpandedPlan, PlanError> {
    let workspace_profiles = load_workspace_profiles(&default_workspace_profile_directory())?;
    validate_and_expand_with_profiles(plan, paths, &workspace_profiles, declared_default)
}

pub fn validate_and_expand_with_profiles(
    plan: &ExperimentPlan,
    paths: &BenchmarkPaths,
    workspace_profiles: &WorkspaceProfileCatalog,
    declared_default: Option<&ExperimentPlan>,
) -> Result<ExpandedPlan, PlanError> {
    validate_and_expand_with_profiles_and_environment(
        plan,
        paths,
        workspace_profiles,
        declared_default,
        None,
    )
}

pub fn validate_and_expand_with_profiles_and_environment(
    plan: &ExperimentPlan,
    paths: &BenchmarkPaths,
    workspace_profiles: &WorkspaceProfileCatalog,
    declared_default: Option<&ExperimentPlan>,
    runtime_environment: Option<&RuntimeEnvironmentSnapshot>,
) -> Result<ExpandedPlan, PlanError> {
    workspace_profiles.validate()?;
    let mut canonical_plan = plan.clone();
    let mut validation = validate_common(&canonical_plan);
    validation.extend(validate_operation_set(&canonical_plan));
    validation.extend(validate_client_cohort(&canonical_plan));

    let operation_ids = canonical_plan
        .operations
        .iter()
        .map(OperationPlan::id)
        .collect::<Vec<_>>();
    if operation_ids.iter().copied().collect::<BTreeSet<_>>().len() == operation_ids.len() {
        canonical_plan.operations.sort_by_key(OperationPlan::id);
    }

    let preflight = expansion_preflight(&canonical_plan);
    let expansion_allowed = preflight.allows_expansion();
    validation.extend(preflight.findings.iter().cloned());
    let (profile_validation, selected_workspace_profiles) =
        validate_workspace_profiles(&canonical_plan, workspace_profiles);
    validation.extend(profile_validation);

    let mut typed_cells = Vec::new();
    if expansion_allowed {
        for operation in &canonical_plan.operations {
            validation.extend(
                validate_operation_plan(operation)
                    .into_iter()
                    .map(operation_finding),
            );
            match expand_operation_plan(operation) {
                Ok(cells) => typed_cells.extend(cells),
                Err(errors) => {
                    for error in errors {
                        let finding = operation_finding(error);
                        if !validation.iter().any(|existing| existing == &finding) {
                            validation.push(finding);
                        }
                    }
                }
            }
        }
    }

    let effective_environment = effective_environment(paths, &canonical_plan, runtime_environment);
    let fixed_lifecycle_policy = fixed_lifecycle_policy();
    let mut cells = typed_cells
        .into_iter()
        .map(|operation| {
            expanded_cell(
                operation,
                &canonical_plan,
                &effective_environment,
                workspace_profiles,
            )
        })
        .collect::<Result<Vec<_>, PlanError>>()?;
    let execution_blocks = order_cells(&mut cells, canonical_plan.seed);
    let estimates = estimates(
        &cells,
        &execution_blocks,
        &selected_workspace_profiles,
        (!expansion_allowed).then_some(preflight.counts),
    );
    if estimates.cell_count == 0
        && !validation
            .iter()
            .any(|finding| finding.severity == FindingSeverity::Error)
    {
        validation.push(error_finding(
            "no_enabled_operations",
            "At least one operation must be enabled.",
            Some("operations"),
        ));
    }
    for warning in &estimates.warnings {
        validation.push(warning_finding("large_design", warning, Some("operations")));
    }
    if let (Some(available), Some(required)) = (
        effective_environment.free_space_bytes,
        estimates.required_free_space_bytes,
    ) {
        if available < required {
            validation.push(error_finding(
                "insufficient_free_space",
                &format!(
                    "The benchmark workspace has {available} free bytes, but this plan requires at least {required} bytes."
                ),
                Some("environment"),
            ));
        }
    }

    let revisions = definition_revisions();
    let hash_material = PlanHashMaterial {
        schema_version: EXPANDED_PLAN_SCHEMA_VERSION,
        plan_hash_revision: PLAN_HASH_REVISION,
        definition_schema_version: DEFINITION_SCHEMA_VERSION,
        canonical_plan: &canonical_plan,
        effective_environment: PlanHashEffectiveEnvironment::from(&effective_environment),
        fixed_lifecycle_policy,
        definition_revisions: &revisions,
        selected_workspace_profiles: &selected_workspace_profiles,
        cells: &cells,
        execution_blocks: &execution_blocks,
    };
    let plan_hash = sha256_json(&hash_material)?;
    let runnable = !validation
        .iter()
        .any(|finding| finding.severity == FindingSeverity::Error);
    let is_customized = declared_default.is_none_or(|default| &canonical_plan != default);

    Ok(ExpandedPlan {
        schema_version: EXPANDED_PLAN_SCHEMA_VERSION,
        runnable,
        is_customized,
        plan_hash,
        canonical_plan,
        effective_environment,
        fixed_lifecycle_policy,
        selected_workspace_profiles,
        cells,
        execution_blocks,
        estimates,
        validation,
    })
}

fn validate_client_cohort(plan: &ExperimentPlan) -> Vec<ValidationFinding> {
    let unsupported = plan
        .operations
        .iter()
        .filter(|operation| operation.enabled())
        .map(OperationPlan::id)
        .filter(|operation| {
            !definition(*operation)
                .supported_cohorts
                .contains(&plan.environment.client_cohort)
        })
        .collect::<Vec<_>>();
    if unsupported.is_empty() {
        return Vec::new();
    }
    vec![error_finding(
        "unsupported_client_cohort",
        &format!(
            "Client cohort {:?} has no executable adapter for selected operation(s): {}.",
            plan.environment.client_cohort,
            unsupported
                .iter()
                .map(|operation| format!("{operation:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Some("environment.client_cohort"),
    )]
}

fn validate_common(plan: &ExperimentPlan) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();
    if plan.schema_version != PLAN_SCHEMA_VERSION {
        findings.push(error_finding(
            "unsupported_plan_schema",
            "Only experiment plan schema version 1 is supported.",
            Some("schema_version"),
        ));
    }
    validate_bounded_name(
        &mut findings,
        "invalid_plan_name",
        "name",
        &plan.name,
        MAX_PLAN_NAME_BYTES,
    );
    validate_bounded_name(
        &mut findings,
        "invalid_configuration_base",
        "configuration_base.id",
        &plan.configuration_base.id,
        MAX_CONFIGURATION_ID_BYTES,
    );
    if plan.configuration_base.version == 0 {
        findings.push(error_finding(
            "invalid_configuration_version",
            "Configuration base version must be positive.",
            Some("configuration_base.version"),
        ));
    }
    let image = &plan.environment.image.0;
    if image.is_empty()
        || image.len() > MAX_IMAGE_REFERENCE_BYTES
        || image.chars().any(char::is_whitespace)
    {
        findings.push(error_finding(
            "invalid_image_reference",
            "Image reference must be non-empty, bounded, and contain no whitespace.",
            Some("environment.image"),
        ));
    }
    if !(MIN_RESOURCE_INTERVAL_MS..=MAX_RESOURCE_INTERVAL_MS)
        .contains(&plan.protocol.resource_interval_ms)
    {
        findings.push(error_finding(
            "resource_interval_out_of_range",
            "Resource interval must be between 20 and 1000 ms.",
            Some("protocol.resource_interval_ms"),
        ));
    }
    for (path, counts) in [
        (
            "protocol.trial_defaults.fast",
            plan.protocol.trial_defaults.fast,
        ),
        (
            "protocol.trial_defaults.destructive",
            plan.protocol.trial_defaults.destructive,
        ),
    ] {
        if counts.warmups > MAX_WARMUPS
            || counts.measured_trials == 0
            || counts.measured_trials > MAX_MEASURED_TRIALS
        {
            findings.push(error_finding(
                "trial_count_out_of_range",
                "Warmups must be at most 100 and measured trials must be between 1 and 1000.",
                Some(path),
            ));
        }
    }
    for (path, timeout) in [
        (
            "protocol.timeout_ms.default",
            plan.protocol.timeout_ms.default,
        ),
        (
            "protocol.timeout_ms.squash_layerstack",
            plan.protocol.timeout_ms.squash_layerstack,
        ),
    ] {
        if timeout == 0 || timeout > MAX_TIMEOUT_MS {
            findings.push(error_finding(
                "timeout_out_of_range",
                "Timeout must be between 1 ms and 3600000 ms.",
                Some(path),
            ));
        }
    }
    if let Some(comparison) = &plan.comparison {
        validate_bounded_name(
            &mut findings,
            "invalid_comparison_protocol",
            "comparison.protocol_id",
            &comparison.protocol_id,
            MAX_CONFIGURATION_ID_BYTES,
        );
        if comparison.protocol_version == 0 {
            findings.push(error_finding(
                "invalid_comparison_protocol_version",
                "Comparison protocol version must be positive.",
                Some("comparison.protocol_version"),
            ));
        }
    }
    findings
}

fn expansion_preflight(plan: &ExperimentPlan) -> ExpansionPreflight {
    let mut findings = factor_value_limit_findings(plan);
    if !findings.is_empty() {
        return ExpansionPreflight {
            counts: DesignCounts::default(),
            findings,
        };
    }

    let counts = design_counts(plan);
    if counts.cell_count > MAX_EXPANDED_CELLS {
        findings.push(error_finding(
            "expanded_cell_count_exceeded",
            &format!(
                "The plan expands to {} test combinations; the fixed runner cap is {MAX_EXPANDED_CELLS}.",
                counts.cell_count
            ),
            Some("operations"),
        ));
    }
    if counts.trial_batch_count > MAX_TRIAL_BATCH_COUNT {
        findings.push(error_finding(
            "trial_batch_count_exceeded",
            &format!(
                "The plan schedules {} trial batches; the fixed runner cap is {MAX_TRIAL_BATCH_COUNT}.",
                counts.trial_batch_count
            ),
            Some("protocol.trial_defaults"),
        ));
    }
    if counts.issued_operation_request_count > MAX_ISSUED_OPERATION_REQUEST_COUNT {
        findings.push(error_finding(
            "issued_operation_request_count_exceeded",
            &format!(
                "The plan issues {} product operation requests; the fixed runner cap is {MAX_ISSUED_OPERATION_REQUEST_COUNT}.",
                counts.issued_operation_request_count
            ),
            Some("operations"),
        ));
    }

    ExpansionPreflight { counts, findings }
}

fn factor_value_limit_findings(plan: &ExperimentPlan) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();
    for (operation_index, operation) in plan.operations.iter().enumerate() {
        macro_rules! check {
            ($name:literal, $factor:expr) => {
                check_factor_value_count(
                    &mut findings,
                    operation_index,
                    $name,
                    $factor.values.len(),
                );
            };
        }
        match operation {
            OperationPlan::ExecCommand(plan) => {
                check!("concurrent_requests", plan.factors.concurrent_requests);
                check!("workspace_profile", plan.factors.workspace_profile);
                check!("session_mode", plan.factors.session_mode);
                check!("command_case", plan.factors.command_case);
            }
            OperationPlan::FileRead(plan) => {
                check!("concurrent_requests", plan.factors.concurrent_requests);
                check!("returned_bytes", plan.factors.returned_bytes);
                check!("source", plan.factors.source);
                check!("target_mode", plan.factors.target_mode);
            }
            OperationPlan::FileWrite(plan) => {
                check!("concurrent_requests", plan.factors.concurrent_requests);
                check!("content_bytes", plan.factors.content_bytes);
                check!("destination", plan.factors.destination);
                check!("target_mode", plan.factors.target_mode);
            }
            OperationPlan::FileEdit(plan) => {
                check!("concurrent_requests", plan.factors.concurrent_requests);
                check!("file_bytes", plan.factors.file_bytes);
                check!("replacement_count", plan.factors.replacement_count);
                check!("match_density", plan.factors.match_density);
                check!("destination", plan.factors.destination);
                check!("target_mode", plan.factors.target_mode);
            }
            OperationPlan::FileBlame(plan) => {
                check!("concurrent_requests", plan.factors.concurrent_requests);
                check!("line_count", plan.factors.line_count);
                check!("ownership_segments", plan.factors.ownership_segments);
                check!(
                    "auditability_event_count",
                    plan.factors.auditability_event_count
                );
            }
            OperationPlan::CreateWorkspace(plan) => {
                check!("workspace_count", plan.factors.workspace_count);
                check!("workspace_profile", plan.factors.workspace_profile);
                check!("network_profile", plan.factors.network_profile);
            }
            OperationPlan::SquashLayerstack(plan) => {
                check!("live_sessions", plan.factors.live_sessions);
                check!(
                    "requested_migration_ratio",
                    plan.factors.requested_migration_ratio
                );
                check!("remount_parallelism", plan.factors.remount_parallelism);
                check!("squashable_blocks", plan.factors.squashable_blocks);
                check!("layers_per_block", plan.factors.layers_per_block);
                check!("payload_bytes", plan.factors.payload_bytes);
                check!("session_activity", plan.factors.session_activity);
            }
        }
    }
    findings
}

fn check_factor_value_count(
    findings: &mut Vec<ValidationFinding>,
    operation_index: usize,
    factor: &str,
    value_count: usize,
) {
    if value_count <= MAX_FACTOR_VALUES {
        return;
    }
    let path = format!("operations.{operation_index}.configuration.factors.{factor}.values");
    findings.push(error_finding(
        "factor_value_count_exceeded",
        &format!(
            "Factor {factor} has {value_count} values; the fixed runner cap is {MAX_FACTOR_VALUES}."
        ),
        Some(&path),
    ));
}

fn design_counts(plan: &ExperimentPlan) -> DesignCounts {
    let fast_batches = trial_batches(plan.protocol.trial_defaults.fast);
    let destructive_batches = trial_batches(plan.protocol.trial_defaults.destructive);
    let mut total = DesignCounts::default();
    for operation in &plan.operations {
        total.add(operation_design_counts(
            operation,
            fast_batches,
            destructive_batches,
            plan.protocol.timeout_ms.default,
            plan.protocol.timeout_ms.squash_layerstack,
        ));
    }
    total
}

fn operation_design_counts(
    operation: &OperationPlan,
    fast_batches: u64,
    destructive_batches: u64,
    default_timeout_ms: u64,
    squash_timeout_ms: u64,
) -> DesignCounts {
    if !operation.enabled() {
        return DesignCounts::default();
    }

    let (cell_count, trial_batch_count, issued_operation_request_count, timeout_ms) =
        match operation {
            OperationPlan::ExecCommand(plan) => {
                let factors = &plan.factors;
                let other_count = product(&[
                    factor_len(&factors.concurrent_requests),
                    factor_len(&factors.workspace_profile),
                    factor_len(&factors.command_case),
                ]);
                let session_batches =
                    factors.session_mode.values.iter().fold(0_u64, |sum, mode| {
                        sum.saturating_add(match mode {
                            CommandSessionMode::Explicit => fast_batches,
                            CommandSessionMode::Automatic => destructive_batches,
                        })
                    });
                (
                    other_count.saturating_mul(factor_len(&factors.session_mode)),
                    other_count.saturating_mul(session_batches),
                    product(&[
                        sum_u32(&factors.concurrent_requests.values),
                        factor_len(&factors.workspace_profile),
                        factor_len(&factors.command_case),
                        session_batches,
                    ]),
                    default_timeout_ms,
                )
            }
            OperationPlan::FileRead(plan) => {
                let factors = &plan.factors;
                let cell_count = product(&[
                    factor_len(&factors.concurrent_requests),
                    factor_len(&factors.returned_bytes),
                    factor_len(&factors.source),
                    factor_len(&factors.target_mode),
                ]);
                (
                    cell_count,
                    cell_count.saturating_mul(fast_batches),
                    product(&[
                        sum_u32(&factors.concurrent_requests.values),
                        factor_len(&factors.returned_bytes),
                        factor_len(&factors.source),
                        factor_len(&factors.target_mode),
                        fast_batches,
                    ]),
                    default_timeout_ms,
                )
            }
            OperationPlan::FileWrite(plan) => {
                let factors = &plan.factors;
                let other_count = product(&[
                    factor_len(&factors.concurrent_requests),
                    factor_len(&factors.content_bytes),
                    factor_len(&factors.target_mode),
                ]);
                let destination_batches = destination_batches(
                    &factors.destination.values,
                    fast_batches,
                    destructive_batches,
                );
                (
                    other_count.saturating_mul(factor_len(&factors.destination)),
                    other_count.saturating_mul(destination_batches),
                    product(&[
                        sum_u32(&factors.concurrent_requests.values),
                        factor_len(&factors.content_bytes),
                        factor_len(&factors.target_mode),
                        destination_batches,
                    ]),
                    default_timeout_ms,
                )
            }
            OperationPlan::FileEdit(plan) => {
                let factors = &plan.factors;
                let other_count = product(&[
                    factor_len(&factors.concurrent_requests),
                    factor_len(&factors.file_bytes),
                    factor_len(&factors.replacement_count),
                    factor_len(&factors.match_density),
                    factor_len(&factors.target_mode),
                ]);
                let destination_batches = destination_batches(
                    &factors.destination.values,
                    fast_batches,
                    destructive_batches,
                );
                (
                    other_count.saturating_mul(factor_len(&factors.destination)),
                    other_count.saturating_mul(destination_batches),
                    product(&[
                        sum_u32(&factors.concurrent_requests.values),
                        factor_len(&factors.file_bytes),
                        factor_len(&factors.replacement_count),
                        factor_len(&factors.match_density),
                        factor_len(&factors.target_mode),
                        destination_batches,
                    ]),
                    default_timeout_ms,
                )
            }
            OperationPlan::FileBlame(plan) => {
                let factors = &plan.factors;
                let cell_count = product(&[
                    factor_len(&factors.concurrent_requests),
                    factor_len(&factors.line_count),
                    factor_len(&factors.ownership_segments),
                    factor_len(&factors.auditability_event_count),
                ]);
                (
                    cell_count,
                    cell_count.saturating_mul(fast_batches),
                    product(&[
                        sum_u32(&factors.concurrent_requests.values),
                        factor_len(&factors.line_count),
                        factor_len(&factors.ownership_segments),
                        factor_len(&factors.auditability_event_count),
                        fast_batches,
                    ]),
                    default_timeout_ms,
                )
            }
            OperationPlan::CreateWorkspace(plan) => {
                let factors = &plan.factors;
                let cell_count = product(&[
                    factor_len(&factors.workspace_count),
                    factor_len(&factors.workspace_profile),
                    factor_len(&factors.network_profile),
                ]);
                (
                    cell_count,
                    cell_count.saturating_mul(destructive_batches),
                    product(&[
                        sum_u32(&factors.workspace_count.values),
                        factor_len(&factors.workspace_profile),
                        factor_len(&factors.network_profile),
                        destructive_batches,
                    ]),
                    default_timeout_ms,
                )
            }
            OperationPlan::SquashLayerstack(plan) => {
                let factors = &plan.factors;
                let cell_count = product(&[
                    factor_len(&factors.live_sessions),
                    factor_len(&factors.requested_migration_ratio),
                    factor_len(&factors.remount_parallelism),
                    factor_len(&factors.squashable_blocks),
                    factor_len(&factors.layers_per_block),
                    factor_len(&factors.payload_bytes),
                    factor_len(&factors.session_activity),
                ]);
                let trial_batch_count = cell_count.saturating_mul(destructive_batches);
                (
                    cell_count,
                    trial_batch_count,
                    trial_batch_count,
                    squash_timeout_ms,
                )
            }
        };

    DesignCounts {
        cell_count,
        trial_batch_count,
        issued_operation_request_count,
        duration_maximum_ns: trial_batch_count
            .saturating_mul(timeout_ms)
            .saturating_mul(1_000_000),
    }
}

fn destination_batches(
    destinations: &[MutationDestination],
    fast_batches: u64,
    destructive_batches: u64,
) -> u64 {
    destinations.iter().fold(0_u64, |sum, destination| {
        sum.saturating_add(match destination {
            MutationDestination::Session => fast_batches,
            MutationDestination::Publish => destructive_batches,
        })
    })
}

fn trial_batches(counts: TrialCountPlan) -> u64 {
    u64::from(counts.warmups).saturating_add(u64::from(counts.measured_trials))
}

fn factor_len<T>(factor: &Factor<T>) -> u64 {
    u64::try_from(factor.values.len()).unwrap_or(u64::MAX)
}

fn sum_u32(values: &[u32]) -> u64 {
    values
        .iter()
        .fold(0_u64, |sum, value| sum.saturating_add(u64::from(*value)))
}

fn product(values: &[u64]) -> u64 {
    values
        .iter()
        .fold(1_u64, |product, value| product.saturating_mul(*value))
}

fn validate_operation_set(plan: &ExperimentPlan) -> Vec<ValidationFinding> {
    let expected = expected_operations(plan.configuration_base.scope);
    let mut seen = BTreeSet::new();
    let mut findings = Vec::new();
    for operation in &plan.operations {
        if !seen.insert(operation.id()) {
            findings.push(error_finding(
                "duplicate_operation",
                &format!("Operation {:?} appears more than once.", operation.id()),
                Some("operations"),
            ));
        }
        if !expected.contains(&operation.id()) {
            findings.push(error_finding(
                "operation_outside_scope",
                &format!(
                    "Operation {:?} is not part of configuration scope {:?}.",
                    operation.id(),
                    plan.configuration_base.scope
                ),
                Some("operations"),
            ));
        }
    }
    for operation in expected.difference(&seen) {
        findings.push(error_finding(
            "missing_operation",
            &format!("Operation {operation:?} is required by this complete scoped plan."),
            Some("operations"),
        ));
    }
    findings
}

fn validate_workspace_profiles(
    plan: &ExperimentPlan,
    catalog: &WorkspaceProfileCatalog,
) -> (Vec<ValidationFinding>, Vec<WorkspaceProfileEnvelope>) {
    let mut findings = Vec::new();
    let mut selected = BTreeSet::new();
    let mut warned = BTreeSet::new();
    for (operation_index, operation) in plan.operations.iter().enumerate() {
        let Some(factor) = operation_workspace_profile_factor(operation) else {
            continue;
        };
        for (value_index, profile_id) in factor.values.iter().enumerate() {
            let path = format!(
                "operations.{operation_index}.configuration.factors.workspace_profile.values.{value_index}"
            );
            let Some(profile) = catalog.get(profile_id) else {
                findings.push(error_finding(
                    "unknown_workspace_profile",
                    &format!(
                        "Workspace profile {profile_id} is not present in the versioned profile catalog."
                    ),
                    Some(&path),
                ));
                continue;
            };
            if operation.enabled() {
                selected.insert(profile_id.clone());
                if !profile.standard && warned.insert(profile_id.clone()) {
                    findings.push(warning_finding(
                        "opt_in_workspace_profile",
                        &format!(
                            "Workspace profile {} ({}) is opt-in; review its disk and duration cost before starting.",
                            profile.label, profile.id
                        ),
                        Some(&path),
                    ));
                }
            }
        }
    }
    let selected = selected
        .into_iter()
        .filter_map(|id| catalog.get(&id).cloned())
        .collect();
    (findings, selected)
}

fn operation_workspace_profile_factor(
    operation: &OperationPlan,
) -> Option<&Factor<WorkspaceProfileId>> {
    match operation {
        OperationPlan::ExecCommand(plan) => Some(&plan.factors.workspace_profile),
        OperationPlan::CreateWorkspace(plan) => Some(&plan.factors.workspace_profile),
        OperationPlan::FileRead(_)
        | OperationPlan::FileWrite(_)
        | OperationPlan::FileEdit(_)
        | OperationPlan::FileBlame(_)
        | OperationPlan::SquashLayerstack(_) => None,
    }
}

fn expanded_cell(
    operation: ExpandedOperationCell,
    plan: &ExperimentPlan,
    environment: &EffectiveEnvironment,
    workspace_profiles: &WorkspaceProfileCatalog,
) -> Result<ExpandedCell, PlanError> {
    let operation_id = operation.id();
    let operation_definition = definition(operation_id);
    let destructive = is_destructive(operation_id, operation.resolved_isolation());
    let counts = if destructive {
        plan.protocol.trial_defaults.destructive
    } else {
        plan.protocol.trial_defaults.fast
    };
    let timeout_ms = if operation_id == OperationId::SquashLayerstack {
        plan.protocol.timeout_ms.squash_layerstack
    } else {
        plan.protocol.timeout_ms.default
    };
    let protocol = EffectiveCellProtocol {
        destructive,
        warmups: counts.warmups,
        measured_trials: counts.measured_trials,
        timeout_ms,
        cleanup: operation_definition.cleanup,
    };
    let workspace_profile = operation_workspace_profile_id(&operation)
        .and_then(|profile_id| workspace_profiles.get(profile_id));
    let cell_id = sha256_json(&(
        operation_definition.family,
        operation_id,
        operation_definition.semantic_revision,
        operation_definition.factor_schema_revision,
        environment.workspace_root_identity.as_str(),
        plan.environment.client_cohort,
        &protocol,
        workspace_profile,
        &operation,
    ))?;
    let comparison_key = operation_comparison_key(&operation);
    Ok(ExpandedCell {
        cell_id,
        family_id: operation_definition.family,
        operation_id,
        operation_semantic_revision: operation_definition.semantic_revision,
        factor_schema_revision: operation_definition.factor_schema_revision,
        protocol,
        comparison_key,
        operation,
    })
}

fn operation_workspace_profile_id(
    operation: &ExpandedOperationCell,
) -> Option<&WorkspaceProfileId> {
    match operation {
        ExpandedOperationCell::ExecCommand(cell) => Some(&cell.workspace_profile),
        ExpandedOperationCell::CreateWorkspace(cell) => Some(&cell.workspace_profile),
        ExpandedOperationCell::FileRead(_)
        | ExpandedOperationCell::FileWrite(_)
        | ExpandedOperationCell::FileEdit(_)
        | ExpandedOperationCell::FileBlame(_)
        | ExpandedOperationCell::SquashLayerstack(_) => None,
    }
}

fn order_cells(cells: &mut Vec<ExpandedCell>, seed: u64) -> Vec<ExecutionBlock> {
    let mut by_family = BTreeMap::<FamilyId, Vec<ExpandedCell>>::new();
    for cell in cells.drain(..) {
        by_family.entry(cell.family_id).or_default().push(cell);
    }
    let mut ordered = Vec::new();
    let mut blocks = Vec::new();
    for family in FamilyId::ALL {
        let Some(family_cells) = by_family.remove(&family) else {
            continue;
        };
        if family == FamilyId::LayerStack {
            let mut by_remount_parallelism = BTreeMap::<u32, Vec<ExpandedCell>>::new();
            for cell in family_cells {
                let ExpandedOperationCell::SquashLayerstack(layerstack) = &cell.operation else {
                    unreachable!("family and typed operation are definition-derived")
                };
                by_remount_parallelism
                    .entry(layerstack.remount_parallelism)
                    .or_default()
                    .push(cell);
            }
            for (index, (width, mut block_cells)) in by_remount_parallelism.into_iter().enumerate()
            {
                deterministic_shuffle(
                    &mut block_cells,
                    seed ^ family_seed(family) ^ u64::from(width),
                );
                let restart_reason =
                    (index > 0).then(|| format!("layerstack_remount_parallelism_changed:{width}"));
                append_execution_block(
                    &mut ordered,
                    &mut blocks,
                    family,
                    block_cells,
                    restart_reason,
                );
            }
        } else {
            let mut block_cells = family_cells;
            deterministic_shuffle(&mut block_cells, seed ^ family_seed(family));
            append_execution_block(&mut ordered, &mut blocks, family, block_cells, None);
        }
    }
    *cells = ordered;
    blocks
}

fn append_execution_block(
    ordered: &mut Vec<ExpandedCell>,
    blocks: &mut Vec<ExecutionBlock>,
    family: FamilyId,
    block_cells: Vec<ExpandedCell>,
    restart_reason: Option<String>,
) {
    let cell_ids = block_cells
        .iter()
        .map(|cell| cell.cell_id.clone())
        .collect::<Vec<_>>();
    let block_id = sha256_bytes(format!("v1:{family:?}:{}", cell_ids.join(":")).as_bytes());
    blocks.push(ExecutionBlock {
        block_id,
        family_id: family,
        cell_ids,
        restart_reason,
    });
    ordered.extend(block_cells);
}

fn estimates(
    cells: &[ExpandedCell],
    blocks: &[ExecutionBlock],
    selected_workspace_profiles: &[WorkspaceProfileEnvelope],
    preflight_counts: Option<DesignCounts>,
) -> PlanEstimates {
    let counts = preflight_counts.unwrap_or_else(|| expanded_design_counts(cells));
    let mut warnings = Vec::new();
    if counts.cell_count > LARGE_DESIGN_CELL_THRESHOLD {
        warnings.push(format!(
            "The plan expands to {} test combinations; review disk and duration before starting.",
            counts.cell_count
        ));
    }
    if counts.trial_batch_count > LARGE_DESIGN_TRIAL_BATCH_THRESHOLD {
        warnings.push(format!(
            "The plan schedules {} trial batches across warmup and measured trial kinds.",
            counts.trial_batch_count
        ));
    }
    if counts.issued_operation_request_count > LARGE_DESIGN_REQUEST_THRESHOLD {
        warnings.push(format!(
            "The plan issues {} product operation requests across those trial batches.",
            counts.issued_operation_request_count
        ));
    }
    let estimated_peak_disk_bytes = selected_workspace_profiles
        .iter()
        .map(|profile| profile.fixture.logical_bytes)
        .reduce(u64::saturating_add);
    let required_free_space_bytes = estimated_peak_disk_bytes.map(|bytes| bytes.saturating_mul(2));
    PlanEstimates {
        cell_count: counts.cell_count,
        trial_batch_count: counts.trial_batch_count,
        issued_operation_request_count: counts.issued_operation_request_count,
        duration_range: DurationRange {
            minimum_ns: 0,
            maximum_ns: counts.duration_maximum_ns,
        },
        estimated_peak_disk_bytes,
        required_free_space_bytes,
        gateway_restart_count: u64::try_from(
            blocks
                .iter()
                .filter(|block| block.restart_reason.is_some())
                .count(),
        )
        .unwrap_or(u64::MAX),
        warnings,
    }
}

fn expanded_design_counts(cells: &[ExpandedCell]) -> DesignCounts {
    let mut counts = DesignCounts {
        cell_count: u64::try_from(cells.len()).unwrap_or(u64::MAX),
        ..DesignCounts::default()
    };
    for cell in cells {
        let batches = u64::from(cell.protocol.warmups)
            .saturating_add(u64::from(cell.protocol.measured_trials));
        counts.trial_batch_count = counts.trial_batch_count.saturating_add(batches);
        counts.issued_operation_request_count =
            counts.issued_operation_request_count.saturating_add(
                batches.saturating_mul(u64::from(cell.operation.measured_invocation_count())),
            );
        counts.duration_maximum_ns = counts.duration_maximum_ns.saturating_add(
            batches
                .saturating_mul(cell.protocol.timeout_ms)
                .saturating_mul(1_000_000),
        );
    }
    counts
}

pub(crate) fn effective_environment(
    paths: &BenchmarkPaths,
    plan: &ExperimentPlan,
    runtime: Option<&RuntimeEnvironmentSnapshot>,
) -> EffectiveEnvironment {
    let canonical = paths.root.display().to_string();
    EffectiveEnvironment {
        workspace_root_identity: sha256_bytes(canonical.as_bytes()),
        test_workspace_root: canonical,
        client_cohort: plan.environment.client_cohort,
        image_digest: runtime.map(|runtime| runtime.image_digest.clone()),
        filesystem: runtime.and_then(|runtime| runtime.filesystem.clone()),
        free_space_bytes: runtime.and_then(|runtime| runtime.free_space_bytes),
        gateway_mode: GatewayMode::Isolated,
    }
}

const fn fixed_lifecycle_policy() -> FixedLifecyclePolicy {
    FixedLifecyclePolicy {
        lifecycle_revision: LIFECYCLE_POLICY_REVISION,
        failure_revision: FAILURE_POLICY_REVISION,
        stabilization_revision: STABILIZATION_POLICY_REVISION,
        automatic_retries: 0,
        one_active_campaign: true,
        sequential_families: true,
    }
}

fn definition_revisions() -> Vec<DefinitionRevision> {
    OperationId::ALL
        .into_iter()
        .map(|operation_id| {
            let operation = definition(operation_id);
            DefinitionRevision {
                operation_id,
                semantic_revision: operation.semantic_revision,
                factor_schema_revision: operation.factor_schema_revision,
                comparison_projection_revision: operation.comparison.semantic_revision,
            }
        })
        .collect()
}

const fn is_destructive(operation: OperationId, isolation: ResolvedIsolationPolicy) -> bool {
    match operation {
        OperationId::CreateWorkspace | OperationId::SquashLayerstack => true,
        OperationId::ExecCommand
        | OperationId::FileRead
        | OperationId::FileWrite
        | OperationId::FileEdit
        | OperationId::FileBlame => matches!(
            isolation,
            ResolvedIsolationPolicy::FreshSandboxPerTrial
                | ResolvedIsolationPolicy::FreshTopologyPerTrial
        ),
    }
}

fn expected_operations(scope: ConfigurationScope) -> BTreeSet<OperationId> {
    OperationId::ALL
        .into_iter()
        .filter(|operation| scope_includes_operation(scope, definition(*operation).family))
        .collect()
}

const fn scope_includes_operation(scope: ConfigurationScope, family: FamilyId) -> bool {
    match scope {
        ConfigurationScope::All => true,
        ConfigurationScope::Command => matches!(family, FamilyId::Command),
        ConfigurationScope::Files => matches!(family, FamilyId::Files),
        ConfigurationScope::Workspace => matches!(family, FamilyId::WorkspaceLifecycle),
        ConfigurationScope::LayerStack => matches!(family, FamilyId::LayerStack),
    }
}

fn operation_finding(error: OperationValidationError) -> ValidationFinding {
    error_finding(
        "invalid_operation_factor",
        &format!(
            "Operation {:?} factor {:?} violates {:?}.",
            error.operation, error.factor, error.violation
        ),
        Some("operations"),
    )
}

fn validate_bounded_name(
    findings: &mut Vec<ValidationFinding>,
    code: &str,
    path: &'static str,
    value: &str,
    maximum: usize,
) {
    if value.is_empty() || value.len() > maximum {
        findings.push(error_finding(
            code,
            &format!("{path} must contain between 1 and {maximum} bytes."),
            Some(path),
        ));
    }
}

fn error_finding(code: &str, message: &str, path: Option<&str>) -> ValidationFinding {
    finding(FindingSeverity::Error, code, message, path)
}

fn warning_finding(code: &str, message: &str, path: Option<&str>) -> ValidationFinding {
    finding(FindingSeverity::Warning, code, message, path)
}

fn finding(
    severity: FindingSeverity,
    code: &str,
    message: &str,
    path: Option<&str>,
) -> ValidationFinding {
    ValidationFinding {
        severity,
        code: code.to_owned(),
        message: message.to_owned(),
        path: path.map(str::to_owned),
    }
}

fn sha256_json<T: Serialize>(value: &T) -> Result<String, PlanError> {
    Ok(sha256_bytes(&serde_json::to_vec(value)?))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn deterministic_shuffle<T>(values: &mut [T], seed: u64) {
    let mut state = seed;
    for index in (1..values.len()).rev() {
        state = splitmix64(state);
        let upper = u64::try_from(index + 1).unwrap_or(u64::MAX);
        let selected = usize::try_from(state % upper).unwrap_or(0);
        values.swap(index, selected);
    }
}

const fn splitmix64(mut state: u64) -> u64 {
    state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut value = state;
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

const fn family_seed(family: FamilyId) -> u64 {
    match family {
        FamilyId::Command => 0x434f_4d4d_414e_4401,
        FamilyId::Files => 0x4649_4c45_5300_0002,
        FamilyId::WorkspaceLifecycle => 0x574f_524b_5350_4303,
        FamilyId::LayerStack => 0x4c41_5945_5253_5404,
    }
}
