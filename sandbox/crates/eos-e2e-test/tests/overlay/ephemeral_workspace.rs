use anyhow::Result;
use eos_protocol::ops;
use serde_json::json;

use crate::common::{array, as_i64, as_str, live_pool_or_skip, stdout};

#[test]
fn exec_overlay_mount_publishes_changed_paths() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let exec = lease.call_ok(
        ops::API_V1_EXEC_COMMAND,
        json!({
            "cmd": "mkdir -p overlay && printf from-overlay > overlay/exec.txt",
            "yield_time_ms": 1000,
            "timeout_seconds": 10,
            "max_output_tokens": 1000
        }),
    )?;
    assert_eq!(as_str(&exec, "status")?, "ok");
    assert_eq!(as_i64(&exec, "exit_code")?, 0);
    assert!(
        array(&exec, "changed_paths")?
            .iter()
            .any(|path| path.as_str() == Some("overlay/exec.txt")),
        "exec overlay should publish captured upperdir paths: {exec}"
    );
    let read = lease.call_ok(ops::API_V1_READ_FILE, json!({"path": "overlay/exec.txt"}))?;
    assert_eq!(as_str(&read, "content")?, "from-overlay");
    Ok(())
}

#[test]
fn read_overlay_grep_returns_timings_and_cleanup() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    lease.call_ok(
        ops::API_V1_WRITE_FILE,
        json!({"path": "overlay/grep.txt", "content": "needle\n", "overwrite": true}),
    )?;
    let mut audit = lease.audit_tap()?;
    let grep = lease.call_ok(
        ops::API_V1_GREP,
        json!({"pattern": "needle", "path": "overlay", "output_mode": "content"}),
    )?;
    assert!(as_str(&grep, "content")?.contains("needle"));
    assert!(
        grep.get("timings")
            .and_then(|timings| timings.get("api.grep.total_s"))
            .is_some(),
        "overlay grep should expose protocol timing: {grep}"
    );
    audit.collect()?;
    assert!(
        audit.any("layer_stack.lease_released"),
        "overlay read should release its transient lease: {:?}",
        audit.events()
    );
    if audit.any("overlay_workspace.cleanup") {
        assert!(
            audit
                .first("overlay_workspace.cleanup")
                .and_then(|event| event.get("payload"))
                .is_some(),
            "cleanup event should carry payload"
        );
    }
    Ok(())
}

#[test]
fn glob_merged_view_sees_newest_layer() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    lease.call_ok(
        ops::API_V1_WRITE_FILE,
        json!({"path": "overlay/newest.txt", "content": "old\n", "overwrite": true}),
    )?;
    lease.call_ok(
        ops::API_V1_WRITE_FILE,
        json!({"path": "overlay/newest.txt", "content": "new\n", "overwrite": true}),
    )?;
    let glob = lease.call_ok(ops::API_V1_GLOB, json!({"pattern": "overlay/*.txt"}))?;
    assert!(
        array(&glob, "filenames")?
            .iter()
            .any(|path| path.as_str() == Some("overlay/newest.txt")),
        "glob should see merged overlay view: {glob}"
    );
    let read = lease.call_ok(ops::API_V1_READ_FILE, json!({"path": "overlay/newest.txt"}))?;
    assert_eq!(as_str(&read, "content")?, "new\n");
    Ok(())
}

#[test]
fn read_only_overlay_does_not_publish_mutations() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    lease.call_ok(
        ops::API_V1_WRITE_FILE,
        json!({"path": "overlay/read-only.txt", "content": "needle\n", "overwrite": true}),
    )?;
    let before = lease.call_ok(ops::API_LAYER_METRICS, json!({}))?;
    let grep = lease.call_ok(
        ops::API_V1_GREP,
        json!({"pattern": "needle", "path": "overlay", "output_mode": "content"}),
    )?;
    assert_eq!(
        array(&grep, "changed_paths")?.len(),
        0,
        "grep is read-only: {grep}"
    );
    let after = lease.call_ok(ops::API_LAYER_METRICS, json!({}))?;
    assert_eq!(
        before.get("manifest_version"),
        after.get("manifest_version"),
        "read-only overlay should not publish a new manifest"
    );
    assert!(
        stdout(&grep).is_empty(),
        "grep response should use content field, not stdout"
    );
    Ok(())
}
