use std::error::Error;
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::mpla_speed_scorecard::{
    approved_storage_profile, publication_roots_match, require_command_exit, require_regular_file,
    required_string, required_u64, sync_directory, validate_build_commit, validate_identifier,
    validate_merged_publication_oracle, CliInvocation, OracleValidation, RuntimeClient,
};

type StreamResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

const RUNTIME_CLI: &str = "/eos/layer-stack/base/B000001-base/_campaign-tools/sandbox-runtime-cli";
const TOKEN_FILE: &str = "/eos/layer-stack/base/B000001-base/_campaign-tools/gateway.token";
const GIB: u64 = 1024 * 1024 * 1024;
const MIB: u64 = 1024 * 1024;
const S2_FILE_COUNT: u64 = 4;
const S2_FILE_BYTES: u64 = GIB / S2_FILE_COUNT;
const S2_EXTENT_BYTES: u64 = 64 * MIB;
const S2_EXTENTS_PER_FILE: u64 = 4;
const S2_DATA_BYTES: u64 = S2_FILE_COUNT * S2_EXTENTS_PER_FILE * S2_EXTENT_BYTES;
const S2_ZERO_FILE_SHA256: &str =
    "a6d72ac7690f53be6ae46ba88506bd97302a093f7108472bd9efc3cefda06484";
const SEMANTIC_APPLICATION_POOL_BYTES: u64 = 8 * MIB;
const SEMANTIC_SPOOL_RUN_BYTES: u64 = 4 * MIB;
const STREAM_PROGRESS_PATH: &str = "/workspace/scorecard-stream-progress.jsonl";
const MAX_PROGRESS_EVENT_BYTES: usize = 16 * 1024;
const S2_WRITE_COMMAND: &str = r#"set -euo pipefail
files_json=
separator=
allocated_total=0
extent_plan='[{"offset":0,"length":67108864},{"offset":67108864,"length":67108864},{"offset":134217728,"length":67108864},{"offset":201326592,"length":67108864}]'
for index in 00 01 02 03; do
    name="changed-upper-s2-${index}.bin"
    for offset in 0 67108864 134217728 201326592; do
        fallocate -o "$offset" -l 67108864 -- "$name"
    done
    logical_bytes=$(stat -c %s -- "$name")
    allocated_blocks=$(stat -c %b -- "$name")
    allocated_bytes=$((allocated_blocks * 512))
    test "$logical_bytes" -eq 268435456
    test "$allocated_bytes" -ge "$logical_bytes"
    digest_line=$(sha256sum -- "$name")
    digest=${digest_line%% *}
    case "$digest" in
        ''|*[!0-9a-f]*) exit 1 ;;
    esac
    entry=$(printf '{"name":"%s","logical_bytes":%s,"allocated_bytes":%s,"payload_sha256":"%s","logical_extent_operations":%s}' "$name" "$logical_bytes" "$allocated_bytes" "$digest" "$extent_plan")
    files_json="${files_json}${separator}${entry}"
    separator=,
    allocated_total=$((allocated_total + allocated_bytes))
done
sync -f changed-upper-s2-00.bin
printf '{"schema_version":1,"kind":"mpla_s2_four_file_dense_deterministic_fixture_v1","creation_method":"fallocate_zero_extents","files":[%s],"logical_bytes":1073741824,"data_bytes":1073741824,"allocated_bytes":%s,"deterministic":true,"sparse":false}\n' "$files_json" "$allocated_total""#;

#[derive(Debug, Serialize)]
struct StreamEvidence {
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
    semantic_resource_maxima: Value,
    semantic_resource_bounds: bool,
    create: CliInvocation,
    mount: CliInvocation,
    initial_write: CliInvocation,
    initial_publish: CliInvocation,
    activation: CliInvocation,
    changed_upper_write: CliInvocation,
    s2_fixture: Value,
    stream_publish: CliInvocation,
    oracle: OracleValidation,
    changed_upper_bytes: u64,
    storage_stable_callback_elapsed_ns: u64,
    semantic_build_elapsed_ns: u64,
    stream_elapsed_ns: u64,
    throughput_bytes: u64,
    throughput_bytes_per_second_floor: u64,
    required: bool,
    preferred: bool,
    checksum_inside_timer: bool,
    zero_immutable_payload_reads: bool,
    no_second_payload_allocation: bool,
    durable: bool,
    oracle_exact_match: bool,
}

struct StreamProgressLedger {
    file: File,
}

impl StreamProgressLedger {
    fn create(path: &Path) -> StreamResult<Self> {
        Ok(Self {
            file: File::options().create_new(true).write(true).open(path)?,
        })
    }

    fn record(&mut self, stage: &str, details: Value) -> StreamResult {
        let event = json!({
            "schema_version": 1,
            "kind": "mpla_booster_stream_progress_v1",
            "stage": stage,
            "details": details,
        });
        let mut encoded = serde_json::to_vec(&event)?;
        if encoded.len() > MAX_PROGRESS_EVENT_BYTES {
            return Err(format!(
                "stream progress event {stage} exceeds {MAX_PROGRESS_EVENT_BYTES} bytes"
            )
            .into());
        }
        encoded.push(b'\n');
        self.file.write_all(&encoded)?;
        self.file.sync_data()?;
        Ok(())
    }
}

pub fn run(run_id: &str, candidate_sandbox_id: &str, build_commit: &str) -> StreamResult<Value> {
    validate_identifier(run_id, "run_id")?;
    validate_identifier(candidate_sandbox_id, "candidate_sandbox_id")?;
    validate_build_commit(build_commit)?;
    require_regular_file(Path::new(RUNTIME_CLI), "runtime CLI")?;
    require_regular_file(Path::new(TOKEN_FILE), "gateway token")?;

    let result_path = Path::new("/workspace/scorecard-stream-result.json");
    if result_path.exists() {
        return Err(format!(
            "stream scorecard result already exists: {}",
            result_path.display()
        )
        .into());
    }
    let progress_path = Path::new(STREAM_PROGRESS_PATH);
    if progress_path.exists() {
        return Err(format!(
            "stream scorecard progress already exists: {}",
            progress_path.display()
        )
        .into());
    }
    let mut progress = StreamProgressLedger::create(progress_path)?;
    progress.record(
        "started",
        json!({
            "run_id": run_id,
            "candidate_sandbox_id": candidate_sandbox_id,
            "build_commit": build_commit,
            "phase": "P6",
            "runner": "mpla_stream_scorecard",
            "suggested_budget_seconds": 20,
            "multiplier": 2_000,
            "independent_cap_seconds": 40,
            "bounded_work": {
                "changed_upper_bytes": GIB,
                "file_count": S2_FILE_COUNT,
                "file_bytes": S2_FILE_BYTES,
                "dense": true,
            },
        }),
    )?;
    match run_inner(
        run_id,
        candidate_sandbox_id,
        build_commit,
        result_path,
        &mut progress,
    ) {
        Ok(result) => Ok(result),
        Err(error) => {
            let bounded_error = error.to_string().chars().take(2_048).collect::<String>();
            let _ = progress.record("failed", json!({ "error": bounded_error }));
            Err(error)
        }
    }
}

fn run_inner(
    run_id: &str,
    candidate_sandbox_id: &str,
    build_commit: &str,
    result_path: &Path,
    progress: &mut StreamProgressLedger,
) -> StreamResult<Value> {
    let run_root = Path::new("/eos/workspace/mpla-poc/scorecard").join(format!("{run_id}-stream"));
    fs::create_dir_all(run_root.parent().ok_or("stream run root lacks a parent")?)?;
    fs::create_dir(&run_root)?;

    let authority = super::capability_receipt()?;
    let backing = super::persistent_backing(&run_root)?;
    let cgroup_dir = super::current_cgroup_v2_dir()?;
    let cgroup = json!({
        "path": cgroup_dir,
        "memory_high": super::read_limit(&cgroup_dir.join("memory.high"))?,
        "memory_max": super::read_limit(&cgroup_dir.join("memory.max"))?,
        "membership_proven": super::cgroup_contains_self(&cgroup_dir)?,
    });
    let monitor = super::ResourceMonitor::start_heavy(&cgroup_dir, &run_root)?;
    let client = RuntimeClient::new(candidate_sandbox_id)?;
    let create = client.invoke(
        Some(&format!("{run_id}-stream-create")),
        "create_mpla_workspace_session",
        &["--run-id".to_owned(), run_id.to_owned()],
    )?;
    let workspace_session_id =
        required_string(&create.response, "workspace_session_id", "stream create")?;
    let profile = approved_storage_profile(
        &required_string(
            &create.response,
            "storage_admin_profile_id",
            "stream create",
        )?,
        "stream create",
    )?;
    let mount_operation_id = format!("{run_id}-stream-mount");
    let mount_request = json!({
        "schema_version": 1,
        "interface_version": "m2r-iface-v1",
        "profile_id": profile,
        "operation_id": mount_operation_id,
        "action": "mount",
        "scope": create
            .response
            .get("storage_admin_scope")
            .ok_or("stream create omitted storage_admin_scope")?,
    });
    let mount = client.invoke(
        Some(&format!("{run_id}-stream-mount")),
        "mpla_storage_admin",
        &[serde_json::to_string(&mount_request)?],
    )?;
    let initial_write = client.invoke(
        None,
        "exec_command",
        &[
            "--workspace-session-id".to_owned(),
            workspace_session_id.clone(),
            "--timeout-ms".to_owned(),
            "120000".to_owned(),
            "--yield-time-ms".to_owned(),
            "120000".to_owned(),
            "dd if=/dev/zero of=base-marker.bin bs=1 count=1 conv=fsync status=none".to_owned(),
        ],
    )?;
    require_command_exit(&initial_write.response, "stream base marker write")?;
    let initial_publish = client.invoke(
        Some(&format!("{run_id}-stream-initial-publish")),
        "publish_mpla_workspace_session",
        &[
            "--workspace-session-id".to_owned(),
            workspace_session_id,
            "--branch".to_owned(),
            "main".to_owned(),
        ],
    )?;
    require_initial_publication(&initial_publish)?;

    let activation = client.invoke(
        Some(&format!("{run_id}-stream-activate")),
        "activate_workspace_session",
        &[
            "--run-id".to_owned(),
            run_id.to_owned(),
            "--branch".to_owned(),
            "main".to_owned(),
        ],
    )?;
    let changed_session_id = required_string(
        &activation.response,
        "workspace_session_id",
        "stream activation",
    )?;
    let changed_upper_write = client.invoke(
        None,
        "exec_command",
        &[
            "--workspace-session-id".to_owned(),
            changed_session_id.clone(),
            "--timeout-ms".to_owned(),
            "180000".to_owned(),
            "--yield-time-ms".to_owned(),
            "180000".to_owned(),
            S2_WRITE_COMMAND.to_owned(),
        ],
    )?;
    let s2_fixture = require_s2_fixture(&changed_upper_write)?;
    progress.record(
        "dense_fixture_completed",
        json!({
            "fixture_sha256": value_sha256(&s2_fixture)?,
            "logical_bytes": GIB,
            "data_bytes": S2_DATA_BYTES,
            "file_count": S2_FILE_COUNT,
            "sparse": false,
        }),
    )?;
    let stream_publish = client.invoke(
        Some(&format!("{run_id}-stream-publish")),
        "publish_mpla_workspace_session",
        &[
            "--workspace-session-id".to_owned(),
            changed_session_id,
            "--branch".to_owned(),
            "main".to_owned(),
        ],
    )?;
    require_stream_publication(&stream_publish)?;
    let semantic_resource_maxima = require_semantic_resource_maxima(&stream_publish)?;
    let semantic_resource_bounds = true;
    progress.record(
        "stream_publication_completed",
        json!({
            "request_id": stream_publish.request_id,
            "outer_elapsed_ns": stream_publish.outer_elapsed_ns,
            "response_sha256": value_sha256(&stream_publish.response)?,
            "roots": stream_publish.response.get("roots"),
            "changed_upper_bytes": GIB,
            "semantic_resource_maxima": semantic_resource_maxima,
        }),
    )?;
    let oracle = validate_merged_publication_oracle(
        &client,
        run_id,
        "stream",
        "main",
        &stream_publish,
        None,
    )?;
    progress.record(
        "oracle_completed",
        json!({
            "exact_match": oracle.exact_match,
            "summary_sha256": value_sha256(&oracle.summary)?,
        }),
    )?;

    let phases = stream_publish
        .response
        .get("phase_elapsed_ns")
        .ok_or("stream publication omitted phase_elapsed_ns")?;
    let storage_stable_callback_elapsed_ns = required_u64(
        phases,
        "storage_stable_callback",
        "stream publication phases",
    )?;
    let semantic_build_elapsed_ns =
        required_u64(phases, "semantic_build", "stream publication phases")?;
    let stream_elapsed_ns =
        storage_stable_callback_elapsed_ns.saturating_add(semantic_build_elapsed_ns);
    if stream_elapsed_ns == 0 {
        return Err("stream publication reported a zero streaming boundary".into());
    }
    let throughput_bytes = S2_FILE_COUNT * S2_FILE_BYTES;
    let throughput_bytes_per_second_floor =
        throughput_bytes_per_second_floor(throughput_bytes, stream_elapsed_ns);
    let checksum_inside_timer = stream_publish
        .response
        .pointer("/semantic/record_stream_sha256")
        .and_then(Value::as_str)
        .is_some_and(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        && storage_stable_callback_elapsed_ns > 0
        && semantic_build_elapsed_ns > 0;
    let zero_immutable_payload_reads = required_u64(
        &stream_publish.response,
        "immutable_payload_bytes_read",
        "stream publication",
    )? == 0;
    let no_second_payload_allocation = stream_publish
        .response
        .pointer("/stationary/no_second_payload_allocation")
        .and_then(Value::as_bool)
        == Some(true);
    let durable = publication_is_durable(&stream_publish.response);
    let oracle_exact_match = oracle.exact_match;
    let common = checksum_inside_timer
        && zero_immutable_payload_reads
        && no_second_payload_allocation
        && durable
        && oracle_exact_match
        && semantic_resource_bounds;
    let required =
        common && throughput_at_least_gib_per_second(throughput_bytes, stream_elapsed_ns, 1);
    let preferred =
        common && throughput_at_least_gib_per_second(throughput_bytes, stream_elapsed_ns, 5);
    let resources = monitor.finish()?;
    super::validate_resource_observation(&resources)?;
    let resources = serde_json::to_value(resources)?;
    let resource_bounds = semantic_resource_bounds;
    progress.record(
        "resources_completed",
        json!({
            "coordinator_resource_observation_sha256": value_sha256(&resources)?,
            "semantic_resource_maxima_sha256": value_sha256(&semantic_resource_maxima)?,
            "resource_bounds": resource_bounds,
        }),
    )?;
    let evidence = StreamEvidence {
        schema_version: 1,
        kind: "mpla_booster_stream_scorecard_v1".to_owned(),
        run_id: run_id.to_owned(),
        candidate_sandbox_id: candidate_sandbox_id.to_owned(),
        build_commit: build_commit.to_owned(),
        authority,
        backing,
        cgroup,
        resources,
        resource_bounds,
        semantic_resource_maxima,
        semantic_resource_bounds,
        create,
        mount,
        initial_write,
        initial_publish,
        activation,
        changed_upper_write,
        s2_fixture,
        stream_publish,
        oracle,
        changed_upper_bytes: throughput_bytes,
        storage_stable_callback_elapsed_ns,
        semantic_build_elapsed_ns,
        stream_elapsed_ns,
        throughput_bytes,
        throughput_bytes_per_second_floor,
        required,
        preferred,
        checksum_inside_timer,
        zero_immutable_payload_reads,
        no_second_payload_allocation,
        durable,
        oracle_exact_match,
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
            .ok_or("stream scorecard result lacks a parent")?,
    )?;
    progress.record(
        "result_completed",
        json!({
            "result_path": result_path,
            "result_sha256": result_sha256,
            "result_bytes": bytes.len(),
            "required": evidence.required,
            "preferred": evidence.preferred,
            "stream_elapsed_ns": evidence.stream_elapsed_ns,
            "throughput_bytes_per_second_floor": evidence.throughput_bytes_per_second_floor,
        }),
    )?;
    Ok(json!({
        "result_path": result_path,
        "result_sha256": result_sha256,
        "result_bytes": bytes.len(),
        "stream_required": evidence.required,
        "stream_preferred": evidence.preferred,
        "stream_elapsed_ns": evidence.stream_elapsed_ns,
        "throughput_bytes": evidence.throughput_bytes,
        "throughput_elapsed_ns": evidence.stream_elapsed_ns,
        "throughput_bytes_per_second_floor": evidence.throughput_bytes_per_second_floor,
    }))
}

fn value_sha256(value: &Value) -> StreamResult<String> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(value)?)))
}

fn require_initial_publication(publication: &CliInvocation) -> StreamResult {
    if required_u64(
        &publication.response,
        "affected_path_count",
        "stream initial publication",
    )? != 1
        || required_u64(
            &publication.response,
            "affected_payload_bytes_read",
            "stream initial publication",
        )? != 0
        || publication
            .response
            .pointer("/semantic/bytes_read")
            .and_then(Value::as_u64)
            != Some(1)
        || publication
            .response
            .pointer("/stationary/stable/after/logical_bytes")
            .and_then(Value::as_u64)
            != Some(1)
        || !publication_roots_match(&publication.response)
        || publication
            .response
            .pointer("/stationary/no_second_payload_allocation")
            .and_then(Value::as_bool)
            != Some(true)
        || !publication_is_durable(&publication.response)
    {
        return Err(format!(
            "stream initial publication receipt is incomplete: {}",
            publication.response
        )
        .into());
    }
    Ok(())
}

fn require_s2_fixture(write: &CliInvocation) -> StreamResult<Value> {
    require_command_exit(&write.response, "stream S2 changed-upper fixture write")?;
    let receipt: Value = serde_json::from_str(&required_string(
        &write.response,
        "output",
        "stream S2 changed-upper fixture write",
    )?)?;
    if receipt.get("schema_version").and_then(Value::as_u64) != Some(1)
        || receipt.get("kind").and_then(Value::as_str)
            != Some("mpla_s2_four_file_dense_deterministic_fixture_v1")
        || receipt.get("creation_method").and_then(Value::as_str) != Some("fallocate_zero_extents")
        || receipt.get("logical_bytes").and_then(Value::as_u64) != Some(GIB)
        || receipt.get("data_bytes").and_then(Value::as_u64) != Some(S2_DATA_BYTES)
        || receipt.get("deterministic").and_then(Value::as_bool) != Some(true)
        || receipt.get("sparse").and_then(Value::as_bool) != Some(false)
    {
        return Err("stream S2 fixture receipt violates its fixed contract".into());
    }
    let files = receipt
        .get("files")
        .and_then(Value::as_array)
        .ok_or("stream S2 fixture receipt omitted files")?;
    if u64::try_from(files.len())? != S2_FILE_COUNT {
        return Err("stream S2 fixture receipt has the wrong file count".into());
    }
    for (index, file) in files.iter().enumerate() {
        let expected_name = format!("changed-upper-s2-{index:02}.bin");
        let name = file
            .get("name")
            .and_then(Value::as_str)
            .ok_or("stream S2 fixture file omitted name")?;
        let logical_bytes = file
            .get("logical_bytes")
            .and_then(Value::as_u64)
            .ok_or("stream S2 fixture file omitted logical bytes")?;
        let allocated_bytes = file
            .get("allocated_bytes")
            .and_then(Value::as_u64)
            .ok_or("stream S2 fixture file omitted allocated bytes")?;
        let digest = file
            .get("payload_sha256")
            .and_then(Value::as_str)
            .ok_or("stream S2 fixture file omitted payload digest")?;
        let logical_extent_operations = file
            .get("logical_extent_operations")
            .and_then(Value::as_array)
            .ok_or("stream S2 fixture file omitted logical extent operations")?;
        if name != expected_name
            || logical_bytes != S2_FILE_BYTES
            || allocated_bytes < S2_FILE_BYTES
            || digest != S2_ZERO_FILE_SHA256
            || u64::try_from(logical_extent_operations.len())? != S2_EXTENTS_PER_FILE
        {
            return Err("stream S2 fixture file violates its fixed contract".into());
        }
        for (extent_index, operation) in logical_extent_operations.iter().enumerate() {
            let operation_fields = operation
                .as_object()
                .ok_or("stream S2 logical extent operation is not an object")?;
            let offset = operation
                .get("offset")
                .and_then(Value::as_u64)
                .ok_or("stream S2 logical extent operation omitted offset")?;
            let length = operation
                .get("length")
                .and_then(Value::as_u64)
                .ok_or("stream S2 logical extent operation omitted length")?;
            if operation_fields.len() != 2
                || !operation_fields.contains_key("offset")
                || !operation_fields.contains_key("length")
                || offset != u64::try_from(extent_index)?.saturating_mul(S2_EXTENT_BYTES)
                || length != S2_EXTENT_BYTES
            {
                return Err("stream S2 logical extent operations violate the fixed plan".into());
            }
        }
    }
    Ok(receipt)
}

fn require_stream_publication(publication: &CliInvocation) -> StreamResult {
    let semantic_input_bytes = publication
        .response
        .pointer("/semantic/bytes_read")
        .and_then(Value::as_u64)
        .ok_or("stream publication omitted semantic bytes_read")?;
    if required_u64(
        &publication.response,
        "affected_path_count",
        "stream publication",
    )? != S2_FILE_COUNT
        || required_u64(
            &publication.response,
            "affected_payload_bytes_read",
            "stream publication",
        )? != GIB
        || semantic_input_bytes == 0
        || !publication_roots_match(&publication.response)
        || publication
            .response
            .pointer("/stationary/stable/after/logical_bytes")
            .and_then(Value::as_u64)
            != Some(GIB)
        || publication
            .response
            .pointer("/stationary/representative_inodes_unchanged")
            .and_then(Value::as_bool)
            != Some(true)
        || publication
            .response
            .pointer("/stationary/allocated_bytes_unchanged")
            .and_then(Value::as_bool)
            != Some(true)
        || !publication_is_durable(&publication.response)
    {
        return Err(format!(
            "stream publication receipt failed exact 1 GiB qualification: {}",
            publication.response
        )
        .into());
    }
    Ok(())
}

fn require_semantic_resource_maxima(publication: &CliInvocation) -> StreamResult<Value> {
    let maxima = publication
        .response
        .pointer("/semantic/resource_maxima")
        .ok_or("stream publication omitted semantic resource maxima")?;
    let application_pool_bytes = required_u64(
        maxima,
        "application_pool_bytes",
        "stream semantic resource maxima",
    )?;
    let peak_managed_bytes = required_u64(
        maxima,
        "peak_managed_bytes",
        "stream semantic resource maxima",
    )?;
    let scan_window_bytes = required_u64(
        maxima,
        "scan_window_bytes",
        "stream semantic resource maxima",
    )?;
    let spool_run_bytes =
        required_u64(maxima, "spool_run_bytes", "stream semantic resource maxima")?;
    let merge_fan_in = required_u64(maxima, "merge_fan_in", "stream semantic resource maxima")?;
    let peak_open_data_fds = required_u64(
        maxima,
        "peak_open_data_fds",
        "stream semantic resource maxima",
    )?;
    let peak_data_workers = required_u64(
        maxima,
        "peak_data_workers",
        "stream semantic resource maxima",
    )?;
    let trie_fan_out = required_u64(maxima, "trie_fan_out", "stream semantic resource maxima")?;
    if application_pool_bytes != SEMANTIC_APPLICATION_POOL_BYTES
        || peak_managed_bytes > SEMANTIC_APPLICATION_POOL_BYTES
        || scan_window_bytes == 0
        || scan_window_bytes > SEMANTIC_APPLICATION_POOL_BYTES
        || spool_run_bytes != SEMANTIC_SPOOL_RUN_BYTES
        || merge_fan_in != 8
        || peak_open_data_fds > 16
        || peak_data_workers == 0
        || peak_data_workers > 4
        || trie_fan_out == 0
    {
        return Err(format!("stream semantic resource envelope violated: {maxima}").into());
    }
    Ok(maxima.clone())
}

fn publication_is_durable(response: &Value) -> bool {
    [
        "files_fsynced",
        "object_directory_fsynced",
        "manifest_fsynced",
        "manifest_directory_fsynced",
    ]
    .into_iter()
    .all(|field| {
        response
            .pointer(&format!("/semantic/durability/{field}"))
            .and_then(Value::as_bool)
            == Some(true)
    })
}

fn throughput_at_least_gib_per_second(bytes: u64, elapsed_ns: u64, gib_per_second: u64) -> bool {
    elapsed_ns != 0
        && (bytes as u128) * 1_000_000_000_u128
            >= (gib_per_second as u128) * (GIB as u128) * (elapsed_ns as u128)
}

fn throughput_bytes_per_second_floor(bytes: u64, elapsed_ns: u64) -> u64 {
    if elapsed_ns == 0 {
        return 0;
    }
    u64::try_from((bytes as u128 * 1_000_000_000_u128) / elapsed_ns as u128).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn throughput_gate_uses_exact_inclusive_integer_arithmetic() {
        assert!(throughput_at_least_gib_per_second(GIB, 1_000_000_000, 1));
        assert!(!throughput_at_least_gib_per_second(GIB, 1_000_000_001, 1));
        assert!(throughput_at_least_gib_per_second(
            5 * GIB,
            1_000_000_000,
            5
        ));
        assert_eq!(throughput_bytes_per_second_floor(GIB, 1_000_000_000), GIB);
        assert_eq!(throughput_bytes_per_second_floor(GIB, 0), 0);
    }

    #[test]
    fn dense_stream_fixture_requires_one_genuinely_changed_gib() {
        let extent_operations = (0..S2_EXTENTS_PER_FILE)
            .map(|index| {
                json!({
                    "offset": index * S2_EXTENT_BYTES,
                    "length": S2_EXTENT_BYTES,
                })
            })
            .collect::<Vec<_>>();
        let files = (0..S2_FILE_COUNT)
            .map(|index| {
                json!({
                    "name": format!("changed-upper-s2-{index:02}.bin"),
                    "logical_bytes": S2_FILE_BYTES,
                    "allocated_bytes": S2_FILE_BYTES,
                    "payload_sha256": S2_ZERO_FILE_SHA256,
                    "logical_extent_operations": extent_operations,
                })
            })
            .collect::<Vec<_>>();
        let fixture = json!({
            "schema_version": 1,
            "kind": "mpla_s2_four_file_dense_deterministic_fixture_v1",
            "creation_method": "fallocate_zero_extents",
            "files": files,
            "logical_bytes": GIB,
            "data_bytes": GIB,
            "allocated_bytes": GIB,
            "deterministic": true,
            "sparse": false,
        });
        let write = CliInvocation {
            operation: "exec_command".to_owned(),
            request_id: None,
            outer_elapsed_ns: 0,
            response: json!({
                "status": "ok",
                "exit_code": 0,
                "end_offset": 0,
                "total_lines": 0,
                "output": fixture.to_string(),
            }),
        };

        let receipt = require_s2_fixture(&write).expect("dense 1-GiB fixture is accepted");

        assert_eq!(receipt["data_bytes"], GIB);
        assert_eq!(receipt["sparse"], false);

        let invocation = |fixture: &Value| CliInvocation {
            operation: "exec_command".to_owned(),
            request_id: None,
            outer_elapsed_ns: 0,
            response: json!({
                "status": "ok",
                "exit_code": 0,
                "end_offset": 0,
                "total_lines": 0,
                "output": fixture.to_string(),
            }),
        };
        let mut corruptions = Vec::new();
        let mut missing = fixture.clone();
        missing["files"][0]
            .as_object_mut()
            .expect("fixture file is an object")
            .remove("logical_extent_operations");
        corruptions.push(missing);
        let mut short = fixture.clone();
        short["files"][0]["logical_extent_operations"]
            .as_array_mut()
            .expect("logical extent operations are an array")
            .pop();
        corruptions.push(short);
        let mut extra = fixture.clone();
        extra["files"][0]["logical_extent_operations"]
            .as_array_mut()
            .expect("logical extent operations are an array")
            .push(json!({"offset": S2_FILE_BYTES, "length": S2_EXTENT_BYTES}));
        corruptions.push(extra);
        let mut wrong_offset = fixture.clone();
        wrong_offset["files"][0]["logical_extent_operations"][1]["offset"] = json!(1);
        corruptions.push(wrong_offset);
        let mut wrong_length = fixture.clone();
        wrong_length["files"][0]["logical_extent_operations"][1]["length"] =
            json!(S2_EXTENT_BYTES - 1);
        corruptions.push(wrong_length);
        let mut extra_field = fixture;
        extra_field["files"][0]["logical_extent_operations"][0]["untrusted"] = json!(0);
        corruptions.push(extra_field);
        for corrupt in corruptions {
            assert!(require_s2_fixture(&invocation(&corrupt)).is_err());
        }
    }

    #[test]
    fn dense_stream_fixture_command_uses_proven_allocated_zero_extents() {
        assert!(S2_WRITE_COMMAND.starts_with("set -euo pipefail\n"));
        assert!(S2_WRITE_COMMAND.contains("for offset in 0 67108864 134217728 201326592"));
        assert!(S2_WRITE_COMMAND.contains("fallocate -o \"$offset\" -l 67108864"));
        assert!(S2_WRITE_COMMAND.contains("\"logical_extent_operations\":%s"));
        assert!(!S2_WRITE_COMMAND.contains("fallocate -l 268435456"));
        assert!(S2_WRITE_COMMAND.contains("sync -f changed-upper-s2-00.bin"));
        assert!(S2_WRITE_COMMAND.contains("sha256sum --"));
        assert!(S2_WRITE_COMMAND.contains("stat -c %s"));
        assert!(S2_WRITE_COMMAND.contains("stat -c %b"));
        assert!(!S2_WRITE_COMMAND.contains("dd if=/dev/zero"));
        assert!(!S2_WRITE_COMMAND.contains("python"));
    }

    #[test]
    fn dense_stream_fixture_command_is_valid_bash() {
        let status = std::process::Command::new("/bin/bash")
            .args(["--noprofile", "--norc", "-n", "-c", S2_WRITE_COMMAND])
            .status()
            .expect("run the same Bash syntax parser used by the sandbox");
        assert!(status.success());
    }

    #[test]
    fn semantic_resource_maxima_are_independently_enforced() {
        let publication = CliInvocation {
            operation: "publish_mpla_workspace_session".to_owned(),
            request_id: Some("stream-publish".to_owned()),
            outer_elapsed_ns: 1,
            response: json!({
                "semantic": {
                    "resource_maxima": {
                        "application_pool_bytes": SEMANTIC_APPLICATION_POOL_BYTES,
                        "peak_managed_bytes": SEMANTIC_APPLICATION_POOL_BYTES,
                        "scan_window_bytes": MIB,
                        "spool_run_bytes": SEMANTIC_SPOOL_RUN_BYTES,
                        "merge_fan_in": 8,
                        "peak_open_data_fds": 11,
                        "peak_data_workers": 4,
                        "trie_fan_out": 256,
                    }
                }
            }),
        };
        assert!(require_semantic_resource_maxima(&publication).is_ok());

        let mut over_limit = publication;
        over_limit.response["semantic"]["resource_maxima"]["peak_managed_bytes"] =
            json!(SEMANTIC_APPLICATION_POOL_BYTES + 1);
        assert!(require_semantic_resource_maxima(&over_limit).is_err());
    }

    #[test]
    fn stream_progress_is_create_new_bounded_jsonl() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "mpla-stream-progress-{}-{nonce}.jsonl",
            std::process::id()
        ));
        let mut ledger = StreamProgressLedger::create(&path).expect("create progress ledger");
        ledger
            .record("started", json!({"changed_upper_bytes": GIB}))
            .expect("record bounded progress");
        assert!(StreamProgressLedger::create(&path).is_err());
        assert!(ledger
            .record(
                "oversized",
                json!({"payload": "x".repeat(MAX_PROGRESS_EVENT_BYTES)})
            )
            .is_err());
        drop(ledger);

        let content = fs::read_to_string(&path).expect("read progress ledger");
        let lines = content.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 1);
        let event: Value = serde_json::from_str(lines[0]).expect("parse progress event");
        assert_eq!(event["stage"], "started");
        fs::remove_file(path).expect("remove focused-test progress ledger");
    }
}
