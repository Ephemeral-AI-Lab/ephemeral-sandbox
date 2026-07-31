use std::error::Error;
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

use sandbox_runtime_mpla_poc::{
    STORAGE_ADMIN_OVERLAYFS_DAC_OVERRIDE_QUALIFICATION_PROFILE_ID, STORAGE_ADMIN_PROFILE_ID,
};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::mpla_speed_scorecard::{
    approved_storage_profile, cleanup_mounted_workspace, require_command_exit,
    require_regular_file, required_string, sync_directory, validate_build_commit,
    validate_identifier, CliFailure, CliInvocation, RuntimeClient,
};

type QualificationResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

const RUNTIME_CLI: &str = "/eos/layer-stack/base/B000001-base/_campaign-tools/sandbox-runtime-cli";
const TOKEN_FILE: &str = "/eos/layer-stack/base/B000001-base/_campaign-tools/gateway.token";
const TRUSTED_HELPER: &str = "/usr/local/libexec/ephemeral-sandbox/mpla-storage-admin-v1";
const SENTINEL: &str = "main-sentinel";
const FORK_SENTINEL: &str = "fork-only";
// Restart activation leaves the workspace mounted. Quiesce is the only
// phase-legal public storage action, so negative binding probes reach the
// profile/namespace validation without changing lifecycle state.
const MOUNTED_NEGATIVE_PROBE_ACTION: &str = "quiesce";

#[derive(Debug, Serialize)]
struct QualificationEvidence {
    schema_version: u32,
    kind: String,
    run_id: String,
    candidate_sandbox_id: String,
    build_commit: String,
    authority: Value,
    backing: Value,
    cgroup: Value,
    resources: Value,
    resource_bounds: bool,
    initial_create: CliInvocation,
    initial_mount: CliInvocation,
    initial_helper_receipt_valid: bool,
    initial_main_sentinel_write: CliInvocation,
    initial_publish: CliInvocation,
    fork: CliInvocation,
    fork_activation: CliInvocation,
    fork_sentinel_write: CliInvocation,
    fork_publish: CliInvocation,
    main_activation_before_rollback: CliInvocation,
    fork_isolation_check: CliInvocation,
    rollback: CliInvocation,
    rollback_content_check: CliInvocation,
    session_restart_activation: CliInvocation,
    session_restart_content_check: CliInvocation,
    wrong_namespace_rejection: CliFailure,
    wrong_namespace_rejected: bool,
    wrong_profile_rejection: CliFailure,
    wrong_profile_rejected: bool,
    restart_storage_cleanup: Vec<CliInvocation>,
    restart_destroy: CliInvocation,
    ordinary_workload_probe: CliInvocation,
    ordinary_workload_denied_privilege: bool,
    exact_content_mode_ownership: bool,
    fork_isolation: bool,
    rollback_correctness: bool,
    session_restart_durability: bool,
}

pub fn run(
    run_id: &str,
    candidate_sandbox_id: &str,
    build_commit: &str,
) -> QualificationResult<Value> {
    validate_identifier(run_id, "run_id")?;
    validate_identifier(candidate_sandbox_id, "candidate_sandbox_id")?;
    validate_build_commit(build_commit)?;
    require_regular_file(Path::new(RUNTIME_CLI), "runtime CLI")?;
    require_regular_file(Path::new(TOKEN_FILE), "gateway token")?;

    let run_root =
        Path::new("/eos/workspace/mpla-poc/scorecard").join(format!("{run_id}-qualification"));
    fs::create_dir_all(
        run_root
            .parent()
            .ok_or("qualification run root lacks a parent")?,
    )?;
    fs::create_dir(&run_root)?;
    let result_path = Path::new("/workspace/scorecard-qualification-result.json");
    if result_path.exists() {
        return Err(format!(
            "qualification scorecard result already exists: {}",
            result_path.display()
        )
        .into());
    }

    let authority = super::capability_receipt()?;
    let backing = super::persistent_backing(&run_root)?;
    let cgroup_dir = super::current_cgroup_v2_dir()?;
    let cgroup = json!({
        "path": cgroup_dir,
        "memory_high": super::read_limit(&cgroup_dir.join("memory.high"))?,
        "memory_max": super::read_limit(&cgroup_dir.join("memory.max"))?,
        "membership_proven": super::cgroup_contains_self(&cgroup_dir)?,
    });
    let monitor = super::ResourceMonitor::start(&cgroup_dir, &run_root)?;
    let client = RuntimeClient::new(candidate_sandbox_id)?;

    let initial_create = client.invoke(
        Some(&format!("{run_id}-qualification-create")),
        "create_mpla_workspace_session",
        &["--run-id".to_owned(), run_id.to_owned()],
    )?;
    let initial_workspace_session_id = required_string(
        &initial_create.response,
        "workspace_session_id",
        "qualification initial create",
    )?;
    let initial_profile =
        require_profile(&initial_create.response, "qualification initial create")?;
    let initial_scope = initial_create
        .response
        .get("storage_admin_scope")
        .ok_or("qualification initial create omitted storage_admin_scope")?;
    let initial_mount_operation = format!("{run_id}-qualification-mount");
    let initial_mount = storage_admin(
        &client,
        &initial_mount_operation,
        "mount",
        &initial_profile,
        initial_scope,
    )?;
    let initial_helper_receipt_valid =
        exact_helper_receipt(&initial_mount, initial_scope, &initial_profile)?;

    let initial_main_sentinel_write = exec_in_session(
        &client,
        &initial_workspace_session_id,
        main_sentinel_write_command(),
        "qualification initial main sentinel write",
    )?;
    let initial_sentinel_valid = check_sentinel_output(
        &initial_main_sentinel_write,
        SENTINEL,
        b"mpla-qualification-main-v1\n",
        "qualification initial sentinel",
    )?;
    let initial_publish = client.invoke(
        Some(&format!("{run_id}-qualification-publish-main")),
        "publish_mpla_workspace_session",
        &[
            "--workspace-session-id".to_owned(),
            initial_workspace_session_id,
            "--branch".to_owned(),
            "main".to_owned(),
        ],
    )?;
    require_destroyed(&initial_publish, "qualification initial publish")?;

    let fork = client.invoke(
        Some(&format!("{run_id}-qualification-fork")),
        "fork_workspace_session",
        &[
            "--run-id".to_owned(),
            run_id.to_owned(),
            "--source-branch".to_owned(),
            "main".to_owned(),
            "--branch".to_owned(),
            "qualified-fork".to_owned(),
        ],
    )?;
    let fork_activation = activate(&client, run_id, "qualified-fork", "qualification-fork")?;
    require_same_profile(
        &fork_activation.response,
        "qualification fork activation",
        &initial_profile,
    )?;
    let fork_session_id = required_string(
        &fork_activation.response,
        "workspace_session_id",
        "qualification fork activation",
    )?;
    let fork_sentinel_write = exec_in_session(
        &client,
        &fork_session_id,
        fork_sentinel_write_command(),
        "qualification fork sentinel write",
    )?;
    let fork_sentinel_valid = check_sentinel_output(
        &fork_sentinel_write,
        FORK_SENTINEL,
        b"mpla-qualification-fork-v1\n",
        "qualification fork sentinel",
    )?;
    let fork_publish = client.invoke(
        Some(&format!("{run_id}-qualification-publish-fork")),
        "publish_mpla_workspace_session",
        &[
            "--workspace-session-id".to_owned(),
            fork_session_id,
            "--branch".to_owned(),
            "qualified-fork".to_owned(),
        ],
    )?;
    require_destroyed(&fork_publish, "qualification fork publish")?;

    let main_activation_before_rollback = activate(
        &client,
        run_id,
        "main",
        "qualification-main-before-rollback",
    )?;
    require_same_profile(
        &main_activation_before_rollback.response,
        "qualification main activation before rollback",
        &initial_profile,
    )?;
    let main_session_before_rollback = required_string(
        &main_activation_before_rollback.response,
        "workspace_session_id",
        "qualification main activation before rollback",
    )?;
    let fork_isolation_check = exec_in_session(
        &client,
        &main_session_before_rollback,
        main_isolation_command(),
        "qualification fork isolation",
    )?;
    let fork_isolation = check_sentinel_output(
        &fork_isolation_check,
        SENTINEL,
        b"mpla-qualification-main-v1\n",
        "qualification main isolation sentinel",
    )?;
    let main_scope_before_rollback = main_activation_before_rollback
        .response
        .get("storage_admin_scope")
        .ok_or("qualification main activation omitted storage_admin_scope")?;
    let _main_cleanup_before_rollback = cleanup_mounted_workspace(
        &client,
        run_id,
        "qualification-main-before-rollback",
        &initial_profile,
        main_scope_before_rollback,
    )?;
    let _main_destroy_before_rollback = destroy_session(
        &client,
        &main_session_before_rollback,
        "qualification main destroy before rollback",
    )?;

    let rollback = client.invoke(
        Some(&format!("{run_id}-qualification-rollback")),
        "rollback_workspace_session",
        &[
            "--run-id".to_owned(),
            run_id.to_owned(),
            "--branch".to_owned(),
            "main".to_owned(),
            "--target-branch".to_owned(),
            "qualified-fork".to_owned(),
        ],
    )?;
    let rollback_session_id = required_string(
        &rollback.response,
        "workspace_session_id",
        "qualification rollback",
    )?;
    require_same_profile(
        &rollback.response,
        "qualification rollback",
        &initial_profile,
    )?;
    let rollback_content_check = exec_in_session(
        &client,
        &rollback_session_id,
        rollback_content_command(),
        "qualification rollback content",
    )?;
    let rollback_main_sentinel = check_sentinel_output(
        &rollback_content_check,
        SENTINEL,
        b"mpla-qualification-main-v1\n",
        "qualification rollback main sentinel",
    )?;
    let rollback_fork_sentinel = check_sentinel_output_nth(
        &rollback_content_check,
        FORK_SENTINEL,
        b"mpla-qualification-fork-v1\n",
        "qualification rollback fork sentinel",
        1,
    )?;
    let rollback_scope = rollback
        .response
        .get("storage_admin_scope")
        .ok_or("qualification rollback omitted storage_admin_scope")?;
    let _rollback_cleanup = cleanup_mounted_workspace(
        &client,
        run_id,
        "qualification-rollback",
        &initial_profile,
        rollback_scope,
    )?;
    let _rollback_destroy = destroy_session(
        &client,
        &rollback_session_id,
        "qualification rollback destroy",
    )?;

    let session_restart_activation =
        activate(&client, run_id, "main", "qualification-session-restart")?;
    require_same_profile(
        &session_restart_activation.response,
        "qualification session restart activation",
        &initial_profile,
    )?;
    let restart_session_id = required_string(
        &session_restart_activation.response,
        "workspace_session_id",
        "qualification session restart activation",
    )?;
    let session_restart_content_check = exec_in_session(
        &client,
        &restart_session_id,
        rollback_content_command(),
        "qualification session restart content",
    )?;
    let restart_main_sentinel = check_sentinel_output(
        &session_restart_content_check,
        SENTINEL,
        b"mpla-qualification-main-v1\n",
        "qualification session restart main sentinel",
    )?;
    let restart_fork_sentinel = check_sentinel_output_nth(
        &session_restart_content_check,
        FORK_SENTINEL,
        b"mpla-qualification-fork-v1\n",
        "qualification session restart fork sentinel",
        1,
    )?;
    let restart_scope = session_restart_activation
        .response
        .get("storage_admin_scope")
        .ok_or("qualification session restart omitted storage_admin_scope")?;
    let wrong_namespace_rejection =
        wrong_namespace_probe(&client, run_id, &initial_profile, restart_scope)?;
    let wrong_namespace_rejected = rejection_mentions_mount_namespace(&wrong_namespace_rejection);
    if !wrong_namespace_rejected {
        return Err(format!(
            "wrong mount-namespace request did not expose the expected public rejection: stdout={} stderr={}",
            wrong_namespace_rejection.stdout, wrong_namespace_rejection.stderr
        )
        .into());
    }
    let wrong_profile_rejection = wrong_profile_probe(&client, run_id, restart_scope)?;
    let wrong_profile_rejected = rejection_mentions_profile(&wrong_profile_rejection);
    if !wrong_profile_rejected {
        return Err(format!(
            "wrong profile request did not expose the expected public rejection: stdout={} stderr={}",
            wrong_profile_rejection.stdout, wrong_profile_rejection.stderr
        )
        .into());
    }
    let restart_storage_cleanup = cleanup_mounted_workspace(
        &client,
        run_id,
        "qualification-session-restart",
        &initial_profile,
        restart_scope,
    )?;
    let restart_destroy = destroy_session(
        &client,
        &restart_session_id,
        "qualification session restart destroy",
    )?;

    let ordinary_workload_probe = client.invoke(
        None,
        "exec_command",
        &[
            "--timeout-ms".to_owned(),
            "120000".to_owned(),
            "--yield-time-ms".to_owned(),
            "120000".to_owned(),
            ordinary_workload_probe_command(run_id),
        ],
    )?;
    let ordinary_workload_denied_privilege =
        require_ordinary_workload_denial(&ordinary_workload_probe)?;

    let resources = monitor.finish()?;
    super::validate_resource_observation(&resources)?;
    let resource_bounds = true;
    let resources = serde_json::to_value(resources)?;
    let exact_content_mode_ownership = initial_sentinel_valid
        && fork_sentinel_valid
        && fork_isolation
        && rollback_main_sentinel
        && rollback_fork_sentinel
        && restart_main_sentinel
        && restart_fork_sentinel;
    let rollback_correctness = rollback_main_sentinel && rollback_fork_sentinel;
    let session_restart_durability = restart_main_sentinel && restart_fork_sentinel;

    let evidence = QualificationEvidence {
        schema_version: 1,
        kind: "mpla_booster_qualification_scorecard_v1".to_owned(),
        run_id: run_id.to_owned(),
        candidate_sandbox_id: candidate_sandbox_id.to_owned(),
        build_commit: build_commit.to_owned(),
        authority,
        backing,
        cgroup,
        resources,
        resource_bounds,
        initial_create,
        initial_mount,
        initial_helper_receipt_valid,
        initial_main_sentinel_write,
        initial_publish,
        fork,
        fork_activation,
        fork_sentinel_write,
        fork_publish,
        main_activation_before_rollback,
        fork_isolation_check,
        rollback,
        rollback_content_check,
        session_restart_activation,
        session_restart_content_check,
        wrong_namespace_rejection,
        wrong_namespace_rejected,
        wrong_profile_rejection,
        wrong_profile_rejected,
        restart_storage_cleanup,
        restart_destroy,
        ordinary_workload_probe,
        ordinary_workload_denied_privilege,
        exact_content_mode_ownership,
        fork_isolation,
        rollback_correctness,
        session_restart_durability,
    };
    let bytes = serde_json::to_vec_pretty(&evidence)?;
    let result_sha256 = format!("{:x}", Sha256::digest(&bytes));
    let mut file = File::options()
        .create_new(true)
        .write(true)
        .open(result_path)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    sync_directory(
        result_path
            .parent()
            .ok_or("qualification result lacks a parent")?,
    )?;
    Ok(json!({
        "result_path": result_path,
        "result_sha256": result_sha256,
        "result_bytes": bytes.len(),
        "helper_receipt_valid": evidence.initial_helper_receipt_valid,
        "ordinary_workload_denied_privilege": evidence.ordinary_workload_denied_privilege,
        "wrong_namespace_rejected": evidence.wrong_namespace_rejected,
        "wrong_profile_rejected": evidence.wrong_profile_rejected,
        "fork_isolation": evidence.fork_isolation,
        "rollback_correctness": evidence.rollback_correctness,
        "session_restart_durability": evidence.session_restart_durability,
    }))
}

fn storage_admin(
    client: &RuntimeClient,
    operation_id: &str,
    action: &str,
    profile: &str,
    scope: &Value,
) -> QualificationResult<CliInvocation> {
    let profile = approved_storage_profile(profile, "qualification storage request")?;
    let request = json!({
        "schema_version": 1,
        "interface_version": "m2r-iface-v1",
        "profile_id": profile,
        "operation_id": operation_id,
        "action": action,
        "scope": scope,
    });
    Ok(client.invoke(
        Some(operation_id),
        "mpla_storage_admin",
        &[serde_json::to_string(&request)?],
    )?)
}

fn activate(
    client: &RuntimeClient,
    run_id: &str,
    branch: &str,
    label: &str,
) -> QualificationResult<CliInvocation> {
    Ok(client.invoke(
        Some(&format!("{run_id}-{label}-activate")),
        "activate_workspace_session",
        &[
            "--run-id".to_owned(),
            run_id.to_owned(),
            "--branch".to_owned(),
            branch.to_owned(),
        ],
    )?)
}

fn exec_in_session(
    client: &RuntimeClient,
    workspace_session_id: &str,
    command: String,
    label: &str,
) -> QualificationResult<CliInvocation> {
    let invocation = client.invoke(
        None,
        "exec_command",
        &[
            "--workspace-session-id".to_owned(),
            workspace_session_id.to_owned(),
            "--timeout-ms".to_owned(),
            "120000".to_owned(),
            "--yield-time-ms".to_owned(),
            "120000".to_owned(),
            command,
        ],
    )?;
    require_command_exit(&invocation.response, label)?;
    Ok(invocation)
}

fn destroy_session(
    client: &RuntimeClient,
    workspace_session_id: &str,
    label: &str,
) -> QualificationResult<CliInvocation> {
    let destroy = client.invoke(
        None,
        "destroy_workspace_session",
        &[
            "--workspace-session-id".to_owned(),
            workspace_session_id.to_owned(),
            "--grace-s".to_owned(),
            "0".to_owned(),
        ],
    )?;
    require_destroyed(&destroy, label)?;
    Ok(destroy)
}

fn require_profile(response: &Value, label: &str) -> QualificationResult<String> {
    approved_storage_profile(
        &required_string(response, "storage_admin_profile_id", label)?,
        label,
    )
}

fn require_same_profile(
    response: &Value,
    label: &str,
    expected_profile: &str,
) -> QualificationResult {
    let observed_profile = require_profile(response, label)?;
    if observed_profile != expected_profile {
        return Err(format!(
            "{label} selected storage profile {observed_profile}, expected {expected_profile}"
        )
        .into());
    }
    Ok(())
}

fn require_destroyed(invocation: &CliInvocation, label: &str) -> QualificationResult {
    if invocation
        .response
        .get("destroyed")
        .and_then(Value::as_bool)
        != Some(true)
    {
        return Err(format!("{label} did not destroy its workspace session").into());
    }
    Ok(())
}

fn exact_helper_receipt(
    receipt: &CliInvocation,
    scope: &Value,
    profile: &str,
) -> QualificationResult<bool> {
    let response = &receipt.response;
    let (expected_capabilities, expected_capability_mask) = expected_helper_capabilities(profile)?;
    let expected_syscalls = json!(["mount", "umount2", "setns", "syncfs"]);
    let exact = response.get("profile_id") == Some(&json!(profile))
        && response.get("action") == Some(&json!("mount"))
        && response.get("outcome") == Some(&json!("succeeded"))
        && response.get("scope") == Some(scope)
        && response.get("effective_capabilities") == Some(&expected_capabilities)
        && response.get("allowed_privileged_syscalls") == Some(&expected_syscalls)
        && response.get("trusted_executable") == Some(&json!(TRUSTED_HELPER))
        && response
            .pointer("/process_evidence/executable")
            .and_then(Value::as_str)
            == Some(TRUSTED_HELPER)
        && response
            .pointer("/process_evidence/executable_sha256")
            .and_then(Value::as_str)
            .is_some_and(is_sha256)
        && response
            .pointer("/process_evidence/seccomp/no_new_privs")
            .and_then(Value::as_bool)
            == Some(true)
        && response
            .pointer("/process_evidence/capabilities/effective")
            .and_then(Value::as_u64)
            == Some(expected_capability_mask)
        && response
            .pointer("/process_evidence/capabilities/permitted")
            .and_then(Value::as_u64)
            == Some(expected_capability_mask)
        && response
            .pointer("/process_evidence/capabilities/bounding")
            .and_then(Value::as_u64)
            == Some(expected_capability_mask)
        && response
            .pointer("/mount_attestation/profile_id")
            .and_then(Value::as_str)
            == Some(profile)
        && response
            .get("mount_receipt_binding")
            .is_some_and(Value::is_object);
    if !exact {
        return Err(format!(
            "storage helper mount receipt is not an exact bound receipt: {response}"
        )
        .into());
    }
    Ok(true)
}

fn expected_helper_capabilities(profile: &str) -> QualificationResult<(Value, u64)> {
    let profile = approved_storage_profile(profile, "helper receipt profile")?;
    match profile.as_str() {
        STORAGE_ADMIN_PROFILE_ID => Ok((json!(["CAP_SYS_ADMIN"]), 1 << 21)),
        STORAGE_ADMIN_OVERLAYFS_DAC_OVERRIDE_QUALIFICATION_PROFILE_ID => Ok((
            json!(["CAP_SYS_ADMIN", "CAP_DAC_OVERRIDE"]),
            (1 << 21) | (1 << 1),
        )),
        _ => unreachable!("approved_storage_profile accepts only the two matched arms"),
    }
}

fn wrong_namespace_probe(
    client: &RuntimeClient,
    run_id: &str,
    profile: &str,
    scope: &Value,
) -> QualificationResult<CliFailure> {
    let profile = approved_storage_profile(profile, "wrong namespace probe")?;
    let mut mutated_scope = scope.clone();
    mutated_scope["mount_namespace_id"] = json!("mnt:[1]");
    let operation_id = format!("{run_id}-qualification-wrong-namespace");
    let request = json!({
        "schema_version": 1,
        "interface_version": "m2r-iface-v1",
        "profile_id": profile,
        "operation_id": operation_id,
        "action": MOUNTED_NEGATIVE_PROBE_ACTION,
        "scope": mutated_scope,
    });
    Ok(client.invoke_expect_failure(
        Some(&operation_id),
        "mpla_storage_admin",
        &[serde_json::to_string(&request)?],
    )?)
}

fn rejection_mentions_mount_namespace(rejection: &CliFailure) -> bool {
    let diagnostic = format!("{} {}", rejection.stdout, rejection.stderr).to_ascii_lowercase();
    diagnostic.contains("mount namespace") && diagnostic.contains("does not match")
}

fn wrong_profile_probe(
    client: &RuntimeClient,
    run_id: &str,
    scope: &Value,
) -> QualificationResult<CliFailure> {
    let operation_id = format!("{run_id}-qualification-wrong-profile");
    let request = json!({
        "schema_version": 1,
        "interface_version": "m2r-iface-v1",
        "profile_id": "ordinary-workload-v1",
        "operation_id": operation_id,
        "action": MOUNTED_NEGATIVE_PROBE_ACTION,
        "scope": scope,
    });
    Ok(client.invoke_expect_failure(
        Some(&operation_id),
        "mpla_storage_admin",
        &[serde_json::to_string(&request)?],
    )?)
}

fn rejection_mentions_profile(rejection: &CliFailure) -> bool {
    let diagnostic = format!("{} {}", rejection.stdout, rejection.stderr).to_ascii_lowercase();
    diagnostic.contains("profile") && diagnostic.contains("daemon-selected")
}

fn main_sentinel_write_command() -> String {
    format!("printf '%s\\n' mpla-qualification-main-v1 > {SENTINEL}; chmod 640 {SENTINEL}; sha256sum {SENTINEL}; stat -c '%a:%u:%g' {SENTINEL}")
}

fn fork_sentinel_write_command() -> String {
    format!("printf '%s\\n' mpla-qualification-fork-v1 > {FORK_SENTINEL}; chmod 640 {FORK_SENTINEL}; sha256sum {FORK_SENTINEL}; stat -c '%a:%u:%g' {FORK_SENTINEL}")
}

fn main_isolation_command() -> String {
    format!("test ! -e {FORK_SENTINEL}; sha256sum {SENTINEL}; stat -c '%a:%u:%g' {SENTINEL}")
}

fn rollback_content_command() -> String {
    format!("sha256sum {SENTINEL} {FORK_SENTINEL}; stat -c '%a:%u:%g' {SENTINEL}; stat -c '%a:%u:%g' {FORK_SENTINEL}")
}

fn check_sentinel_output(
    invocation: &CliInvocation,
    filename: &str,
    expected: &[u8],
    label: &str,
) -> QualificationResult<bool> {
    check_sentinel_output_nth(invocation, filename, expected, label, 0)
}

fn check_sentinel_output_nth(
    invocation: &CliInvocation,
    filename: &str,
    expected: &[u8],
    label: &str,
    mode_ownership_line_index: usize,
) -> QualificationResult<bool> {
    let output = required_string(&invocation.response, "output", label)?;
    let expected_digest = format!("{:x}", Sha256::digest(expected));
    let digest_line = output
        .lines()
        .filter(|line| line.ends_with(filename))
        .next()
        .ok_or_else(|| format!("{label} omitted the sha256sum line for {filename}"))?;
    let observed_digest = digest_line
        .split_ascii_whitespace()
        .next()
        .ok_or_else(|| format!("{label} sha256sum line is malformed"))?;
    let mode_line = output
        .lines()
        .filter(|line| line.starts_with("640:"))
        .nth(mode_ownership_line_index)
        .ok_or_else(|| format!("{label} omitted mode/ownership evidence"))?;
    let fields = mode_line.split(':').collect::<Vec<_>>();
    if fields.len() != 3
        || fields[0] != "640"
        || fields[1].parse::<u32>().is_err()
        || fields[2].parse::<u32>().is_err()
        || observed_digest != expected_digest
    {
        return Err(format!("{label} content, mode, or ownership differs: {output}").into());
    }
    Ok(true)
}

fn ordinary_workload_probe_command(run_id: &str) -> String {
    format!(
        "mount_bin=$(command -v mount) || exit 127; p='ordinary-{run_id}'; mkdir \"$p\" || exit 126; cap_eff=$(awk '/^CapEff:/{{print $2; exit}}' /proc/self/status) || exit 125; cap_mask=$((16#$cap_eff)); if (( cap_mask & (1 << 21) )); then cap_sys_admin=true; else cap_sys_admin=false; fi; \"$mount_bin\" -t tmpfs -o size=4096 tmpfs \"$p\" >/dev/null 2>&1; mount_result=$?; if [ \"$mount_result\" -eq 0 ]; then umount \"$p\" >/dev/null 2>&1 || true; fi; printf '{{\"cap_sys_admin\":%s,\"mount_result\":%s,\"mount_command_available\":true}}\\n' \"$cap_sys_admin\" \"$mount_result\"; test \"$cap_sys_admin\" = false && test \"$mount_result\" -ne 0"
    )
}

fn require_ordinary_workload_denial(invocation: &CliInvocation) -> QualificationResult<bool> {
    require_command_exit(&invocation.response, "ordinary workload capability probe")?;
    let output = required_string(
        &invocation.response,
        "output",
        "ordinary workload capability probe",
    )?;
    let receipt: Value = serde_json::from_str(&output)?;
    if receipt.get("cap_sys_admin").and_then(Value::as_bool) != Some(false)
        || receipt.get("mount_result").and_then(Value::as_i64) == Some(0)
        || receipt
            .get("mount_command_available")
            .and_then(Value::as_bool)
            != Some(true)
    {
        return Err(
            format!("ordinary workload retained prohibited mount authority: {receipt}").into(),
        );
    }
    Ok(true)
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invocation_with_output(output: String) -> CliInvocation {
        CliInvocation {
            operation: "test".to_owned(),
            request_id: None,
            outer_elapsed_ns: 0,
            response: json!({"output": output}),
        }
    }

    #[test]
    fn direct_fork_sentinel_accepts_its_single_mode_ownership_line() {
        let expected = b"mpla-qualification-fork-v1\n";
        let output = format!(
            "{:x}  {FORK_SENTINEL}\n640:1000:1000\n",
            Sha256::digest(expected)
        );
        assert!(check_sentinel_output(
            &invocation_with_output(output),
            FORK_SENTINEL,
            expected,
            "direct fork",
        )
        .expect("the direct fork receipt is valid"));
    }

    #[test]
    fn combined_sentinel_receipt_requires_the_explicit_second_mode_line() {
        let main = b"mpla-qualification-main-v1\n";
        let fork = b"mpla-qualification-fork-v1\n";
        let output = format!(
            "{:x}  {SENTINEL}\n{:x}  {FORK_SENTINEL}\n640:1000:1000\n640:1001:1001\n",
            Sha256::digest(main),
            Sha256::digest(fork),
        );
        let invocation = invocation_with_output(output);
        assert!(
            check_sentinel_output(&invocation, SENTINEL, main, "combined main")
                .expect("the first mode line is valid")
        );
        assert!(
            check_sentinel_output_nth(&invocation, FORK_SENTINEL, fork, "combined fork", 1,)
                .expect("the second mode line is valid")
        );
    }

    #[test]
    fn mounted_negative_probes_use_quiesce() {
        assert_eq!(MOUNTED_NEGATIVE_PROBE_ACTION, "quiesce");
    }

    #[test]
    fn ordinary_workload_probe_uses_minimal_image_tools_and_requires_denial() {
        let command = ordinary_workload_probe_command("run-1");
        assert!(command.contains("command -v mount"));
        assert!(!command.contains("python"));

        let denied = CliInvocation {
            response: json!({
                "status": "ok",
                "exit_code": 0,
                "end_offset": 1,
                "total_lines": 1,
                "output": "{\"cap_sys_admin\":false,\"mount_result\":1,\"mount_command_available\":true}",
            }),
            ..invocation_with_output(String::new())
        };
        assert!(require_ordinary_workload_denial(&denied)
            .expect("the denied workload receipt is valid"));

        let allowed = CliInvocation {
            response: json!({
                "status": "ok",
                "exit_code": 0,
                "end_offset": 1,
                "total_lines": 1,
                "output": "{\"cap_sys_admin\":true,\"mount_result\":0,\"mount_command_available\":true}",
            }),
            ..invocation_with_output(String::new())
        };
        assert!(require_ordinary_workload_denial(&allowed).is_err());
    }
}
