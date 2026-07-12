mod support;

use std::path::Path;

use sandbox_benchmark::config::BenchmarkPaths;
use sandbox_benchmark::model::{
    ClientCohort, ConfigurationScope, ExpandedOperationCell, FactorRole, OperationPlan,
    TrialCountPlan,
};
use sandbox_benchmark::plan::{
    load_plan, load_presets, slice_default, validate_and_expand, MAX_EXPANDED_CELLS,
    MAX_FACTOR_VALUES,
};

use crate::support::{create_fake_repository, TestRoot};

#[test]
fn oversized_factor_is_rejected_without_cartesian_expansion() {
    let mut plan = scoped_default(ConfigurationScope::Command);
    let OperationPlan::ExecCommand(command) = &mut plan.operations[0] else {
        panic!("command scope contains exec_command")
    };
    command.factors.concurrent_requests.values =
        (1..=u32::try_from(MAX_FACTOR_VALUES + 1).expect("cap fits u32")).collect();
    command.factors.concurrent_requests.control = Some(1);

    let (_root, paths) = test_paths("factor-cap");
    let expanded = validate_and_expand(&plan, &paths, None).expect("validate bounded plan");

    assert!(!expanded.runnable);
    assert!(expanded.cells.is_empty());
    assert!(expanded
        .validation
        .iter()
        .any(|finding| finding.code == "factor_value_count_exceeded"));
}

#[test]
fn cohort_without_an_executor_is_rejected_instead_of_relabelled() {
    let mut plan = scoped_default(ConfigurationScope::Command);
    plan.environment.client_cohort = ClientCohort::CliE2e;
    let (_root, paths) = test_paths("unsupported-cohort");

    let expanded = validate_and_expand(&plan, &paths, None).expect("validate cohort");

    assert!(!expanded.runnable);
    assert_eq!(
        expanded.effective_environment.client_cohort,
        ClientCohort::CliE2e
    );
    assert!(expanded.validation.iter().any(|finding| {
        finding.code == "unsupported_client_cohort"
            && finding.path.as_deref() == Some("environment.client_cohort")
    }));
}

#[test]
fn malicious_cartesian_design_is_rejected_before_cells_are_allocated() {
    let mut plan = scoped_default(ConfigurationScope::Files);
    let edit = plan
        .operations
        .iter_mut()
        .find_map(|operation| match operation {
            OperationPlan::FileEdit(edit) => Some(edit),
            OperationPlan::ExecCommand(_)
            | OperationPlan::FileRead(_)
            | OperationPlan::FileWrite(_)
            | OperationPlan::FileBlame(_)
            | OperationPlan::CreateWorkspace(_)
            | OperationPlan::SquashLayerstack(_) => None,
        })
        .expect("files scope contains file_edit");
    edit.factors.concurrent_requests.values = (1..=22).collect();
    edit.factors.concurrent_requests.control = Some(1);
    edit.factors.file_bytes.values = (1_u64..=22).collect();
    edit.factors.file_bytes.control = Some(1);
    edit.factors.replacement_count.values = (1..=22).collect();
    edit.factors.replacement_count.control = Some(1);

    let (_root, paths) = test_paths("cell-cap");
    let first = validate_and_expand(&plan, &paths, None).expect("reject oversized design");
    let second = validate_and_expand(&plan, &paths, None).expect("repeat canonical validation");

    assert!(!first.runnable);
    assert!(first.cells.is_empty());
    assert!(first.estimates.cell_count > MAX_EXPANDED_CELLS);
    assert!(first
        .validation
        .iter()
        .any(|finding| finding.code == "expanded_cell_count_exceeded"));
    assert_eq!(first.plan_hash, second.plan_hash);
}

#[test]
fn aggregate_trial_and_issued_request_caps_are_independent() {
    let mut trial_heavy = scoped_default(ConfigurationScope::Files);
    trial_heavy.protocol.trial_defaults.fast = TrialCountPlan {
        warmups: 100,
        measured_trials: 1_000,
    };
    let edit = trial_heavy
        .operations
        .iter_mut()
        .find_map(|operation| match operation {
            OperationPlan::FileEdit(edit) => Some(edit),
            OperationPlan::ExecCommand(_)
            | OperationPlan::FileRead(_)
            | OperationPlan::FileWrite(_)
            | OperationPlan::FileBlame(_)
            | OperationPlan::CreateWorkspace(_)
            | OperationPlan::SquashLayerstack(_) => None,
        })
        .expect("files scope contains file_edit");
    edit.factors.concurrent_requests.values = (1..=10).collect();
    edit.factors.concurrent_requests.control = Some(1);
    edit.factors.file_bytes.values = (1_u64..=10).collect();
    edit.factors.file_bytes.control = Some(1);
    edit.factors.replacement_count.values = (1..=10).collect();
    edit.factors.replacement_count.control = Some(1);

    let (_trial_root, trial_paths) = test_paths("trial-cap");
    let expanded =
        validate_and_expand(&trial_heavy, &trial_paths, None).expect("validate trial-heavy plan");
    assert!(expanded
        .validation
        .iter()
        .any(|finding| finding.code == "trial_batch_count_exceeded"));
    assert!(!expanded
        .validation
        .iter()
        .any(|finding| finding.code == "expanded_cell_count_exceeded"));

    let mut request_heavy = scoped_default(ConfigurationScope::Command);
    let OperationPlan::ExecCommand(command) = &mut request_heavy.operations[0] else {
        panic!("command scope contains exec_command")
    };
    command.factors.concurrent_requests.role = FactorRole::Controlled;
    command.factors.concurrent_requests.values = vec![10_000_000];
    command.factors.concurrent_requests.control = None;
    let (_request_root, request_paths) = test_paths("request-cap");
    let expanded = validate_and_expand(&request_heavy, &request_paths, None)
        .expect("validate request-heavy plan");
    assert!(expanded
        .validation
        .iter()
        .any(|finding| finding.code == "issued_operation_request_count_exceeded"));
    assert!(!expanded
        .validation
        .iter()
        .any(|finding| finding.code == "trial_batch_count_exceeded"));
}

#[test]
fn layerstack_live_sessions_never_multiply_product_request_count() {
    let plan = scoped_default(ConfigurationScope::LayerStack);
    let (_root, paths) = test_paths("layerstack-counts");
    let expanded = validate_and_expand(&plan, &paths, Some(&plan)).expect("expand layerstack");

    assert!(expanded.runnable);
    assert_eq!(expanded.estimates.cell_count, 4);
    assert_eq!(expanded.estimates.trial_batch_count, 44);
    assert_eq!(expanded.estimates.issued_operation_request_count, 44);
    assert!(expanded
        .cells
        .iter()
        .all(|cell| cell.operation.measured_invocation_count() == 1));
}

#[test]
fn large_design_warning_names_issued_requests_without_calling_them_measured() {
    let mut plan = scoped_default(ConfigurationScope::Command);
    let OperationPlan::ExecCommand(command) = &mut plan.operations[0] else {
        panic!("command scope contains exec_command")
    };
    command.factors.concurrent_requests.role = FactorRole::Controlled;
    command.factors.concurrent_requests.values = vec![5_000];
    command.factors.concurrent_requests.control = None;

    let (_root, paths) = test_paths("request-warning");
    let expanded = validate_and_expand(&plan, &paths, None).expect("expand warning plan");
    let warning = expanded
        .estimates
        .warnings
        .iter()
        .find(|warning| warning.contains("product operation requests"))
        .expect("issued request warning");

    assert!(expanded.runnable);
    assert!(!warning.contains("measured product requests"));
    assert!(!warning.contains("including warmups"));
}

#[test]
fn gateway_restart_estimate_counts_only_layerstack_width_changes() {
    let mut plan = scoped_default(ConfigurationScope::All);
    let layerstack = plan
        .operations
        .iter_mut()
        .find_map(|operation| match operation {
            OperationPlan::SquashLayerstack(layerstack) => Some(layerstack),
            OperationPlan::ExecCommand(_)
            | OperationPlan::FileRead(_)
            | OperationPlan::FileWrite(_)
            | OperationPlan::FileEdit(_)
            | OperationPlan::FileBlame(_)
            | OperationPlan::CreateWorkspace(_) => None,
        })
        .expect("all scope contains squash_layerstack");
    layerstack.factors.remount_parallelism.role = FactorRole::Varied;
    layerstack.factors.remount_parallelism.values = vec![1, 4, 8];
    layerstack.factors.remount_parallelism.control = Some(4);

    let (_root, paths) = test_paths("restart-blocks");
    let expanded = validate_and_expand(&plan, &paths, None).expect("expand restart blocks");

    assert!(expanded.runnable);
    assert_eq!(expanded.execution_blocks.len(), 6);
    assert_eq!(expanded.estimates.gateway_restart_count, 2);
    assert!(expanded.execution_blocks[..4]
        .iter()
        .all(|block| block.restart_reason.is_none()));
    assert!(expanded.execution_blocks[4..]
        .iter()
        .all(|block| block.restart_reason.is_some()));
}

#[test]
fn quick_smoke_preserves_the_bounded_live_release_matrix() {
    let preset_directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../presets");
    let quick_smoke = load_presets(&preset_directory)
        .expect("load strict presets")
        .into_iter()
        .find(|preset| preset.id == "quick-smoke" && preset.version == 1)
        .expect("quick smoke v1");
    let (_root, paths) = test_paths("quick-smoke-matrix");
    let expanded = validate_and_expand(&quick_smoke.plan, &paths, None)
        .expect("expand the release-gate quick smoke plan");

    assert!(expanded.runnable, "{:?}", expanded.validation);
    let mut command = expanded
        .cells
        .iter()
        .filter_map(|cell| match &cell.operation {
            ExpandedOperationCell::ExecCommand(command) => Some(command.concurrent_requests),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut workspace = expanded
        .cells
        .iter()
        .filter_map(|cell| match &cell.operation {
            ExpandedOperationCell::CreateWorkspace(workspace) => Some(workspace.workspace_count),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut layerstack = expanded
        .cells
        .iter()
        .filter_map(|cell| match &cell.operation {
            ExpandedOperationCell::SquashLayerstack(layerstack) => Some(layerstack.live_sessions),
            _ => None,
        })
        .collect::<Vec<_>>();

    command.sort_unstable();
    workspace.sort_unstable();
    layerstack.sort_unstable();
    assert_eq!(command, [1, 5]);
    assert_eq!(workspace, [1, 5]);
    assert_eq!(layerstack, [0, 1]);
    let file_operations = expanded
        .cells
        .iter()
        .filter(|cell| cell.family_id == sandbox_benchmark::model::FamilyId::Files)
        .map(|cell| cell.operation_id)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        file_operations,
        [
            sandbox_benchmark::model::OperationId::FileRead,
            sandbox_benchmark::model::OperationId::FileWrite,
        ]
        .into_iter()
        .collect()
    );
    assert!(expanded
        .cells
        .iter()
        .all(|cell| { cell.protocol.warmups == 1 && cell.protocol.measured_trials == 5 }));
}

fn scoped_default(scope: ConfigurationScope) -> sandbox_benchmark::model::ExperimentPlan {
    let default_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../defaults/standard-local.yml");
    let default = load_plan(&default_path).expect("load standard default");
    slice_default(&default, scope)
}

fn test_paths(label: &str) -> (TestRoot, BenchmarkPaths) {
    let root = TestRoot::new(label);
    let repository = root.join("repository");
    create_fake_repository(&repository);
    let paths = BenchmarkPaths::initialize(&root.join("workspace"), &repository)
        .expect("initialize benchmark paths");
    (root, paths)
}
