use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sandbox_observability_telemetry::record::{names, proc};
use sandbox_observability_telemetry::{
    Observer, ObserverConfig, RawFilter, Reader, Record, Sink, SpanStatus,
};
use sandbox_runtime::layerstack::LayerStackService;
use sandbox_runtime::{LayerstackRuntimeConfig, SandboxRuntimeOperations};
use sandbox_runtime_layerstack::{LayerChange, LayerPath, LayerStack};

mod support;

#[test]
fn omitted_policy_starts_no_worker_and_never_squashes() {
    let log = TempTraceLog::new("disabled");
    let observer = observed(&log);
    let (operations, root) = operations(None, observer);

    amend(&operations.layerstack, "one.txt", b"one");
    amend(&operations.layerstack, "two.txt", b"two");
    amend(&operations.layerstack, "three.txt", b"three");

    let manifest = manifest(&root);
    assert_eq!(manifest.layers.len(), 4);
    assert!(manifest
        .layers
        .iter()
        .all(|layer| !layer.layer_id.starts_with('S')));
    assert!(records(&log).into_iter().all(|record| match record {
        Record::Span(span) => span.name != names::LAYERSTACK_AUTOSQUASH_EVALUATE,
        Record::Event(event) => !event.name.starts_with("layerstack.autosquash."),
        Record::Sample(_) => true,
    }));
}

#[test]
fn exact_threshold_squashes_and_records_the_exact_internal_trace_tree() {
    let log = TempTraceLog::new("threshold");
    let observer = observed(&log);
    let (operations, root) = operations(Some(3), observer);

    amend(&operations.layerstack, "one.txt", b"one");
    amend(&operations.layerstack, "two.txt", b"two");

    wait_for(Duration::from_secs(5), || {
        let current = manifest(&root);
        current.layers.len() == 2
            && current
                .layers
                .iter()
                .any(|layer| layer.layer_id.starts_with('S'))
    });
    let completed = wait_for_record(&log, |record| match record {
        Record::Event(event) if event.name == names::LAYERSTACK_AUTOSQUASH_COMPLETED => {
            Some((event.trace.clone(), event.attrs.clone()))
        }
        _ => None,
    });
    assert_eq!(completed.1["threshold"], 3);
    assert_eq!(completed.1["before_layers"], 3);
    assert_eq!(completed.1["after_layers"], 2);
    assert_eq!(completed.1["blocks_committed"], 1);
    assert_eq!(completed.1["status"], "completed");

    let trace = records(&log)
        .into_iter()
        .filter(|record| match record {
            Record::Span(span) => span.trace == completed.0,
            Record::Event(event) => event.trace == completed.0,
            Record::Sample(_) => false,
        })
        .collect::<Vec<_>>();
    let spans = trace
        .iter()
        .filter_map(|record| match record {
            Record::Span(span) => Some(span),
            _ => None,
        })
        .collect::<Vec<_>>();
    let events = trace
        .iter()
        .filter_map(|record| match record {
            Record::Event(event) => Some(event),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut span_names = spans
        .iter()
        .map(|span| span.name.as_ref())
        .collect::<Vec<_>>();
    span_names.sort_unstable();
    let mut expected_span_names = vec![
        names::LAYERSTACK_AUTOSQUASH_EVALUATE,
        names::LAYERSTACK_SQUASH,
        names::LAYERSTACK_SQUASH_COMMIT,
        names::LAYERSTACK_SQUASH_FLATTEN,
        names::LAYERSTACK_SQUASH_PLAN,
        names::LAYERSTACK_SQUASH_REMOUNT_SWEEP,
    ];
    expected_span_names.sort_unstable();
    assert_eq!(span_names, expected_span_names);
    assert!(spans
        .iter()
        .all(|span| span.status == SpanStatus::Completed));

    let evaluate = only_span(&spans, names::LAYERSTACK_AUTOSQUASH_EVALUATE);
    let squash = only_span(&spans, names::LAYERSTACK_SQUASH);
    assert_eq!(evaluate.parent, None);
    assert_eq!(squash.parent.as_deref(), Some(evaluate.span.as_str()));
    assert_eq!(squash.attrs["cause"], "autosquash");
    assert_eq!(squash.attrs["policy"], "squash_at_n_layers");
    assert_eq!(squash.attrs["threshold"], 3);
    assert_eq!(squash.attrs["observed_layers"], 3);
    for name in [
        names::LAYERSTACK_SQUASH_PLAN,
        names::LAYERSTACK_SQUASH_FLATTEN,
        names::LAYERSTACK_SQUASH_COMMIT,
        names::LAYERSTACK_SQUASH_REMOUNT_SWEEP,
    ] {
        assert_eq!(
            only_span(&spans, name).parent.as_deref(),
            Some(squash.span.as_str())
        );
    }
    assert_eq!(
        sorted_keys(&evaluate.attrs),
        vec![
            "coalesced_notifications",
            "decision",
            "observed_layers",
            "policy",
            "queue_delay_ms",
            "threshold",
            "trigger_reason",
        ]
    );
    assert_eq!(evaluate.attrs["decision"], "trigger");
    assert_eq!(evaluate.attrs["observed_layers"], 3);
    assert_eq!(evaluate.attrs["policy"], "squash_at_n_layers");
    assert_eq!(evaluate.attrs["threshold"], 3);

    assert_eq!(events.len(), 2);
    assert_eq!(
        events
            .iter()
            .filter(|event| event.name == names::LAYERSTACK_AUTOSQUASH_TRIGGERED)
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.name == names::LAYERSTACK_AUTOSQUASH_COMPLETED)
            .count(),
        1
    );
    assert!(events
        .iter()
        .all(|event| event.parent.as_deref() == Some(evaluate.span.as_str())));
    assert!(trace.iter().all(|record| match record {
        Record::Span(span) => !span.name.starts_with("operation."),
        Record::Event(event) => !event.name.starts_with("operation."),
        Record::Sample(_) => true,
    }));
}

#[test]
fn startup_rechecks_an_existing_above_threshold_manifest() {
    let log = TempTraceLog::new("startup");
    let observer = observed(&log);
    let layerstack = support::observed_layerstack_service_with_config(
        observer,
        LayerstackRuntimeConfig {
            autosquash_squash_at_n_layers: Some(3),
            ..LayerstackRuntimeConfig::default()
        },
    );
    let root = layerstack.layer_stack_root().to_path_buf();
    publish(&root, "one.txt", b"one");
    publish(&root, "two.txt", b"two");
    let operations = operations_from_layerstack(Arc::clone(&layerstack));

    wait_for(Duration::from_secs(5), || manifest(&root).layers.len() == 2);
    let evaluate = wait_for_record(&log, |record| match record {
        Record::Span(span)
            if span.name == names::LAYERSTACK_AUTOSQUASH_EVALUATE
                && span.attrs.get("trigger_reason")
                    == Some(&serde_json::Value::String("startup".to_owned())) =>
        {
            Some(span.clone())
        }
        _ => None,
    });
    assert_eq!(evaluate.attrs["observed_layers"], 3);
    assert_eq!(evaluate.attrs["decision"], "trigger");
    drop(operations);
}

#[test]
fn no_op_amend_creates_neither_layer_nor_notification() {
    let log = TempTraceLog::new("noop");
    let observer = observed(&log);
    let (operations, root) = operations(Some(10), observer);

    amend(&operations.layerstack, "file.txt", b"same");
    wait_for_evaluation(&log, 2);
    let before = manifest(&root);
    amend(&operations.layerstack, "file.txt", b"same");
    let after_no_op = manifest(&root);
    assert_eq!(after_no_op.version, before.version);
    assert_eq!(after_no_op.layers, before.layers);
    amend(&operations.layerstack, "file.txt", b"different");
    wait_for_evaluation(&log, 3);

    let layer_commit_evaluations = records(&log)
        .into_iter()
        .filter_map(|record| match record {
            Record::Span(span)
                if span.name == names::LAYERSTACK_AUTOSQUASH_EVALUATE
                    && span.attrs.get("trigger_reason")
                        == Some(&serde_json::Value::String("layer_committed".to_owned())) =>
            {
                Some(span)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(layer_commit_evaluations.len(), 2);
    assert!(layer_commit_evaluations
        .iter()
        .all(|span| span.attrs["coalesced_notifications"] == 0));
}

#[test]
fn layer_commit_evaluation_starts_an_internal_root_trace() {
    let log = TempTraceLog::new("internal-root");
    let observer = observed(&log);
    let (operations, _root) = operations(Some(10), observer.clone());

    observer
        .scope("operation.file_write", |_| {
            amend(&operations.layerstack, "file.txt", b"content");
            Ok::<_, std::convert::Infallible>(())
        })
        .unwrap_or_else(|never| match never {});
    let evaluation = wait_for_record(&log, |record| match record {
        Record::Span(span)
            if span.name == names::LAYERSTACK_AUTOSQUASH_EVALUATE
                && span.attrs.get("trigger_reason")
                    == Some(&serde_json::Value::String("layer_committed".to_owned())) =>
        {
            Some(span.clone())
        }
        _ => None,
    });

    assert_eq!(evaluation.parent, None);
    assert!(evaluation.trace.starts_with("autosquash-layer-committed-"));
    let trace_records = records(&log)
        .into_iter()
        .filter(|record| match record {
            Record::Span(span) => span.trace == evaluation.trace,
            Record::Event(event) => event.trace == evaluation.trace,
            Record::Sample(_) => false,
        })
        .collect::<Vec<_>>();
    assert!(trace_records.iter().all(|record| match record {
        Record::Span(span) => !span.name.starts_with("operation."),
        Record::Event(event) => !event.name.starts_with("operation."),
        Record::Sample(_) => true,
    }));
}

#[test]
fn an_existing_s_layer_counts_toward_the_threshold() {
    let log = TempTraceLog::new("s-layer-count");
    let observer = observed(&log);
    let (operations, root) = operations(Some(3), observer);

    amend(&operations.layerstack, "one.txt", b"one");
    amend(&operations.layerstack, "two.txt", b"two");
    wait_for(Duration::from_secs(5), || {
        let current = manifest(&root);
        current.layers.len() == 2
            && current
                .layers
                .iter()
                .any(|layer| layer.layer_id.starts_with('S'))
    });

    amend(&operations.layerstack, "three.txt", b"three");
    let evaluation = wait_for_record(&log, |record| match record {
        Record::Span(span)
            if span.name == names::LAYERSTACK_AUTOSQUASH_EVALUATE
                && span.attrs.get("trigger_reason")
                    == Some(&serde_json::Value::String("layer_committed".to_owned()))
                && span.attrs.get("observed_layers") == Some(&serde_json::Value::from(3)) =>
        {
            Some(span.clone())
        }
        _ => None,
    });

    assert_eq!(evaluation.attrs["decision"], "trigger");
    wait_for(Duration::from_secs(5), || manifest(&root).layers.len() == 2);
}

#[test]
fn startup_manifest_failure_emits_one_failed_terminal_record() {
    let log = TempTraceLog::new("startup-failure");
    let observer = observed(&log);
    let layerstack = support::observed_layerstack_service_with_config(
        observer,
        LayerstackRuntimeConfig {
            autosquash_squash_at_n_layers: Some(3),
            ..LayerstackRuntimeConfig::default()
        },
    );
    std::fs::write(
        layerstack.layer_stack_root().join("manifest.json"),
        "not-json",
    )
    .expect("corrupt manifest for deterministic startup failure");
    let operations = operations_from_layerstack(layerstack);

    let failed = wait_for_record(&log, |record| match record {
        Record::Event(event) if event.name == names::LAYERSTACK_AUTOSQUASH_FAILED => {
            Some((event.trace.clone(), event.attrs.clone()))
        }
        _ => None,
    });
    let trace = records(&log)
        .into_iter()
        .filter(|record| match record {
            Record::Span(span) => span.trace == failed.0,
            Record::Event(event) => event.trace == failed.0,
            Record::Sample(_) => false,
        })
        .collect::<Vec<_>>();
    let evaluate = trace
        .iter()
        .find_map(|record| match record {
            Record::Span(span) if span.name == names::LAYERSTACK_AUTOSQUASH_EVALUATE => Some(span),
            _ => None,
        })
        .expect("failed evaluation span");

    assert_eq!(evaluate.status, SpanStatus::Error);
    assert_eq!(evaluate.attrs["trigger_reason"], "startup");
    assert_eq!(evaluate.attrs["policy"], "squash_at_n_layers");
    assert_eq!(evaluate.attrs["threshold"], 3);
    assert!(evaluate.attrs["error"].is_string());
    assert_eq!(failed.1["policy"], "squash_at_n_layers");
    assert_eq!(failed.1["threshold"], 3);
    assert_eq!(failed.1["observed_layers"], serde_json::Value::Null);
    assert_eq!(failed.1["status"], "failed");
    assert!(failed.1["error"].is_string());
    assert!(failed.1["queue_delay_ms"].is_number());
    assert!(failed.1["elapsed_ms"].is_number());
    assert_eq!(
        trace
            .iter()
            .filter(|record| matches!(record, Record::Event(event) if event.name == names::LAYERSTACK_AUTOSQUASH_FAILED))
            .count(),
        1
    );
    assert!(trace.iter().all(|record| match record {
        Record::Event(event) => {
            event.name != names::LAYERSTACK_AUTOSQUASH_TRIGGERED
                && event.name != names::LAYERSTACK_AUTOSQUASH_COMPLETED
                && !event.name.starts_with("operation.")
        }
        Record::Span(span) => !span.name.starts_with("operation."),
        Record::Sample(_) => true,
    }));
    drop(operations);
}

fn operations(threshold: Option<usize>, observer: Observer) -> (SandboxRuntimeOperations, PathBuf) {
    let layerstack = support::observed_layerstack_service_with_config(
        observer,
        LayerstackRuntimeConfig {
            autosquash_squash_at_n_layers: threshold,
            ..LayerstackRuntimeConfig::default()
        },
    );
    let root = layerstack.layer_stack_root().to_path_buf();
    (operations_from_layerstack(layerstack), root)
}

fn operations_from_layerstack(layerstack: Arc<LayerStackService>) -> SandboxRuntimeOperations {
    let fake = Arc::new(support::FakeWorkspaceService::new());
    let services = support::build_services_with_launch_driver_and_layerstack(
        fake,
        Arc::new(support::FakeLaunchDriver::new()),
        Arc::clone(&layerstack),
    );
    SandboxRuntimeOperations::new(
        services.command,
        services.workspace,
        layerstack,
        support::test_file_service(),
    )
}

fn amend(layerstack: &LayerStackService, path: &str, content: &[u8]) {
    layerstack
        .amend_path(
            &LayerPath::parse(path).expect("valid path"),
            "autosquash-test",
            1024,
            |_| Ok::<_, std::convert::Infallible>(content.to_vec()),
        )
        .expect("amend succeeds");
}

fn publish(root: &Path, path: &str, content: &[u8]) {
    let mut stack = LayerStack::open(root.to_path_buf()).expect("open layerstack");
    stack
        .publish_layer(&[LayerChange::Write {
            path: LayerPath::parse(path).expect("valid path"),
            content: content.to_vec(),
        }])
        .expect("publish layer");
}

fn manifest(root: &Path) -> sandbox_runtime_layerstack::Manifest {
    LayerStack::open(root.to_path_buf())
        .and_then(|stack| stack.read_active_manifest())
        .expect("read active manifest")
}

fn wait_for_evaluation(log: &TempTraceLog, observed_layers: usize) {
    let _ = wait_for_record(log, |record| match record {
        Record::Span(span)
            if span.name == names::LAYERSTACK_AUTOSQUASH_EVALUATE
                && span.attrs.get("observed_layers")
                    == Some(&serde_json::Value::from(observed_layers)) =>
        {
            Some(())
        }
        _ => None,
    });
}

fn wait_for_record<T>(log: &TempTraceLog, mut select: impl FnMut(&Record) -> Option<T>) -> T {
    let mut selected = None;
    wait_for(Duration::from_secs(5), || {
        selected = records(log).iter().find_map(&mut select);
        selected.is_some()
    });
    selected.expect("record appears before deadline")
}

fn wait_for(timeout: Duration, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "condition timed out after {timeout:?}"
        );
        std::thread::yield_now();
    }
}

fn only_span<'a>(
    spans: &'a [&sandbox_observability_telemetry::Span],
    name: &str,
) -> &'a sandbox_observability_telemetry::Span {
    let matches = spans
        .iter()
        .copied()
        .filter(|span| span.name == name)
        .collect::<Vec<_>>();
    assert_eq!(matches.len(), 1, "exactly one {name} span");
    matches[0]
}

fn sorted_keys(attrs: &serde_json::Map<String, serde_json::Value>) -> Vec<&str> {
    let mut keys = attrs.keys().map(String::as_str).collect::<Vec<_>>();
    keys.sort_unstable();
    keys
}

fn records(log: &TempTraceLog) -> Vec<Record> {
    Reader::new(log.path.clone(), log.path.with_extension("absent"))
        .raw(RawFilter::default())
        .into_iter()
        .map(|line| serde_json::from_str(&line).expect("valid observability record"))
        .collect()
}

fn observed(log: &TempTraceLog) -> Observer {
    Observer::new(
        ObserverConfig {
            proc: proc::DAEMON,
            enabled: true,
        },
        Sink::new(
            log.path.clone(),
            sandbox_observability_telemetry::MAX_LINE_BYTES,
        ),
    )
}

struct TempTraceLog {
    root: PathBuf,
    path: PathBuf,
}

impl TempTraceLog {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "sandbox-autosquash-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create observability directory");
        Self {
            path: root.join("observability.ndjson"),
            root,
        }
    }
}

impl Drop for TempTraceLog {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
