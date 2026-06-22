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
