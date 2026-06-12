use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use eos_e2e_test::{unique_suffix, NodeLease};
use eos_operation::core::catalog;
use serde_json::{json, Value};

use crate::support::{
    array, as_bool, as_i64, as_str, clean_stdout, conflict_reason, finalize_foreground_command,
    live_pool_or_skip, seed_base_files, stdout, strip_transcript_timestamps,
    wait_for_active_leases, wait_for_session_count,
};

/// Run a foreground `exec_command` and finalize it to its terminal outcome. Under
/// x86-on-arm64 emulation a quick command can outlast its yield window and return
/// `"running"`; this polls it to completion so the caller's assertions on the
/// finalized payload (status, exit_code, changed_paths, upperdir timings) hold
/// whether it finished foreground or just after. Used only for foreground execs;
/// the background/running-path tests keep their explicit `lease.call_ok`.
fn exec_settled(lease: &NodeLease<'_>, args: Value) -> Result<Value> {
    let response = lease.call_ok(catalog::SANDBOX_COMMAND_EXEC, args)?;
    finalize_foreground_command(lease, response, Instant::now() + Duration::from_secs(25))
}

/// Read a nested `timings.<key>` number from a response.
fn timing_f64(value: &Value, key: &str) -> Option<f64> {
    value
        .get("timings")
        .and_then(|timings| timings.get(key))
        .and_then(Value::as_f64)
}

fn has_timing(value: &Value, key: &str) -> bool {
    timing_f64(value, key).is_some_and(|timing| timing >= 0.0)
}

#[test]
fn exec_multi_path_route_timings_and_read_intent_no_publish() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let dir = format!("route-edges-{}", eos_e2e_test::unique_suffix());
    let first = format!("{dir}/first.txt");
    let second = format!("{dir}/nested/second.txt");

    let exec = exec_settled(
        &lease,
        json!({
            "cmd": format!("mkdir -p {dir}/nested && printf first > {first} && printf second > {second}"),
            "yield_time_ms": 8000,
            "timeout_seconds": 10,}),
    )?;
    assert_eq!(as_str(&exec, "status")?, "ok", "{exec}");
    let changed = array(&exec, "changed_paths")?;
    for expected in [&first, &second] {
        assert!(
            changed.iter().any(|path| path.as_str() == Some(expected)),
            "multi-path shell write must publish {expected}: {exec}"
        );
    }
    assert!(
        has_timing(&exec, "command_exec.total_s")
            || has_timing(&exec, "api.exec_command.dispatch_total_s"),
        "exec response must expose command dispatch timing: {exec}"
    );
    assert!(
        has_timing(&exec, "occ.commit.total_s"),
        "exec response must expose OCC publish timing: {exec}"
    );

    let read_only = exec_settled(
        &lease,
        json!({
            "cmd": format!("cat {first} {second}"),
            "yield_time_ms": 8000,
            "timeout_seconds": 10,}),
    )?;
    assert_eq!(as_str(&read_only, "status")?, "ok", "{read_only}");
    assert_eq!(clean_stdout(&read_only), "firstsecond", "{read_only}");
    assert!(
        array(&read_only, "changed_paths")?.is_empty(),
        "read-intent exec must not publish changed paths: {read_only}"
    );
    Ok(())
}

#[test]
fn exec_write_outside_workspace_is_not_captured() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let marker = format!("/tmp/eos_outside_{}", unique_suffix().replace('-', "_"));
    let exec = exec_settled(
        &lease,
        json!({
            "cmd": format!("mkdir -p scope_in && printf inside > scope_in/inside.txt && printf outside > {marker}"),
            "yield_time_ms": 8000,
            "timeout_seconds": 10,}),
    )?;
    assert_eq!(as_str(&exec, "status")?, "ok", "{exec}");
    // The overlay captures only the upperdir over workspace_root: the in-workspace
    // path is published, the /tmp write is invisible to OCC (merged to the shared
    // container FS directly).
    let changed = array(&exec, "changed_paths")?;
    assert!(
        changed
            .iter()
            .any(|path| path.as_str() == Some("scope_in/inside.txt")),
        "in-workspace write must be captured: {exec}"
    );
    assert!(
        changed
            .iter()
            .all(|path| !path.as_str().unwrap_or_default().contains("tmp")),
        "an out-of-workspace /tmp write must not appear in changed_paths: {exec}"
    );
    // Secondary: the outside write landed on the real container /tmp and a fresh
    // ephemeral exec re-derived over / still sees it.
    let read_back = exec_settled(
        &lease,
        json!({"cmd": format!("cat {marker}"), "yield_time_ms": 8000, "timeout_seconds": 10}),
    )?;
    assert_eq!(clean_stdout(&read_back), "outside", "{read_back}");
    Ok(())
}

#[test]
fn exec_mount_mask_uses_test_config_hidden_paths() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    lease
        .container()
        .exec(&[
            "sh",
            "-lc",
            "rm -rf /tmp/eos-mask-test && mkdir -p /tmp/eos-mask-test && printf host > /tmp/eos-mask-test/host-visible.txt",
        ])
        .context("seed extra mount-mask probe dir")?;

    let exec = exec_settled(
        &lease,
        json!({
            "cmd": r#"set -eu
mount_fstype() {
  awk -v target="$1" '
    $5 == target {
      for (i = 1; i <= NF; i++) {
        if ($i == "-") {
          fstype = $(i + 1)
          found = 1
        }
      }
    }
    END {
      if (found) {
        print fstype
      } else {
        print "missing"
      }
    }
  ' /proc/self/mountinfo
}
dir_state() {
  if [ -d "$1" ] && [ -z "$(ls -A "$1" 2>/dev/null)" ]; then
    printf empty
  else
    printf visible
  fi
}
printf 'proc=%s\n' "$(test -r /proc/self/mountinfo && printf visible || printf hidden)"
printf 'eos_fs=%s\n' "$(mount_fstype /eos)"
printf 'eos_state=%s\n' "$(dir_state /eos)"
printf 'cgroup_fs=%s\n' "$(mount_fstype /sys/fs/cgroup)"
printf 'cgroup_state=%s\n' "$(dir_state /sys/fs/cgroup)"
printf 'extra_fs=%s\n' "$(mount_fstype /tmp/eos-mask-test)"
printf 'extra_state=%s\n' "$(dir_state /tmp/eos-mask-test)"
"#,
            "yield_time_ms": 8000,
            "timeout_seconds": 10,}),
    )?;
    assert_eq!(as_str(&exec, "status")?, "ok", "{exec}");
    let output = strip_transcript_timestamps(stdout(&exec));
    for expected in [
        "proc=visible",
        "eos_fs=tmpfs",
        "eos_state=empty",
        "cgroup_fs=tmpfs",
        "cgroup_state=empty",
        "extra_fs=tmpfs",
        "extra_state=empty",
    ] {
        assert!(
            output.lines().any(|line| line == expected),
            "missing {expected:?} in mount-mask probe output {output:?}; response={exec}"
        );
    }

    let host_probe = lease
        .container()
        .exec(&["sh", "-lc", "cat /tmp/eos-mask-test/host-visible.txt"])
        .context("read extra mount-mask probe dir after exec")?;
    assert_eq!(
        host_probe.trim(),
        "host",
        "fresh namespace mask must not mutate the container mount namespace"
    );
    Ok(())
}

#[test]
fn foreground_exec_recycles_overlay_scratch() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let exec = exec_settled(
        &lease,
        json!({
            "cmd": "mkdir -p scratchscope && printf x > scratchscope/a.txt",
            "yield_time_ms": 8000,
            "timeout_seconds": 10,}),
    )?;
    assert_eq!(as_str(&exec, "status")?, "ok", "{exec}");
    assert!(exec.get("command_id").is_none(), "{exec}");
    // The overlay scratch (upperdir + workdir) is torn down on finalize and the
    // lease is released — observable as active_leases back to 0.
    let metrics = wait_for_active_leases(&lease, 0)?;
    assert_eq!(as_i64(&metrics, "active_leases")?, 0, "{metrics}");
    Ok(())
}

#[test]
fn overlay_delete_replacement_write_and_foreign_publish_are_readable() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let dir = format!("whiteout-resync-{}", eos_e2e_test::unique_suffix());
    let deleted = format!("{dir}/delete-me.txt");
    let replaced = format!("{dir}/replace");
    let old = format!("{replaced}/old.txt");
    let replacement = format!("{replaced}/new.txt");
    let foreign = format!("{dir}/foreign.txt");

    lease.call_ok(
        catalog::SANDBOX_FILE_WRITE,
        json!({"path": &deleted, "content": "delete me\n", "overwrite": true}),
    )?;
    lease.call_ok(
        catalog::SANDBOX_FILE_WRITE,
        json!({"path": &old, "content": "old\n", "overwrite": true}),
    )?;

    let overlay = exec_settled(
        &lease,
        json!({
            "cmd": format!("rm -f {deleted} {old} && mkdir -p {replaced} && printf new > {replacement}"),
            "yield_time_ms": 8000,
            "timeout_seconds": 10,}),
    )?;
    assert_eq!(as_str(&overlay, "status")?, "ok", "{overlay}");
    let changed = array(&overlay, "changed_paths")?;
    assert!(
        changed
            .iter()
            .any(|path| path.as_str() == Some(deleted.as_str())),
        "delete whiteout should be reported as a changed path: {overlay}"
    );
    assert!(
        changed
            .iter()
            .any(|path| path.as_str() == Some(replacement.as_str())),
        "replacement write should publish the new file: {overlay}"
    );

    let deleted_read = lease.call_ok(catalog::SANDBOX_FILE_READ, json!({"path": &deleted}))?;
    assert!(
        !as_bool(&deleted_read, "exists")?,
        "deleted file must stay masked after overlay publish: {deleted_read}"
    );
    let replacement_read =
        lease.call_ok(catalog::SANDBOX_FILE_READ, json!({"path": &replacement}))?;
    assert_eq!(
        as_str(&replacement_read, "content")?,
        "new",
        "{replacement_read}"
    );
    let old_read = lease.call_ok(catalog::SANDBOX_FILE_READ, json!({"path": &old}))?;
    assert!(
        !as_bool(&old_read, "exists")?,
        "old replaced file must stay masked after overlay publish: {old_read}"
    );

    lease.client().request(
        catalog::SANDBOX_FILE_WRITE,
        &eos_e2e_test::next_invocation_id(),
        &json!({
            "layer_stack_root": lease.root(),
            "caller_id": format!("foreign-{}", eos_e2e_test::unique_suffix()),
            "path": &foreign,
            "content": "foreign publish\n",
            "overwrite": true
        }),
    )?;
    let foreign_read = lease.call_ok(catalog::SANDBOX_FILE_READ, json!({"path": &foreign}))?;
    assert_eq!(
        as_str(&foreign_read, "content")?,
        "foreign publish\n",
        "later reads must observe foreign-published workspace state: {foreign_read}"
    );
    Ok(())
}

#[test]
fn exec_upperdir_captures_only_the_delta() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    // Seed a 200KB base file via the fast path (lands in the lower layer stack).
    lease.call_ok(
        catalog::SANDBOX_FILE_WRITE,
        json!({"path": "perf/base_big.txt", "content": "x".repeat(200_000), "overwrite": true}),
    )?;
    // A tiny overlay write must capture only its own delta — the overlay does NOT
    // copy the 200KB base into the upperdir (the O(1)-lowerdir-disk property).
    let exec = exec_settled(
        &lease,
        json!({
            "cmd": "printf SMALL > perf/delta.txt",
            "yield_time_ms": 8000,
            "timeout_seconds": 10,}),
    )?;
    assert_eq!(as_str(&exec, "status")?, "ok", "{exec}");
    let upperdir_bytes = timing_f64(&exec, "resource.command_exec.upperdir_tree_bytes")
        .context("exec response must carry resource.command_exec.upperdir_tree_bytes")?;
    assert!(
        upperdir_bytes < 100_000.0,
        "upperdir delta must not copy the 200KB base (got {upperdir_bytes} bytes): {exec}"
    );
    assert!(
        array(&exec, "changed_paths")?
            .iter()
            .any(|path| path.as_str() == Some("perf/delta.txt")),
        "delta write must be captured: {exec}"
    );
    Ok(())
}

#[test]
fn exec_upperdir_is_flat_across_base_sizes() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    // Each overlay exec writes the same tiny delta over a progressively larger
    // lowerdir base. The daemon caps one write at 2 MiB, so each base is built
    // from many ~1MB files. The mount(2) overlay shares the base as a lowerdir,
    // so the captured upperdir stays delta-sized regardless of base size — the
    // O(1)-w.r.t.-workspace-size property, proven across a 15x base sweep rather
    // than at a single point.
    let mut upperdirs = Vec::new();
    for (index, file_count) in [1usize, 5, 15].into_iter().enumerate() {
        let total = seed_base_files(
            &lease,
            &format!("perf/flat/base-{index}"),
            file_count,
            1_000_000,
        )?;
        let exec = exec_settled(
            &lease,
            json!({
                "cmd": format!("printf SMALL > perf/flat/delta-{index}.txt"),
                "yield_time_ms": 8000,
                "timeout_seconds": 15,}),
        )?;
        assert_eq!(as_str(&exec, "status")?, "ok", "{exec}");
        let upperdir_bytes = timing_f64(&exec, "resource.command_exec.upperdir_tree_bytes")
            .context("exec response must carry resource.command_exec.upperdir_tree_bytes")?;
        assert!(
            upperdir_bytes < 100_000.0,
            "upperdir must stay delta-sized over a {total}-byte base (got {upperdir_bytes}): {exec}"
        );
        upperdirs.push(upperdir_bytes);
    }
    let max = upperdirs.iter().copied().fold(0.0_f64, f64::max);
    let min = upperdirs.iter().copied().fold(f64::MAX, f64::min);
    assert!(
        max - min < 50_000.0,
        "upperdir delta must stay flat across 15x lowerdir growth (got {upperdirs:?})"
    );
    wait_for_active_leases(&lease, 0)?;
    Ok(())
}

#[test]
fn exec_run_dir_scratch_stays_bounded() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    // Seed a ~5MB base into the lowerdir (5 sub-cap files), then write a tiny
    // delta. The overlay run dir holds only the published delta plus scratch
    // metadata, never the shared lowerdir, so its measured tree stays bounded
    // and untruncated.
    seed_base_files(&lease, "perf/scratch/base", 5, 1_000_000)?;
    let exec = exec_settled(
        &lease,
        json!({
            "cmd": "printf TINY > perf/scratch/delta.txt",
            "yield_time_ms": 8000,
            "timeout_seconds": 10,}),
    )?;
    assert_eq!(as_str(&exec, "status")?, "ok", "{exec}");
    let run_dir_bytes = timing_f64(&exec, "resource.command_exec.run_dir_tree_bytes")
        .context("exec response must carry resource.command_exec.run_dir_tree_bytes")?;
    assert!(
        run_dir_bytes < 1_000_000.0,
        "overlay scratch must stay bounded and exclude the 5MB base (got {run_dir_bytes}): {exec}"
    );
    let truncated = timing_f64(&exec, "resource.command_exec.run_dir_tree_truncated")
        .context("exec response must carry resource.command_exec.run_dir_tree_truncated")?;
    assert_eq!(
        truncated, 0.0,
        "run dir resource sample must not be truncated: {exec}"
    );
    wait_for_active_leases(&lease, 0)?;
    Ok(())
}

#[test]
fn cancelled_background_exec_does_not_publish_partial_workspace_mutation() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let path = format!("cancel-no-partial/{}.txt", eos_e2e_test::unique_suffix());
    let exec = lease.call_ok(
        catalog::SANDBOX_COMMAND_EXEC,
        json!({
            "cmd": format!("bash -lc 'printf READY; sleep 30; mkdir -p cancel-no-partial; printf partial > {path}'"),
            "yield_time_ms": 500,
            "timeout_seconds": 60,}),
    )?;
    assert_eq!(as_str(&exec, "status")?, "running", "{exec}");
    assert!(
        stdout(&exec).contains("READY"),
        "background command must reach the pre-write point before cancel: {exec}"
    );
    let session_id = as_str(&exec, "command_id")?.to_owned();
    lease.call(
        catalog::SANDBOX_COMMAND_CANCEL,
        json!({"command_id": session_id}),
    )?;
    wait_for_session_count(&lease, 0)?;
    let metrics = wait_for_active_leases(&lease, 0)?;
    assert_eq!(as_i64(&metrics, "active_leases")?, 0, "{metrics}");

    let read = lease.call_ok(catalog::SANDBOX_FILE_READ, json!({"path": path}))?;
    assert!(
        !as_bool(&read, "exists")?,
        "cancelled background exec must not publish the later workspace write: {read}"
    );
    Ok(())
}

#[test]
fn exec_overlay_mount_publishes_changed_paths() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let exec = exec_settled(
        &lease,
        json!({
            "cmd": "mkdir -p overlay && printf from-overlay > overlay/exec.txt",
            "yield_time_ms": 8000,
            "timeout_seconds": 10,}),
    )?;
    assert_eq!(as_str(&exec, "status")?, "ok");
    assert_eq!(as_i64(&exec, "exit_code")?, 0);
    assert!(
        array(&exec, "changed_paths")?
            .iter()
            .any(|path| path.as_str() == Some("overlay/exec.txt")),
        "exec overlay should publish captured upperdir paths: {exec}"
    );
    let read = lease.call_ok(
        catalog::SANDBOX_FILE_READ,
        json!({"path": "overlay/exec.txt"}),
    )?;
    assert_eq!(as_str(&read, "content")?, "from-overlay");
    Ok(())
}

#[test]
fn long_running_exec_conflicts_after_direct_write() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    let path = format!("stale-exec/{}.txt", unique_suffix().replace('-', "_"));
    lease.call_ok(
        catalog::SANDBOX_FILE_WRITE,
        json!({"path": path, "content": "base\n", "overwrite": true}),
    )?;

    let exec = lease.call_ok(
        catalog::SANDBOX_COMMAND_EXEC,
        json!({
            "cmd": format!("bash -lc 'printf SNAPSHOT_READY; sleep 2; printf stale-session > {path}'"),
            "yield_time_ms": 500,
            "timeout_seconds": 30,}),
    )?;
    assert_eq!(
        as_str(&exec, "status")?,
        "running",
        "long-running exec must hold its old snapshot: {exec}"
    );
    assert!(
        stdout(&exec).contains("SNAPSHOT_READY"),
        "exec must have started before the direct write races it: {exec}"
    );
    let session_id = as_str(&exec, "command_id")?.to_owned();

    let body = (|| -> Result<()> {
        let direct = lease.call_ok(
            catalog::SANDBOX_FILE_WRITE,
            json!({"path": path, "content": "direct-write\n", "overwrite": true}),
        )?;
        assert!(
            as_bool(&direct, "published")?,
            "direct write should publish the newer content: {direct}"
        );

        let result = wait_for_completion(&lease, &session_id)?;
        assert_eq!(
            as_str(&result, "workspace")?,
            "ephemeral",
            "background exec completion should finalize through ephemeral workspace: {result}"
        );
        assert_eq!(
            as_str(&result, "status")?,
            "ok",
            "the command process itself should complete normally: {result}"
        );
        assert!(
            !as_bool(&result, "success")?,
            "stale publish must not report a successful workspace mutation: {result}"
        );
        assert_eq!(
            conflict_reason(&result),
            "aborted_version",
            "stale snapshot publish should surface the OCC stale-version conflict: {result}"
        );
        assert!(
            array(&result, "changed_paths")?.is_empty(),
            "conflicted stale exec must not publish changed paths: {result}"
        );
        wait_for_session_count(&lease, 0)?;

        let read = lease.call_ok(catalog::SANDBOX_FILE_READ, json!({"path": path}))?;
        assert_eq!(
            as_str(&read, "content")?,
            "direct-write\n",
            "newer direct-write content must be preserved after stale exec finalization: {read}"
        );
        Ok(())
    })();

    if body.is_err() {
        let _ = lease.call(
            catalog::SANDBOX_COMMAND_CANCEL,
            json!({"command_id": session_id}),
        );
        let _ = wait_for_session_count(&lease, 0);
    }
    body
}

fn wait_for_completion(lease: &NodeLease<'_>, session_id: &str) -> Result<Value> {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let collected = lease.call_ok(
            catalog::SANDBOX_COMMAND_COLLECT_COMPLETED,
            json!({"command_ids": [session_id]}),
        )?;
        if let Some(completion) = array(&collected, "completions")?.first() {
            return completion
                .get("result")
                .cloned()
                .context("completion missing result");
        }
        if Instant::now() >= deadline {
            bail!("session {session_id} never completed");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
