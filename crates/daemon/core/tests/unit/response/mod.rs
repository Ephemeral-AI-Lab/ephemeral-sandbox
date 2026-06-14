//! Saturating-converter semantics for daemon-emitted timings/metrics.

use super::parse_pressure_metrics;
use crate::response::u64_to_f64_saturating;

/// Regression: a workspace / upperdir / run-dir tree larger than ~4.29 GB must
/// NOT be silently clamped to `u32::MAX` on the `*_tree_bytes` wire path. The
/// daemon converter previously capped at `u32::MAX`; it now uses
/// the uncapped converter directly.
#[test]
fn tree_bytes_above_u32_max_are_not_clamped() {
    let five_gb: u64 = 5_000_000_000;
    assert!(five_gb > u64::from(u32::MAX));
    assert!(u64_to_f64_saturating(five_gb) > f64::from(u32::MAX));
}

#[test]
fn pressure_metrics_parse_some_and_full_levels() {
    let metrics = parse_pressure_metrics(
        "memory",
        "some avg10=1.25 avg60=0.50 avg300=0.10 total=42\nfull avg10=0.25 avg60=0.05 avg300=0.01 total=7\n",
    );

    assert_eq!(metrics.get("memory_some_avg10").copied(), Some(1.25));
    assert_eq!(metrics.get("memory_some_total").copied(), Some(42.0));
    assert_eq!(metrics.get("memory_full_avg60").copied(), Some(0.05));
    assert_eq!(metrics.get("memory_full_total").copied(), Some(7.0));
}
