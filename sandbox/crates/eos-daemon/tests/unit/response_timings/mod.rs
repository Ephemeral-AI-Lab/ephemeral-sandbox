//! Saturating-converter semantics for daemon-emitted timings/metrics.

use crate::response_timings::u64_to_f64_saturating;

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
