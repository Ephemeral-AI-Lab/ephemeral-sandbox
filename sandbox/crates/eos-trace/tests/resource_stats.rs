use eos_trace::{BoundedJson, DetailBudget, ResourceStats, ResourceStatsKind};
use serde_json::json;

#[test]
fn unavailable_stats_record_source_errors_explicitly() {
    let stats = ResourceStats::unavailable(
        ResourceStatsKind::CgroupProcess,
        Some("before".to_owned()),
        "/sys/fs/cgroup/cpu.stat",
        Some("permission denied".to_owned()),
        None,
        35,
        2,
    );

    assert!(!stats.meta.source_available);
    assert_eq!(stats.meta.read_error.as_deref(), Some("permission denied"));
    assert_eq!(stats.meta.inflight_requests, 2);
}

#[test]
fn bounded_json_overflow_keeps_digest_and_original_length() {
    let bounded = BoundedJson::capture(
        json!({"payload": "x".repeat(128)}),
        DetailBudget::Custom(16),
    );

    assert!(bounded.truncated);
    assert_eq!(bounded.sha256.as_deref().expect("digest").len(), 64);
    assert!(bounded.original_len > 16);
    assert_eq!(bounded.value["truncated"], true);
}
