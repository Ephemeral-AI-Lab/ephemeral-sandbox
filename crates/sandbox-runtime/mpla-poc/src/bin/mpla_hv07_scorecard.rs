use std::error::Error;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use sandbox_runtime_mpla_poc::{durable, NamedFaultPoint};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::mpla_speed_scorecard::{
    require_regular_file, sync_directory, validate_build_commit, validate_identifier,
};

type Hv07Result<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

const BASE_ROOT: &str = "/eos/layer-stack/base/B000001-base";
const TOOL_ROOT: &str = "/eos/layer-stack/base/B000001-base/_campaign-tools";
const HV07_TEST: &str = "/eos/layer-stack/base/B000001-base/_campaign-tools/hv07_campaign";
const ORACLE: &str = "/eos/layer-stack/base/B000001-base/_campaign-tools/mpla-poc-oracle";
const RUNTIME_CLI: &str = "/eos/layer-stack/base/B000001-base/_campaign-tools/sandbox-runtime-cli";
const PRODUCT_CATALOG: &str =
    "/eos/layer-stack/base/B000001-base/_campaign-tools/product-catalog.json";
const HV07_FIXTURE_BYTES: u64 = 128 * 1024 * 1024;
const HV07_ACCEPTANCE_SECONDS: u64 = 60;
const HV07_OUTER_WATCHDOG_SECONDS: u64 = 120;

pub fn run(run_id: &str, candidate_sandbox_id: &str, build_commit: &str) -> Hv07Result<Value> {
    validate_identifier(run_id, "run_id")?;
    validate_identifier(candidate_sandbox_id, "candidate_sandbox_id")?;
    validate_build_commit(build_commit)?;
    require_regular_file(Path::new(HV07_TEST), "HV-07 campaign test")?;
    require_regular_file(Path::new(ORACLE), "independent oracle")?;
    require_regular_file(Path::new(RUNTIME_CLI), "runtime CLI")?;
    require_regular_file(Path::new(PRODUCT_CATALOG), "product catalog")?;

    let run_root = Path::new("/eos/workspace/mpla-poc/scorecard").join(format!("{run_id}-hv07"));
    let parent = run_root.parent().ok_or("HV-07 run root lacks a parent")?;
    fs::create_dir_all(parent)?;
    if run_root.exists() {
        return Err(format!("HV-07 run root already exists: {}", run_root.display()).into());
    }
    fs::create_dir(&run_root)?;
    let backing = super::persistent_backing(&run_root)?;
    let cgroup_dir = super::current_cgroup_v2_dir()?;
    let cgroup_procs_path = cgroup_dir.join("cgroup.procs");
    require_regular_file(&cgroup_procs_path, "HV-07 workload cgroup membership")?;
    let cgroup = json!({
        "path": cgroup_dir,
        "memory_high": super::read_limit(&cgroup_dir.join("memory.high"))?,
        "memory_max": super::read_limit(&cgroup_dir.join("memory.max"))?,
        "membership_proven": super::cgroup_contains_self(&cgroup_dir)?,
    });
    let monitor = super::ResourceMonitor::start_heavy(&cgroup_dir, &run_root)?;
    let result_path = Path::new("/workspace/hv07-result.json");
    if result_path.exists() {
        return Err(format!("HV-07 result already exists: {}", result_path.display()).into());
    }
    let qualification_path = run_root.join("qualification.json");
    durable::replace_json(
        &qualification_path,
        &json!({"schema_version": 1, "kind": "hv07_not_used_qualification_v1"}),
    )?;
    let mut command = Command::new(HV07_TEST);
    command
        .args([
            "--ignored",
            "--exact",
            "hv_07_fresh_crash_sweep",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("MPLA_POC_RUN_ID", run_id)
        .env("MPLA_POC_PAYLOAD_ROOT", run_root.join("payload"))
        .env("MPLA_POC_CONTROL_ROOT", run_root.join("control"))
        .env("MPLA_POC_FIXTURES_ROOT", run_root.join("fixtures"))
        .env("MPLA_POC_EVIDENCE_ROOT", run_root.join("evidence"))
        .env("MPLA_POC_QUALIFICATION_PATH", &qualification_path)
        .env("MPLA_POC_ORACLE_BIN", ORACLE)
        .env("MPLA_POC_CLI_BIN", RUNTIME_CLI)
        .env("MPLA_POC_CATALOG_BINDING_PATH", PRODUCT_CATALOG)
        .env(
            "MPLA_POC_HV07_FIXTURE_BYTES",
            HV07_FIXTURE_BYTES.to_string(),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
        .env("MPLA_POC_STORAGE_CGROUP_DIR", &cgroup_dir)
        .env("MPLA_POC_CGROUP_PROCS", &cgroup_procs_path);
    let campaign_started = Instant::now();
    let output = run_with_watchdog(
        &mut command,
        Duration::from_secs(HV07_OUTER_WATCHDOG_SECONDS),
    )?;
    let campaign_elapsed_ns =
        u64::try_from(campaign_started.elapsed().as_nanos()).unwrap_or(u64::MAX);
    let internal_result = run_root
        .join("evidence")
        .join("cases")
        .join("HV-07")
        .join("fresh-sweep-result.json");
    let result: Value = durable::read_json(&internal_result)?;
    let fresh_sweep_validation_error = validate_fresh_sweep(&result)
        .err()
        .map(|error| error.to_string());
    let passed = result
        .get("passed")
        .and_then(Value::as_bool)
        .ok_or("HV-07 campaign result omitted passed")?;
    let exit_result_agreement = passed == output.status.success();
    let resources = monitor.finish()?;
    let resource_validation_error = super::validate_resource_observation(&resources)
        .err()
        .map(|error| error.to_string());
    let bytes = serde_json::to_vec_pretty(&json!({
        "schema_version": 1,
        "kind": "mpla_hv07_scorecard_result_v1",
        "run_id": run_id,
        "candidate_sandbox_id": candidate_sandbox_id,
        "build_commit": build_commit,
        "base_root": BASE_ROOT,
        "tool_root": TOOL_ROOT,
        "backing": backing,
        "cgroup": cgroup,
        "resources": resources,
        "resource_bounds": resource_validation_error.is_none(),
        "resource_validation_error": resource_validation_error,
        "fixture_logical_bytes": HV07_FIXTURE_BYTES,
        "acceptance_budget_seconds": HV07_ACCEPTANCE_SECONDS,
        "outer_watchdog_seconds": HV07_OUTER_WATCHDOG_SECONDS,
        "campaign_elapsed_ns": campaign_elapsed_ns,
        "outer_watchdog_pass": campaign_elapsed_ns
            <= HV07_OUTER_WATCHDOG_SECONDS * 1_000_000_000,
        "test_exit_code": output.status.code(),
        "exit_result_agreement": exit_result_agreement,
        "fresh_sweep_validated": fresh_sweep_validation_error.is_none(),
        "fresh_sweep_validation_error": fresh_sweep_validation_error,
        "fresh_sweep": result,
    }))?;
    let result_sha256 = format!("{:x}", Sha256::digest(&bytes));
    let mut file = File::options()
        .create_new(true)
        .write(true)
        .open(result_path)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    sync_directory(result_path.parent().ok_or("HV-07 result lacks a parent")?)?;
    if let Some(error) = fresh_sweep_validation_error {
        return Err(error.into());
    }
    if !exit_result_agreement {
        return Err(format!(
            "HV-07 campaign exit/result disagreement: exit={:?}, passed={passed}",
            output.status.code(),
        )
        .into());
    }
    if let Some(error) = resource_validation_error {
        return Err(error.into());
    }
    Ok(json!({
        "result_path": result_path,
        "result_sha256": result_sha256,
        "result_bytes": bytes.len(),
        "hv07_required": true,
    }))
}

fn run_with_watchdog(command: &mut Command, timeout: Duration) -> Hv07Result<Output> {
    use std::os::unix::process::CommandExt as _;

    command.process_group(0);
    let mut child = command.spawn()?;
    let child_process_group = i32::try_from(child.id()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "HV-07 watchdog child PID exceeds i32",
        )
    })?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or("HV-07 child stdout is not piped")?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or("HV-07 child stderr is not piped")?;
    let stdout_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).map(|_| bytes)
    });
    let started = Instant::now();
    let (status, timed_out) = loop {
        if let Some(status) = child.try_wait()? {
            break (status, false);
        }
        if started.elapsed() >= timeout {
            let signal_error = if unsafe { libc::kill(-child_process_group, libc::SIGKILL) } == 0 {
                None
            } else {
                let error = std::io::Error::last_os_error();
                (error.raw_os_error() != Some(libc::ESRCH)).then_some(error)
            };
            let status = child.wait()?;
            if let Some(error) = signal_error {
                return Err(error.into());
            }
            break (status, true);
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let stdout = stdout_reader
        .join()
        .map_err(|_| "HV-07 stdout reader panicked")??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| "HV-07 stderr reader panicked")??;
    let output = Output {
        status,
        stdout,
        stderr,
    };
    if timed_out {
        return Err(format!(
            "HV-07 campaign exceeded its independent {}-second outer watchdog; \
             exit={:?}, stdout={}, stderr={}",
            timeout.as_secs(),
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        )
        .into());
    }
    Ok(output)
}

fn validate_fresh_sweep(result: &Value) -> Hv07Result {
    let expected_points = NamedFaultPoint::ALL
        .iter()
        .map(|point| {
            serde_json::to_value(point)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .ok_or_else(|| "HV-07 fault registry did not serialize to a string".into())
        })
        .collect::<Hv07Result<std::collections::BTreeSet<_>>>()?;
    if expected_points.len() != 46 {
        return Err(format!(
            "HV-07 compiled registry has {} points, expected 46",
            expected_points.len()
        )
        .into());
    }
    if result.get("schema_version").and_then(Value::as_u64) != Some(1)
        || result.get("case_id").and_then(Value::as_str) != Some("HV-07")
        || result.get("fixture_logical_bytes").and_then(Value::as_u64) != Some(HV07_FIXTURE_BYTES)
        || result.get("required_fault_points").and_then(Value::as_u64)
            != Some(expected_points.len() as u64)
        || result
            .get("canonical_semantic_builds")
            .and_then(Value::as_u64)
            != Some(1)
        || result
            .get("semantic_receipt_reuses")
            .and_then(Value::as_u64)
            != Some((expected_points.len() - 1) as u64)
        || result.get("hard_stop_ns").and_then(Value::as_u64)
            != Some(HV07_ACCEPTANCE_SECONDS * 1_000_000_000)
        || result
            .get("elapsed_ns")
            .and_then(Value::as_u64)
            .is_none_or(|elapsed| elapsed > HV07_ACCEPTANCE_SECONDS * 1_000_000_000)
        || result
            .get("failures")
            .and_then(Value::as_array)
            .is_none_or(|failures| !failures.is_empty())
        || result.get("passed").and_then(Value::as_bool) != Some(true)
    {
        return Err("HV-07 fresh sweep top-level receipt is not an exact passing receipt".into());
    }

    let summary = result
        .get("summary")
        .and_then(Value::as_object)
        .ok_or("HV-07 fresh sweep omitted its summary")?;
    for (field, expected) in [
        ("required_fault_points", expected_points.len() as u64),
        ("recorded_attempts", expected_points.len() as u64),
        ("passing_fault_points", expected_points.len() as u64),
        (
            "physical_passing_fault_points",
            expected_points.len() as u64,
        ),
        ("failed_attempts", 0),
    ] {
        if summary.get(field).and_then(Value::as_u64) != Some(expected) {
            return Err(format!("HV-07 summary field {field} is not exactly {expected}").into());
        }
    }
    for field in ["missing_fault_points", "physical_missing_fault_points"] {
        if summary
            .get(field)
            .and_then(Value::as_array)
            .is_none_or(|missing| !missing.is_empty())
        {
            return Err(format!("HV-07 summary field {field} is not empty").into());
        }
    }
    if summary
        .get("complete_for_requested_mode")
        .and_then(Value::as_bool)
        != Some(true)
    {
        return Err("HV-07 summary is not complete for physical execution".into());
    }

    let points = result
        .get("points")
        .and_then(Value::as_array)
        .ok_or("HV-07 fresh sweep omitted point receipts")?;
    if points.len() != expected_points.len() {
        return Err(format!(
            "HV-07 fresh sweep returned {} point receipts, expected {}",
            points.len(),
            expected_points.len()
        )
        .into());
    }
    let required_assertions = [
        "exact_fixture_bytes",
        "physical_attempt_passed",
        "durable_stop_then_sigkill",
        "same_operation_exact_replay",
        "old_or_complete_new_visibility",
        "no_failed_attempts_or_unclassified_debt",
        "case_hard_stop",
    ];
    let mut observed_points = std::collections::BTreeSet::new();
    for point in points {
        let point_name = point
            .get("fault_point")
            .and_then(Value::as_str)
            .ok_or("HV-07 point receipt omitted fault_point")?;
        if !expected_points.contains(point_name) || !observed_points.insert(point_name.to_owned()) {
            return Err(
                format!("HV-07 point receipt is unknown or duplicated: {point_name}").into(),
            );
        }
        if point.get("passed").and_then(Value::as_bool) != Some(true) {
            return Err(format!("HV-07 point did not pass: {point_name}").into());
        }
        let assertions = point
            .get("assertions")
            .and_then(Value::as_array)
            .ok_or_else(|| format!("HV-07 point {point_name} omitted assertions"))?;
        for required in required_assertions {
            let matches = assertions
                .iter()
                .filter(|assertion| {
                    assertion.get("name").and_then(Value::as_str) == Some(required)
                        && assertion.get("passed").and_then(Value::as_bool) == Some(true)
                })
                .count();
            if matches != 1 {
                return Err(format!(
                    "HV-07 point {point_name} has {matches} passing {required} assertions"
                )
                .into());
            }
        }
        validate_point_witness(point_name, point)?;
    }
    if observed_points != expected_points {
        return Err("HV-07 point receipt set differs from the compiled registry".into());
    }
    Ok(())
}

fn validate_point_witness(point_name: &str, point: &Value) -> Hv07Result {
    let record = point
        .pointer("/details/record")
        .and_then(Value::as_object)
        .ok_or_else(|| format!("HV-07 point {point_name} omitted its crash record"))?;
    let record_sha256 = record.get("record_sha256").and_then(Value::as_str);
    if record.get("schema_version").and_then(Value::as_u64) != Some(1)
        || record.get("format").and_then(Value::as_str) != Some("mpla-poc-crash-sweep-v1")
        || record.get("passed").and_then(Value::as_bool) != Some(true)
        || record
            .get("failures")
            .and_then(Value::as_array)
            .is_none_or(|failures| !failures.is_empty())
        || record_sha256.is_none_or(|digest| {
            digest.len() != 64
                || digest
                    .bytes()
                    .any(|byte| !byte.is_ascii_digit() && !(b'a'..=b'f').contains(&byte))
        })
    {
        return Err(format!("HV-07 point {point_name} has an invalid crash record").into());
    }
    let observation = record
        .get("observation")
        .and_then(Value::as_object)
        .ok_or_else(|| format!("HV-07 point {point_name} omitted its crash observation"))?;
    let operation_id = observation
        .get("operation_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());
    let kill = observation
        .get("physical_kill_witness")
        .and_then(Value::as_object);
    let real_operation = observation
        .get("real_operation_witness")
        .and_then(Value::as_object);
    let stationary_payload_path_before = real_operation
        .and_then(|value| value.get("stationary_payload_path_before"))
        .and_then(Value::as_str);
    let stationary_payload_path_after = real_operation
        .and_then(|value| value.get("stationary_payload_path_after"))
        .and_then(Value::as_str);
    let expected_visibility = expected_selected_visibility(point_name);
    if observation.get("fault_point").and_then(Value::as_str) != Some(point_name)
        || observation.get("execution_mode").and_then(Value::as_str) != Some("process_sigkill")
        || operation_id.is_none()
        || observation
            .get("retry_operation_id")
            .and_then(Value::as_str)
            != operation_id
        || observation
            .get("idempotent_retry_same_result")
            .and_then(Value::as_bool)
            != Some(true)
        || observation
            .get("post_sealing_session_resumed")
            .and_then(Value::as_bool)
            != Some(false)
        || observation
            .get("failed_span_retained")
            .and_then(Value::as_bool)
            != Some(true)
        || observation
            .get("cancelled_span_retained")
            .and_then(Value::as_bool)
            != Some(true)
        || observation
            .get("unclassified_debt_bytes")
            .and_then(Value::as_u64)
            != Some(0)
        || observation
            .get("selected_visibility")
            .and_then(Value::as_str)
            != expected_visibility
        || kill
            .and_then(|value| value.get("fault_point"))
            .and_then(Value::as_str)
            != Some(point_name)
        || kill
            .and_then(|value| value.get("operation_id"))
            .and_then(Value::as_str)
            != operation_id
        || kill
            .and_then(|value| value.get("signal"))
            .and_then(Value::as_i64)
            != Some(9)
        || kill
            .and_then(|value| value.get("durable_marker_observed"))
            .and_then(Value::as_bool)
            != Some(true)
        || kill
            .and_then(|value| value.get("marker_parent_synced"))
            .and_then(Value::as_bool)
            != Some(true)
        || kill
            .and_then(|value| value.get("terminated_by_expected_signal"))
            .and_then(Value::as_bool)
            != Some(true)
        || real_operation
            .and_then(|value| value.get("fault_point"))
            .and_then(Value::as_str)
            != Some(point_name)
        || real_operation
            .and_then(|value| value.get("operation_id"))
            .and_then(Value::as_str)
            != operation_id
        || real_operation
            .and_then(|value| value.get("payload_bytes_moved"))
            .and_then(Value::as_u64)
            != Some(0)
        || real_operation
            .and_then(|value| value.get("payload_bytes_copied"))
            .and_then(Value::as_u64)
            != Some(0)
        || stationary_payload_path_before.is_none_or(str::is_empty)
        || stationary_payload_path_before != stationary_payload_path_after
    {
        return Err(format!("HV-07 point {point_name} has an invalid crash observation").into());
    }
    let recovery = observation
        .get("recovery_replay_witness")
        .and_then(Value::as_object)
        .ok_or_else(|| format!("HV-07 point {point_name} omitted its replay witness"))?;
    if recovery.get("fault_point").and_then(Value::as_str) != Some(point_name)
        || recovery.get("operation_id").and_then(Value::as_str) != operation_id
        || recovery.get("retry_operation_id").and_then(Value::as_str) != operation_id
        || recovery.get("selected_visibility").and_then(Value::as_str) != expected_visibility
    {
        return Err(format!("HV-07 point {point_name} replay identity is not exact").into());
    }
    for field in [
        "recovery_invoked",
        "recovery_completed",
        "terminal_invariant_verified",
        "exact_owner_verified",
        "exact_locator_verified",
        "exact_ref_verified",
        "stationary_payload_verified",
        "failed_attempt_bundle_durable",
        "cancelled_attempt_bundle_durable",
        "idempotent_retry_verified",
    ] {
        if recovery.get(field).and_then(Value::as_bool) != Some(true) {
            return Err(format!(
                "HV-07 point {point_name} replay witness field {field} did not pass"
            )
            .into());
        }
    }
    Ok(())
}

fn expected_selected_visibility(point_name: &str) -> Option<&'static str> {
    match point_name {
        "fence_before_close"
        | "fence_after_close"
        | "fence_after_drain"
        | "sealing_before_write"
        | "sealing_after_file_fsync"
        | "activate_after_ref_select"
        | "activate_after_locator_pin"
        | "activate_after_fresh_owner"
        | "activate_after_mount"
        | "activate_after_ready" => Some("old"),
        "sealing_after_dir_fsync"
        | "quiesce_before_stop"
        | "quiesce_after_reap"
        | "quiesce_after_fd_audit"
        | "unmount_before_strict"
        | "unmount_after_strict"
        | "flush_before_syncfs"
        | "flush_after_syncfs"
        | "inventory_after_first"
        | "inventory_after_stable_second"
        | "owner_before_intent"
        | "owner_after_intent_fsync"
        | "owner_before_compare"
        | "owner_after_generation_fsync"
        | "owner_after_journal_commit"
        | "owner_after_selector_rename"
        | "owner_after_selector_dir_fsync"
        | "owner_before_receipt"
        | "owner_after_receipt_dir_fsync"
        | "canonical_before_install"
        | "canonical_after_object_fsync"
        | "canonical_after_object_dir_fsync"
        | "canonical_after_root_manifest_fsync"
        | "locator_after_forward"
        | "locator_after_reverse"
        | "locator_after_manifest_fsync"
        | "locator_after_selector_rename"
        | "locator_after_selector_dir_fsync"
        | "ref_before_temp"
        | "ref_after_temp_fsync"
        | "ref_after_replace"
        | "ref_after_parent_fsync"
        | "response_loss_publish"
        | "response_loss_activate"
        | "response_loss_rollback"
        | "activate_after_binding_fsync" => Some("complete_new"),
        _ => None,
    }
}
