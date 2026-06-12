//! Plugin op adapter tests: wire arg parsing, response shaping, registered-op
//! routing through the dispatcher, and the isolated-caller gate. Service
//! process behavior (start/refresh/restart/health) lives in
//! the operation-runtime tests.

mod support;

use support::*;

use crate::wire::Request;
use eos_layerstack::{ChangesetResult, CommitStatus, FileResult, LayerPath};
use eos_namespace::protocol::RunResult;
use eos_operation::{
    plugin::{
        EnsureReady, PackageEnsureReport, PluginOverlayOutcome, PluginRuntimeError,
        PluginSetupReport, PpcError, PpcTraceEvent, ServiceHealthReport, ServiceProcessStatus,
        StatusOutcome,
    },
    ChangedPathKind,
};
use eos_plugin::PluginServiceState;
use eos_workspace::TreeResourceStats;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Instant;

fn ok_result(response: &Value) -> &Value {
    assert_eq!(response["status"], "ok", "{response}");
    assert!(response.get("success").is_none(), "{response}");
    response.get("result").expect("ok envelope has result")
}

fn error_fault<'a>(response: &'a Value, status: &str) -> &'a Value {
    assert_eq!(response["status"], status, "{response}");
    assert!(response.get("success").is_none(), "{response}");
    response.get("error").expect("error envelope has fault")
}

#[test]
fn ensure_records_manifest_services_and_status_lists_them() -> TestResult {
    let daemon = TestDaemon::new();
    let response = daemon.op_ensure(&json!({
        "manifest": generic_service_manifest("digest-a", "hover"),
        "layer_stack_root": "/eos/plugin/layer-stack",
        "workspace_root": "/eos/plugin/workspace"
    }))?;
    assert_eq!(response["success"], true);
    assert_eq!(response["registered_ops"], json!(["plugin.generic.hover"]));
    assert_eq!(
        response["operation_routes"][0]["dispatch_mode"],
        "read_only_service"
    );
    assert_eq!(response["services"][0]["state"], "stopped");
    assert_eq!(response["service_processes"][0]["service_id"], "worker");
    assert!(value_str(
        &response["service_processes"][0]["socket_path"],
        "socket path must be a string"
    )?
    .starts_with("/eos/plugin/ppc/"));
    assert!(value_str(
        &response["service_processes"][0]["stderr_path"],
        "stderr path must be a string"
    )?
    .ends_with(".stderr.log"));

    let status = daemon.op_status(&json!({}))?;
    assert_eq!(status["loaded_plugins"][0]["name"], "generic");
    Ok(())
}

#[test]
fn ensure_exposes_package_roots_to_service_process_specs() -> TestResult {
    let daemon = TestDaemon::new();
    let response = daemon.op_ensure(&json!({
        "manifest": generic_service_manifest("digest-a", "hover"),
        "layer_stack_root": "/eos/plugin/layer-stack",
        "workspace_root": "/eos/plugin/workspace"
    }))?;
    let process = &response["service_processes"][0];
    assert_eq!(
        process["package_root"],
        "/eos/runtime/plugins/catalog/generic/digest-a"
    );
    assert_eq!(
        process["dependency_root"],
        "/eos/runtime/packages/generic/digest-a"
    );
    assert_eq!(
        process["working_dir"],
        "/eos/runtime/plugins/catalog/generic/digest-a"
    );
    assert_eq!(
        process["env"]["EOS_PLUGIN_PACKAGE_ROOT"],
        "/eos/runtime/plugins/catalog/generic/digest-a"
    );
    assert_eq!(
        process["env"]["EOS_PLUGIN_DEPENDENCY_ROOT"],
        "/eos/runtime/packages/generic/digest-a"
    );
    Ok(())
}

#[test]
fn ensure_resolves_service_relative_command_under_package_working_dir() -> TestResult {
    let daemon = TestDaemon::new();
    let mut manifest =
        generic_service_manifest_with_command("digest-a", "hover", vec!["./server.py"]);
    manifest["services"][0]["working_dir"] = json!("runtime");
    let response = daemon.op_ensure(&json!({
        "manifest": manifest,
        "layer_stack_root": "/eos/plugin/layer-stack",
        "workspace_root": "/eos/plugin/workspace"
    }))?;
    let expected = "/eos/runtime/plugins/catalog/generic/digest-a/runtime/server.py";
    assert_eq!(response["service_processes"][0]["command"][0], expected);
    assert_eq!(
        response["operation_routes"][0]["service_command"][0],
        expected
    );
    assert_eq!(
        response["service_processes"][0]["working_dir"],
        "/eos/runtime/plugins/catalog/generic/digest-a/runtime"
    );
    Ok(())
}

#[test]
fn ensure_is_idempotent_for_same_digest() -> TestResult {
    let daemon = TestDaemon::new();
    let first = daemon.op_ensure(&json!({"plugin": "demo", "digest": "a"}))?;
    let second = daemon.op_ensure(&json!({"plugin": "demo", "digest": "a"}))?;
    assert_eq!(first["already_loaded"], false);
    assert_eq!(second["already_loaded"], true);
    Ok(())
}

#[test]
fn package_warm_missing_returns_needs_upload() -> TestResult {
    let daemon = TestDaemon::new();
    let roots = PackageTestRoots::new("warm-missing")?;
    let response = daemon.op_ensure(&roots.args(
        package_manifest("digest-a", "setup-a", vec!["./setup.sh"]),
        None,
    ))?;
    assert_eq!(response["success"], true);
    assert_eq!(response["needs_upload"], true);
    assert_eq!(response["ready"], false);
    assert_eq!(response["plugin"], "generic");
    roots.cleanup();
    Ok(())
}

#[test]
fn package_cold_publish_setup_and_warm_reensure_are_idempotent() -> TestResult {
    let daemon = TestDaemon::new();
    let roots = PackageTestRoots::new("cold-publish")?;
    let staged = roots.stage_package(
        "digest-a",
        r#"#!/bin/sh
set -eu
count_file="$EOS_PLUGIN_DEPENDENCY_ROOT/cache/setup-count"
count=0
if [ -f "$count_file" ]; then count="$(cat "$count_file")"; fi
count=$((count + 1))
printf '%s' "$count" > "$count_file"
printf tmp > "$TMPDIR/setup.tmp"
"#,
    )?;
    let manifest = package_manifest("digest-a", "setup-a", vec!["./setup.sh"]);

    let cold = daemon.op_ensure(&roots.args(manifest.clone(), Some(&staged)))?;
    assert_eq!(cold["success"], true);
    assert_eq!(cold["package"]["package_published"], true);
    assert_eq!(cold["package"]["setup_ran"], true);
    assert!(roots
        .package_root("digest-a")
        .join(".package-sha256")
        .is_file());
    assert!(roots
        .package_root("digest-a")
        .join(".setup-sha256")
        .is_file());
    assert_eq!(
        std::fs::read_to_string(roots.dependency_root("digest-a").join("cache/setup-count"))?,
        "1"
    );
    assert!(roots.setup_root("digest-a").join("tmp/setup.tmp").is_file());

    let warm = daemon.op_ensure(&roots.args(manifest, None))?;
    assert_eq!(warm["success"], true);
    assert_eq!(warm["package"]["needs_upload"], false);
    assert_eq!(warm["package"]["setup_ran"], false);
    assert_eq!(
        std::fs::read_to_string(roots.dependency_root("digest-a").join("cache/setup-count"))?,
        "1"
    );

    roots.cleanup();
    Ok(())
}

#[test]
fn package_changed_digest_runs_setup_for_new_dependency_root() -> TestResult {
    let daemon = TestDaemon::new();
    let roots = PackageTestRoots::new("changed-digest")?;
    let setup_script = r#"#!/bin/sh
set -eu
printf setup > "$EOS_PLUGIN_DEPENDENCY_ROOT/cache/setup-ran"
"#;

    let staged_a = roots.stage_package("digest-a", setup_script)?;
    let cold_a = daemon.op_ensure(&roots.args(
        package_manifest("digest-a", "setup-a", vec!["./setup.sh"]),
        Some(&staged_a),
    ))?;
    assert_eq!(cold_a["package"]["setup_ran"], true);

    let staged_b = roots.stage_package("digest-b", setup_script)?;
    let cold_b = daemon.op_ensure(&roots.args(
        package_manifest("digest-b", "setup-b", vec!["./setup.sh"]),
        Some(&staged_b),
    ))?;
    assert_eq!(cold_b["package"]["setup_ran"], true);
    assert!(roots
        .dependency_root("digest-a")
        .join("cache/setup-ran")
        .is_file());
    assert!(roots
        .dependency_root("digest-b")
        .join("cache/setup-ran")
        .is_file());

    roots.cleanup();
    Ok(())
}

#[test]
fn package_rejects_staging_outside_digest_upload_root() -> TestResult {
    let daemon = TestDaemon::new();
    let roots = PackageTestRoots::new("bad-stage")?;
    let outside = roots.root.join("outside/package");
    std::fs::create_dir_all(&outside)?;
    let err = daemon
        .op_ensure(&roots.args(
            package_manifest("digest-a", "setup-a", vec!["./setup.sh"]),
            Some(&outside),
        ))
        .expect_err("staging outside digest upload root must be rejected");
    assert!(err
        .to_string()
        .contains("staged_package_root must be under"));
    roots.cleanup();
    Ok(())
}

#[test]
fn package_setup_failure_is_visible_in_status_and_prevents_service_start() -> TestResult {
    let daemon = TestDaemon::new();
    let roots = PackageTestRoots::new("setup-failure")?;
    let staged = roots.stage_package("digest-a", "#!/bin/sh\nexit 7\n")?;
    let err = daemon
        .op_ensure(&roots.args(
            package_manifest("digest-a", "setup-a", vec!["./setup.sh"]),
            Some(&staged),
        ))
        .expect_err("setup failure must reject package ensure");
    assert!(err.to_string().contains("plugin setup failed"));

    let status = daemon.op_status(&json!({}))?;
    assert_eq!(status["setup_failures"][0]["plugin"], "generic");
    assert_eq!(status["running_service_processes"], json!([]));
    roots.cleanup();
    Ok(())
}

#[test]
fn package_setup_rejects_forbidden_rootfs_writes() -> TestResult {
    let daemon = TestDaemon::new();
    let roots = PackageTestRoots::new("forbidden-root")?;
    let staged = roots.stage_package(
        "digest-a",
        r#"#!/bin/sh
set -eu
touch /root/plugin
"#,
    )?;
    let err = daemon
        .op_ensure(&roots.args(
            package_manifest("digest-a", "setup-a", vec!["./setup.sh"]),
            Some(&staged),
        ))
        .expect_err("setup script with forbidden rootfs write must be rejected");
    assert!(err.to_string().contains("forbidden managed root /root"));
    roots.cleanup();
    Ok(())
}

#[test]
fn ensure_reloads_same_digest_when_workspace_root_changes() -> TestResult {
    let daemon = TestDaemon::new();
    let first = daemon.op_ensure(&json!({
        "manifest": generic_service_manifest("digest-a", "hover"),
        "layer_stack_root": "/eos/plugin/layer-stack",
        "workspace_root": "/testbed"
    }))?;
    let second = daemon.op_ensure(&json!({
        "manifest": generic_service_manifest("digest-a", "hover"),
        "layer_stack_root": "/eos/plugin/layer-stack",
        "workspace_root": "/ephemeral-os"
    }))?;

    assert_eq!(first["already_loaded"], false);
    assert_eq!(second["already_loaded"], false);
    assert_eq!(
        first["service_processes"][0]["env"]["EOS_PLUGIN_WORKSPACE_ROOT"],
        "/testbed"
    );
    assert_eq!(
        second["service_processes"][0]["env"]["EOS_PLUGIN_WORKSPACE_ROOT"],
        "/ephemeral-os"
    );
    Ok(())
}

#[test]
fn builtin_dispatch_routes_plugin_status_and_ensure() -> TestResult {
    let daemon = TestDaemon::new();
    let ensure = daemon.dispatch(&Request {
        op: "sandbox.plugin.ensure".to_owned(),
        invocation_id: "plugin-ensure-test".to_owned(),
        args: json!({"plugin": "demo", "digest": "a"}),
    });
    let ensure = ok_result(&ensure);
    assert_eq!(ensure["success"], true);

    let status = daemon.dispatch(&Request {
        op: "sandbox.plugin.status".to_owned(),
        invocation_id: "plugin-status-test".to_owned(),
        args: json!({}),
    });
    let status = ok_result(&status);
    assert_eq!(status["success"], true);
    let loaded = value_array(&status["loaded_plugins"], "loaded_plugins must be an array")?;
    assert!(loaded.iter().any(|plugin| plugin["name"] == "demo"));
    Ok(())
}

#[test]
fn status_trace_events_include_service_health_and_exit_facts() {
    let sink = crate::trace::RequestTraceEventSink::default();
    let context = crate::DispatchContext::empty().with_trace_events(sink.clone());
    let outcome = StatusOutcome {
        loaded_plugins: Vec::new(),
        running_service_processes: Vec::new(),
        exited_service_processes: vec![ServiceProcessStatus {
            service_id: "worker".to_owned(),
            service_instance_id: "generic:worker:profile-a".to_owned(),
            pid: 1234,
            process_group_id: Some(1234),
            running: false,
            exit_status: None,
            exit_signal: Some(15),
            status_raw: Some(15),
            socket_path: PathBuf::from("/eos/plugin/ppc/worker.sock"),
            stderr_path: PathBuf::from("/eos/plugin/ppc/worker.stderr.log"),
        }],
        connected_ppc_routes: Vec::new(),
        connected_ppc_services: Vec::new(),
        setup_failures: Vec::new(),
        service_health: vec![ServiceHealthReport {
            success: true,
            plugin: "generic".to_owned(),
            service_id: "worker".to_owned(),
            service_instance_id: "generic:worker:profile-a".to_owned(),
            manifest_key: "manifest:7".to_owned(),
            state: PluginServiceState::Ready,
            restart_count: 2,
            refresh_count: 3,
            last_error: Some("previous refresh failed".to_owned()),
            accepted: Some(true),
            error: None,
            teardown_error: None,
        }],
    };

    super::record_plugin_status_trace_events(&context, &outcome);

    let events = sink.drain();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].module, "plugin");
    assert_eq!(events[0].name, "service_health_checked");
    assert_eq!(events[0].details["plugin"], "generic");
    assert_eq!(events[0].details["service_id"], "worker");
    assert_eq!(
        events[0].details["service_instance_id"],
        "generic:worker:profile-a"
    );
    assert_eq!(events[0].details["manifest_key"], "manifest:7");
    assert_eq!(events[0].details["state"], "ready");
    assert_eq!(events[0].details["restart_count"], 2);
    assert_eq!(events[0].details["refresh_count"], 3);
    assert_eq!(events[0].details["last_error"], "previous refresh failed");
    assert_eq!(events[0].details["accepted"], true);
    assert_eq!(events[0].details["success"], true);

    assert_eq!(events[1].module, "plugin");
    assert_eq!(events[1].name, "service_exited");
    assert_eq!(events[1].details["service_id"], "worker");
    assert_eq!(
        events[1].details["service_instance_id"],
        "generic:worker:profile-a"
    );
    assert_eq!(events[1].details["pid"], 1234);
    assert_eq!(events[1].details["process_group_id"], 1234);
    assert!(events[1].details["exit_code"].is_null());
    assert_eq!(events[1].details["signal"], 15);
    assert_eq!(events[1].details["status_raw"], 15);
    assert_eq!(
        events[1].details["socket_path"],
        "/eos/plugin/ppc/worker.sock"
    );
    assert_eq!(
        events[1].details["stderr_path"],
        "/eos/plugin/ppc/worker.stderr.log"
    );
}

#[test]
fn ppc_trace_events_are_recorded_in_request_sidecar_sink() {
    let sink = crate::trace::RequestTraceEventSink::default();
    let context = crate::DispatchContext::empty().with_trace_events(sink.clone());

    super::record_ppc_trace_events(
        &context,
        vec![
            PpcTraceEvent::new(
                "callback_request",
                json!({
                    "message_id": "callback-1",
                    "parent_message_id": "plugin-op-1",
                    "direction": "request",
                    "op": "daemon.occ.apply_changeset",
                }),
            ),
            PpcTraceEvent::new(
                "ppc_reply_orphaned",
                json!({
                    "message_id": "late-reply",
                    "direction": "reply",
                    "op": "reply",
                    "reason": "unknown_message_id",
                }),
            ),
        ],
    );

    let events = sink.drain();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].module, "plugin");
    assert_eq!(events[0].name, "callback_request");
    assert_eq!(events[0].details["message_id"], "callback-1");
    assert_eq!(events[0].details["parent_message_id"], "plugin-op-1");
    assert_eq!(events[1].name, "ppc_reply_orphaned");
    assert_eq!(events[1].details["message_id"], "late-reply");
    assert_eq!(events[1].details["reason"], "unknown_message_id");
}

#[test]
fn ensure_trace_events_include_service_started_stderr_path() {
    let sink = crate::trace::RequestTraceEventSink::default();
    let context = crate::DispatchContext::empty().with_trace_events(sink.clone());
    let ready = EnsureReady {
        plugin_id: "generic".to_owned(),
        digest: "digest-a".to_owned(),
        registered_ops: Vec::new(),
        runtime_loaded: true,
        started_count: 1,
        already_loaded: false,
        operation_routes: Vec::new(),
        services: Vec::new(),
        service_processes: Vec::new(),
        started_service_processes: vec![ServiceProcessStatus {
            service_id: "worker".to_owned(),
            service_instance_id: "generic:worker:profile-a".to_owned(),
            pid: 1234,
            process_group_id: Some(1234),
            running: true,
            exit_status: None,
            exit_signal: None,
            status_raw: None,
            socket_path: PathBuf::from("/eos/plugin/ppc/worker.sock"),
            stderr_path: PathBuf::from("/eos/plugin/ppc/worker.stderr.log"),
        }],
        running_service_processes: Vec::new(),
        connected_ppc_routes: Vec::new(),
        connected_ppc_services: Vec::new(),
        package: PackageEnsureReport::default(),
    };

    super::record_plugin_ensure_trace_events(&context, &ready);

    let events = sink.drain();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].module, "plugin");
    assert_eq!(events[0].name, "service_started");
    assert_eq!(events[0].details["plugin"], "generic");
    assert_eq!(events[0].details["service_id"], "worker");
    assert_eq!(
        events[0].details["service_instance_id"],
        "generic:worker:profile-a"
    );
    assert_eq!(events[0].details["pid"], 1234);
    assert_eq!(events[0].details["process_group_id"], 1234);
    assert_eq!(events[0].details["running"], true);
    assert_eq!(
        events[0].details["socket_path"],
        "/eos/plugin/ppc/worker.sock"
    );
    assert_eq!(
        events[0].details["stderr_path"],
        "/eos/plugin/ppc/worker.stderr.log"
    );
}

#[test]
fn ensure_trace_events_include_setup_finished_success_report() {
    let sink = crate::trace::RequestTraceEventSink::default();
    let context = crate::DispatchContext::empty().with_trace_events(sink.clone());
    let ready = EnsureReady {
        plugin_id: "generic".to_owned(),
        digest: "digest-a".to_owned(),
        registered_ops: Vec::new(),
        runtime_loaded: true,
        started_count: 0,
        already_loaded: false,
        operation_routes: Vec::new(),
        services: Vec::new(),
        service_processes: Vec::new(),
        started_service_processes: Vec::new(),
        running_service_processes: Vec::new(),
        connected_ppc_routes: Vec::new(),
        connected_ppc_services: Vec::new(),
        package: PackageEnsureReport {
            active: true,
            needs_upload: false,
            package_root: None,
            dependency_root: None,
            package_published: true,
            setup_ran: true,
            setup: Some(PluginSetupReport {
                plugin: "generic".to_owned(),
                digest: "digest-a".to_owned(),
                ran: true,
                success: true,
                exit_code: Some(0),
                output_tail: Some("setup ok\n".to_owned()),
                spawn_error: None,
            }),
        },
    };

    super::record_plugin_ensure_trace_events(&context, &ready);

    let events = sink.drain();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].module, "plugin");
    assert_eq!(events[0].name, "setup_finished");
    assert_eq!(events[0].details["plugin"], "generic");
    assert_eq!(events[0].details["digest"], "digest-a");
    assert_eq!(events[0].details["ran"], true);
    assert_eq!(events[0].details["success"], true);
    assert_eq!(events[0].details["exit_code"], 0);
    assert_eq!(events[0].details["output_tail"], "setup ok\n");
    assert!(events[0].details["spawn_error"].is_null());
}

#[test]
fn ensure_error_trace_events_include_setup_finished_failure_report() {
    let sink = crate::trace::RequestTraceEventSink::default();
    let context = crate::DispatchContext::empty().with_trace_events(sink.clone());
    let err = PluginRuntimeError::Ppc(PpcError::SetupFailed {
        report: PluginSetupReport {
            plugin: "generic".to_owned(),
            digest: "digest-a".to_owned(),
            ran: true,
            success: false,
            exit_code: Some(7),
            output_tail: Some("boom\n".to_owned()),
            spawn_error: None,
        },
        message: "plugin setup failed with status Some(7): boom".to_owned(),
    });

    super::record_plugin_ensure_error_trace_events(&context, &err);

    let events = sink.drain();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].module, "plugin");
    assert_eq!(events[0].name, "setup_finished");
    assert_eq!(events[0].details["plugin"], "generic");
    assert_eq!(events[0].details["digest"], "digest-a");
    assert_eq!(events[0].details["ran"], true);
    assert_eq!(events[0].details["success"], false);
    assert_eq!(events[0].details["exit_code"], 7);
    assert_eq!(events[0].details["output_tail"], "boom\n");
    assert!(events[0].details["spawn_error"].is_null());
}

#[test]
fn overlay_trace_events_include_started_and_finished_facts() {
    let sink = crate::trace::RequestTraceEventSink::default();
    let context = crate::DispatchContext::empty().with_trace_events(sink.clone());
    let overlay = PluginOverlayOutcome {
        layer_stack_root: PathBuf::from("/eos/layer-stack"),
        runner: RunResult {
            exit_code: 0,
            payload: json!({
                "exit_code": 0,
                "timings": {
                    "workspace.mount_s": 0.15,
                    "workspace.unmount_s": 0.05,
                    "workspace.layer_count": 3,
                    "workspace.fsconfig_calls": 6,
                },
                "workspace_unmount_error": null,
            }),
        },
        changeset: ChangesetResult {
            files: vec![FileResult {
                path: LayerPath::parse("src/main.rs").expect("valid test layer path"),
                status: CommitStatus::Committed,
                message: String::new(),
                observed_version: None,
                observed_state: None,
            }],
            published_manifest_version: Some(42),
            timings: BTreeMap::new(),
            events: Vec::new(),
        },
        plugin_result: Some(json!({"success": true})),
        layer_count: 3,
        path_kinds: vec![("src/main.rs".to_owned(), ChangedPathKind::Write)],
        lease_acquire_s: 0.1,
        capture_s: 0.2,
        occ_s: 0.3,
        lease_release_error: Some("release failed".to_owned()),
        upperdir_stats: TreeResourceStats {
            files: 1,
            dirs: 2,
            symlinks: 3,
            bytes: 4096,
            truncated: true,
            read_error_count: 2,
            first_error_path: Some("/eos/work/blocked".to_owned()),
        },
    };
    let response = json!({
        "success": true,
        "status": "committed",
    });

    super::record_plugin_overlay_started(
        &context,
        "plugin.generic.write",
        "plugin-overlay-test",
        &overlay,
    );
    super::record_plugin_overlay_mount_finished(
        &context,
        "plugin.generic.write",
        "plugin-overlay-test",
        &overlay,
    );
    super::record_plugin_overlay_unmount_finished(
        &context,
        "plugin.generic.write",
        "plugin-overlay-test",
        &overlay,
    );
    super::record_plugin_overlay_capture_started(
        &context,
        "plugin.generic.write",
        "plugin-overlay-test",
        &overlay,
    );
    super::record_plugin_overlay_capture_finished(
        &context,
        "plugin.generic.write",
        "plugin-overlay-test",
        &overlay,
    );
    super::record_occ_changeset_trace_events(&context, &overlay.changeset);
    super::record_plugin_overlay_finished(
        &context,
        "plugin.generic.write",
        "plugin-overlay-test",
        &overlay,
        &response,
        None,
    );
    super::record_plugin_overlay_lease_release_failed(
        &context,
        "plugin.generic.write",
        "plugin-overlay-test",
        &overlay,
    );

    let events = sink.drain();
    assert_eq!(events.len(), 11);
    assert_eq!(events[0].module, "plugin");
    assert_eq!(events[0].name, "overlay_started");
    assert_eq!(events[0].details["op"], "plugin.generic.write");
    assert_eq!(events[0].details["invocation_id"], "plugin-overlay-test");
    assert_eq!(events[0].details["layer_stack_root"], "/eos/layer-stack");

    assert_eq!(events[1].module, "overlay");
    assert_eq!(events[1].name, "mount_finished");
    assert_eq!(events[1].details["op"], "plugin.generic.write");
    assert_eq!(events[1].details["invocation_id"], "plugin-overlay-test");
    assert_eq!(events[1].details["source"], "plugin_oneshot_overlay");
    assert_eq!(events[1].details["layer_stack_root"], "/eos/layer-stack");
    assert_eq!(events[1].details["success"], true);
    assert_eq!(events[1].details["duration_s"], 0.15);
    assert_eq!(events[1].details["duration_available"], true);
    assert_eq!(events[1].details["layer_count"], 3);
    assert_eq!(events[1].details["fsconfig_calls"], 6.0);
    assert_eq!(events[1].details["fsconfig_calls_available"], true);

    assert_eq!(events[2].module, "resource");
    assert_eq!(events[2].name, "resource_stats");
    assert_eq!(events[2].details["meta"]["stats_kind"], "mount_cost");
    assert_eq!(events[2].details["meta"]["phase"], "after");
    assert_eq!(events[2].details["meta"]["source"], "plugin.overlay.mount");
    assert_eq!(events[2].details["meta"]["source_available"], true);
    assert_eq!(events[2].details["meta"]["sampler_duration_us"], 0);
    assert!(events[2].details["meta"]["inflight_requests"].is_number());
    assert_eq!(events[2].details["mount"]["duration_us"], 150000);
    assert_eq!(events[2].details["mount"]["duration_available"], true);
    assert_eq!(events[2].details["mount"]["layer_count"], 3);
    assert_eq!(events[2].details["mount"]["fsconfig_calls"], 6.0);
    assert_eq!(events[2].details["mount"]["fsconfig_calls_available"], true);

    assert_eq!(events[3].module, "overlay");
    assert_eq!(events[3].name, "unmount_finished");
    assert_eq!(events[3].details["op"], "plugin.generic.write");
    assert_eq!(events[3].details["invocation_id"], "plugin-overlay-test");
    assert_eq!(events[3].details["source"], "plugin_oneshot_overlay");
    assert_eq!(events[3].details["layer_stack_root"], "/eos/layer-stack");
    assert_eq!(events[3].details["success"], true);
    assert_eq!(events[3].details["duration_s"], 0.05);
    assert_eq!(events[3].details["duration_available"], true);
    assert_eq!(events[3].details["layer_count"], 3);
    assert!(events[3].details["error"].is_null());

    assert_eq!(events[4].module, "overlay");
    assert_eq!(events[4].name, "capture_started");
    assert_eq!(events[4].details["op"], "plugin.generic.write");
    assert_eq!(events[4].details["invocation_id"], "plugin-overlay-test");
    assert_eq!(events[4].details["source"], "plugin_oneshot_overlay");
    assert_eq!(events[4].details["layer_stack_root"], "/eos/layer-stack");

    assert_eq!(events[5].module, "overlay");
    assert_eq!(events[5].name, "capture_finished");
    assert_eq!(events[5].details["op"], "plugin.generic.write");
    assert_eq!(events[5].details["invocation_id"], "plugin-overlay-test");
    assert_eq!(events[5].details["source"], "plugin_oneshot_overlay");
    assert_eq!(events[5].details["layer_stack_root"], "/eos/layer-stack");
    assert_eq!(events[5].details["success"], true);
    assert_eq!(events[5].details["duration_s"], 0.2);
    assert_eq!(events[5].details["changed_path_count"], 1);
    assert_eq!(events[5].details["bytes"], 4096);
    assert_eq!(events[5].details["file_count"], 1);
    assert_eq!(events[5].details["dir_count"], 2);
    assert_eq!(events[5].details["symlink_count"], 3);
    assert_eq!(events[5].details["entry_count"], 6);
    assert_eq!(events[5].details["truncated"], true);
    assert_eq!(events[5].details["read_error_count"], 2);
    assert_eq!(events[5].details["failing_path"], "/eos/work/blocked");

    assert_eq!(events[6].module, "occ");
    assert_eq!(events[6].name, "commit_started");
    assert_eq!(events[6].details["file_count"], 1);

    assert_eq!(events[7].module, "occ");
    assert_eq!(events[7].name, "validate_groups_finished");
    assert_eq!(events[7].details["file_count"], 1);
    assert_eq!(events[7].details["committed_file_count"], 1);

    assert_eq!(events[8].module, "occ");
    assert_eq!(events[8].name, "commit_finished");
    assert_eq!(events[8].details["success"], true);
    assert_eq!(events[8].details["published_manifest_version"], 42);
    assert_eq!(events[8].details["file_count"], 1);
    assert_eq!(events[8].details["published_file_count"], 1);
    assert_eq!(events[8].details["committed_file_count"], 1);

    assert_eq!(events[9].module, "plugin");
    assert_eq!(events[9].name, "overlay_finished");
    assert_eq!(events[9].details["op"], "plugin.generic.write");
    assert_eq!(events[9].details["invocation_id"], "plugin-overlay-test");
    assert_eq!(events[9].details["success"], true);
    assert_eq!(events[9].details["status"], "committed");
    assert!(events[9].details["error_kind"].is_null());
    assert!(events[9].details["adapter_error"].is_null());
    assert_eq!(events[9].details["worker_exit_code"], 0);
    assert_eq!(events[9].details["changed_path_count"], 1);
    assert_eq!(events[9].details["published_manifest_version"], 42);
    assert_eq!(events[9].details["lease_acquire_s"], 0.1);
    assert_eq!(events[9].details["lease_release_error"], "release failed");
    assert_eq!(events[9].details["capture_s"], 0.2);
    assert_eq!(events[9].details["occ_s"], 0.3);
    assert_eq!(events[9].details["upperdir_files"], 1);
    assert_eq!(events[9].details["upperdir_dirs"], 2);
    assert_eq!(events[9].details["upperdir_symlinks"], 3);
    assert_eq!(events[9].details["upperdir_bytes"], 4096);

    assert_eq!(events[10].module, "layer_stack");
    assert_eq!(events[10].name, "lease_release_failed");
    assert_eq!(events[10].details["op"], "plugin.generic.write");
    assert_eq!(events[10].details["invocation_id"], "plugin-overlay-test");
    assert_eq!(
        events[10].details["reason"],
        "plugin_overlay_release_failed"
    );
    assert_eq!(events[10].details["error"], "release failed");
}

#[test]
fn overlay_resource_stats_events_include_before_and_after_gauges() {
    let sink = crate::trace::RequestTraceEventSink::default();
    let context = crate::DispatchContext::empty().with_trace_events(sink.clone());
    let mut before = serde_json::Map::new();
    before.insert("resource.cgroup.cpu_usage_usec".to_owned(), json!(10.0));
    before.insert(
        "resource.cgroup.memory_current_bytes".to_owned(),
        json!(2048.0),
    );
    before.insert("resource.cgroup.io_rbytes".to_owned(), json!(32.0));
    before.insert("resource.cgroup.psi_cpu_some_avg10".to_owned(), json!(0.25));
    before.insert("resource.process.rss_bytes".to_owned(), json!(4096.0));
    before.insert(
        "resource.sampler.cgroup_process_duration_us".to_owned(),
        json!(17),
    );
    let mut after = before.clone();
    after.insert("resource.process.max_rss_bytes".to_owned(), json!(8192.0));
    after.insert(
        "resource.sampler.cgroup_process_duration_us".to_owned(),
        json!(19),
    );

    super::record_plugin_overlay_resource_stats(&context, "before", &before);
    super::record_plugin_overlay_host_resource_stats(&context, "before", &before);
    super::record_plugin_overlay_resource_stats(&context, "after", &after);
    super::record_plugin_overlay_host_resource_stats(&context, "after", &after);

    let events = sink.drain();
    assert_eq!(events.len(), 4);
    assert_eq!(events[0].module, "resource");
    assert_eq!(events[0].name, "resource_stats");
    assert_eq!(events[0].details["meta"]["stats_kind"], "cgroup_process");
    assert_eq!(events[0].details["meta"]["phase"], "before");
    assert_eq!(events[0].details["meta"]["source"], "plugin.overlay.run");
    assert_eq!(events[0].details["meta"]["source_available"], true);
    assert_eq!(events[0].details["meta"]["sampler_duration_us"], 17);
    assert!(events[0].details["meta"]["inflight_requests"].is_number());
    assert_eq!(events[0].details["cgroup"]["source_available"], true);
    assert_eq!(events[0].details["cgroup"]["cpu"]["usage_usec"], 10.0);
    assert_eq!(
        events[0].details["cgroup"]["memory"]["current_bytes"],
        2048.0
    );
    assert_eq!(events[0].details["cgroup"]["io"]["rbytes"], 32.0);
    assert_eq!(events[0].details["cgroup"]["psi"]["cpu_some_avg10"], 0.25);
    assert_eq!(events[0].details["process"]["source_available"], true);
    assert_eq!(events[0].details["process"]["gauges"]["rss_bytes"], 4096.0);

    assert_eq!(events[1].module, "resource");
    assert_eq!(events[1].name, "resource_stats");
    assert_eq!(events[1].details["meta"]["stats_kind"], "host");
    assert_eq!(events[1].details["meta"]["phase"], "before");
    assert_eq!(events[1].details["meta"]["source"], "daemon.process");
    assert_eq!(events[1].details["meta"]["source_available"], true);
    assert!(events[1].details["meta"]["inflight_requests"].is_number());
    assert_eq!(events[1].details["host"]["process"]["rss_bytes"], 4096.0);

    assert_eq!(events[2].module, "resource");
    assert_eq!(events[2].name, "resource_stats");
    assert_eq!(events[2].details["meta"]["stats_kind"], "cgroup_process");
    assert_eq!(events[2].details["meta"]["phase"], "after");
    assert_eq!(events[2].details["meta"]["sampler_duration_us"], 19);
    assert_eq!(
        events[2].details["process"]["gauges"]["max_rss_bytes"],
        8192.0
    );

    assert_eq!(events[3].module, "resource");
    assert_eq!(events[3].name, "resource_stats");
    assert_eq!(events[3].details["meta"]["stats_kind"], "host");
    assert_eq!(events[3].details["meta"]["phase"], "after");
    assert_eq!(
        events[3].details["host"]["process"]["max_rss_bytes"],
        8192.0
    );
}

#[test]
fn plugin_overlay_response_strips_timings_but_keeps_trace_samples() -> TestResult {
    let (layer_stack_root, _workspace_root) = test_bound_workspace("plugin-overlay-timings")?;
    let overlay = PluginOverlayOutcome {
        layer_stack_root: layer_stack_root.clone(),
        runner: RunResult {
            exit_code: 0,
            payload: json!({
                "timings": {
                    "workspace.mount_s": 0.15,
                    "workspace.unmount_s": 0.05,
                    "workspace.layer_count": 3,
                    "workspace.fsconfig_calls": 6,
                },
            }),
        },
        changeset: ChangesetResult {
            files: vec![FileResult {
                path: LayerPath::parse("src/main.rs").expect("valid test layer path"),
                status: CommitStatus::Committed,
                message: String::new(),
                observed_version: None,
                observed_state: None,
            }],
            published_manifest_version: Some(42),
            timings: BTreeMap::from([("occ.commit.total_s".to_owned(), 0.03)]),
            events: Vec::new(),
        },
        plugin_result: Some(json!({"success": true})),
        layer_count: 3,
        path_kinds: vec![("src/main.rs".to_owned(), ChangedPathKind::Write)],
        lease_acquire_s: 0.1,
        capture_s: 0.2,
        occ_s: 0.3,
        lease_release_error: None,
        upperdir_stats: TreeResourceStats {
            files: 1,
            dirs: 2,
            symlinks: 0,
            bytes: 4096,
            truncated: false,
            read_error_count: 0,
            first_error_path: None,
        },
    };

    let wire = super::plugin_overlay_response(&overlay, Instant::now())?;

    assert_eq!(wire.response["success"], true);
    assert_eq!(wire.response["status"], "committed");
    assert!(
        wire.response.get("timings").is_none(),
        "{:?}",
        wire.response
    );
    assert_eq!(
        wire.timings
            .get("workspace.mount_s")
            .and_then(serde_json::Value::as_f64),
        Some(0.15)
    );
    assert!(
        wire.timings.get("api.plugin_overlay.total_s").is_some(),
        "{:?}",
        wire.timings
    );
    assert_eq!(
        wire.timings
            .get("resource.command_exec.upperdir_tree_bytes")
            .and_then(serde_json::Value::as_f64),
        Some(4096.0)
    );

    remove_test_tree(&layer_stack_root)?;
    Ok(())
}

#[test]
fn registered_plugin_op_routes_to_deferred_dispatch_not_unknown_op() -> TestResult {
    let daemon = TestDaemon::new();
    let ensure = daemon.dispatch(&Request {
        op: "sandbox.plugin.ensure".to_owned(),
        invocation_id: "plugin-ensure-test".to_owned(),
        args: json!({
            "manifest": generic_self_managed_manifest("digest-a", "apply"),
            "layer_stack_root": "/eos/plugin/layer-stack",
            "workspace_root": "/eos/plugin/workspace"
        }),
    });
    let ensure = ok_result(&ensure);
    assert_eq!(ensure["success"], true);
    assert_eq!(
        ensure["operation_routes"][0]["dispatch_mode"],
        "self_managed_callback"
    );

    let routed = daemon.dispatch(&Request {
        op: "plugin.generic.apply".to_owned(),
        invocation_id: "plugin-apply-test".to_owned(),
        args: json!({"caller_id": "caller-plugin"}),
    });
    let routed = ok_result(&routed);
    assert_eq!(routed["success"], false);
    assert_eq!(routed["status"], "deferred");
    assert_eq!(routed["error"]["kind"], "plugin_dispatch_deferred");
    assert_eq!(routed["dispatch_mode"], "self_managed_callback");

    let missing = daemon.dispatch(&Request {
        op: "plugin.generic.missing".to_owned(),
        invocation_id: "plugin-missing-test".to_owned(),
        args: json!({}),
    });
    let missing = error_fault(&missing, "error");
    assert_eq!(missing["kind"], "unknown_op");
    Ok(())
}

#[test]
fn dynamic_plugin_op_is_blocked_in_isolated_workspace_before_route_lookup() -> TestResult {
    let _env_guard = crate::op_adapter::isolation::lock_isolated_test_state();
    let (layer_stack_root, _workspace_root) = test_bound_workspace("plugin-iws-block")?;
    let scratch = some_value(
        layer_stack_root.parent(),
        "test layer root must have a parent",
    )?
    .join("scratch");
    let daemon = TestDaemon::with_isolated_workspace(&scratch, Path::new("/testbed"));
    let _harness = TestEnvVar::set("EOS_ISOLATED_WORKSPACE_TEST_HARNESS", "true");

    let _ = daemon.dispatch(&Request {
        op: "sandbox.isolation.test_reset".to_owned(),
        invocation_id: "iws-reset-before-plugin-block".to_owned(),
        args: json!({}),
    });
    let entered = daemon.dispatch(&Request {
        op: "sandbox.isolation.enter".to_owned(),
        invocation_id: "iws-enter-before-plugin-block".to_owned(),
        args: json!({
            "caller_id": "caller-plugin",
            "layer_stack_root": layer_stack_root.to_string_lossy(),
        }),
    });
    let entered = ok_result(&entered);
    assert_eq!(entered["success"], true);

    let blocked = daemon.dispatch(&Request {
        op: "plugin.generic.not_loaded_yet".to_owned(),
        invocation_id: "plugin-dynamic-iws-block".to_owned(),
        args: json!({"caller_id": "caller-plugin"}),
    });
    let blocked = error_fault(&blocked, "error");
    assert_eq!(blocked["kind"], "forbidden_in_isolated_workspace");

    let exited = daemon.dispatch(&Request {
        op: "sandbox.isolation.exit".to_owned(),
        invocation_id: "iws-exit-after-plugin-block".to_owned(),
        args: json!({"caller_id": "caller-plugin"}),
    });
    let exited = ok_result(&exited);
    assert_eq!(exited["success"], true);
    let _ = daemon.dispatch(&Request {
        op: "sandbox.isolation.test_reset".to_owned(),
        invocation_id: "iws-reset-after-plugin-block".to_owned(),
        args: json!({}),
    });
    remove_test_tree(&layer_stack_root)?;
    Ok(())
}

#[test]
fn ensure_records_oneshot_overlay_route_without_starting_process() -> TestResult {
    let daemon = TestDaemon::new();
    let response = daemon.op_ensure(&json!({
        "manifest": oneshot_overlay_manifest("digest-a", "write"),
        "layer_stack_root": "/eos/plugin/layer-stack",
        "workspace_root": "/eos/plugin/workspace",
        "start_services": true
    }))?;

    assert_eq!(response["success"], true);
    assert_eq!(response["service_processes"], json!([]));
    assert_eq!(response["service_processes_started"], false);
    assert_eq!(
        response["operation_routes"][0]["dispatch_mode"],
        "write_allowed_oneshot_overlay"
    );
    assert_eq!(
        response["operation_routes"][0]["service_mode"],
        "oneshot_overlay"
    );
    assert_eq!(
        response["operation_routes"][0]["service_command"],
        json!(["python3", "/eos/plugin/oneshot.py"])
    );
    assert_eq!(
        response["services"][0]["last_error"],
        "oneshot overlay worker starts per operation"
    );
    Ok(())
}

#[test]
fn digest_reload_replaces_dynamic_plugin_routes() -> TestResult {
    let daemon = TestDaemon::new();
    let first = daemon.dispatch(&Request {
        op: "sandbox.plugin.ensure".to_owned(),
        invocation_id: "plugin-ensure-a".to_owned(),
        args: json!({
            "manifest": generic_service_manifest("digest-a", "hover"),
            "layer_stack_root": "/eos/plugin/layer-stack",
            "workspace_root": "/eos/plugin/workspace"
        }),
    });
    let first = ok_result(&first);
    assert_eq!(first["registered_ops"], json!(["plugin.generic.hover"]));

    let second = daemon.dispatch(&Request {
        op: "sandbox.plugin.ensure".to_owned(),
        invocation_id: "plugin-ensure-b".to_owned(),
        args: json!({
            "manifest": generic_service_manifest("digest-b", "diagnostics"),
            "layer_stack_root": "/eos/plugin/layer-stack",
            "workspace_root": "/eos/plugin/workspace"
        }),
    });
    let second = ok_result(&second);
    assert_eq!(
        second["registered_ops"],
        json!(["plugin.generic.diagnostics"])
    );

    let old = daemon.dispatch(&Request {
        op: "plugin.generic.hover".to_owned(),
        invocation_id: "plugin-hover-old".to_owned(),
        args: json!({}),
    });
    let old = error_fault(&old, "error");
    assert_eq!(old["kind"], "unknown_op");

    let current = daemon.dispatch(&Request {
        op: "plugin.generic.diagnostics".to_owned(),
        invocation_id: "plugin-diagnostics-current".to_owned(),
        args: json!({}),
    });
    let current = ok_result(&current);
    assert_eq!(current["error"]["kind"], "plugin_dispatch_deferred");
    Ok(())
}

struct TestEnvVar {
    key: &'static str,
    previous: Option<String>,
}

impl TestEnvVar {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for TestEnvVar {
    fn drop(&mut self) {
        if let Some(previous) = &self.previous {
            std::env::set_var(self.key, previous);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

struct PackageTestRoots {
    root: PathBuf,
}

impl PackageTestRoots {
    fn new(name: &str) -> Result<Self, TestError> {
        let root =
            std::env::temp_dir().join(format!("eos-plugin-package-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    fn args(&self, manifest: serde_json::Value, staged: Option<&Path>) -> serde_json::Value {
        let mut args = json!({
            "manifest": manifest,
            "package_runtime_root": self.root.join("runtime/plugins/catalog").to_string_lossy(),
            "package_dependency_root": self.root.join("runtime/packages").to_string_lossy(),
            "package_upload_root": self.root.join("scratch/uploads/plugins").to_string_lossy(),
            "package_setup_root": self.root.join("scratch/setup").to_string_lossy(),
        });
        if let Some(staged) = staged {
            args["staged_package_root"] = json!(staged.to_string_lossy());
        }
        args
    }

    fn stage_package(&self, digest: &str, setup_script: &str) -> Result<PathBuf, TestError> {
        let package = self
            .root
            .join("scratch/uploads/plugins/generic")
            .join(digest)
            .join("upload-1/package");
        std::fs::create_dir_all(package.join("runtime"))?;
        std::fs::write(package.join("sandbox-plugin.json"), "{}\n")?;
        std::fs::write(package.join(".package-sha256"), digest)?;
        let setup = package.join("setup.sh");
        std::fs::write(&setup, setup_script)?;
        std::fs::set_permissions(&setup, std::fs::Permissions::from_mode(0o755))?;
        std::fs::write(package.join("runtime/server.sh"), "#!/bin/sh\n")?;
        Ok(package)
    }

    fn package_root(&self, digest: &str) -> PathBuf {
        self.root
            .join("runtime/plugins/catalog/generic")
            .join(digest)
    }

    fn dependency_root(&self, digest: &str) -> PathBuf {
        self.root.join("runtime/packages/generic").join(digest)
    }

    fn setup_root(&self, digest: &str) -> PathBuf {
        self.root.join("scratch/setup/generic").join(digest)
    }

    fn cleanup(&self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn package_manifest(digest: &str, setup_digest: &str, command: Vec<&str>) -> serde_json::Value {
    json!({
        "plugin_id": "generic",
        "plugin_version": "0.1.0",
        "plugin_digest": digest,
        "package": {
            "runtime_dir": "runtime",
            "dependency_scope": "package_digest"
        },
        "setup": {
            "command": command,
            "working_dir": ".",
            "setup_marker_digest": setup_digest,
            "timeout_ms": 30_000
        },
        "services": [],
        "operations": []
    })
}
