use sandbox_runtime_mpla_poc::PREPARED_FIXTURE_PROFILE;
use sandbox_runtime_namespace_execution::CommandSecurityProfile;

pub(super) fn selected_command_security_profile(
    configured: CommandSecurityProfile,
    command: &str,
) -> CommandSecurityProfile {
    if configured == CommandSecurityProfile::MplaBenchmarkQualification
        && is_frozen_mpla_benchmark_command(command)
    {
        CommandSecurityProfile::MplaBenchmarkQualification
    } else {
        CommandSecurityProfile::Standard
    }
}

fn is_frozen_mpla_benchmark_command(command: &str) -> bool {
    const NORMAL_TOOL_ROOT: &str = "/eos/layer-stack/base/B000001-base/_campaign-tools";
    const STAGED_COORDINATOR_ROOT: &str = "/workspace/_campaign-tools";
    const AUTHORITY_ROOT: &str = "/eos/workspace/mpla-poc/authority/";
    const SPEED_ROOT: &str = "/eos/workspace/mpla-poc/speed/";
    const LEDGER: &str = "/eos/workspace/samples.jsonl";

    let fixture_builder_tool_root = format!(
        "/eos/mpla-fixtures/{PREPARED_FIXTURE_PROFILE}/layer-stack/base/B000001-base/_campaign-tools"
    );
    let fields = command.split_ascii_whitespace().collect::<Vec<_>>();
    if fields.iter().any(|field| {
        field.is_empty()
            || !field.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'-')
            })
    }) {
        return false;
    }
    let Some(tool_root) = fields.first().and_then(|program| {
        [
            NORMAL_TOOL_ROOT,
            STAGED_COORDINATOR_ROOT,
            fixture_builder_tool_root.as_str(),
        ]
        .into_iter()
        .find(|tool_root| *program == format!("{tool_root}/mpla-speed-poc-v1"))
    }) else {
        return false;
    };
    let is_normal_root = tool_root == NORMAL_TOOL_ROOT;
    let is_fixture_builder_root = tool_root == fixture_builder_tool_root;
    let oracle = format!("{tool_root}/mpla-poc-oracle");
    let exporter = format!("{tool_root}/sandbox-catalog-export");
    let catalog = format!("{tool_root}/product-catalog.json");

    match fields.as_slice() {
        [_, "authority-probe", "--probe-root", probe_root] => {
            is_normal_root && is_direct_safe_child(probe_root, AUTHORITY_ROOT)
        }
        [_, "prepare-publication-fixture", "--run-id", run_id, "--candidate-sandbox-id", candidate_sandbox_id, "--build-commit", build_commit, "--fixture-profile", fixture_profile]
            if *fixture_profile == PREPARED_FIXTURE_PROFILE =>
        {
            is_safe_identifier(run_id)
                && is_safe_identifier(candidate_sandbox_id)
                && is_git_commit(build_commit)
        }
        [_, "prepare-lifecycle-control", "--run-id", run_id, "--phase", phase, "--candidate-sandbox-id", candidate_sandbox_id, "--build-commit", build_commit]
            if matches!(*phase, "activation" | "fork" | "rollback") =>
        {
            !is_fixture_builder_root
                && is_safe_identifier(run_id)
                && is_safe_identifier(candidate_sandbox_id)
                && is_git_commit(build_commit)
        }
        [_, "build-publication-fixture-cache", "--candidate-sandbox-id", candidate_sandbox_id, "--build-commit", build_commit] => {
            is_fixture_builder_root
                && is_safe_identifier(candidate_sandbox_id)
                && is_git_commit(build_commit)
        }
        [_, "inspect-prepared-fixture-cache"] => is_normal_root,
        [_, "scorecard-case", "--run-id", run_id, "--case", case, "--candidate-sandbox-id", candidate_sandbox_id, "--build-commit", build_commit]
            if matches!(
                *case,
                "qualification"
                    | "activation"
                    | "fork"
                    | "rollback"
                    | "publication"
                    | "squash"
                    | "stream"
                    | "recovery"
            ) =>
        {
            is_safe_identifier(run_id)
                && is_safe_identifier(candidate_sandbox_id)
                && is_git_commit(build_commit)
        }
        [_, "measure", "--run-id", run_id, "--run-root", run_root, "--oracle", candidate_oracle, "--catalog-exporter", candidate_exporter, "--catalog", candidate_catalog, "--build-commit", build_commit, "--samples-ledger", LEDGER] => {
            is_normal_root
                && *candidate_oracle == oracle
                && *candidate_exporter == exporter
                && *candidate_catalog == catalog
                && is_safe_identifier(run_id)
                && *run_root == format!("{SPEED_ROOT}{run_id}")
                && is_git_commit(build_commit)
        }
        _ => false,
    }
}

fn is_direct_safe_child(path: &str, parent: &str) -> bool {
    path.strip_prefix(parent)
        .is_some_and(|name| is_safe_identifier(name) && !name.contains('/'))
}

fn is_safe_identifier(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn is_git_commit(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
