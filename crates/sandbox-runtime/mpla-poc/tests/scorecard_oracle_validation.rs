use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

type BenchResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

#[allow(dead_code)]
#[path = "../src/bin/mpla_speed_scorecard.rs"]
mod mpla_speed_scorecard;

fn capability_receipt() -> BenchResult<Value> {
    unreachable!("oracle comparator tests do not collect capabilities")
}

fn persistent_backing(_run_root: &Path) -> BenchResult<Value> {
    unreachable!("oracle comparator tests do not inspect backing storage")
}

fn current_cgroup_v2_dir() -> BenchResult<PathBuf> {
    unreachable!("oracle comparator tests do not inspect cgroups")
}

fn read_limit(_path: &Path) -> BenchResult<Option<u64>> {
    unreachable!("oracle comparator tests do not inspect cgroup limits")
}

fn cgroup_contains_self(_cgroup_dir: &Path) -> BenchResult<bool> {
    unreachable!("oracle comparator tests do not inspect cgroup membership")
}

fn process_rss_bytes() -> BenchResult<u64> {
    unreachable!("oracle comparator tests do not inspect process RSS")
}

// This module includes only the scorecard helper.  Keep its newly shared
// resource-monitor boundary type-checkable without starting a monitor: these
// oracle/routing tests never call `run`.
#[derive(serde::Serialize)]
struct ResourceObservation;

struct ResourceMonitor;

impl ResourceMonitor {
    fn start_heavy(_cgroup_dir: &Path, _run_root: &Path) -> BenchResult<Self> {
        unreachable!("oracle comparator tests do not start a resource monitor")
    }

    fn finish(self) -> BenchResult<ResourceObservation> {
        unreachable!("oracle comparator tests do not collect resource observations")
    }
}

fn validate_resource_observation(_observation: &ResourceObservation) -> BenchResult {
    unreachable!("oracle comparator tests do not validate resource observations")
}

fn publication(response: Value) -> mpla_speed_scorecard::CliInvocation {
    mpla_speed_scorecard::CliInvocation {
        operation: "publish".to_string(),
        request_id: Some("request-1".to_string()),
        outer_elapsed_ns: 1,
        response,
    }
}

fn matching_publication() -> mpla_speed_scorecard::CliInvocation {
    publication(json!({
        "roots": {
            "root_id": "root-1",
            "attribution_root_id": "attribution-1"
        },
        "semantic": {
            "roots": {
                "root_id": "root-1",
                "attribution_root_id": "attribution-1"
            },
            "record_stream_sha256": "stream-1",
            "entry_count": 3
        }
    }))
}

fn matching_oracle() -> Value {
    json!({
        "root_id": "root-1",
        "attribution_root_id": "attribution-1",
        "record_stream_sha256": "stream-1",
        "entry_count": 3
    })
}

#[test]
fn rollback_sample_accepts_the_public_rollback_response_schema() -> BenchResult {
    let sample = mpla_speed_scorecard::rollback_sample(
        "rollback-00",
        mpla_speed_scorecard::CliInvocation {
            operation: "rollback_workspace_session".to_owned(),
            request_id: Some("rollback-sample-test".to_owned()),
            outer_elapsed_ns: 17,
            response: json!({
                "workspace_session_id": "session-1",
                "fresh_allocation_id": "allocation-1",
                "run_id": "run-1",
                "branch": "main",
                "target_branch": "rollback-target",
                "projection": {
                    "roots": {
                        "root_id": "root-1",
                        "attribution_root_id": "attribution-1",
                    },
                },
                "timings": {
                    "projection_elapsed_ns": 1,
                    "session_create_elapsed_ns": 2,
                    "storage_mount_elapsed_ns": 3,
                },
                "lifecycle": {
                    "selected_ref": "ref-1",
                },
                "service_elapsed_ns": 11,
            }),
        },
    )?;
    let sample = serde_json::to_value(sample)?;

    assert_eq!(sample["label"], "rollback-00");
    assert_eq!(sample["service_elapsed_ns"], 11);
    assert_eq!(sample["selected_ref"], "ref-1");
    assert_eq!(sample["timings"]["session_create_elapsed_ns"], 2);
    Ok(())
}

#[test]
fn exact_oracle_summary_is_accepted() {
    mpla_speed_scorecard::require_oracle_match(&matching_publication(), &matching_oracle())
        .expect("fully typed matching roots should pass");
}

#[test]
fn omitted_publication_roots_fail_closed() {
    let publication = publication(json!({
        "semantic": {
            "roots": {
                "root_id": "root-1",
                "attribution_root_id": "attribution-1"
            },
            "record_stream_sha256": "stream-1",
            "entry_count": 3
        }
    }));

    let error = mpla_speed_scorecard::require_oracle_match(&publication, &matching_oracle())
        .expect_err("missing top-level publication roots must not compare as equal");

    assert_eq!(error.to_string(), "publication omitted roots.root_id");
}

#[test]
fn omitted_oracle_fields_fail_closed() {
    let error = mpla_speed_scorecard::require_oracle_match(&matching_publication(), &json!({}))
        .expect_err("missing oracle fields must not compare as equal");

    assert_eq!(error.to_string(), "oracle omitted root_id");
}

#[test]
fn mismatched_stream_hash_is_rejected() {
    let mut oracle = matching_oracle();
    oracle["record_stream_sha256"] = json!("stream-2");

    let error = mpla_speed_scorecard::require_oracle_match(&matching_publication(), &oracle)
        .expect_err("different record streams must fail validation");

    assert!(error
        .to_string()
        .starts_with("publication and independent oracle differ:"));
}

#[test]
fn incremental_oracle_scans_the_activated_merged_workspace() {
    let command = mpla_speed_scorecard::merged_publication_oracle_command(
        Path::new("/eos/layer-stack/base/B000001-base/_campaign-tools/mpla-poc-oracle"),
        "/tmp/run-1.oracle.records",
        "sandbox-runtime-publication",
        "run-1",
    )
    .expect("fixed oracle path produces a command");
    let arguments = command.split_whitespace().collect::<Vec<_>>();
    let tree = arguments
        .windows(2)
        .find_map(|pair| (pair[0] == "--tree").then_some(pair[1]));
    let actor = arguments
        .windows(2)
        .find_map(|pair| (pair[0] == "--actor-id").then_some(pair[1]));
    let operation = arguments
        .windows(2)
        .find_map(|pair| (pair[0] == "--semantic-operation-id").then_some(pair[1]));

    assert_eq!(tree, Some("."));
    assert_eq!(actor, Some("sandbox-runtime-publication"));
    assert_eq!(operation, Some("run-1"));
}

#[test]
fn initial_oracle_scans_the_committed_main_branch() {
    assert_eq!(
        mpla_speed_scorecard::initial_publication_oracle_branch(),
        "main",
        "the initial source allocation is destroyed after publication, so the oracle must validate the committed branch"
    );
}

#[test]
fn coordinator_gateway_override_is_strictly_scorecard_scoped() {
    assert_eq!(
        mpla_speed_scorecard::approved_runtime_gateway_socket("host.docker.internal:7882")
            .expect("dedicated builder listener is allowed"),
        "host.docker.internal:7882"
    );
    let error = mpla_speed_scorecard::approved_runtime_gateway_socket("127.0.0.1:7882")
        .expect_err("loopback endpoint must not be caller-selectable");
    assert!(error.to_string().contains("host.docker.internal:7881-7903"));
}

#[test]
fn control_state_reclamation_removes_completed_pair() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time is after Unix epoch")
        .as_nanos();
    let parent = std::env::temp_dir().join(format!(
        "mpla-scorecard-control-reclamation-{}-{nonce}",
        std::process::id()
    ));
    let state_root = parent.join("pair-0");
    fs::create_dir_all(state_root.join("carrier")).expect("test control carrier tree is creatable");
    fs::write(state_root.join("carrier/receipt.json"), b"completed")
        .expect("test control receipt is writable");

    mpla_speed_scorecard::reclaim_control_state(&state_root)
        .expect("completed control state is reclaimed and parent is synchronized");

    assert!(
        parent.is_dir(),
        "control parent remains available for the next pair"
    );
    assert!(
        !state_root.exists(),
        "completed physical control carrier state must not accumulate"
    );
    fs::remove_dir_all(parent).expect("test parent cleanup succeeds");
}
