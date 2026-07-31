//! Observer: disabled is a no-op; `SpanGuard` records one span on drop carrying
//! attrs/status; `scope` self-sets `Error`; `event` drops without context; two
//! cloned observers share the `SpanIds` and thread-local context; nested drop
//! restores the previous parent; `with_context(None)`/unwinding restore.

#[cfg(unix)]
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(unix)]
use std::sync::mpsc;
use std::sync::Arc;
#[cfg(unix)]
use std::time::Duration;

#[cfg(unix)]
use rustix::fs::{flock, FlockOperation};
use sandbox_observability_telemetry::record::proc;
use sandbox_observability_telemetry::{
    Observer, ObserverConfig, RawFilter, Reader, Sink, Span, SpanStatus, TraceContext,
    MAX_LINE_BYTES,
};
use serde_json::{json, Value};

static NEXT: AtomicU64 = AtomicU64::new(0);

fn temp_log(label: &str) -> PathBuf {
    std::env::temp_dir()
        .join(format!(
            "sandbox-obs-observer-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
        .join("observability.ndjson")
}

fn observer(path: &Path, proc_token: &'static str, enabled: bool) -> Observer {
    Observer::new(
        ObserverConfig {
            proc: proc_token,
            enabled,
        },
        Sink::new(path.to_path_buf(), MAX_LINE_BYTES),
    )
}

fn ctx(trace: &str, parent: Option<&str>) -> TraceContext {
    TraceContext {
        trace: Arc::from(trace),
        parent: parent.map(Arc::from),
    }
}

fn spans(path: &Path) -> Vec<Span> {
    let reader = Reader::new(path.to_path_buf(), path.with_extension("ndjson.absent"));
    reader
        .raw(RawFilter::default())
        .iter()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|value| value["kind"] == "span")
        .map(|value| serde_json::from_value::<Span>(value).expect("span"))
        .collect()
}

#[test]
fn disabled_observer_writes_nothing() {
    let path = temp_log("disabled");
    let obs = observer(&path, proc::DAEMON, false);
    obs.with_context(ctx("req", None), || {
        let span = obs.span("command.exec");
        span.attr("op", "x");
        obs.event("lease.acquired", json!({}));
        obs.sample("sandbox", json!({ "cpu_usec": 1 }));
    });
    assert!(!path.exists(), "disabled emits no file");
}

#[test]
fn storage_failure_never_fails_business_operation() {
    let path = temp_log("storage-failure");
    let obs = observer(&path, proc::DAEMON, true);
    let parent = path.parent().expect("parent");
    std::fs::remove_dir_all(parent).expect("remove pre-created directory");
    std::fs::write(parent, b"not a directory").expect("replace directory with file");

    obs.sample("sandbox", json!({ "attempt": 1 }));
    obs.sample("sandbox", json!({ "attempt": 2 }));

    assert_eq!(
        obs.sink_stats().dropped_storage,
        2,
        "ordinary observer calls swallow storage errors and count once per attempt"
    );
    std::fs::remove_file(parent).expect("cleanup blocker");
}

#[cfg(unix)]
#[test]
fn event_does_not_wait_for_a_held_cross_process_lock() {
    let path = temp_log("held-lock");
    let lock_path = PathBuf::from(format!("{}.lock", path.display()));
    std::fs::create_dir_all(path.parent().expect("parent")).expect("create event directory");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .expect("open cross-process lock");
    flock(&lock, FlockOperation::LockExclusive).expect("hold cross-process lock");

    let obs = observer(&path, proc::DAEMON, true);
    let worker_obs = obs.clone();
    let (sent, received) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        worker_obs.with_context(ctx("req", None), || {
            worker_obs.event("lease.acquired", json!({}));
        });
        sent.send(()).expect("return from event");
    });

    let timely = received.recv_timeout(Duration::from_millis(100));
    flock(&lock, FlockOperation::Unlock).expect("release cross-process lock");
    match timely {
        Ok(()) => {}
        Err(mpsc::RecvTimeoutError::Timeout) => {
            worker.join().expect("blocked event completes after unlock");
            panic!("observer event waited for the cross-process lock");
        }
        Err(error) => panic!("receive event completion: {error}"),
    }
    worker.join().expect("event worker completes");
    assert_eq!(obs.sink_stats().dropped_storage, 1);
    let _ = std::fs::remove_dir_all(path.parent().expect("parent"));
}

#[test]
fn missing_store_directory_is_recreated_without_a_drop() {
    let path = temp_log("missing-directory");
    let obs = observer(&path, proc::DAEMON, true);
    std::fs::remove_dir_all(path.parent().expect("parent")).expect("remove parent");

    obs.sample("sandbox", json!({ "value": 1 }));

    assert!(path.exists());
    assert_eq!(obs.sink_stats().dropped_storage, 0);
    std::fs::remove_dir_all(path.parent().expect("parent")).expect("cleanup");
}

#[test]
fn span_records_on_drop_with_attrs_and_status() {
    let path = temp_log("span-drop");
    let obs = observer(&path, proc::DAEMON, true);
    obs.with_context(ctx("req-1", None), || {
        let span = obs.span("command.exec");
        span.attr("op", "exec").attr("session_created", true);
        span.status(SpanStatus::Error);
    });

    let spans = spans(&path);
    assert_eq!(spans.len(), 1);
    let span = &spans[0];
    assert_eq!(span.name, "command.exec");
    assert_eq!(span.trace, "req-1");
    assert_eq!(span.parent, None, "root span");
    assert_eq!(span.status, SpanStatus::Error);
    assert_eq!(span.attrs["op"], "exec");
    assert_eq!(span.attrs["session_created"], true);
    assert!(
        span.dur_ms >= 0.0,
        "start = ts - dur_ms reconstructs a non-negative start"
    );
}

#[test]
fn scope_self_sets_error_but_keeps_explicit_status() {
    let path = temp_log("scope");
    let obs = observer(&path, proc::DAEMON, true);
    obs.with_context(ctx("req", None), || {
        let failed: Result<(), &str> = obs.scope("command.exec", |span| {
            span.attr("op", "x");
            Err("boom")
        });
        assert!(failed.is_err());

        let ok: Result<i32, &str> = obs.scope("workspace_session.create", |_| Ok(7));
        assert_eq!(ok, Ok(7));

        let timed: Result<(), &str> = obs.scope("layerstack.publish", |span| {
            span.status(SpanStatus::TimedOut);
            Err("late")
        });
        assert!(timed.is_err());
    });

    let spans = spans(&path);
    let exec = spans
        .iter()
        .find(|s| s.name == "command.exec")
        .expect("exec span");
    assert_eq!(exec.status, SpanStatus::Error, "Err body self-sets Error");
    assert_eq!(exec.attrs["op"], "x");
    let created = spans
        .iter()
        .find(|s| s.name == "workspace_session.create")
        .expect("create span");
    assert_eq!(
        created.status,
        SpanStatus::Completed,
        "Ok body stays Completed"
    );
    let published = spans
        .iter()
        .find(|s| s.name == "layerstack.publish")
        .expect("publish span");
    assert_eq!(
        published.status,
        SpanStatus::TimedOut,
        "explicit status is not clobbered to Error"
    );
}

#[test]
fn event_without_context_drops_but_writes_within_context() {
    let path = temp_log("event-ctx");
    let obs = observer(&path, proc::DAEMON, true);
    obs.event("lease.acquired", json!({ "layer_id": "l0" }));
    assert!(
        !path.exists(),
        "no enclosing context ⇒ event dropped, no orphan"
    );

    obs.with_context(ctx("req", Some("d-0")), || {
        obs.event("lease.acquired", json!({ "layer_id": "l0" }));
    });
    let reader = Reader::new(path.clone(), path.with_extension("ndjson.absent"));
    let events = reader.events(RawFilter::default());
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].parent.as_deref(), Some("d-0"));
}

#[test]
fn cloned_observers_share_ids_and_thread_local_context() {
    let path = temp_log("cloned");
    let daemon = observer(&path, proc::DAEMON, true);
    let runtime = daemon.clone();
    daemon.with_context(ctx("req", None), || {
        let _dispatch = daemon.span("daemon.dispatch");
        let _exec = runtime.span("command.exec");
    });

    let spans = spans(&path);
    let dispatch = spans
        .iter()
        .find(|s| s.name == "daemon.dispatch")
        .expect("dispatch");
    let exec = spans
        .iter()
        .find(|s| s.name == "command.exec")
        .expect("exec");
    assert_eq!(dispatch.span, "d-0", "shared SpanIds: monotonic from 0");
    assert_eq!(dispatch.parent, None);
    assert_eq!(exec.span, "d-1", "no duplicate d-0 across cloned observers");
    assert_eq!(
        exec.parent.as_deref(),
        Some("d-0"),
        "runtime span nests under the daemon-set parent"
    );
}

#[test]
fn sibling_spans_do_not_nest() {
    let path = temp_log("siblings");
    let obs = observer(&path, proc::DAEMON, true);
    obs.with_context(ctx("req", None), || {
        {
            let _a = obs.span("command.exec");
        }
        {
            let _b = obs.span("workspace_session.create");
        }
    });

    let spans = spans(&path);
    for span in &spans {
        assert_eq!(
            span.parent, None,
            "nested drop restored the root parent for siblings"
        );
    }
}

#[test]
fn with_context_none_runs_and_restores() {
    let path = temp_log("ctx-none");
    let obs = observer(&path, proc::DAEMON, true);
    obs.with_context(ctx("req", None), || {
        let ran = std::cell::Cell::new(false);
        obs.with_context(None, || {
            ran.set(true);
            assert!(obs.context().is_none(), "cleared inside None scope");
            obs.event("lease.acquired", json!({}));
        });
        assert!(ran.get(), "f runs even when ctx is None");
        let restored = obs.context().expect("restored");
        assert_eq!(restored.trace.as_ref(), "req");
    });
    assert!(!path.exists(), "event inside None context dropped");
}

#[test]
fn with_context_restores_after_unwinding_body() {
    let path = temp_log("ctx-panic");
    let obs = observer(&path, proc::DAEMON, true);
    obs.with_context(ctx("outer", None), || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            obs.with_context(ctx("inner", None), || panic!("boom"));
        }));
        assert!(result.is_err(), "inner body unwound");
        let restored = obs.context().expect("restored after panic");
        assert_eq!(restored.trace.as_ref(), "outer");
    });
}

#[test]
fn span_ids_unique_across_proc_tokens() {
    let daemon_path = temp_log("proc-daemon");
    let ns_path = temp_log("proc-ns");
    let daemon = observer(&daemon_path, proc::DAEMON, true);
    let ns = observer(&ns_path, proc::NAMESPACE_PROCESS, true);
    daemon.with_context(ctx("t", None), || {
        let _ = daemon.span("daemon.dispatch");
    });
    ns.with_context(ctx("t", None), || {
        let _ = ns.span("command.exec");
    });

    assert_eq!(spans(&daemon_path)[0].span, "d-0");
    assert_eq!(
        spans(&ns_path)[0].span,
        "np-0",
        "namespace-process proc token keeps ids disjoint"
    );
}
