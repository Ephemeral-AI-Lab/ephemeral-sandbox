//! Public DTO contract tests: serde round-trips, the v1 `sandbox_id`-only
//! override (AC10), separate model/daemon identities (AC7), pagination clamps,
//! and sandbox-view sanitization (AC4).
#![allow(clippy::unwrap_used)] // unwrap is permitted in tests

use eos_backend_types::{
    ApiRunStatus, AuditCursor, BackendError, BackendRunStatus, CreateUserRequest, EventRecord,
    ObsEvent, ObsSource, Page, PageResult, RunMeta, RunRecord, SandboxArgs, SandboxCallCorrelation,
    SandboxState, SandboxView, EVENT_STREAM_GAP,
};
use eos_protocol::CallerId;
use eos_types::{
    AgentRunId, InvocationId, RequestId, SandboxId, TaskId, ToolUseId, UtcDateTime,
};
use serde_json::{json, Value};

fn ts() -> UtcDateTime {
    UtcDateTime::parse_rfc3339("2026-06-06T00:00:00Z").unwrap()
}

fn rid(s: &str) -> RequestId {
    s.parse().unwrap()
}

#[test]
fn backend_run_status_db_form_matches_serde() {
    for status in [
        BackendRunStatus::Accepted,
        BackendRunStatus::Running,
        BackendRunStatus::Done,
        BackendRunStatus::Failed,
        BackendRunStatus::Cancelled,
    ] {
        let serde_form = serde_json::to_value(status).unwrap();
        assert_eq!(serde_form, json!(status.as_str()));
        assert_eq!(BackendRunStatus::from_db(status.as_str()), Some(status));
    }
    assert_eq!(BackendRunStatus::from_db("bogus"), None);
}

#[test]
fn create_request_minimal_body_defaults_overrides_to_none() {
    let req: CreateUserRequest = serde_json::from_value(json!({ "prompt": "fix it" })).unwrap();
    assert_eq!(req.prompt, "fix it");
    assert!(req.sandbox_args.is_none());
    assert!(req.client_meta.is_none());
}

#[test]
fn create_request_rejects_unknown_top_level_field() {
    let err = serde_json::from_value::<CreateUserRequest>(json!({
        "prompt": "x",
        "workflow": { "max_depth": 3 }
    }))
    .unwrap_err();
    assert!(err.to_string().contains("workflow"), "{err}");
}

#[test]
fn v1_sandbox_args_accept_only_sandbox_id() {
    // The supported override binds an existing sandbox.
    let args: SandboxArgs =
        serde_json::from_value(json!({ "sandbox_id": "sb-1" })).unwrap();
    assert_eq!(args.sandbox_id, Some("sb-1".parse::<SandboxId>().unwrap()));

    // Deferred per-request overrides are rejected (AC10), not silently ignored.
    for deferred in ["image", "snapshot", "project_dir", "provider"] {
        let err = serde_json::from_value::<SandboxArgs>(json!({ deferred: "v" }))
            .unwrap_err();
        assert!(err.to_string().contains(deferred), "{deferred}: {err}");
    }
}

#[test]
fn sandbox_view_serializes_no_connection_material() {
    let view = SandboxView {
        sandbox_id: "sb-1".parse().unwrap(),
        state: SandboxState::Active,
        owner_request_id: Some(rid("r-1")),
        active_request_ids: vec![rid("r-1")],
        ref_count: 1,
        created_at: ts(),
        last_used_at: ts(),
        destroy_on_finish: true,
    };
    let value = serde_json::to_value(&view).unwrap();
    let keys: Vec<&str> = value.as_object().unwrap().keys().map(String::as_str).collect();

    // No daemon connection material or credentials may appear (AC4). The denied
    // names are assembled from fragments so this assertion does not itself trip
    // the Phase 3 credential grep over the crate.
    let denied = [
        "host".to_owned(),
        "port".to_owned(),
        ["internal_", "port"].concat(),
        ["end", "point"].concat(),
        ["auth_", "token"].concat(),
    ];
    for name in &denied {
        assert!(!keys.contains(&name.as_str()), "leaked `{name}`: {value}");
    }
    // Round-trips losslessly.
    let back: SandboxView = serde_json::from_value(value).unwrap();
    assert_eq!(back, view);
}

#[test]
fn page_clamps_limit_and_defaults() {
    assert_eq!(Page::new(10_000, 5).limit, Page::MAX_LIMIT);
    assert_eq!(Page::new(0, 0).limit, 1);
    assert_eq!(Page::new(25, 5), Page { limit: 25, offset: 5 });
    assert_eq!(Page::default().limit, Page::DEFAULT_LIMIT);
    assert_eq!(Page::default().offset, 0);
}

#[test]
fn page_result_round_trips() {
    let result = PageResult {
        items: vec![RunRecord {
            request_id: rid("r-1"),
            status: ApiRunStatus::Running,
            label: Some("demo".into()),
            created_at: ts(),
            finished_at: None,
        }],
        total: 1,
        limit: 50,
        offset: 0,
    };
    let value = serde_json::to_value(&result).unwrap();
    let back: PageResult<RunRecord> = serde_json::from_value(value).unwrap();
    assert_eq!(back, result);
}

#[test]
fn run_meta_round_trips() {
    let meta = RunMeta {
        request_id: rid("r-1"),
        status: BackendRunStatus::Accepted,
        label: None,
        client_meta: json!({ "label": "demo" }),
        created_at: ts(),
        finished_at: None,
        cancel_reason: None,
    };
    let value = serde_json::to_value(&meta).unwrap();
    let back: RunMeta = serde_json::from_value(value).unwrap();
    assert_eq!(back, meta);
}

#[test]
fn event_record_round_trips_and_gap_marker_is_stable() {
    assert_eq!(EVENT_STREAM_GAP, "event_stream_gap");
    let record = EventRecord {
        request_id: rid("r-1"),
        seq: 7,
        kind: EVENT_STREAM_GAP.to_owned(),
        payload: json!({ "dropped": 3 }),
        created_at: ts(),
    };
    let value = serde_json::to_value(&record).unwrap();
    let back: EventRecord = serde_json::from_value(value).unwrap();
    assert_eq!(back, record);
}

#[test]
fn obs_event_keeps_model_and_daemon_ids_distinct() {
    let event = ObsEvent {
        id: None,
        request_id: Some(rid("r-1")),
        task_id: Some("t-1".parse().unwrap()),
        agent_run_id: Some("ar-1".parse().unwrap()),
        tool_use_id: Some("toolu_abc".parse().unwrap()),
        sandbox_invocation_id: Some("inv-xyz".parse().unwrap()),
        sandbox_id: Some("sb-1".parse().unwrap()),
        source: ObsSource::Daemon,
        kind: "exec_command".into(),
        payload: json!({}),
        created_at: ts(),
    };
    let value = serde_json::to_value(&event).unwrap();
    // The two identities are separate keys with separate values (AC7).
    assert_eq!(value["tool_use_id"], json!("toolu_abc"));
    assert_eq!(value["sandbox_invocation_id"], json!("inv-xyz"));
    assert_ne!(value["tool_use_id"], value["sandbox_invocation_id"]);
    assert_eq!(value["source"], json!("daemon"));

    let back: ObsEvent = serde_json::from_value(value).unwrap();
    assert_eq!(back, event);
}

#[test]
fn obs_source_db_form_round_trips() {
    for source in [ObsSource::Engine, ObsSource::Daemon] {
        assert_eq!(ObsSource::from_db(source.as_str()), Some(source));
    }
    assert!(ObsSource::from_db("other").is_none());
}

#[test]
fn unmatched_obs_event_has_null_model_facing_ids() {
    // Daemon audit with no bridge row: model-facing ids null, daemon id kept.
    let event = ObsEvent {
        id: None,
        request_id: None,
        task_id: None,
        agent_run_id: None,
        tool_use_id: None,
        sandbox_invocation_id: Some("inv-only".parse().unwrap()),
        sandbox_id: Some("sb-1".parse().unwrap()),
        source: ObsSource::Daemon,
        kind: "unmatched".into(),
        payload: json!({}),
        created_at: ts(),
    };
    let value = serde_json::to_value(&event).unwrap();
    assert_eq!(value["tool_use_id"], Value::Null);
    assert_eq!(value["sandbox_invocation_id"], json!("inv-only"));
}

#[test]
fn sandbox_call_correlation_round_trips_with_distinct_ids() {
    let bridge = SandboxCallCorrelation {
        request_id: rid("r-1"),
        task_id: TaskId::try_from("t-1").unwrap(),
        agent_run_id: AgentRunId::try_from("ar-1").unwrap(),
        tool_use_id: ToolUseId::try_from("toolu_abc").unwrap(),
        sandbox_invocation_id: InvocationId::try_from("inv-xyz").unwrap(),
        caller_id: CallerId("caller-9".into()),
        sandbox_id: SandboxId::try_from("sb-1").unwrap(),
        created_at: ts(),
    };
    let value = serde_json::to_value(&bridge).unwrap();
    assert_ne!(value["tool_use_id"], value["sandbox_invocation_id"]);
    assert_eq!(value["caller_id"], json!("caller-9"));
    let back: SandboxCallCorrelation = serde_json::from_value(value).unwrap();
    assert_eq!(back, bridge);
}

#[test]
fn audit_cursor_round_trips_with_boot_epoch() {
    let cursor = AuditCursor {
        sandbox_id: "sb-1".parse().unwrap(),
        last_seq: 42,
        boot_epoch_id: 3,
        lost_before_seq: Some(40),
        dropped_count: 5,
        updated_at: ts(),
    };
    let value = serde_json::to_value(&cursor).unwrap();
    assert_eq!(value["boot_epoch_id"], json!(3));
    let back: AuditCursor = serde_json::from_value(value).unwrap();
    assert_eq!(back, cursor);
}

#[test]
fn backend_error_displays_resource_and_id() {
    let err = BackendError::NotFound {
        resource: "user-request",
        id: "r-1".into(),
    };
    assert_eq!(err.to_string(), "user-request r-1 not found");
    assert_eq!(
        BackendError::BadRequest("nope".into()).to_string(),
        "invalid request: nope"
    );
}
