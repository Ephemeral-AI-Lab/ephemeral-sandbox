use std::error::Error;
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use std::process::Command;

use sandbox_runtime_mpla_poc::durable;
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
    let output = Command::new(HV07_TEST)
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
        .output()?;
    let internal_result = run_root
        .join("evidence")
        .join("cases")
        .join("HV-07")
        .join("fresh-sweep-result.json");
    let result: Value = durable::read_json(&internal_result)?;
    let passed = result
        .get("passed")
        .and_then(Value::as_bool)
        .ok_or("HV-07 campaign result omitted passed")?;
    if passed != output.status.success() {
        return Err(format!(
            "HV-07 campaign exit/result disagreement: exit={:?}, passed={passed}",
            output.status.code(),
        )
        .into());
    }
    let resources = monitor.finish()?;
    super::validate_resource_observation(&resources)?;
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
        "resource_bounds": true,
        "fixture_logical_bytes": HV07_FIXTURE_BYTES,
        "test_exit_code": output.status.code(),
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
    Ok(json!({
        "result_path": result_path,
        "result_sha256": result_sha256,
        "result_bytes": bytes.len(),
        "hv07_required": true,
    }))
}
