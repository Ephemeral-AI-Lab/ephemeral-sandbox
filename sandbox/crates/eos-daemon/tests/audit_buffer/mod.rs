use serde_json::json;

use super::*;

#[test]
fn pull_preserves_negative_cursor_when_no_events_match() {
    let buffer = AuditBuffer::with_caps(10, 1024, Some(1));

    let pulled = buffer.pull(-3, 10);

    assert_eq!(pulled["cursor"]["after_seq"], json!(-3));
    assert_eq!(pulled["events"], json!([]));
}

#[test]
fn pull_filters_after_seq_with_checked_cursor_conversion() {
    let buffer = AuditBuffer::with_caps(10, 1024, Some(1));
    assert_eq!(buffer.append(json!({"event": "first"}), Lane::Normal), 0);
    assert_eq!(buffer.append(json!({"event": "second"}), Lane::Normal), 1);

    let all = buffer.pull(-1, 10);
    let after_zero = buffer.pull(0, 10);

    assert_eq!(all["events"].as_array().map_or(0, Vec::len), 2);
    assert_eq!(after_zero["events"].as_array().map_or(0, Vec::len), 1);
    assert_eq!(after_zero["events"][0]["seq"], json!(1));
}

#[test]
fn integer_size_helpers_saturate_at_wire_limits() {
    assert_eq!(u128_to_i64_saturating(i64::MAX as u128 + 1), i64::MAX);
    assert_eq!(usize_to_u64_saturating(7), 7);
}
