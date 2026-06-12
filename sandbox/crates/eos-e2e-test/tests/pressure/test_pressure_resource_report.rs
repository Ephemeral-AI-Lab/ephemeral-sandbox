use std::fs;
use std::time::{Duration, Instant};

use anyhow::{ensure, Context, Result};
use eos_e2e_test::unique_suffix;
use eos_operation::core::catalog;
use eos_trace::ResourceStatsKind;
use serde_json::{json, Value};

use crate::helpers::{
    ensure_response_step, ensure_trace_resource, finalize_foreground_command_wire, pressure_levels,
    response_result, trace_resource_number, workload_timeout_s,
};
use crate::support::{
    as_bool, as_i64, as_str, live_pool_or_skip, seed_base_files, wait_for_active_leases,
    wait_for_command_count,
};

#[test]
fn resource_report_smoke() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let levels = pressure_levels(&pool)?;
    let workload = pool.workload().clone();
    let timeout_s = workload_timeout_s(&pool);
    let lease = pool.acquire()?;
    let mut samples = Vec::with_capacity(workload.sample_count);

    for sample in 0..workload.sample_count {
        let path = format!("pressure/resource/sample-{sample}.txt");
        let write_wire = lease.call(
            catalog::SANDBOX_FILE_WRITE,
            json!({
                "path": path,
                "content": format!("resource-sample-{sample}\n"),
                "overwrite": true
            }),
        )?;
        let write = response_result(&write_wire)?.clone();
        let read_wire = lease.call(
            catalog::SANDBOX_FILE_READ,
            json!({"path": format!("pressure/resource/sample-{sample}.txt")}),
        )?;
        let read = response_result(&read_wire)?.clone();
        assert_eq!(
            as_str(&read, "content")?,
            format!("resource-sample-{sample}\n"),
            "resource report readback should match: {read}"
        );

        let exec_wire = lease.call(
            catalog::SANDBOX_COMMAND_EXEC,
            json!({
                "cmd": format!("mkdir -p pressure/resource && printf exec-{sample} > pressure/resource/exec-{sample}.txt"),
                "yield_time_ms": 1000,
                "timeout_seconds": timeout_s,}),
        )?;
        // The command can outlast the 1s yield under emulation and return status
        // "running" (whose timings carry only the runtime.* keys); finalize to the
        // finalized payload so the terminal status and upperdir timings hold.
        let (exec_wire, exec) = finalize_foreground_command_wire(
            &lease,
            exec_wire,
            Instant::now() + Duration::from_secs(timeout_s + 5),
        )?;
        assert_eq!(as_str(&exec, "status")?, "ok", "{exec}");
        assert_eq!(as_i64(&exec, "exit_code")?, 0, "{exec}");
        ensure_response_step(&exec_wire, "dispatch")?;
        ensure_trace_resource(
            &exec_wire,
            ResourceStatsKind::Tree,
            "resource.command_exec.upperdir",
        )?;
        ensure_trace_resource(
            &exec_wire,
            ResourceStatsKind::Tree,
            "resource.command_exec.run_dir",
        )?;
        ensure_response_step(&write_wire, "dispatch")?;
        ensure_response_step(&read_wire, "dispatch")?;

        // Memory gauges are collected per op via the cgroup/process collector.
        // Assert presence and a generous absolute ceiling — these are gauges
        // inflated by page cache on lowerdir reads, not delta-proportional, so a
        // tight O(1) bound would flake. The peak gauge is kernel-dependent
        // (cgroup v2 `memory.peak`, Linux 5.19+) and stays optional.
        let memory_current = trace_resource_number(
            &write_wire,
            ResourceStatsKind::CgroupProcess,
            "daemon.response_timings",
            &["cgroup", "memory", "current_bytes"],
        )?;
        ensure!(
            memory_current > 0.0 && memory_current < 64e9,
            "cgroup memory.current gauge should be present and sane: {memory_current}"
        );

        let session = lease.call_ok(
            catalog::SANDBOX_COMMAND_EXEC,
            json!({
                "cmd": format!("sh -c 'echo resource-report-{sample}; sleep 60'"),
                "yield_time_ms": 100,
                "timeout_seconds": timeout_s,}),
        )?;
        assert_eq!(as_str(&session, "status")?, "running", "{session}");
        // COMMAND_CANCEL returns the cancelled command's own outcome, whose
        // response `success` is false for a killed command — use `call` (the
        // command-cancel convention, as in the isolated-workspace tests) rather
        // than `call_ok`, then assert the structured status below.
        let cancel = lease.call(
            catalog::SANDBOX_COMMAND_CANCEL,
            json!({"command_id": as_str(&session, "command_id")?}),
        )?;
        let cancel = response_result(&cancel)?.clone();
        assert!(
            matches!(as_str(&cancel, "status")?, "cancelled" | "ok" | "error"),
            "resource report cancel should return structured status: {cancel}"
        );
        wait_for_command_count(&lease, 0)?;
        let metrics = wait_for_active_leases(&lease, 0)?;
        let command_count = lease.call_ok(catalog::SANDBOX_COMMAND_COUNT, json!({}))?;

        samples.push(json!({
            "sample": sample,
            "write_status": write.get("status").and_then(Value::as_str),
            "read_trace_step": "dispatch",
            "exec_trace_resources": ["resource.command_exec.upperdir", "resource.command_exec.run_dir"],
            "memory_current_bytes": memory_current,
            "command_status": as_str(&session, "status")?,
            "cancel_status": as_str(&cancel, "status")?,
            "metrics": metrics,
            "command_count": command_count,
        }));
    }

    let final_metrics = wait_for_active_leases(&lease, 0)?;
    let final_command_count = lease.call_ok(catalog::SANDBOX_COMMAND_COUNT, json!({}))?;
    let ready = lease.call_ok(catalog::SANDBOX_RUNTIME_READY, json!({}))?;
    assert!(as_bool(&ready, "ready")?, "{ready}");
    let plugin_status = lease.call_ok(catalog::SANDBOX_PLUGIN_STATUS, json!({}))?;
    let isolated_open = lease.call_ok(catalog::SANDBOX_ISOLATION_LIST_OPEN, json!({}))?;

    let report = json!({
        "schema_version": 1,
        "module": "pressure",
        "scenario": "resource_report_smoke",
        "workload": {
            "concurrency_levels": levels,
            "write_iterations": workload.write_iterations,
            "sample_count": workload.sample_count,
            "timeout_s": timeout_s,
        },
        "samples": samples,
        "leak_counters": {
            "active_leases": as_i64(&final_metrics, "active_leases")?,
            "command_count": as_i64(&final_command_count, "count")?,
            "open_isolated_callers": isolated_open
                .get("open_caller_ids")
                .and_then(Value::as_array)
                .map_or(0, Vec::len),
        },
        "final_metrics": final_metrics,
        "runtime_ready": ready,
        "plugin_status": plugin_status,
        "isolated_open": isolated_open,
    });

    ensure!(
        report["samples"]
            .as_array()
            .is_some_and(|samples| !samples.is_empty()),
        "resource report should include samples: {report}"
    );
    ensure_eq_zero(&report, "active_leases")?;
    ensure_eq_zero(&report, "command_count")?;

    let artifact_dir = workload.perf_artifact_dir;
    fs::create_dir_all(&artifact_dir)
        .with_context(|| format!("create perf artifact dir {}", artifact_dir.display()))?;
    let artifact = artifact_dir.join(format!(
        "pressure-resource-report-{}.json",
        unique_suffix().replace('-', "_")
    ));
    fs::write(&artifact, serde_json::to_vec_pretty(&report)?)
        .with_context(|| format!("write {}", artifact.display()))?;
    let parsed: Value = serde_json::from_slice(
        &fs::read(&artifact).with_context(|| format!("read {}", artifact.display()))?,
    )?;
    assert_eq!(parsed["scenario"], "resource_report_smoke");
    Ok(())
}

#[test]
fn large_base_overlay_keeps_memory_bounded() -> Result<()> {
    let Some(pool) = live_pool_or_skip()? else {
        return Ok(());
    };
    let lease = pool.acquire()?;
    // A large lowerdir base plus a tiny overlay delta must not balloon daemon
    // memory: the base is shared via mount(2), never made resident per op. This
    // is a loose regression gauge (page cache inflates the gauge), not a tight
    // O(1) bound. The ~20MB base is built from sub-cap files (2 MiB write cap).
    seed_base_files(&lease, "pressure/mem/base", 20, 1_000_000)?;
    let exec = lease.call(
        catalog::SANDBOX_COMMAND_EXEC,
        json!({
            "cmd": "printf TINY > pressure/mem/delta.txt",
            "yield_time_ms": 1000,
            "timeout_seconds": 30,}),
    )?;
    let (_, exec) =
        finalize_foreground_command_wire(&lease, exec, Instant::now() + Duration::from_secs(35))?;
    assert_eq!(as_str(&exec, "status")?, "ok", "{exec}");
    // Memory gauges land on the fast-path file response; sample one after the op.
    let probe = lease.call(
        catalog::SANDBOX_FILE_WRITE,
        json!({"path": "pressure/mem/probe.txt", "content": "probe\n", "overwrite": true}),
    )?;
    let memory_current = trace_resource_number(
        &probe,
        ResourceStatsKind::CgroupProcess,
        "daemon.response_timings",
        &["cgroup", "memory", "current_bytes"],
    )?;
    ensure!(
        memory_current < 8e9,
        "daemon cgroup memory.current after a 20MB-base overlay op should stay well under 8GB (got {memory_current}): {probe}"
    );
    if let Ok(rss) = trace_resource_number(
        &probe,
        ResourceStatsKind::CgroupProcess,
        "daemon.response_timings",
        &["process", "gauges", "rss_bytes"],
    ) {
        ensure!(
            rss > 0.0 && rss < 8e9,
            "daemon process RSS gauge should be present and sane: {rss}"
        );
    }
    wait_for_active_leases(&lease, 0)?;
    Ok(())
}

fn ensure_eq_zero(report: &Value, key: &str) -> Result<()> {
    let value = report
        .get("leak_counters")
        .and_then(|counters| counters.get(key))
        .and_then(Value::as_i64)
        .with_context(|| format!("leak_counters.{key} missing in report"))?;
    ensure!(value == 0, "leak_counters.{key} should be zero: {report}");
    Ok(())
}
