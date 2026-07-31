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
const S2_WRITE_COMMAND: &str = r#"python3 -c "import hashlib,json,os,random
size = 268435456
extent = 67108864
chunk = 8388608
offsets = (0, 67108864, 134217728, 201326592)
files = []
allocated_total = 0
for index in range(4):
    name = f'changed-upper-s2-{index:02}.bin'
    generator = random.Random(0x5A17C0DE + index)
    digest = hashlib.sha256()
    with open(name, 'wb') as handle:
        handle.truncate(size)
        for offset in offsets:
            handle.seek(offset)
            for _ in range(extent // chunk):
                payload = generator.randbytes(chunk)
                digest.update(payload)
                handle.write(payload)
        handle.flush()
        os.fsync(handle.fileno())
    details = os.stat(name)
    allocated = details.st_blocks * 512
    if details.st_size != size or allocated < size:
        raise RuntimeError(f'incomplete S2 fixture write: {name}')
    allocated_total += allocated
    files.append({'name': name, 'logical_bytes': details.st_size, 'allocated_bytes': allocated, 'payload_sha256': digest.hexdigest()})
print(json.dumps({'schema_version': 1, 'kind': 'mpla_s2_four_file_dense_deterministic_fixture_v1', 'files': files, 'logical_bytes': size * len(files), 'data_bytes': 1073741824, 'allocated_bytes': allocated_total, 'deterministic': True, 'sparse': False}, sort_keys=True))"#;

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

pub fn run(run_id: &str, candidate_sandbox_id: &str, build_commit: &str) -> StreamResult<Value> {
    validate_identifier(run_id, "run_id")?;
    validate_identifier(candidate_sandbox_id, "candidate_sandbox_id")?;
    validate_build_commit(build_commit)?;
    require_regular_file(Path::new(RUNTIME_CLI), "runtime CLI")?;
    require_regular_file(Path::new(TOKEN_FILE), "gateway token")?;

    let run_root = Path::new("/eos/workspace/mpla-poc/scorecard").join(format!("{run_id}-stream"));
    fs::create_dir_all(run_root.parent().ok_or("stream run root lacks a parent")?)?;
    fs::create_dir(&run_root)?;
    let result_path = Path::new("/workspace/scorecard-stream-result.json");
    if result_path.exists() {
        return Err(format!(
            "stream scorecard result already exists: {}",
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
    let oracle = validate_merged_publication_oracle(
        &client,
        run_id,
        "stream",
        "main",
        &stream_publish,
        None,
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
    let common = checksum_inside_timer
        && zero_immutable_payload_reads
        && no_second_payload_allocation
        && durable
        && oracle.exact_match;
    let required =
        common && throughput_at_least_gib_per_second(throughput_bytes, stream_elapsed_ns, 1);
    let preferred =
        common && throughput_at_least_gib_per_second(throughput_bytes, stream_elapsed_ns, 5);
    let resources = monitor.finish()?;
    super::validate_resource_observation(&resources)?;
    let resource_bounds = true;
    let evidence = StreamEvidence {
        schema_version: 1,
        kind: "mpla_booster_stream_scorecard_v1".to_owned(),
        run_id: run_id.to_owned(),
        candidate_sandbox_id: candidate_sandbox_id.to_owned(),
        build_commit: build_commit.to_owned(),
        authority,
        backing,
        cgroup,
        resources: serde_json::to_value(resources)?,
        resource_bounds,
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
        oracle_exact_match: true,
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
        if name != expected_name
            || logical_bytes != S2_FILE_BYTES
            || allocated_bytes < S2_FILE_BYTES
            || digest.len() != 64
            || !digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("stream S2 fixture file violates its fixed contract".into());
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
        let files = (0..S2_FILE_COUNT)
            .map(|index| {
                json!({
                    "name": format!("changed-upper-s2-{index:02}.bin"),
                    "logical_bytes": S2_FILE_BYTES,
                    "allocated_bytes": S2_FILE_BYTES,
                    "payload_sha256": "0".repeat(64),
                })
            })
            .collect::<Vec<_>>();
        let fixture = json!({
            "schema_version": 1,
            "kind": "mpla_s2_four_file_dense_deterministic_fixture_v1",
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
    }
}
