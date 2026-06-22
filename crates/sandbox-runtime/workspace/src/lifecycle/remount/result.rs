use std::path::PathBuf;

use serde_json::Value;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemountProbe {
    pub path: Option<PathBuf>,
    pub expected_content: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemountOverlayResult {
    pub mount_verified: bool,
    pub failure_summary: Option<String>,
}

impl RemountOverlayResult {
    #[must_use]
    pub fn from_payload(payload: &Value) -> Self {
        let mount_verified = payload
            .get("mount_verified")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let failure_summary = (!mount_verified).then(|| failure_summary_from_payload(payload));
        emit_remount_overlay_result(payload, mount_verified);
        Self {
            mount_verified,
            failure_summary,
        }
    }

    #[must_use]
    pub fn failure_summary(&self) -> &str {
        self.failure_summary
            .as_deref()
            .unwrap_or("mount overlay verification failed")
    }
}

fn failure_summary_from_payload(payload: &Value) -> String {
    if let Some(error) = optional_string(payload, "probe_error") {
        return format!("probe failed: {error}");
    }
    if let Some(error) = optional_string(payload, "rollback_unmount_error") {
        return format!("rollback cleanup failed: {error}");
    }
    format!(
        "mount_verified={}, staged_switch={:?}, staging_verified={:?}, rollback_unmounted={:?}, fs_type={:?}, lowerdir_count={:?}, lowerdir_expected_count={:?}, lowerdir_count_matched={:?}, lowerdir_verified={:?}, probe_read_ok={:?}, probe_content_matched={:?}",
        optional_bool(payload, "mount_verified").unwrap_or(false),
        optional_bool(payload, "staged_switch"),
        optional_bool(payload, "staging_verified"),
        optional_bool(payload, "rollback_unmounted"),
        optional_string(payload, "mountinfo_fs_type"),
        optional_usize(payload, "mountinfo_lowerdir_count"),
        optional_usize(payload, "mountinfo_lowerdir_expected_count"),
        optional_bool(payload, "mountinfo_lowerdir_count_matched"),
        optional_bool(payload, "mountinfo_lowerdir_verified"),
        optional_bool(payload, "probe_read_ok"),
        optional_bool(payload, "probe_content_matched"),
    )
}

fn emit_remount_overlay_result(payload: &Value, mount_verified: bool) {
    tracing::info!(
        name: "workspace_remount_overlay_result",
        mount_verified = mount_verified,
        failure_reason = remount_failure_reason(payload, mount_verified),
        staged_switch = optional_bool(payload, "staged_switch").unwrap_or(false),
        staged_switch_present = optional_bool(payload, "staged_switch").is_some(),
        staging_verified = optional_bool(payload, "staging_verified").unwrap_or(false),
        staging_verified_present = optional_bool(payload, "staging_verified").is_some(),
        rollback_unmounted = optional_bool(payload, "rollback_unmounted").unwrap_or(false),
        rollback_unmounted_present = optional_bool(payload, "rollback_unmounted").is_some(),
        lowerdir_count = optional_usize(payload, "mountinfo_lowerdir_count").unwrap_or(0) as u64,
        lowerdir_count_present = optional_usize(payload, "mountinfo_lowerdir_count").is_some(),
        lowerdir_expected_count =
            optional_usize(payload, "mountinfo_lowerdir_expected_count").unwrap_or(0) as u64,
        lowerdir_expected_count_present =
            optional_usize(payload, "mountinfo_lowerdir_expected_count").is_some(),
        lowerdir_count_matched =
            optional_bool(payload, "mountinfo_lowerdir_count_matched").unwrap_or(false),
        lowerdir_count_matched_present =
            optional_bool(payload, "mountinfo_lowerdir_count_matched").is_some(),
        lowerdir_verified = optional_bool(payload, "mountinfo_lowerdir_verified").unwrap_or(false),
        lowerdir_verified_present = optional_bool(payload, "mountinfo_lowerdir_verified").is_some(),
        probe_read_ok = optional_bool(payload, "probe_read_ok").unwrap_or(false),
        probe_read_ok_present = optional_bool(payload, "probe_read_ok").is_some(),
        probe_content_matched = optional_bool(payload, "probe_content_matched").unwrap_or(false),
        probe_content_matched_present = optional_bool(payload, "probe_content_matched").is_some(),
    );
}

fn remount_failure_reason(payload: &Value, mount_verified: bool) -> &'static str {
    if mount_verified {
        "none"
    } else if payload.get("probe_error").and_then(Value::as_str).is_some() {
        "probe_failed"
    } else if payload
        .get("rollback_unmount_error")
        .and_then(Value::as_str)
        .is_some()
    {
        "rollback_cleanup_failed"
    } else {
        "verification_failed"
    }
}

fn optional_bool(payload: &Value, key: &str) -> Option<bool> {
    payload.get(key).and_then(Value::as_bool)
}

fn optional_usize(payload: &Value, key: &str) -> Option<usize> {
    payload
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
}

fn optional_string(payload: &Value, key: &str) -> Option<String> {
    payload.get(key).and_then(Value::as_str).map(str::to_owned)
}
