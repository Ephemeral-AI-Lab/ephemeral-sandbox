//! Offline unit coverage for the Phase 4 Stage 1 observability node projection.
//! Feeds synthetic `get_observability_tree` nodes through
//! `report::observability_node_from_tree` and asserts the P1 verdict, warning
//! strings, and bounds. Pure: no Docker, no gateway, no run artifacts.

use sandbox_e2e_live_test::report::{
    observability_node_from_tree, ObsPollMeta, ObsSourceCall, ObservabilitySnapshot,
    OBSERVABILITY_SCHEMA_VERSION,
};
use serde_json::json;

#[test]
fn p1_is_unavailable_when_cgroup_reports_unavailable() {
    let node = json!({
        "sandbox_id": "sb-1",
        "lifecycle_state": "ready",
        "availability": "available",
        "sampled_at_unix_ms": 1_700_000_000_000_i64,
        "errors": [],
        "resources": {
            "latest": {
                "sampled_at_unix_ms": 1_700_000_000_000_i64,
                "cgroup": {
                    "available": false,
                    "cpu_usage_usec": null,
                    "memory_current_bytes": null,
                    "memory_max_bytes": null,
                    "memory_max_unlimited": null,
                    "error": "cgroup path unavailable"
                },
                "disk": {}
            },
            "history": []
        },
        "workspaces": [],
        "recent_traces": []
    });

    let (projected, p1, warnings) = observability_node_from_tree("sb-1", &node);

    assert!(
        !p1.available,
        "cgroup.available == false yields P1 unavailable"
    );
    assert_eq!(
        p1.reason.as_deref(),
        Some("cgroup unavailable: cgroup path unavailable")
    );
    assert!(p1.cpu_usage_usec.is_none());
    assert_eq!(projected.availability.as_deref(), Some("available"));
    assert_eq!(projected.workspace_count, 0);
    assert!(
        warnings
            .iter()
            .any(|warning| warning == "P1 unavailable for sb-1: cgroup unavailable"),
        "expected the cgroup-unavailable warning, got {warnings:?}"
    );
}

#[test]
fn p1_is_available_when_cgroup_reports_counters() {
    let node = json!({
        "sandbox_id": "sb-2",
        "availability": "available",
        "resources": {
            "latest": {
                "sampled_at_unix_ms": 1_700_000_000_000_i64,
                "cgroup": {
                    "available": true,
                    "cpu_usage_usec": 4_096_i64,
                    "memory_current_bytes": 8_192_i64,
                    "memory_max_bytes": 16_384_i64,
                    "memory_max_unlimited": false,
                    "error": null
                },
                "disk": {}
            },
            "history": []
        },
        "workspaces": [],
        "recent_traces": []
    });

    let (_node, p1, warnings) = observability_node_from_tree("sb-2", &node);

    assert!(
        p1.available,
        "available cgroup with counters yields P1 available"
    );
    assert_eq!(p1.cpu_usage_usec, Some(4_096));
    assert_eq!(p1.memory_current_bytes, Some(8_192));
    assert!(p1.reason.is_none());
    assert!(
        warnings.is_empty(),
        "available P1 emits no warning, got {warnings:?}"
    );
}

#[test]
fn p1_is_partial_when_available_but_counters_absent() {
    let node = json!({
        "sandbox_id": "sb-3",
        "availability": "available",
        "resources": {
            "latest": {
                "sampled_at_unix_ms": 1_700_000_000_000_i64,
                "cgroup": {
                    "available": true,
                    "cpu_usage_usec": null,
                    "memory_current_bytes": null,
                    "memory_max_bytes": null,
                    "memory_max_unlimited": null,
                    "error": null
                },
                "disk": {}
            },
            "history": []
        },
        "workspaces": [],
        "recent_traces": []
    });

    let (_node, p1, warnings) = observability_node_from_tree("sb-3", &node);

    assert!(!p1.available);
    assert_eq!(
        p1.reason.as_deref(),
        Some("cgroup available but counters absent")
    );
    assert!(warnings
        .iter()
        .any(|warning| warning == "P1 partial for sb-3: counters absent"));
}

#[test]
fn p1_is_unavailable_when_no_resource_sample() {
    let node = json!({
        "sandbox_id": "sb-4",
        "availability": "available",
        "resources": { "latest": null, "history": [] },
        "workspaces": [],
        "recent_traces": []
    });

    let (_node, p1, warnings) = observability_node_from_tree("sb-4", &node);

    assert!(!p1.available);
    assert_eq!(p1.reason.as_deref(), Some("no resource sample"));
    assert!(warnings
        .iter()
        .any(|warning| warning == "P1 unavailable for sb-4: no resource sample"));
}

#[test]
fn unavailable_node_records_a_warning() {
    let node = json!({
        "sandbox_id": "sb-5",
        "availability": "unavailable",
        "errors": ["daemon endpoint unreachable"],
        "resources": { "latest": null, "history": [] },
        "workspaces": [],
        "recent_traces": []
    });

    let (projected, _p1, warnings) = observability_node_from_tree("sb-5", &node);

    assert_eq!(projected.availability.as_deref(), Some("unavailable"));
    assert!(
        warnings
            .iter()
            .any(|warning| warning == "node unavailable for sb-5"),
        "expected the unavailable-node warning, got {warnings:?}"
    );
}

#[test]
fn recent_traces_and_history_are_capped_with_warnings() {
    let traces: Vec<_> = (0..120)
        .map(|index| {
            json!({
                "trace_id": format!("trace-{index}"),
                "kind": "request",
                "operation": "create_sandbox",
                "status": "ok",
                "duration_ms": 3_i64,
                "error_kind": null
            })
        })
        .collect();
    let history: Vec<_> = (0..70)
        .map(|index| {
            json!({
                "sampled_at_unix_ms": 1_700_000_000_000_i64 + i64::from(index),
                "cgroup": { "available": false, "error": "cgroup path unavailable" },
                "disk": {}
            })
        })
        .collect();
    let node = json!({
        "sandbox_id": "sb-6",
        "availability": "available",
        "resources": { "latest": null, "history": history },
        "workspaces": [{}, {}],
        "recent_traces": traces
    });

    let (projected, _p1, warnings) = observability_node_from_tree("sb-6", &node);

    assert_eq!(
        projected.recent_traces.len(),
        50,
        "recent traces capped at 50"
    );
    assert_eq!(
        projected.resources.history.len(),
        50,
        "history capped at 50"
    );
    assert_eq!(projected.workspace_count, 2);
    assert_eq!(
        projected.recent_traces[0].operation.as_deref(),
        Some("create_sandbox")
    );
    assert!(
        warnings
            .iter()
            .any(|warning| warning.starts_with("recent_traces truncated for sb-6")),
        "expected a recent-traces truncation warning, got {warnings:?}"
    );
    assert!(
        warnings
            .iter()
            .any(|warning| warning.starts_with("resource history truncated for sb-6")),
        "expected a history truncation warning, got {warnings:?}"
    );
}

#[test]
fn snapshot_serializes_with_the_artifact_schema() {
    let node = json!({
        "sandbox_id": "sb-7",
        "lifecycle_state": "ready",
        "availability": "available",
        "sampled_at_unix_ms": 1_700_000_000_000_i64,
        "errors": [],
        "resources": {
            "latest": {
                "sampled_at_unix_ms": 1_700_000_000_000_i64,
                "cgroup": {
                    "available": false,
                    "cpu_usage_usec": null,
                    "memory_current_bytes": null,
                    "memory_max_bytes": null,
                    "memory_max_unlimited": null,
                    "error": "cgroup path unavailable"
                },
                "disk": { "upperdir_bytes": 0 }
            },
            "history": []
        },
        "workspaces": [],
        "recent_traces": []
    });
    let (projected, p1, warnings) = observability_node_from_tree("sb-7", &node);

    let snapshot = ObservabilitySnapshot {
        schema_version: OBSERVABILITY_SCHEMA_VERSION,
        sandbox_id: "sb-7".to_owned(),
        captured_at: "20260101T000000Z".to_owned(),
        source_call: ObsSourceCall {
            argv: vec![
                "--gateway-socket".to_owned(),
                "/tmp/gw.sock".to_owned(),
                "manager".to_owned(),
                "get_observability_tree".to_owned(),
                "--resource-window-ms".to_owned(),
                "60000".to_owned(),
            ],
            exit_code: 0,
            latency_ms: 12,
        },
        poll_meta: ObsPollMeta {
            cycles_observed: 3,
            last_cycle_index: 7,
        },
        node: projected,
        p1,
        warnings,
    };

    let value = serde_json::to_value(&snapshot).expect("snapshot serializes");
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["sandbox_id"], "sb-7");
    assert_eq!(value["p1"]["available"], false);
    assert!(
        value["p1"]["reason"].is_string(),
        "p1.reason carries the degraded-resolution reason"
    );
    assert_eq!(value["node"]["availability"], "available");
    assert_eq!(value["node"]["workspace_count"], 0);
    let argv = value["source_call"]["argv"]
        .as_array()
        .expect("source_call.argv is an array");
    assert!(
        argv.iter().any(|entry| entry == "--resource-window-ms"),
        "expected --resource-window-ms in source_call.argv"
    );
    assert!(
        !argv
            .iter()
            .any(|entry| entry == "--include-recent-traces" || entry == "--trace-limit"),
        "source_call.argv should reflect the current public CLI shape"
    );
}
