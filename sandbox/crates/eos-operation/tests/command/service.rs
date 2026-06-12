use eos_command::process::{
    CommandFinalResponsePersistence, CommandPersistenceOutcome, KillReason,
};
use eos_trace::{SpanKind, SpanStatus, TraceKind, TraceLinkKind};
use std::path::PathBuf;

use super::*;

#[test]
fn write_stdin_with_trace_reports_bytes_wait_and_status() {
    let ops = command_ops_with_inactive_isolated_run("cmd_stdin_trace", "caller");

    let outcome = ops
        .write_stdin_with_trace(WriteStdin {
            command_id: "cmd_stdin_trace".to_owned(),
            chars: "abc".to_owned(),
            yield_time_ms: 0,
        })
        .expect("stdin write reaches inactive process scaffold");

    let trace = outcome.trace.expect("stdin trace facts");
    assert_eq!(trace.command_id, "cmd_stdin_trace");
    assert_eq!(trace.bytes, 3);
    assert!(!trace.waited_for_output);
    assert_eq!(trace.status, outcome.response.status);
}

#[test]
fn write_stdin_teardown_control_does_not_emit_stdin_written_fact() {
    let ops = command_ops_with_inactive_isolated_run("cmd_stdin_control", "caller");

    let outcome = ops
        .write_stdin_with_trace(WriteStdin {
            command_id: "cmd_stdin_control".to_owned(),
            chars: "\u{3}".to_owned(),
            yield_time_ms: 0,
        })
        .expect("teardown control routes through cancel");

    assert!(outcome.trace.is_none());
}

#[test]
fn read_progress_with_trace_reports_completed_buffer_source() {
    let ops = CommandOps::new(eos_command::CommandConfig::default());
    ops.push_completed(CommandCompletion {
        command_id: "cmd_progress_trace".to_owned(),
        caller_id: "caller".to_owned(),
        command: "echo ok".to_owned(),
        result: CommandResponse {
            status: CommandStatus::Ok,
            exit_code: Some(0),
            stdout: "first\nsecond\n".to_owned(),
            stderr: String::new(),
            command_id: Some(crate::CommandId::new("cmd_progress_trace".to_owned())),
            finalized: None,
        },
    });

    let outcome = ops
        .read_command_progress_with_trace(ReadCommandProgress {
            command_id: "cmd_progress_trace".to_owned(),
            last_n_lines: 1,
        })
        .expect("read completed progress");

    assert_eq!(outcome.response.stdout, "second\n");
    assert_eq!(outcome.trace.command_id, "cmd_progress_trace");
    assert_eq!(outcome.trace.last_n_lines, 1);
    assert_eq!(outcome.trace.status, CommandStatus::Ok);
    assert_eq!(outcome.trace.source, "completed_buffer");
    assert_eq!(outcome.trace.stdout_bytes, "second\n".len());
}

#[test]
fn command_finalize_trace_record_carries_origin_and_eviction_markers() {
    let facts = CommandFinalizeTraceFacts {
        trace_origin: CommandTraceOrigin {
            trace_id: Some("trace-command-finalize".to_owned()),
            request_id: Some("request-command-finalize".to_owned()),
        },
        command_id: "cmd_finalized".to_owned(),
        caller_id: "caller".to_owned(),
        status: CommandStatus::TimedOut,
        exit_code: Some(124),
        signal: Some(15),
        kill: Some(KillReason::TimedOut),
        command_elapsed_s: 12.5,
        persistence: CommandPersistenceOutcome {
            final_response: Some(CommandFinalResponsePersistence::Persisted {
                path: PathBuf::from("/tmp/final.json"),
                bytes: 42,
            }),
            transcript_error: Some(eos_command::process::CommandTranscriptPersistenceError {
                path: PathBuf::from("/tmp/transcript.log"),
                error: "permission denied".to_owned(),
            }),
        },
        publish_completion: true,
        evictions: vec![CompletionBufferEviction {
            command_id: "cmd_evicted".to_owned(),
            seq: 7,
            max_entries: 1024,
        }],
    };

    let record = command_finalize_trace_record(&facts);

    assert_eq!(record.kind, TraceKind::CommandFinalize);
    assert_eq!(record.trace_id.as_str(), "trace-command-finalize");
    assert_eq!(
        record.request_id.as_ref().map(eos_trace::RequestId::as_str),
        Some("request-command-finalize")
    );
    assert_eq!(
        record
            .links
            .first()
            .map(|link| (&link.kind, link.value.as_str())),
        Some((&TraceLinkKind::Command, "cmd_finalized"))
    );
    let span = record.spans.first().expect("root finalize span");
    assert_eq!(span.name, "command.finalize");
    assert_eq!(span.status, Some(SpanStatus::TimedOut));
    let wait_span = record
        .spans
        .iter()
        .find(|span| span.kind == SpanKind::CommandProcessWait)
        .expect("command process wait span");
    assert_eq!(wait_span.name, "command.process.wait");
    assert_eq!(wait_span.duration_us, 12_500_000);
    assert_eq!(wait_span.status, Some(SpanStatus::TimedOut));

    let finalized = record
        .events
        .iter()
        .find(|event| event.module == "command" && event.name == "finalized")
        .expect("finalized event");
    assert_eq!(finalized.details.value["command_id"], "cmd_finalized");
    assert_eq!(finalized.details.value["signal"], 15);
    assert_eq!(finalized.details.value["kill_reason"], "timed_out");
    assert_eq!(finalized.details.value["publish_completion"], true);

    let exit_taken = record
        .events
        .iter()
        .find(|event| event.module == "command" && event.name == "exit_taken")
        .expect("exit-taken event");
    assert_eq!(exit_taken.details.value["command_id"], "cmd_finalized");
    assert_eq!(exit_taken.details.value["exit_code"], 124);
    assert_eq!(exit_taken.details.value["signal"], 15);
    assert_eq!(exit_taken.details.value["kill_reason"], "timed_out");

    let timed_out = record
        .events
        .iter()
        .find(|event| event.module == "command" && event.name == "timed_out")
        .expect("timed-out event");
    assert_eq!(timed_out.details.value["command_id"], "cmd_finalized");
    assert_eq!(timed_out.details.value["exit_code"], 124);
    assert_eq!(timed_out.details.value["signal"], 15);

    let evicted = record
        .events
        .iter()
        .find(|event| event.module == "command" && event.name == "completion_buffer_evicted")
        .expect("eviction event");
    assert_eq!(evicted.details.value["command_id"], "cmd_evicted");
    assert_eq!(evicted.details.value["seq"], 7);
    assert_eq!(evicted.details.value["max_entries"], 1024);

    let persisted = record
        .events
        .iter()
        .find(|event| event.module == "command" && event.name == "final_persisted")
        .expect("final persist success event");
    assert_eq!(persisted.details.value["path"], "/tmp/final.json");
    assert_eq!(persisted.details.value["bytes"], 42);

    let transcript_failed = record
        .events
        .iter()
        .find(|event| event.module == "command" && event.name == "transcript_failed")
        .expect("transcript failure event");
    assert_eq!(
        transcript_failed.details.value["path"],
        "/tmp/transcript.log"
    );
    assert_eq!(
        transcript_failed.details.value["error"],
        "permission denied"
    );
}

#[test]
fn command_finalize_trace_record_carries_final_persist_failures() {
    let facts = CommandFinalizeTraceFacts {
        trace_origin: CommandTraceOrigin::default(),
        command_id: "cmd_final_failed".to_owned(),
        caller_id: "caller".to_owned(),
        status: CommandStatus::Error,
        exit_code: Some(1),
        signal: None,
        kill: None,
        command_elapsed_s: 0.1,
        persistence: CommandPersistenceOutcome {
            final_response: Some(CommandFinalResponsePersistence::Failed {
                path: PathBuf::from("/tmp/final.json"),
                error: "disk full".to_owned(),
            }),
            transcript_error: None,
        },
        publish_completion: false,
        evictions: Vec::new(),
    };

    let record = command_finalize_trace_record(&facts);
    let failed = record
        .events
        .iter()
        .find(|event| event.module == "command" && event.name == "final_persist_failed")
        .expect("final persist failure event");
    assert_eq!(failed.details.value["path"], "/tmp/final.json");
    assert_eq!(failed.details.value["error"], "disk full");
}

#[test]
fn active_command_advance_trace_record_carries_poll_results() {
    let record = active_command_advance_trace_record(
        3,
        vec!["cmd_timed_out".to_owned()],
        vec!["cmd_finalized".to_owned()],
    );

    assert_eq!(record.kind, TraceKind::ActiveCommandAdvance);
    let span = record.spans.first().expect("advance span");
    assert_eq!(span.name, "command.active.advance");
    assert_eq!(span.kind, SpanKind::CommandProcessWait);
    assert_eq!(span.fields.value["live_count"], 3);
    let event = record.events.first().expect("advance event");
    assert_eq!(event.name, "advance_finished");
    assert_eq!(
        event.details.value["timed_out_commands"],
        json!(["cmd_timed_out"])
    );
    assert_eq!(
        event.details.value["finalized_commands"],
        json!(["cmd_finalized"])
    );
}

#[test]
fn command_process_wait_resource_stats_event_groups_gauges() {
    let mut timings = WorkspaceTimings::new();
    timings.insert("resource.cgroup.cpu_usage_usec".to_owned(), json!(10.0));
    timings.insert(
        "resource.cgroup.memory_current_bytes".to_owned(),
        json!(2048.0),
    );
    timings.insert("resource.cgroup.io_rbytes".to_owned(), json!(32.0));
    timings.insert("resource.cgroup.psi_cpu_some_avg10".to_owned(), json!(0.25));
    timings.insert("resource.process.rss_bytes".to_owned(), json!(4096.0));
    timings.insert(
        "resource.sampler.cgroup_process_duration_us".to_owned(),
        json!(17),
    );

    let event = command_process_wait_resource_stats_event("before", &timings);

    assert_eq!(event.name, "resource_stats");
    assert_eq!(event.details["meta"]["stats_kind"], "cgroup_process");
    assert_eq!(event.details["meta"]["phase"], "before");
    assert_eq!(event.details["meta"]["source"], "command.process.wait");
    assert_eq!(event.details["meta"]["source_available"], true);
    assert_eq!(event.details["meta"]["sampler_duration_us"], 17);
    assert_eq!(event.details["cgroup"]["source_available"], true);
    assert_eq!(event.details["cgroup"]["cpu"]["usage_usec"], 10.0);
    assert_eq!(event.details["cgroup"]["memory"]["current_bytes"], 2048.0);
    assert_eq!(event.details["cgroup"]["io"]["rbytes"], 32.0);
    assert_eq!(event.details["cgroup"]["psi"]["cpu_some_avg10"], 0.25);
    assert_eq!(event.details["process"]["source_available"], true);
    assert_eq!(event.details["process"]["gauges"]["rss_bytes"], 4096.0);
}

#[test]
fn command_process_wait_tree_resource_stats_events_group_tree_timings() {
    let mut timings = WorkspaceTimings::new();
    timings.insert(
        "resource.command_exec.upperdir_tree_bytes".to_owned(),
        json!(4096.0),
    );
    timings.insert(
        "resource.command_exec.upperdir_tree_file_count".to_owned(),
        json!(2.0),
    );
    timings.insert(
        "resource.command_exec.upperdir_tree_truncated".to_owned(),
        json!(1.0),
    );
    timings.insert(
        "resource.command_exec.run_dir_tree_bytes".to_owned(),
        json!(128.0),
    );

    let events = command_process_wait_tree_resource_stats_events(&timings);

    assert_eq!(events.len(), 2);
    let upperdir = events
        .iter()
        .find(|event| event.details["meta"]["source"] == "resource.command_exec.upperdir")
        .expect("upperdir tree resource");
    assert_eq!(upperdir.name, "resource_stats");
    assert_eq!(upperdir.details["meta"]["stats_kind"], "tree");
    assert_eq!(upperdir.details["meta"]["phase"], "after");
    assert_eq!(upperdir.details["meta"]["source_available"], true);
    assert_eq!(upperdir.details["tree"]["bytes"], 4096.0);
    assert_eq!(upperdir.details["tree"]["file_count"], 2.0);
    assert_eq!(upperdir.details["tree"]["truncated"], 1.0);
}

#[test]
fn command_process_wait_host_resource_stats_event_groups_process_gauges() {
    let mut timings = WorkspaceTimings::new();
    timings.insert("resource.process.rss_bytes".to_owned(), json!(4096.0));
    timings.insert("resource.process.max_rss_bytes".to_owned(), json!(8192.0));

    let event = command_process_wait_host_resource_stats_event("after", &timings);

    assert_eq!(event.name, "resource_stats");
    assert_eq!(event.details["meta"]["stats_kind"], "host");
    assert_eq!(event.details["meta"]["phase"], "after");
    assert_eq!(event.details["meta"]["source"], "daemon.process");
    assert_eq!(event.details["meta"]["source_available"], true);
    assert_eq!(event.details["host"]["process"]["rss_bytes"], 4096.0);
    assert_eq!(event.details["host"]["process"]["max_rss_bytes"], 8192.0);

    let unavailable =
        command_process_wait_host_resource_stats_event("before", &WorkspaceTimings::new());
    assert_eq!(unavailable.details["meta"]["source_available"], false);
    assert_eq!(
        unavailable.details["meta"]["read_error"],
        "daemon process gauges unavailable on this platform"
    );
}

#[test]
fn spawn_process_returns_runner_request_artifact_failure_event() {
    let ops = CommandOps::new(eos_command::CommandConfig::default());
    let root = std::env::temp_dir().join(format!(
        "eos-operation-command-spawn-failure-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let request_path = root.join("missing-parent").join("runner-request.json");
    let prepared = PreparedCommand {
        run_request: json!({"mode": "test"}),
        request_path: request_path.clone(),
        output_path: root.join("runner-result.json"),
        final_path: root.join("final.json"),
        transcript_path: root.join("transcript.log"),
        trace_events: Vec::new(),
    };
    let mut trace_events = Vec::new();

    let error = match ops.spawn_process(
        CommandProcessSpec {
            id: "cmd_artifact_failed".to_owned(),
            caller_id: "caller".to_owned(),
            command: "echo ok".to_owned(),
            timeout_seconds: None,
        },
        prepared,
        &mut trace_events,
    ) {
        Ok(_) => panic!("spawn should fail before opening a PTY"),
        Err(error) => error,
    };

    assert!(matches!(
        error.error(),
        eos_command::CommandError::ArtifactWrite {
            artifact: "runner_request",
            ..
        }
    ));
    assert_eq!(trace_events.len(), 1);
    assert_eq!(trace_events[0].name, "artifact_failed");
    assert_eq!(trace_events[0].details["artifact"], "runner_request");
    assert_eq!(
        trace_events[0].details["path"],
        request_path.display().to_string()
    );
    assert_eq!(error.trace_events(), trace_events.as_slice());

    let _ = std::fs::remove_dir_all(root);
}

fn command_ops_with_inactive_isolated_run(id: &str, caller_id: &str) -> CommandOps {
    let ops = CommandOps::new(eos_command::CommandConfig::default());
    let root = std::env::temp_dir().join(format!(
        "eos-operation-command-service-{}-{id}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let layer_stack_root = root.join("layers");
    let workspace_root = root.join("workspace");
    let scratch_dir = root.join("scratch");
    let upperdir = root.join("upper");
    let workdir = root.join("work");
    for path in [
        &layer_stack_root,
        &workspace_root,
        &scratch_dir,
        &upperdir,
        &workdir,
    ] {
        std::fs::create_dir_all(path).expect("create command test scaffold");
    }
    let process = CommandProcess::new(CommandProcessSpec {
        id: id.to_owned(),
        caller_id: caller_id.to_owned(),
        command: "cat".to_owned(),
        timeout_seconds: None,
    });
    ops.registry
        .insert(Arc::new(ActiveCommand::Isolated(IsolatedRun {
            process,
            trace_origin: CommandTraceOrigin::default(),
            binding: IsolatedWorkspaceBinding {
                caller_id: caller_id.to_owned(),
                workspace_handle_id: "workspace-handle".to_owned(),
                layer_stack_root,
                manifest_version: 1,
                manifest_root_hash: "root".to_owned(),
                workspace_root,
                scratch_dir,
                upperdir,
                workdir,
                layer_paths: Vec::new(),
                ns_fds: std::collections::HashMap::new(),
                cgroup_path: None,
            },
        })));
    ops
}
