//! Round-trip persistence for every `backend.db` table, driven through the
//! public `BackendStore` repositories.
#![allow(clippy::unwrap_used)] // unwrap is permitted in tests

use eos_backend_store::BackendStore;
use eos_backend_types::{
    AuditCursor, BackendRunStatus, EventRecord, ObsEvent, ObsSource, Page, RunMeta,
    SandboxCallCorrelation,
};
use eos_protocol::CallerId;
use eos_types::{
    AgentRunId, InvocationId, RequestId, SandboxId, TaskId, ToolUseId, UtcDateTime,
};
use serde_json::json;
use tempfile::TempDir;

async fn open_store() -> (BackendStore, TempDir) {
    let dir = TempDir::new().unwrap();
    let store = BackendStore::open(dir.path().join("backend.db"))
        .await
        .unwrap();
    (store, dir)
}

fn ts(s: &str) -> UtcDateTime {
    UtcDateTime::parse_rfc3339(s).unwrap()
}

fn req(s: &str) -> RequestId {
    s.parse().unwrap()
}

#[tokio::test]
async fn run_meta_round_trips_and_lists() {
    let (store, _dir) = open_store().await;
    let repo = store.run_meta();
    let meta = RunMeta {
        request_id: req("r-1"),
        status: BackendRunStatus::Accepted,
        label: Some("demo".into()),
        client_meta: json!({ "label": "demo" }),
        created_at: ts("2026-06-06T00:00:00Z"),
        finished_at: None,
        cancel_reason: None,
    };
    repo.insert(&meta).await.unwrap();
    assert_eq!(repo.get(&req("r-1")).await.unwrap(), Some(meta.clone()));

    // Terminal transition is persisted and returned.
    let updated = repo
        .set_status(
            &req("r-1"),
            BackendRunStatus::Done,
            Some(ts("2026-06-06T00:05:00Z")),
            None,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(updated.status, BackendRunStatus::Done);
    assert_eq!(updated.finished_at, Some(ts("2026-06-06T00:05:00Z")));

    // Cancellation reason persists.
    repo.set_status(
        &req("r-1"),
        BackendRunStatus::Cancelled,
        Some(ts("2026-06-06T00:06:00Z")),
        Some("user aborted"),
    )
    .await
    .unwrap();
    assert_eq!(
        repo.get(&req("r-1")).await.unwrap().unwrap().cancel_reason,
        Some("user aborted".into())
    );

    let page = repo.list(Page::default()).await.unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].request_id, req("r-1"));

    // Updating an absent run yields None, not an error.
    assert!(repo
        .set_status(&req("ghost"), BackendRunStatus::Failed, None, None)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn event_log_appends_and_replays_by_seq() {
    let (store, _dir) = open_store().await;
    let repo = store.event_log();
    for seq in 1..=3 {
        repo.append(&EventRecord {
            request_id: req("r-1"),
            seq,
            kind: "run_started".into(),
            payload: json!({ "seq": seq }),
            created_at: ts("2026-06-06T00:00:00Z"),
        })
        .await
        .unwrap();
    }
    // A different request's events are isolated.
    repo.append(&EventRecord {
        request_id: req("r-2"),
        seq: 1,
        kind: "run_started".into(),
        payload: json!({}),
        created_at: ts("2026-06-06T00:00:00Z"),
    })
    .await
    .unwrap();

    let all = repo.list_since(&req("r-1"), 0).await.unwrap();
    assert_eq!(all.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![1, 2, 3]);
    assert_eq!(all[0].payload, json!({ "seq": 1 }));

    let after = repo.list_since(&req("r-1"), 1).await.unwrap();
    assert_eq!(after.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![2, 3]);

    assert_eq!(repo.max_seq(&req("r-1")).await.unwrap(), Some(3));
    assert_eq!(repo.max_seq(&req("absent")).await.unwrap(), None);

    // Duplicate (request_id, seq) is rejected by the primary key.
    assert!(repo
        .append(&EventRecord {
            request_id: req("r-1"),
            seq: 1,
            kind: "dup".into(),
            payload: json!({}),
            created_at: ts("2026-06-06T00:00:00Z"),
        })
        .await
        .is_err());
}

#[tokio::test]
async fn obs_event_round_trips_with_distinct_and_null_ids() {
    let (store, _dir) = open_store().await;
    let repo = store.obs_events();

    // A request-scoped event with some null model-facing ids and distinct
    // model/daemon identities (AC7).
    let matched = ObsEvent {
        id: None,
        request_id: Some(req("r-1")),
        task_id: None,
        agent_run_id: None,
        tool_use_id: Some("toolu_abc".parse().unwrap()),
        sandbox_invocation_id: Some("inv-xyz".parse().unwrap()),
        sandbox_id: Some("sb-1".parse().unwrap()),
        source: ObsSource::Engine,
        kind: "exec_command".into(),
        payload: json!({ "ok": true }),
        created_at: ts("2026-06-06T00:00:00Z"),
    };
    let id = repo.insert(&matched).await.unwrap();
    assert!(id > 0);

    let listed = repo.list_for_request(&req("r-1")).await.unwrap();
    assert_eq!(listed.len(), 1);
    let got = &listed[0];
    assert_eq!(got.id, Some(id));
    assert_eq!(got.tool_use_id, matched.tool_use_id);
    assert_eq!(got.sandbox_invocation_id, matched.sandbox_invocation_id);
    assert_ne!(
        got.tool_use_id.as_ref().map(ToolUseId::as_str),
        got.sandbox_invocation_id.as_ref().map(InvocationId::as_str)
    );
    assert!(got.task_id.is_none() && got.agent_run_id.is_none());

    // A fully-unmatched daemon row persists with a null request_id (AC7).
    let unmatched = ObsEvent {
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
        created_at: ts("2026-06-06T00:01:00Z"),
    };
    assert!(repo.insert(&unmatched).await.unwrap() > 0);
    // The unmatched row is not attributed to r-1.
    assert_eq!(repo.list_for_request(&req("r-1")).await.unwrap().len(), 1);
}

#[tokio::test]
async fn sandbox_call_correlation_round_trips() {
    let (store, _dir) = open_store().await;
    let repo = store.correlations();
    let bridge = SandboxCallCorrelation {
        request_id: req("r-1"),
        task_id: TaskId::try_from("t-1").unwrap(),
        agent_run_id: AgentRunId::try_from("ar-1").unwrap(),
        tool_use_id: ToolUseId::try_from("toolu_abc").unwrap(),
        sandbox_invocation_id: InvocationId::try_from("inv-xyz").unwrap(),
        caller_id: CallerId("caller-9".into()),
        sandbox_id: SandboxId::try_from("sb-1").unwrap(),
        created_at: ts("2026-06-06T00:00:00Z"),
    };
    repo.insert(&bridge).await.unwrap();

    let got = repo
        .get(
            &SandboxId::try_from("sb-1").unwrap(),
            &CallerId("caller-9".into()),
            &InvocationId::try_from("inv-xyz").unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(got, bridge);
    assert_ne!(got.tool_use_id.as_str(), got.sandbox_invocation_id.as_str());

    // A non-matching join key misses.
    assert!(repo
        .get(
            &SandboxId::try_from("sb-1").unwrap(),
            &CallerId("caller-9".into()),
            &InvocationId::try_from("other").unwrap(),
        )
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn audit_cursor_upserts_and_tracks_boot_epoch() {
    let (store, _dir) = open_store().await;
    let repo = store.audit_cursors();
    let sandbox = SandboxId::try_from("sb-1").unwrap();

    let initial = AuditCursor {
        sandbox_id: sandbox.clone(),
        last_seq: 10,
        boot_epoch_id: 1,
        lost_before_seq: None,
        dropped_count: 0,
        updated_at: ts("2026-06-06T00:00:00Z"),
    };
    repo.upsert(&initial).await.unwrap();
    assert_eq!(repo.get(&sandbox).await.unwrap(), Some(initial));

    // A daemon reboot bumps boot_epoch_id, resets last_seq, and records loss.
    let after_reboot = AuditCursor {
        sandbox_id: sandbox.clone(),
        last_seq: 0,
        boot_epoch_id: 2,
        lost_before_seq: Some(10),
        dropped_count: 3,
        updated_at: ts("2026-06-06T01:00:00Z"),
    };
    repo.upsert(&after_reboot).await.unwrap();
    let got = repo.get(&sandbox).await.unwrap().unwrap();
    assert_eq!(got.boot_epoch_id, 2);
    assert_eq!(got.last_seq, 0);
    assert_eq!(got.lost_before_seq, Some(10));
    assert_eq!(got.dropped_count, 3);
}

#[tokio::test]
async fn data_persists_across_reopen() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("backend.db");
    {
        let store = BackendStore::open(&path).await.unwrap();
        store
            .run_meta()
            .insert(&RunMeta {
                request_id: req("r-1"),
                status: BackendRunStatus::Running,
                label: None,
                client_meta: json!({}),
                created_at: ts("2026-06-06T00:00:00Z"),
                finished_at: None,
                cancel_reason: None,
            })
            .await
            .unwrap();
    }
    // Reopening the same file re-runs migrations idempotently and sees the row.
    let store = BackendStore::open(&path).await.unwrap();
    assert!(store.run_meta().get(&req("r-1")).await.unwrap().is_some());
}
