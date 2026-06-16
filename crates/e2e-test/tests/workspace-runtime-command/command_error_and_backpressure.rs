use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Result};
use protocol::catalog;
use serde_json::{json, Value};

use crate::support::{
    ack_trace_export, array, as_bool, as_i64, as_str, clean_stdout, container_path_exists,
    finalize_foreground_command, has_trace_event, live_pool_or_skip, stdout, trace_export_records,
    trace_record, unwrap_operation_result, wait_for_active_leases, wait_for_command_count,
    wait_for_command_stdout_contains, wait_for_command_transcript_recycled,
};

#[test]
fn nonzero_exit_and_stderr_are_structured() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let failed = lease.call(
        catalog::SANDBOX_COMMAND_EXEC,
        json!({
            "cmd": "sh -c 'printf stdout-before; printf stderr-before >&2; exit 42'",
            "yield_time_ms": 1000,
            "timeout_seconds": 10,}),
    )?;
    // Under emulation a slow ns-runner spawn can outlast the yield, so the
    // command returns "running"; finalize it to its terminal outcome first.
    let failed =
        finalize_foreground_command(&lease, failed, Instant::now() + Duration::from_secs(20))?;
    ensure!(
        as_str(&failed, "status")? == "error",
        "nonzero command should return an error status: {failed}"
    );
    ensure!(
        as_i64(&failed, "exit_code")? == 42,
        "nonzero command should preserve its exit code: {failed}"
    );
    let output = stdout(&failed);
    ensure!(
        output.contains("stdout-before") && output.contains("stderr-before"),
        "PTY output should merge stdout and stderr into the model stream: {failed}"
    );
    ensure!(
        stderr(&failed).is_empty(),
        "stderr field should stay empty for merged PTY output: {failed}"
    );
    wait_for_command_count(&lease, 0)?;
    wait_for_active_leases(&lease, 0)?;
    Ok(())
}

#[test]
fn nonzero_exit_discards_source_and_ignored_writes_with_publish_lanes() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let dir = format!(
        "publish-lanes-failure/{}",
        e2e_test::unique_suffix().replace('-', "_")
    );
    let source_path = format!("{dir}/source.txt");
    let ignored_path = format!("{dir}/cache/ignored.txt");

    lease.call_ok(
        catalog::SANDBOX_FILE_WRITE,
        json!({
            "path": format!("{dir}/.gitignore"),
            "content": "cache/\n",
            "overwrite": false,
        }),
    )?;
    let before = lease.call_ok(catalog::SANDBOX_CHECKPOINT_LAYER_METRICS, json!({}))?;
    let before_version = as_i64(&before, "manifest_version")?;

    let wire = lease.call(
        catalog::SANDBOX_COMMAND_EXEC,
        json!({
            "cmd": format!(
                "mkdir -p {dir}/cache && printf source > {source_path} && printf ignored > {ignored_path} && exit 42"
            ),
            "yield_time_ms": 8000,
            "timeout_seconds": 10,
        }),
    )?;
    let running_command_id = running_command_id_from_response(&wire)?;
    let result = finalize_foreground_command(
        &lease,
        wire.clone(),
        Instant::now() + Duration::from_secs(30),
    )?;
    ensure!(
        as_str(&result, "status")? == "error",
        "nonzero command should finalize in the foreground for publish-lane trace coverage: {result}"
    );
    ensure!(as_i64(&result, "exit_code")? == 42, "{result}");
    ensure!(
        array(&result, "changed_paths")?.is_empty(),
        "failed command must not publish changed paths: {result}"
    );

    let lanes = &result["publish_lanes"];
    ensure!(
        lanes["source"]["publish_status"] == "dropped_command_failed",
        "source lane must be marked dropped on command failure: {result}"
    );
    ensure!(
        lanes["ignored"]["publish_status"] == "dropped_command_failed",
        "ignored lane must be marked dropped on command failure: {result}"
    );
    ensure!(
        lanes["source"]["path_count"] == 1 && lanes["ignored"]["path_count"] == 1,
        "failed command should report one routed source path and one ignored path: {result}"
    );
    ensure!(
        lanes["routing"]["ignore_route_source"] == "command_snapshot",
        "publish lanes must report snapshot-scoped routing: {result}"
    );

    assert_command_publish_lanes_trace(
        &lease,
        &wire,
        running_command_id.as_deref(),
        Instant::now() + Duration::from_secs(10),
        |details| {
            details["source"]["publish_status"] == "dropped_command_failed"
                && details["ignored"]["publish_status"] == "dropped_command_failed"
                && details["source"]["path_count"] == 1
                && details["ignored"]["path_count"] == 1
                && details["routing"]["ignore_route_source"] == "command_snapshot"
        },
        "command finalize trace must include publish_lanes_decided",
    )?;

    for path in [&source_path, &ignored_path] {
        let read = lease.call_ok(catalog::SANDBOX_FILE_READ, json!({"path": path}))?;
        ensure!(
            !as_bool(&read, "exists")?,
            "failed command write {path} must not publish: {read}"
        );
    }
    let after = lease.call_ok(catalog::SANDBOX_CHECKPOINT_LAYER_METRICS, json!({}))?;
    ensure!(
        as_i64(&after, "manifest_version")? == before_version,
        "failed command must not advance manifest: before={before}, after={after}"
    );
    wait_for_command_count(&lease, 0)?;
    wait_for_active_leases(&lease, 0)?;
    Ok(())
}

#[test]
fn git_metadata_write_rejects_publish_and_ignored_lane() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let dir = format!(
        "publish-lanes-git-reject/{}",
        e2e_test::unique_suffix().replace('-', "_")
    );
    let git_path = format!("{dir}/.git/config");
    let ignored_path = format!("{dir}/cache/ignored.txt");

    lease.call_ok(
        catalog::SANDBOX_FILE_WRITE,
        json!({
            "path": format!("{dir}/.gitignore"),
            "content": ".git/\ncache/\n",
            "overwrite": false,
        }),
    )?;

    let wire = lease.call(
        catalog::SANDBOX_COMMAND_EXEC,
        json!({
            "cmd": format!(
                "mkdir -p {dir}/.git {dir}/cache && printf '[core]\\nrepositoryformatversion = 0\\n' > {git_path} && printf ignored > {ignored_path}"
            ),
            "yield_time_ms": 8000,
            "timeout_seconds": 10,
        }),
    )?;
    let running_command_id = running_command_id_from_response(&wire)?;
    let result = finalize_foreground_command(
        &lease,
        wire.clone(),
        Instant::now() + Duration::from_secs(30),
    )?;
    ensure!(
        as_str(&result, "status")? == "ok",
        "successful command should finalize in foreground: {result}"
    );
    ensure!(
        !as_bool(&result, "success")?,
        "git metadata rejection must fail the workspace mutation: {result}"
    );

    let lanes = &result["publish_lanes"];
    ensure!(
        lanes["source"]["publish_status"] == "empty",
        "git metadata must not be treated as source: {result}"
    );
    ensure!(
        lanes["ignored"]["publish_status"] == "failed",
        "ordinary ignored output must not publish when git metadata rejects: {result}"
    );
    ensure!(
        lanes["ignored"]["path_count"] == 1,
        "only cache output should be counted as ignored: {result}"
    );
    ensure!(
        lanes["routing"]["dropped_path_count"] == 1
            && lanes["routing"]["drop_reason_counts"]["git_metadata_unsupported"] == 1,
        "git metadata rejection reason must be surfaced: {result}"
    );

    assert_command_publish_lanes_trace(
        &lease,
        &wire,
        running_command_id.as_deref(),
        Instant::now() + Duration::from_secs(10),
        |details| {
            details["routing"]["dropped_path_count"] == 1
                && details["routing"]["drop_reason_counts"]["git_metadata_unsupported"] == 1
                && details["ignored"]["publish_status"] == "failed"
        },
        "command finalize trace must include git metadata rejection reason",
    )?;

    let read_git = lease.call_ok(catalog::SANDBOX_FILE_READ, json!({"path": git_path}))?;
    ensure!(
        !as_bool(&read_git, "exists")?,
        ".git metadata must not publish through ordinary command output: {read_git}"
    );
    let read_ignored = lease.call_ok(catalog::SANDBOX_FILE_READ, json!({"path": ignored_path}))?;
    ensure!(
        !as_bool(&read_ignored, "exists")?,
        "ignored output must not publish when git metadata rejects: {read_ignored}"
    );
    wait_for_command_count(&lease, 0)?;
    wait_for_active_leases(&lease, 0)?;
    Ok(())
}

#[test]
fn git_add_without_commit_rejects_durable_staged_index() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    seed_real_git_workspace(&lease)?;
    let before = manifest_version(&lease)?;

    let wire = lease.call(
        catalog::SANDBOX_COMMAND_EXEC,
        json!({
            "cmd": "printf staged > staged.txt && git add staged.txt",
            "yield_time_ms": 8000,
            "timeout_seconds": 10,
        }),
    )?;
    let result = finalize_foreground_command(
        &lease,
        wire.clone(),
        Instant::now() + Duration::from_secs(30),
    )?;
    ensure_git_publish_rejected(
        &lease,
        &wire,
        &result,
        "git_index_staged_state",
        before,
        "git add without commit must reject durable staged index state",
    )?;
    let read = lease.call_ok(catalog::SANDBOX_FILE_READ, json!({"path": "staged.txt"}))?;
    ensure!(
        !as_bool(&read, "exists")?,
        "source file from rejected git add must not publish: {read}"
    );
    Ok(())
}

#[test]
fn git_leftover_lock_rejects_publish() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    seed_real_git_workspace(&lease)?;
    let before = manifest_version(&lease)?;

    let wire = lease.call(
        catalog::SANDBOX_COMMAND_EXEC,
        json!({
            "cmd": "printf lock > .git/index.lock",
            "yield_time_ms": 8000,
            "timeout_seconds": 10,
        }),
    )?;
    let result = finalize_foreground_command(
        &lease,
        wire.clone(),
        Instant::now() + Duration::from_secs(30),
    )?;
    ensure_git_publish_rejected(
        &lease,
        &wire,
        &result,
        "git_lock_file",
        before,
        "leftover git lock must reject publish",
    )?;
    Ok(())
}

#[test]
fn git_hook_write_rejects_publish() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    seed_real_git_workspace(&lease)?;
    let before = manifest_version(&lease)?;

    let wire = lease.call(
        catalog::SANDBOX_COMMAND_EXEC,
        json!({
            "cmd": "printf '#!/bin/sh\\nexit 0\\n' > .git/hooks/pre-commit",
            "yield_time_ms": 8000,
            "timeout_seconds": 10,
        }),
    )?;
    let result = finalize_foreground_command(
        &lease,
        wire.clone(),
        Instant::now() + Duration::from_secs(30),
    )?;
    ensure_git_publish_rejected(
        &lease,
        &wire,
        &result,
        "git_hook_write",
        before,
        "git hook writes must reject publish",
    )?;
    Ok(())
}

#[test]
fn deleting_git_head_and_object_rejects_publish() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let object_path = seed_real_git_workspace_with_loose_object(&lease)?;
    let before = manifest_version(&lease)?;

    let wire = lease.call(
        catalog::SANDBOX_COMMAND_EXEC,
        json!({
            "cmd": format!("rm -f .git/HEAD {object_path}"),
            "yield_time_ms": 8000,
            "timeout_seconds": 10,
        }),
    )?;
    let result = finalize_foreground_command(
        &lease,
        wire.clone(),
        Instant::now() + Duration::from_secs(30),
    )?;
    ensure_git_publish_rejected(
        &lease,
        &wire,
        &result,
        "git_metadata_delete",
        before,
        "deleting HEAD or objects must reject publish",
    )?;
    let head = lease.call_ok(catalog::SANDBOX_FILE_READ, json!({"path": ".git/HEAD"}))?;
    ensure!(
        as_bool(&head, "exists")?,
        "rejected .git/HEAD deletion must leave shared HEAD visible: {head}"
    );
    let object = lease.call_ok(catalog::SANDBOX_FILE_READ, json!({"path": object_path}))?;
    ensure!(
        as_bool(&object, "exists")?,
        "rejected object deletion must leave shared object visible: {object}"
    );
    Ok(())
}

#[test]
fn unsupported_special_file_is_dropped_with_publish_lane_reason() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let dir = format!(
        "publish-lanes-special-drop/{}",
        e2e_test::unique_suffix().replace('-', "_")
    );
    let fifo_path = format!("{dir}/run.fifo");
    let ignored_path = format!("{dir}/cache/ignored.txt");

    lease.call_ok(
        catalog::SANDBOX_FILE_WRITE,
        json!({
            "path": format!("{dir}/.gitignore"),
            "content": "cache/\n",
            "overwrite": false,
        }),
    )?;

    let wire = lease.call(
        catalog::SANDBOX_COMMAND_EXEC,
        json!({
            "cmd": format!(
                "mkdir -p {dir}/cache && mkfifo {fifo_path} && printf ignored > {ignored_path}"
            ),
            "yield_time_ms": 8000,
            "timeout_seconds": 10,
        }),
    )?;
    let running_command_id = running_command_id_from_response(&wire)?;
    let result = finalize_foreground_command(
        &lease,
        wire.clone(),
        Instant::now() + Duration::from_secs(30),
    )?;
    ensure!(
        as_str(&result, "status")? == "ok",
        "successful command should finalize in foreground: {result}"
    );

    let lanes = &result["publish_lanes"];
    ensure!(
        lanes["source"]["publish_status"] == "empty",
        "unsupported special file must not be treated as source: {result}"
    );
    ensure!(
        lanes["ignored"]["publish_status"] == "published_lww",
        "ordinary ignored output should still publish when a special file is dropped: {result}"
    );
    ensure!(
        lanes["ignored"]["path_count"] == 1,
        "only cache output should be counted as ignored: {result}"
    );
    ensure!(
        lanes["routing"]["dropped_path_count"] == 1
            && lanes["routing"]["drop_reason_counts"]["unsupported_special_file"] == 1,
        "special file drop reason must be surfaced: {result}"
    );

    assert_command_publish_lanes_trace(
        &lease,
        &wire,
        running_command_id.as_deref(),
        Instant::now() + Duration::from_secs(10),
        |details| {
            details["routing"]["dropped_path_count"] == 1
                && details["routing"]["drop_reason_counts"]["unsupported_special_file"] == 1
                && details["ignored"]["publish_status"] == "published_lww"
        },
        "command finalize trace must include special file drop reason",
    )?;

    let read_fifo = lease.call_ok(catalog::SANDBOX_FILE_READ, json!({"path": fifo_path}))?;
    ensure!(
        !as_bool(&read_fifo, "exists")?,
        "unsupported special file must not publish through ordinary command output: {read_fifo}"
    );
    let read_ignored = lease.call_ok(catalog::SANDBOX_FILE_READ, json!({"path": ignored_path}))?;
    ensure!(
        as_bool(&read_ignored, "exists")?,
        "ignored output should remain publishable when a special file is dropped: {read_ignored}"
    );
    ensure!(
        as_str(&read_ignored, "content")? == "ignored",
        "ignored output content should publish unchanged: {read_ignored}"
    );
    wait_for_command_count(&lease, 0)?;
    wait_for_active_leases(&lease, 0)?;
    Ok(())
}

#[test]
fn command_scratch_path_is_dropped_with_publish_lane_reason() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let dir = format!(
        "publish-lanes-command-scratch-drop/{}",
        e2e_test::unique_suffix().replace('-', "_")
    );
    let scratch_path = format!("{dir}/.eos-command/cmd/final.json");
    let ignored_path = format!("{dir}/cache/ignored.txt");

    lease.call_ok(
        catalog::SANDBOX_FILE_WRITE,
        json!({
            "path": format!("{dir}/.gitignore"),
            "content": "cache/\n",
            "overwrite": false,
        }),
    )?;

    let wire = lease.call(
        catalog::SANDBOX_COMMAND_EXEC,
        json!({
            "cmd": format!(
                "mkdir -p {dir}/.eos-command/cmd {dir}/cache && printf '{{}}' > {scratch_path} && printf ignored > {ignored_path}"
            ),
            "yield_time_ms": 8000,
            "timeout_seconds": 10,
        }),
    )?;
    let running_command_id = running_command_id_from_response(&wire)?;
    let result = finalize_foreground_command(
        &lease,
        wire.clone(),
        Instant::now() + Duration::from_secs(30),
    )?;
    ensure!(
        as_str(&result, "status")? == "ok",
        "successful command should finalize in foreground: {result}"
    );

    let lanes = &result["publish_lanes"];
    ensure!(
        lanes["source"]["publish_status"] == "empty",
        "command scratch output must not be treated as source: {result}"
    );
    ensure!(
        lanes["ignored"]["publish_status"] == "published_lww",
        "ordinary ignored output should still publish when scratch output is dropped: {result}"
    );
    ensure!(
        lanes["ignored"]["path_count"] == 1,
        "only cache output should be counted as ignored: {result}"
    );
    ensure!(
        lanes["routing"]["dropped_path_count"] == 1
            && lanes["routing"]["drop_reason_counts"]["command_scratch_path"] == 1,
        "command scratch drop reason must be surfaced: {result}"
    );

    assert_command_publish_lanes_trace(
        &lease,
        &wire,
        running_command_id.as_deref(),
        Instant::now() + Duration::from_secs(10),
        |details| {
            details["routing"]["dropped_path_count"] == 1
                && details["routing"]["drop_reason_counts"]["command_scratch_path"] == 1
                && details["ignored"]["publish_status"] == "published_lww"
        },
        "command finalize trace must include command scratch drop reason",
    )?;

    let read_scratch = lease.call_ok(catalog::SANDBOX_FILE_READ, json!({"path": scratch_path}))?;
    ensure!(
        !as_bool(&read_scratch, "exists")?,
        "command scratch output must not publish through ordinary command output: {read_scratch}"
    );
    let read_ignored = lease.call_ok(catalog::SANDBOX_FILE_READ, json!({"path": ignored_path}))?;
    ensure!(
        as_bool(&read_ignored, "exists")?,
        "ignored output should remain publishable when command scratch output is dropped: {read_ignored}"
    );
    ensure!(
        as_str(&read_ignored, "content")? == "ignored",
        "ignored output content should publish unchanged: {read_ignored}"
    );
    wait_for_command_count(&lease, 0)?;
    wait_for_active_leases(&lease, 0)?;
    Ok(())
}

#[test]
fn oversized_ignored_output_drops_ignored_lane_but_publishes_source() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let dir = format!(
        "publish-lanes-ignored-limit/{}",
        e2e_test::unique_suffix().replace('-', "_")
    );
    let source_path = format!("{dir}/src.txt");
    let ignored_path = format!("{dir}/ignored/huge.bin");
    let ignored_len = 8193;

    lease.call_ok(
        catalog::SANDBOX_FILE_WRITE,
        json!({
            "path": format!("{dir}/.gitignore"),
            "content": "ignored/\n",
            "overwrite": false,
        }),
    )?;

    let wire = lease.call(
        catalog::SANDBOX_COMMAND_EXEC,
        json!({
            "cmd": format!(
                "mkdir -p {dir}/ignored && printf source > {source_path} && python3 - <<'PY'\nfrom pathlib import Path\np = Path('{ignored_path}')\np.parent.mkdir(parents=True, exist_ok=True)\nwith p.open('wb') as f:\n    f.truncate({ignored_len})\nPY"
            ),
            "yield_time_ms": 8000,
            "timeout_seconds": 20,
        }),
    )?;
    let running_command_id = running_command_id_from_response(&wire)?;
    let result = finalize_foreground_command(
        &lease,
        wire.clone(),
        Instant::now() + Duration::from_secs(30),
    )?;
    ensure!(
        as_str(&result, "status")? == "ok",
        "successful command should finalize in foreground: {result}"
    );

    let lanes = &result["publish_lanes"];
    ensure!(
        lanes["source"]["publish_status"] == "committed",
        "source lane should publish despite ignored limit drop: {result}"
    );
    ensure!(
        lanes["ignored"]["publish_status"] == "dropped_due_to_limits"
            && lanes["ignored"]["drop_reason"] == "ignored_file_byte_limit"
            && as_i64(&lanes["ignored"], "bytes")? == i64::from(ignored_len),
        "ignored lane should report the stable limit drop: {result}"
    );

    assert_command_publish_lanes_trace(
        &lease,
        &wire,
        running_command_id.as_deref(),
        Instant::now() + Duration::from_secs(10),
        |details| {
            details["source"]["publish_status"] == "committed"
                && details["ignored"]["publish_status"] == "dropped_due_to_limits"
                && details["ignored"]["drop_reason"] == "ignored_file_byte_limit"
        },
        "command finalize trace must include ignored limit drop",
    )?;

    let read_source = lease.call_ok(catalog::SANDBOX_FILE_READ, json!({"path": source_path}))?;
    ensure!(
        as_bool(&read_source, "exists")? && as_str(&read_source, "content")? == "source",
        "source output must publish when ignored lane is dropped by limits: {read_source}"
    );
    let read_ignored = lease.call_ok(catalog::SANDBOX_FILE_READ, json!({"path": ignored_path}))?;
    ensure!(
        !as_bool(&read_ignored, "exists")?,
        "oversized ignored output must not publish: {read_ignored}"
    );
    wait_for_command_count(&lease, 0)?;
    wait_for_active_leases(&lease, 0)?;
    Ok(())
}

#[test]
fn source_conflict_drops_ignored_output_with_publish_lanes() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let dir = format!(
        "publish-lanes-source-conflict/{}",
        e2e_test::unique_suffix().replace('-', "_")
    );
    let source_path = format!("{dir}/src.txt");
    let ignored_path = format!("{dir}/ignored/cache.txt");

    lease.call_ok(
        catalog::SANDBOX_FILE_WRITE,
        json!({
            "path": format!("{dir}/.gitignore"),
            "content": "ignored/\n",
            "overwrite": false,
        }),
    )?;
    lease.call_ok(
        catalog::SANDBOX_FILE_WRITE,
        json!({
            "path": &source_path,
            "content": "base\n",
            "overwrite": true,
        }),
    )?;

    let started = lease.call_ok(
        catalog::SANDBOX_COMMAND_EXEC,
        json!({
            "cmd": format!(
                "bash -lc 'printf SNAPSHOT_READY; read _; mkdir -p {dir}/ignored; printf mine > {source_path}; printf ignored > {ignored_path}'"
            ),
            "yield_time_ms": 500,
            "timeout_seconds": 30,
        }),
    )?;
    ensure!(
        as_str(&started, "status")? == "running",
        "source-conflict command should still be running at the sync point: {started}"
    );
    let command_id = as_str(&started, "command_id")?.to_owned();
    wait_for_command_stdout_contains(&lease, &command_id, "SNAPSHOT_READY")?;

    let body = (|| -> Result<()> {
        lease.call_ok(
            catalog::SANDBOX_FILE_WRITE,
            json!({
                "path": &source_path,
                "content": "newer\n",
                "overwrite": true,
            }),
        )?;
        let terminal_wire = lease.call(
            catalog::SANDBOX_COMMAND_WRITE_STDIN,
            json!({
                "command_id": &command_id,
                "chars": "\n",
                "yield_time_ms": 8000,
            }),
        )?;
        let result = unwrap_operation_result(terminal_wire.clone())?;
        ensure!(
            as_str(&result, "status")? == "ok",
            "the command process itself should exit successfully: {result}"
        );
        ensure!(
            !as_bool(&result, "success")?,
            "source conflict must fail workspace mutation success: {result}"
        );
        ensure!(
            array(&result, "changed_paths")?.is_empty(),
            "source conflict must not publish source or ignored changes: {result}"
        );

        let lanes = &result["publish_lanes"];
        ensure!(
            lanes["source"]["publish_status"] == "conflict"
                && lanes["ignored"]["publish_status"] == "dropped_due_to_source_conflict"
                && lanes["ignored"]["drop_reason"] == "source_not_published",
            "source conflict should drop ignored lane with stable metadata: {result}"
        );
        ensure!(
            lanes["source"]["path_count"] == 1 && lanes["ignored"]["path_count"] == 1,
            "source conflict should report one source and one ignored path: {result}"
        );

        wait_for_command_publish_lanes_trace(
            &lease,
            &command_id,
            Instant::now() + Duration::from_secs(10),
            |details| {
                details["source"]["publish_status"] == "conflict"
                    && details["ignored"]["publish_status"] == "dropped_due_to_source_conflict"
                    && details["ignored"]["drop_reason"] == "source_not_published"
            },
        )?;

        let read_source =
            lease.call_ok(catalog::SANDBOX_FILE_READ, json!({"path": &source_path}))?;
        ensure!(
            as_str(&read_source, "content")? == "newer\n",
            "competing source writer should remain visible: {read_source}"
        );
        let read_ignored =
            lease.call_ok(catalog::SANDBOX_FILE_READ, json!({"path": &ignored_path}))?;
        ensure!(
            !as_bool(&read_ignored, "exists")?,
            "ignored command output must not publish after source conflict: {read_ignored}"
        );
        wait_for_command_count(&lease, 0)?;
        wait_for_active_leases(&lease, 0)?;
        Ok(())
    })();

    if body.is_err() {
        let _ = lease.call(
            catalog::SANDBOX_COMMAND_CANCEL,
            json!({"command_id": command_id}),
        );
        let _ = wait_for_command_count(&lease, 0);
    }
    body
}

#[test]
fn source_and_ignored_success_publish_as_one_lane_result() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let dir = format!(
        "publish-lanes-source-ignored-success/{}",
        e2e_test::unique_suffix().replace('-', "_")
    );
    let source_path = format!("{dir}/src.txt");
    let ignored_path = format!("{dir}/ignored/cache.txt");

    lease.call_ok(
        catalog::SANDBOX_FILE_WRITE,
        json!({
            "path": format!("{dir}/.gitignore"),
            "content": "ignored/\n",
            "overwrite": false,
        }),
    )?;
    let before = lease.call_ok(catalog::SANDBOX_CHECKPOINT_LAYER_METRICS, json!({}))?;
    let before_version = as_i64(&before, "manifest_version")?;

    let wire = lease.call(
        catalog::SANDBOX_COMMAND_EXEC,
        json!({
            "cmd": format!(
                "mkdir -p {dir}/ignored && printf source > {source_path} && printf ignored > {ignored_path}"
            ),
            "yield_time_ms": 8000,
            "timeout_seconds": 10,
        }),
    )?;
    let running_command_id = running_command_id_from_response(&wire)?;
    let result = finalize_foreground_command(
        &lease,
        wire.clone(),
        Instant::now() + Duration::from_secs(30),
    )?;
    ensure!(
        as_str(&result, "status")? == "ok" && as_bool(&result, "success")?,
        "source+ignored command should publish successfully: {result}"
    );
    let changed_paths = array(&result, "changed_paths")?;
    ensure!(
        changed_paths
            .iter()
            .any(|path| path.as_str() == Some(source_path.as_str()))
            && changed_paths
                .iter()
                .any(|path| path.as_str() == Some(ignored_path.as_str())),
        "source+ignored command should report both changed paths: {result}"
    );

    let lanes = &result["publish_lanes"];
    ensure!(
        lanes["source"]["publish_status"] == "committed"
            && lanes["ignored"]["publish_status"] == "published_lww"
            && lanes["ignored"]["publish_mode"] == "direct_lww"
            && lanes["source"]["path_count"] == 1
            && lanes["ignored"]["path_count"] == 1,
        "source+ignored success should report committed source and direct LWW ignored: {result}"
    );

    assert_command_publish_lanes_trace(
        &lease,
        &wire,
        running_command_id.as_deref(),
        Instant::now() + Duration::from_secs(10),
        |details| {
            details["source"]["publish_status"] == "committed"
                && details["ignored"]["publish_status"] == "published_lww"
                && details["ignored"]["publish_mode"] == "direct_lww"
        },
        "command finalize trace must report mixed lane success",
    )?;

    let after = lease.call_ok(catalog::SANDBOX_CHECKPOINT_LAYER_METRICS, json!({}))?;
    ensure!(
        as_i64(&after, "manifest_version")? == before_version + 1,
        "source+ignored command should advance the manifest once: before={before}, after={after}"
    );
    let read_source = lease.call_ok(catalog::SANDBOX_FILE_READ, json!({"path": source_path}))?;
    ensure!(
        as_bool(&read_source, "exists")? && as_str(&read_source, "content")? == "source",
        "source output must publish: {read_source}"
    );
    let read_ignored = lease.call_ok(catalog::SANDBOX_FILE_READ, json!({"path": ignored_path}))?;
    ensure!(
        as_bool(&read_ignored, "exists")? && as_str(&read_ignored, "content")? == "ignored",
        "ignored output must publish: {read_ignored}"
    );
    wait_for_command_count(&lease, 0)?;
    wait_for_active_leases(&lease, 0)?;
    Ok(())
}

#[test]
fn ignored_only_later_writer_wins_with_direct_lww_publish() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let dir = format!(
        "publish-lanes-ignored-lww/{}",
        e2e_test::unique_suffix().replace('-', "_")
    );
    let ignored_path = format!("{dir}/ignored/cache.txt");

    lease.call_ok(
        catalog::SANDBOX_FILE_WRITE,
        json!({
            "path": format!("{dir}/.gitignore"),
            "content": "ignored/\n",
            "overwrite": false,
        }),
    )?;
    let before = lease.call_ok(catalog::SANDBOX_CHECKPOINT_LAYER_METRICS, json!({}))?;
    let before_version = as_i64(&before, "manifest_version")?;

    let first = finalize_foreground_command(
        &lease,
        lease.call(
            catalog::SANDBOX_COMMAND_EXEC,
            json!({
                "cmd": format!("mkdir -p {dir}/ignored && printf first > {ignored_path}"),
                "yield_time_ms": 8000,
                "timeout_seconds": 10,
            }),
        )?,
        Instant::now() + Duration::from_secs(30),
    )?;
    ensure!(
        as_str(&first, "status")? == "ok" && as_bool(&first, "success")?,
        "first ignored-only writer should publish: {first}"
    );
    ensure!(
        first["publish_lanes"]["source"]["publish_status"] == "empty"
            && first["publish_lanes"]["ignored"]["publish_status"] == "published_lww"
            && first["publish_lanes"]["ignored"]["publish_mode"] == "direct_lww",
        "first ignored-only writer should report direct LWW publish: {first}"
    );

    let second_wire = lease.call(
        catalog::SANDBOX_COMMAND_EXEC,
        json!({
            "cmd": format!("mkdir -p {dir}/ignored && printf second > {ignored_path}"),
            "yield_time_ms": 8000,
            "timeout_seconds": 10,
        }),
    )?;
    let running_command_id = running_command_id_from_response(&second_wire)?;
    let second = finalize_foreground_command(
        &lease,
        second_wire.clone(),
        Instant::now() + Duration::from_secs(30),
    )?;
    ensure!(
        as_str(&second, "status")? == "ok" && as_bool(&second, "success")?,
        "second ignored-only writer should publish: {second}"
    );
    ensure!(
        second["publish_lanes"]["source"]["publish_status"] == "empty"
            && second["publish_lanes"]["ignored"]["publish_status"] == "published_lww"
            && second["publish_lanes"]["ignored"]["publish_mode"] == "direct_lww"
            && second["publish_lanes"]["ignored"]["path_count"] == 1,
        "second ignored-only writer should report direct LWW publish: {second}"
    );

    assert_command_publish_lanes_trace(
        &lease,
        &second_wire,
        running_command_id.as_deref(),
        Instant::now() + Duration::from_secs(10),
        |details| {
            details["source"]["publish_status"] == "empty"
                && details["ignored"]["publish_status"] == "published_lww"
                && details["ignored"]["publish_mode"] == "direct_lww"
        },
        "command finalize trace must report ignored-only direct LWW publish",
    )?;

    let after = lease.call_ok(catalog::SANDBOX_CHECKPOINT_LAYER_METRICS, json!({}))?;
    ensure!(
        as_i64(&after, "manifest_version")? == before_version + 2,
        "two ignored-only writers should advance the manifest twice: before={before}, after={after}"
    );
    let read = lease.call_ok(catalog::SANDBOX_FILE_READ, json!({"path": ignored_path}))?;
    ensure!(
        as_bool(&read, "exists")? && as_str(&read, "content")? == "second",
        "later ignored-only writer should win in the layered view: {read}"
    );
    wait_for_command_count(&lease, 0)?;
    wait_for_active_leases(&lease, 0)?;
    Ok(())
}

#[test]
fn source_publish_failure_drops_spooled_ignored_output() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let dir = format!(
        "publish-lanes-fail-source-spool/{}",
        e2e_test::unique_suffix().replace('-', "_")
    );
    let source_path = format!("{dir}/src.txt");
    let ignored_path = format!("{dir}/ignored/large.bin");
    let ignored_len = 4096;

    lease.call_ok(
        catalog::SANDBOX_FILE_WRITE,
        json!({
            "path": format!("{dir}/.gitignore"),
            "content": "ignored/\n",
            "overwrite": false,
        }),
    )?;
    let before = lease.call_ok(catalog::SANDBOX_CHECKPOINT_LAYER_METRICS, json!({}))?;
    let before_version = as_i64(&before, "manifest_version")?;
    let marker_path = inject_next_layer_publish_failure(&lease)?;

    let wire = lease.call(
        catalog::SANDBOX_COMMAND_EXEC,
        json!({
            "cmd": format!(
                "mkdir -p {dir}/ignored && printf source > {source_path} && python3 - <<'PY'\nfrom pathlib import Path\nPath('{ignored_path}').write_bytes(b'x' * {ignored_len})\nPY"
            ),
            "yield_time_ms": 8000,
            "timeout_seconds": 20,
        }),
    )?;
    let running_command_id = running_command_id_from_response(&wire)?;
    let result = finalize_foreground_command(
        &lease,
        wire.clone(),
        Instant::now() + Duration::from_secs(30),
    )?;
    ensure!(
        as_str(&result, "status")? == "ok" && !as_bool(&result, "success")?,
        "command process should succeed while publish fails atomically: {result}"
    );
    ensure!(
        array(&result, "changed_paths")?.is_empty(),
        "failed publish must not report changed paths: {result}"
    );
    let lanes = &result["publish_lanes"];
    ensure!(
        lanes["source"]["publish_status"] == "failed"
            && lanes["ignored"]["publish_status"] == "dropped_due_to_source_conflict"
            && lanes["ignored"]["drop_reason"] == "source_not_published"
            && as_i64(&lanes["ignored"], "spooled_bytes")? == i64::from(ignored_len),
        "source publish failure should drop spooled ignored output from the same command: {result}"
    );

    let trace_matches = |details: &Value| {
        details["source"]["publish_status"] == "failed"
            && details["ignored"]["publish_status"] == "dropped_due_to_source_conflict"
            && details["ignored"]["drop_reason"] == "source_not_published"
            && details["ignored"]["spooled_bytes"].as_i64() == Some(i64::from(ignored_len))
    };
    assert_command_publish_lanes_trace(
        &lease,
        &wire,
        running_command_id.as_deref(),
        Instant::now() + Duration::from_secs(10),
        trace_matches,
        "command finalize trace must report publish failure lane drops",
    )?;
    let after = lease.call_ok(catalog::SANDBOX_CHECKPOINT_LAYER_METRICS, json!({}))?;
    ensure!(
        as_i64(&after, "manifest_version")? == before_version,
        "failed publish must leave the manifest unchanged: before={before}, after={after}"
    );
    let read_source = lease.call_ok(catalog::SANDBOX_FILE_READ, json!({"path": source_path}))?;
    ensure!(
        !as_bool(&read_source, "exists")?,
        "source output must not publish after injected failure: {read_source}"
    );
    let read_ignored = lease.call_ok(catalog::SANDBOX_FILE_READ, json!({"path": ignored_path}))?;
    ensure!(
        !as_bool(&read_ignored, "exists")?,
        "ignored output must not publish after injected failure: {read_ignored}"
    );
    ensure!(
        !container_path_exists(&lease, &marker_path)?,
        "publish failure marker should be consumed after one failure"
    );
    wait_for_command_count(&lease, 0)?;
    wait_for_active_leases(&lease, 0)?;
    Ok(())
}

#[test]
fn large_ignored_output_publishes_through_spool_backed_capture() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let dir = format!(
        "publish-lanes-ignored-spool/{}",
        e2e_test::unique_suffix().replace('-', "_")
    );
    let ignored_path = format!("{dir}/ignored/large.bin");
    let ignored_len = 4096;

    lease.call_ok(
        catalog::SANDBOX_FILE_WRITE,
        json!({
            "path": format!("{dir}/.gitignore"),
            "content": "ignored/\n",
            "overwrite": false,
        }),
    )?;

    let wire = lease.call(
        catalog::SANDBOX_COMMAND_EXEC,
        json!({
            "cmd": format!(
                "mkdir -p {dir}/ignored && python3 - <<'PY'\nfrom pathlib import Path\np = Path('{ignored_path}')\np.write_bytes(b'x' * {ignored_len})\nPY"
            ),
            "yield_time_ms": 8000,
            "timeout_seconds": 20,
        }),
    )?;
    let running_command_id = running_command_id_from_response(&wire)?;
    let result = finalize_foreground_command(
        &lease,
        wire.clone(),
        Instant::now() + Duration::from_secs(30),
    )?;
    ensure!(
        as_str(&result, "status")? == "ok",
        "successful command should finalize in foreground: {result}"
    );

    let lanes = &result["publish_lanes"];
    ensure!(
        lanes["source"]["publish_status"] == "empty",
        "large ignored-only command should not report source output: {result}"
    );
    ensure!(
        lanes["ignored"]["publish_status"] == "published_lww"
            && lanes["ignored"]["publish_mode"] == "direct_lww"
            && as_i64(&lanes["ignored"], "bytes")? == i64::from(ignored_len)
            && as_i64(&lanes["ignored"], "spooled_bytes")? == i64::from(ignored_len),
        "large ignored output should publish through the spool path: {result}"
    );

    let trace_matches = |details: &Value| {
        details["ignored"]["publish_status"] == "published_lww"
            && details["ignored"]["spooled_bytes"].as_i64() == Some(i64::from(ignored_len))
    };
    assert_command_publish_lanes_trace(
        &lease,
        &wire,
        running_command_id.as_deref(),
        Instant::now() + Duration::from_secs(10),
        trace_matches,
        "command finalize trace must report spooled ignored bytes",
    )?;

    let verify = finalize_foreground_command(
        &lease,
        lease.call(
            catalog::SANDBOX_COMMAND_EXEC,
            json!({
                "cmd": format!(
                    "python3 - <<'PY'\nfrom pathlib import Path\nassert Path('{ignored_path}').stat().st_size == {ignored_len}\nprint('size-ok')\nPY"
                ),
                "yield_time_ms": 8000,
                "timeout_seconds": 10,
            }),
        )?,
        Instant::now() + Duration::from_secs(20),
    )?;
    ensure!(
        as_str(&verify, "status")? == "ok" && stdout(&verify).contains("size-ok"),
        "published ignored spool payload should have the expected size: {verify}"
    );
    wait_for_command_count(&lease, 0)?;
    wait_for_active_leases(&lease, 0)?;
    Ok(())
}

#[test]
fn multiple_ignored_outputs_publish_through_aggregate_spool_backed_capture() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let dir = format!(
        "publish-lanes-ignored-aggregate-spool/{}",
        e2e_test::unique_suffix().replace('-', "_")
    );
    let ignored_len = 800;
    let ignored_total = ignored_len * 2;

    lease.call_ok(
        catalog::SANDBOX_FILE_WRITE,
        json!({
            "path": format!("{dir}/.gitignore"),
            "content": "ignored/\n",
            "overwrite": false,
        }),
    )?;

    let wire = lease.call(
        catalog::SANDBOX_COMMAND_EXEC,
        json!({
            "cmd": format!(
                "mkdir -p {dir}/ignored && python3 - <<'PY'\nfrom pathlib import Path\nbase = Path('{dir}/ignored')\n(base / 'a.bin').write_bytes(b'a' * {ignored_len})\n(base / 'b.bin').write_bytes(b'b' * {ignored_len})\nPY"
            ),
            "yield_time_ms": 8000,
            "timeout_seconds": 20,
        }),
    )?;
    let running_command_id = running_command_id_from_response(&wire)?;
    let result = finalize_foreground_command(
        &lease,
        wire.clone(),
        Instant::now() + Duration::from_secs(30),
    )?;
    ensure!(
        as_str(&result, "status")? == "ok",
        "successful aggregate-spool command should finalize in foreground: {result}"
    );

    let lanes = &result["publish_lanes"];
    ensure!(
        lanes["source"]["publish_status"] == "empty",
        "aggregate ignored-only command should not report source output: {result}"
    );
    ensure!(
        lanes["ignored"]["publish_status"] == "published_lww"
            && lanes["ignored"]["publish_mode"] == "direct_lww"
            && as_i64(&lanes["ignored"], "bytes")? == i64::from(ignored_total)
            && as_i64(&lanes["ignored"], "spooled_bytes")? == i64::from(ignored_total),
        "aggregate ignored output should publish through the spool path: {result}"
    );

    let trace_matches = |details: &Value| {
        details["ignored"]["publish_status"] == "published_lww"
            && details["ignored"]["spooled_bytes"].as_i64() == Some(i64::from(ignored_total))
    };
    assert_command_publish_lanes_trace(
        &lease,
        &wire,
        running_command_id.as_deref(),
        Instant::now() + Duration::from_secs(10),
        trace_matches,
        "command finalize trace must report aggregate spooled ignored bytes",
    )?;

    let verify = finalize_foreground_command(
        &lease,
        lease.call(
            catalog::SANDBOX_COMMAND_EXEC,
            json!({
                "cmd": format!(
                    "python3 - <<'PY'\nfrom pathlib import Path\nbase = Path('{dir}/ignored')\nassert (base / 'a.bin').stat().st_size == {ignored_len}\nassert (base / 'b.bin').stat().st_size == {ignored_len}\nprint('aggregate-size-ok')\nPY"
                ),
                "yield_time_ms": 8000,
                "timeout_seconds": 10,
            }),
        )?,
        Instant::now() + Duration::from_secs(20),
    )?;
    ensure!(
        as_str(&verify, "status")? == "ok" && stdout(&verify).contains("aggregate-size-ok"),
        "published aggregate spool payloads should have the expected sizes: {verify}"
    );
    wait_for_command_count(&lease, 0)?;
    wait_for_active_leases(&lease, 0)?;
    Ok(())
}

#[test]
fn stderr_and_stdin_output_keep_long_lived_session_running() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let started = lease.call_ok(
        catalog::SANDBOX_COMMAND_EXEC,
        json!({
            "cmd": "python3 -u -c 'import sys,time; print(\"stderr-ready\", file=sys.stderr, flush=True); payload=sys.stdin.readline().strip(); print(\"stderr-reply:\" + payload, file=sys.stderr, flush=True); time.sleep(60)'",
            "yield_time_ms": 500,
            "timeout_seconds": 120,}),
    )?;
    ensure!(
        as_str(&started, "status")? == "running",
        "stderr prompt command should keep running after first stderr output: {started}"
    );
    ensure!(
        stdout(&started).contains("stderr-ready"),
        "PTY stdout stream should expose initial stderr output: {started}"
    );
    ensure!(
        stderr(&started).is_empty(),
        "stderr field should stay empty for merged PTY output: {started}"
    );
    let command_id = as_str(&started, "command_id")?.to_owned();

    let body = (|| -> Result<()> {
        let answered = lease.call_ok(
            catalog::SANDBOX_COMMAND_WRITE_STDIN,
            json!({
                "command_id": &command_id,
                "chars": "payload\n",
                "yield_time_ms": 1500,}),
        )?;
        ensure!(
            as_str(&answered, "status")? == "running",
            "stdin reply on a long-lived stderr command should remain running: {answered}"
        );
        ensure!(
            !stdout(&answered).contains("stderr-ready"),
            "stdin output should be scoped to text produced after the write: {answered}"
        );
        let reply = if stdout(&answered).contains("stderr-reply:payload") {
            answered
        } else {
            poll_read_progress_until_stdout_contains(
                &lease,
                &command_id,
                "stderr-reply:payload",
                Instant::now() + Duration::from_secs(10),
            )?
        };
        ensure!(
            stdout(&reply).contains("stderr-reply:payload"),
            "PTY stdout stream should expose stderr produced after stdin: {reply}"
        );

        let not_done = lease.call_ok(
            catalog::SANDBOX_COMMAND_COLLECT_COMPLETED,
            json!({"command_ids": [command_id.clone()]}),
        )?;
        ensure!(
            array(&not_done, "completions")?.is_empty(),
            "sleeping stderr/stdin command must not collect before cancellation: {not_done}"
        );

        let cancelled = unwrap_operation_result(lease.call(
            catalog::SANDBOX_COMMAND_CANCEL,
            json!({"command_id": &command_id}),
        )?)?;
        ensure!(
            matches!(as_str(&cancelled, "status")?, "cancelled" | "ok" | "error"),
            "cancel should return terminal-ish status after long-lived stderr/stdin output: {cancelled}"
        );
        wait_for_command_count(&lease, 0)?;
        wait_for_active_leases(&lease, 0)?;
        wait_for_command_transcript_recycled(&lease, &command_id)?;
        Ok(())
    })();

    if body.is_err() {
        let _ = lease.call(
            catalog::SANDBOX_COMMAND_CANCEL,
            json!({"command_id": &command_id}),
        );
        let _ = wait_for_command_count(&lease, 0);
    }
    body
}

#[test]
fn missing_command_and_invalid_command_ids_are_structured() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let missing = lease.call(
        catalog::SANDBOX_COMMAND_EXEC,
        json!({
            "cmd": "definitely_missing_eos_e2e_command",
            "yield_time_ms": 1000,
            "timeout_seconds": 10,}),
    )?;
    let missing =
        finalize_foreground_command(&lease, missing, Instant::now() + Duration::from_secs(20))?;
    ensure!(
        as_str(&missing, "status")? == "error",
        "missing command should return an error status: {missing}"
    );
    ensure!(
        as_i64(&missing, "exit_code")? != 0,
        "missing command should preserve a nonzero exit code: {missing}"
    );
    ensure!(
        stdout(&missing).contains("not found") || stderr(&missing).contains("not found"),
        "missing command should expose shell diagnostic output: {missing}"
    );

    let bogus = format!(
        "missing-command-{}",
        e2e_test::unique_suffix().replace('-', "_")
    );
    let stdin = unwrap_operation_result(lease.call(
        catalog::SANDBOX_COMMAND_WRITE_STDIN,
        json!({
            "command_id": bogus,
            "chars": "ignored\n",
            "yield_time_ms": 100,}),
    )?)?;
    ensure!(
        as_str(&stdin, "status")? == "error",
        "write_stdin against an unknown command should return a structured error: {stdin}"
    );
    ensure!(
        stderr(&stdin).contains("command_not_found"),
        "write_stdin unknown-command error should carry a stable diagnostic: {stdin}"
    );

    let cancel = unwrap_operation_result(lease.call(
        catalog::SANDBOX_COMMAND_CANCEL,
        json!({"command_id": bogus}),
    )?)?;
    ensure!(
        as_str(&cancel, "status")? == "error",
        "cancel against an unknown command should return a structured error: {cancel}"
    );
    ensure!(
        stderr(&cancel).contains("command_not_found"),
        "cancel unknown-command error should carry a stable diagnostic: {cancel}"
    );

    let collect = lease.call_ok(
        catalog::SANDBOX_COMMAND_COLLECT_COMPLETED,
        json!({"command_ids": [bogus]}),
    )?;
    ensure!(
        array(&collect, "completions")?.is_empty(),
        "collect_completed for an unknown command should be an empty read, not an error: {collect}"
    );
    wait_for_command_count(&lease, 0)?;
    wait_for_active_leases(&lease, 0)?;
    Ok(())
}

#[test]
fn output_backpressure_preserves_utf8_and_drains_on_cancel() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let started = lease.call_ok(
        catalog::SANDBOX_COMMAND_EXEC,
        json!({
            "cmd": "python3 -u - <<'PY'\nimport sys, time\nsys.stdout.write('Ω' * 20000)\nsys.stdout.flush()\ntime.sleep(60)\nPY",
            "yield_time_ms": 500,
            "timeout_seconds": 120,}),
    )?;
    ensure!(
        as_str(&started, "status")? == "running",
        "large-output command should stay running for transcript/backpressure checks: {started}"
    );
    ensure!(
        stdout(&started).contains('Ω'),
        "initial output should expose the timestamped transcript burst: {started}"
    );
    ensure_valid_utf8_prefix(&started)?;
    let command_id = as_str(&started, "command_id")?.to_owned();

    let body = (|| -> Result<()> {
        for _ in 0..2 {
            let poll = lease.call_ok(
                catalog::SANDBOX_COMMAND_POLL,
                json!({
                    "command_id": &command_id,
                    "last_n_lines": 1,
                }),
            )?;
            ensure!(
                stdout(&poll).contains('Ω'),
                "read_progress should return the timestamped transcript tail under backpressure: {poll}"
            );
            ensure_valid_utf8_prefix(&poll)?;
        }
        let cancelled = unwrap_operation_result(lease.call(
            catalog::SANDBOX_COMMAND_CANCEL,
            json!({"command_id": &command_id}),
        )?)?;
        ensure!(
            matches!(as_str(&cancelled, "status")?, "cancelled" | "ok" | "error"),
            "cancel should return a terminal-ish status after output pressure: {cancelled}"
        );
        wait_for_command_count(&lease, 0)?;
        wait_for_active_leases(&lease, 0)?;
        wait_for_command_transcript_recycled(&lease, &command_id)?;
        Ok(())
    })();

    if body.is_err() {
        let _ = lease.call(
            catalog::SANDBOX_COMMAND_CANCEL,
            json!({"command_id": &command_id}),
        );
        let _ = wait_for_command_count(&lease, 0);
    }
    body
}

fn ensure_valid_utf8_prefix(response: &Value) -> Result<()> {
    // Strip the per-line `[ISO-8601] ` transcript timestamp prefix; the property
    // under test is that the Ω burst keeps its codepoint boundaries (a split Ω
    // would surface as U+FFFD `�`, which still fails the char check below).
    let output = clean_stdout(response);
    ensure!(
        output
            .chars()
            .all(|ch| ch == 'Ω' || ch == '\r' || ch == '\n'),
        "capped output should preserve UTF-8 codepoint boundaries: {response}"
    );
    Ok(())
}

fn poll_read_progress_until_stdout_contains(
    lease: &e2e_test::NodeLease<'_>,
    command_id: &str,
    needle: &str,
    deadline: Instant,
) -> Result<Value> {
    let mut last = None;
    while Instant::now() < deadline {
        let poll = lease.call_ok(
            catalog::SANDBOX_COMMAND_POLL,
            json!({
                "command_id": command_id,
                "last_n_lines": 8,
            }),
        )?;
        if stdout(&poll).contains(needle) {
            return Ok(poll);
        }
        last = Some(poll);
    }
    bail!("read_progress did not surface {needle:?} before deadline; last poll: {last:?}");
}

fn wait_for_command_publish_lanes_trace(
    lease: &e2e_test::NodeLease<'_>,
    command_id: &str,
    deadline: Instant,
    publish_lanes: impl Fn(&Value) -> bool,
) -> Result<()> {
    loop {
        let exported = lease.call_ok(catalog::SANDBOX_TRACE_EXPORT, json!({"max_records": 64}))?;
        let records = trace_export_records(&exported)?;
        ack_trace_export(lease, &exported)?;
        for record in records {
            let finalized_command = has_trace_event(&record, "command", "finalized", |details| {
                details.get("command_id").and_then(Value::as_str) == Some(command_id)
            });
            if finalized_command
                && has_trace_event(
                    &record,
                    "command",
                    "command.publish_lanes_decided",
                    |details| publish_lanes(details),
                )
            {
                return Ok(());
            }
        }
        if Instant::now() >= deadline {
            bail!(
                "command finalize trace with publish_lanes_decided was not exported for {command_id}"
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn running_command_id_from_response(response: &Value) -> Result<Option<String>> {
    let result = unwrap_operation_result(response.clone())?;
    if as_str(&result, "status")? == "running" {
        Ok(Some(as_str(&result, "command_id")?.to_owned()))
    } else {
        Ok(None)
    }
}

fn assert_command_publish_lanes_trace(
    lease: &e2e_test::NodeLease<'_>,
    initial_response: &Value,
    running_command_id: Option<&str>,
    deadline: Instant,
    publish_lanes: impl Fn(&Value) -> bool,
    failure_message: &str,
) -> Result<()> {
    if let Some(command_id) = running_command_id {
        wait_for_command_publish_lanes_trace(lease, command_id, deadline, publish_lanes)?;
        return Ok(());
    }
    let record = trace_record(initial_response)?;
    ensure!(
        has_trace_event(
            &record,
            "command",
            "command.publish_lanes_decided",
            publish_lanes
        ),
        "{failure_message}: {record:?}"
    );
    Ok(())
}

fn seed_real_git_workspace(lease: &e2e_test::NodeLease<'_>) -> Result<()> {
    seed_real_git_workspace_inner(lease, false).map(|_| ())
}

fn seed_real_git_workspace_with_loose_object(lease: &e2e_test::NodeLease<'_>) -> Result<String> {
    seed_real_git_workspace_inner(lease, true)
}

fn seed_real_git_workspace_inner(
    lease: &e2e_test::NodeLease<'_>,
    create_object: bool,
) -> Result<String> {
    let mut script = format!(
        "set -e\ncd {root}\ngit init -q\ngit config user.email e2e@example.invalid\ngit config user.name 'E2E Test'\n",
        root = lease.workspace_root()
    );
    if create_object {
        script.push_str(
            "printf seed > object-seed.txt\nsha=$(git hash-object -w object-seed.txt)\nprintf '%s' \"$sha\"\n",
        );
    }
    let output = lease.container().exec(&["sh", "-lc", &script])?;
    lease.call_ok(
        catalog::SANDBOX_CHECKPOINT_BUILD_BASE,
        json!({
            "workspace_root": lease.workspace_root(),
            "reset": true,
        }),
    )?;
    let sha = output.trim();
    if create_object {
        ensure!(
            sha.len() >= 3,
            "git hash-object returned invalid sha: {output:?}"
        );
        Ok(format!(".git/objects/{}/{}", &sha[..2], &sha[2..]))
    } else {
        Ok(String::new())
    }
}

fn manifest_version(lease: &e2e_test::NodeLease<'_>) -> Result<i64> {
    let metrics = lease.call_ok(catalog::SANDBOX_CHECKPOINT_LAYER_METRICS, json!({}))?;
    as_i64(&metrics, "manifest_version")
}

fn ensure_git_publish_rejected(
    lease: &e2e_test::NodeLease<'_>,
    initial_response: &Value,
    result: &Value,
    reason: &str,
    before_manifest_version: i64,
    context: &str,
) -> Result<()> {
    ensure!(
        as_str(result, "status")? == "ok",
        "{context}: command should exit successfully and fail only publish: {result}"
    );
    ensure!(
        !as_bool(result, "success")?,
        "{context}: workspace mutation must fail: {result}"
    );
    ensure!(
        result["publish_lanes"]["routing"]["drop_reason_counts"][reason]
            .as_i64()
            .unwrap_or_default()
            >= 1,
        "{context}: publish_lanes must carry {reason}: {result}"
    );
    ensure!(
        array(result, "changed_paths")?.is_empty(),
        "{context}: rejected git metadata must publish no paths: {result}"
    );
    let after = manifest_version(lease)?;
    ensure!(
        after == before_manifest_version,
        "{context}: manifest must not advance on git metadata rejection; before={before_manifest_version}, after={after}"
    );
    assert_command_publish_lanes_trace(
        lease,
        initial_response,
        running_command_id_from_response(initial_response)?.as_deref(),
        Instant::now() + Duration::from_secs(10),
        |details| {
            details["routing"]["drop_reason_counts"][reason]
                .as_i64()
                .unwrap_or_default()
                >= 1
        },
        context,
    )?;
    wait_for_command_count(lease, 0)?;
    wait_for_active_leases(lease, 0)?;
    Ok(())
}

fn inject_next_layer_publish_failure(lease: &e2e_test::NodeLease<'_>) -> Result<String> {
    let marker = format!("{}/.layer-metadata/fail-next-publish", lease.root());
    let script = format!(
        r#"import pathlib

path = pathlib.Path({marker:?})
path.parent.mkdir(parents=True, exist_ok=True)
path.write_text("fail\n")
"#
    );
    lease.container().exec(&["python3", "-c", &script])?;
    Ok(marker)
}

fn stderr(value: &Value) -> &str {
    value
        .get("output")
        .and_then(|output| output.get("stderr"))
        .and_then(Value::as_str)
        .or_else(|| value.get("stderr").and_then(Value::as_str))
        .unwrap_or_default()
}

#[test]
fn stdin_to_non_reading_consumer_stays_bounded_and_cancellable() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    // A consumer that never reads stdin. A small stdin write fits the PTY buffer
    // and returns immediately while the command stays cancellable. The over-buffer
    // case (where the non-blocking writer must bound the push by a deadline) is
    // covered by `over_buffer_stdin_to_non_reading_consumer_returns_backpressure`.
    let started = lease.call_ok(
        catalog::SANDBOX_COMMAND_EXEC,
        json!({
            "cmd": "sh -c 'echo no-read-ready; sleep 60'",
            "yield_time_ms": 800,
            "timeout_seconds": 120,}),
    )?;
    ensure!(
        as_str(&started, "status")? == "running",
        "non-reading consumer should start: {started}"
    );
    let id = as_str(&started, "command_id")?.to_owned();

    let payload = format!("{}\n", "x".repeat(1024));
    let write_started = Instant::now();
    let wrote = lease.call_ok(
        catalog::SANDBOX_COMMAND_WRITE_STDIN,
        json!({
            "command_id": &id,
            "chars": payload,
            "yield_time_ms": 300,}),
    )?;
    ensure!(
        as_str(&wrote, "status")? == "running",
        "command should stay running after stdin to a non-reading consumer: {wrote}"
    );
    ensure!(
        write_started.elapsed() < Duration::from_secs(10),
        "a bounded stdin write must return promptly, not wedge: took {:?}",
        write_started.elapsed()
    );

    let cancelled = unwrap_operation_result(
        lease.call(catalog::SANDBOX_COMMAND_CANCEL, json!({"command_id": &id}))?,
    )?;
    ensure!(
        matches!(as_str(&cancelled, "status")?, "cancelled" | "ok" | "error"),
        "command must stay cancellable after stdin pressure: {cancelled}"
    );
    wait_for_command_count(&lease, 0)?;
    wait_for_command_transcript_recycled(&lease, &id)?;
    wait_for_active_leases(&lease, 0)?;
    Ok(())
}

#[test]
fn over_buffer_stdin_to_non_reading_consumer_returns_backpressure() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    // A consumer that never reads stdin, plus a payload far larger than the kernel
    // PTY input buffer. The non-blocking writer must bound the push by a deadline
    // and return a structured backpressure error instead of wedging, and the
    // command must stay cancellable. (Before the non-blocking rewrite this write
    // blocked until the command timeout.)
    let started = lease.call_ok(
        catalog::SANDBOX_COMMAND_EXEC,
        json!({
            "cmd": "sh -c 'echo no-read-ready; sleep 60'",
            "yield_time_ms": 800,
            "timeout_seconds": 120,
        }),
    )?;
    ensure!(
        as_str(&started, "status")? == "running",
        "non-reading consumer should start: {started}"
    );
    let id = as_str(&started, "command_id")?.to_owned();

    // Many newline-terminated lines, far past the ~4 KiB cooked PTY input buffer.
    // A single overlong line would be dropped past MAX_CANON without blocking; only
    // accumulated unread lines fill the input queue and exert real backpressure.
    let payload = "eos-e2e-backpressure-line\n".repeat(16384);
    let write_started = Instant::now();
    let pushed = unwrap_operation_result(lease.call(
        catalog::SANDBOX_COMMAND_WRITE_STDIN,
        json!({
            "command_id": &id,
            "chars": payload,
            "yield_time_ms": 300,
        }),
    )?)?;
    let elapsed = write_started.elapsed();
    ensure!(
        elapsed < Duration::from_secs(15),
        "over-buffer stdin must return bounded, not wedge: took {elapsed:?}"
    );
    ensure!(
        pushed.to_string().contains("backpressure"),
        "over-buffer stdin to a non-reading consumer should surface a backpressure diagnostic: {pushed}"
    );

    let cancelled = unwrap_operation_result(
        lease.call(catalog::SANDBOX_COMMAND_CANCEL, json!({"command_id": &id}))?,
    )?;
    ensure!(
        matches!(as_str(&cancelled, "status")?, "cancelled" | "ok" | "error"),
        "command must stay cancellable after backpressure: {cancelled}"
    );
    wait_for_command_count(&lease, 0)?;
    wait_for_active_leases(&lease, 0)?;
    Ok(())
}
