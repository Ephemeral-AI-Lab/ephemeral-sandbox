//! Sink: single-write append keeps lines intact under concurrent appenders, and
//! an over-cap line becomes a `{"_truncated": n}` marker (never a split line),
//! with the documented Span-nested / Sample-top-level asymmetry.

use std::borrow::Cow;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::os::unix::process::CommandExt;

use sandbox_observability_telemetry::{
    Attrs, Record, Sample, Sink, Span, SpanStatus, MAX_LINE_BYTES,
};
use serde_json::{json, Value};

static NEXT: AtomicU64 = AtomicU64::new(0);

fn temp_log(label: &str) -> PathBuf {
    std::env::temp_dir()
        .join(format!(
            "sandbox-obs-sink-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
        .join("observability.ndjson")
}

fn attrs(value: Value) -> Attrs {
    value.as_object().cloned().unwrap_or_default()
}

fn rotated(path: &std::path::Path) -> PathBuf {
    PathBuf::from(format!("{}.1", path.display()))
}

fn file_len(path: &std::path::Path) -> u64 {
    fs::metadata(path).map_or(0, |metadata| metadata.len())
}

fn assert_store(path: &std::path::Path, budget: u64) {
    let sibling = rotated(path);
    let half = budget / 2;
    assert!(file_len(path) <= half, "active segment exceeds half budget");
    assert!(
        file_len(&sibling) <= half,
        "rotated segment exceeds half budget"
    );
    assert!(
        file_len(path) + file_len(&sibling) <= budget,
        "total event store exceeds hard budget"
    );
    for segment in [path, sibling.as_path()] {
        let bytes = match fs::read(segment) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => panic!("read {}: {error}", segment.display()),
        };
        assert!(bytes.last().is_none_or(|byte| *byte == b'\n'));
        for line in bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
        {
            serde_json::from_slice::<Record>(line).expect("every persisted line parses");
        }
    }
}

fn sample(ts: i64, blob: String) -> Record {
    Record::Sample(Sample {
        ts,
        scope: "sandbox".to_owned(),
        metrics: attrs(json!({ "blob": blob })),
    })
}

fn sample_with_line_len(target: usize) -> Record {
    let empty = sample(1, String::new());
    let overhead = serde_json::to_vec(&empty).expect("serialize").len() + 1;
    assert!(target >= overhead);
    let record = sample(1, "x".repeat(target - overhead));
    assert_eq!(
        serde_json::to_vec(&record).expect("serialize").len() + 1,
        target
    );
    record
}

#[test]
fn append_never_exceeds_total_budget() {
    let path = temp_log("hard-cap");
    let budget = 64 * 1024;
    let sink = Sink::with_budget(path.clone(), MAX_LINE_BYTES, budget);
    let mut rotations = 0;
    let mut previous_rotated = Vec::new();

    for ts in 0..64 {
        sink.append(&sample(ts, "x".repeat(15_700)))
            .expect("append maximum record");
        assert_store(&path, budget);
        let current = fs::read(rotated(&path)).unwrap_or_default();
        if !current.is_empty() && current != previous_rotated {
            rotations += 1;
            previous_rotated = current;
        }
    }

    assert!(rotations >= 10, "observed {rotations} complete rotations");
    let _ = fs::remove_dir_all(path.parent().expect("parent"));
}

#[test]
fn boundary_append_rotates_before_write() {
    let budget = 2_048;
    let cap = budget / 2;
    let tail = sample_with_line_len(400);

    for (label, initial, should_rotate) in [
        ("exact", cap as usize - 400, false),
        ("one-short", cap as usize - 401, false),
        ("one-over", cap as usize - 399, true),
    ] {
        let path = temp_log(label);
        let sink = Sink::with_budget(path.clone(), cap as usize, budget);
        sink.append(&sample_with_line_len(initial))
            .expect("initial append");
        sink.append(&tail).expect("boundary append");
        assert_eq!(rotated(&path).exists(), should_rotate, "{label}");
        assert_store(&path, budget);
        if should_rotate {
            assert_eq!(file_len(&path), 400, "new record is in active segment");
        } else {
            assert_eq!(file_len(&path), initial as u64 + 400, "{label}");
        }
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }
}

#[test]
fn strict_append_drops_oversized_samples_without_a_marker_or_write() {
    let path = temp_log("strict-oversized");
    let sink = Sink::with_budget(path.clone(), 512, 2_048);
    sink.append_strict(&sample(1, "x".repeat(1_000)))
        .expect("oversized resource sample is a counted drop");

    assert_eq!(file_len(&path), 0);
    assert_eq!(file_len(&rotated(&path)), 0);
    assert_eq!(sink.stats().dropped_oversized, 1);
    assert_eq!(sink.stats().truncated_records, 0);
    let _ = fs::remove_dir_all(path.parent().expect("parent"));
}

#[test]
fn producer_process() {
    let Ok(path) = std::env::var("SANDBOX_OBS_PRODUCER_PATH") else {
        return;
    };
    let count: usize = std::env::var("SANDBOX_OBS_PRODUCER_COUNT")
        .expect("count")
        .parse()
        .expect("integer count");
    let producer: i64 = std::env::var("SANDBOX_OBS_PRODUCER_INDEX")
        .expect("index")
        .parse()
        .expect("integer index");
    let budget: u64 = std::env::var("SANDBOX_OBS_PRODUCER_BUDGET")
        .expect("budget")
        .parse()
        .expect("integer budget");
    let sink = Sink::with_budget(PathBuf::from(path), MAX_LINE_BYTES, budget);
    for index in 0..count {
        let blob = if index % 17 == 0 {
            "m".repeat(15_700)
        } else {
            format!("producer-{producer}-record-{index}")
        };
        sink.append(&sample(
            producer.saturating_mul(1_000_000) + i64::try_from(index).unwrap_or(i64::MAX),
            blob,
        ))
        .expect("producer append");
    }
}

#[cfg(unix)]
#[test]
fn permission_denied_process() {
    let Ok(path) = std::env::var("SANDBOX_OBS_PERMISSION_DENIED_PATH") else {
        return;
    };
    let sink = Sink::with_budget(PathBuf::from(path), MAX_LINE_BYTES, 4_096);
    let error = sink
        .append(&sample(1, "business-continues".to_owned()))
        .expect_err("inaccessible event directory rejects the append");
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    assert_eq!(sink.stats().dropped_storage, 1, "one attempt, one drop");
}

#[cfg(unix)]
#[test]
fn permission_denied_is_counted_once_without_retry() {
    let path = temp_log("permission-denied");
    let parent = path.parent().expect("parent");
    fs::create_dir_all(parent).expect("create parent");
    fs::set_permissions(parent, fs::Permissions::from_mode(0o000))
        .expect("make event directory inaccessible");

    let executable = std::env::current_exe().expect("current test executable");
    let mut child = Command::new(executable);
    child
        .args(["--exact", "permission_denied_process", "--nocapture"])
        .env("SANDBOX_OBS_PERMISSION_DENIED_PATH", &path)
        .stdout(Stdio::null());
    let running_as_root = Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .is_some_and(|uid| uid.trim() == "0");
    if running_as_root {
        child.uid(65_534);
    }
    let status = child.status().expect("spawn permission-denied producer");

    fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
        .expect("restore directory permissions");
    assert!(status.success(), "permission-denied child failed: {status}");
    let _ = fs::remove_dir_all(parent);
}

#[test]
fn concurrent_processes_preserve_budget_and_lines() {
    const PRODUCERS: usize = 32;
    const APPENDS: usize = 100_000;
    let path = temp_log("processes");
    let budget = 1024 * 1024;
    let executable = std::env::current_exe().expect("current test executable");
    let mut children = Vec::with_capacity(PRODUCERS);
    for producer in 0..PRODUCERS {
        let count = APPENDS / PRODUCERS;
        children.push(
            Command::new(&executable)
                .args(["--exact", "producer_process", "--nocapture"])
                .env("SANDBOX_OBS_PRODUCER_PATH", &path)
                .env("SANDBOX_OBS_PRODUCER_COUNT", count.to_string())
                .env("SANDBOX_OBS_PRODUCER_INDEX", producer.to_string())
                .env("SANDBOX_OBS_PRODUCER_BUDGET", budget.to_string())
                .stdout(Stdio::null())
                .spawn()
                .expect("spawn producer"),
        );
    }
    for mut child in children {
        assert!(child.wait().expect("wait for producer").success());
    }
    assert_store(&path, budget);
    let _ = fs::remove_dir_all(path.parent().expect("parent"));
}

#[test]
fn concurrent_appends_keep_every_line_intact() {
    let path = temp_log("concurrent");
    let sink = Arc::new(Sink::new(path.clone(), MAX_LINE_BYTES));
    let threads = 8;
    let per_thread = 64;

    std::thread::scope(|scope| {
        for thread in 0..threads {
            let sink = Arc::clone(&sink);
            scope.spawn(move || {
                for index in 0..per_thread {
                    let record = Record::Sample(Sample {
                        ts: i64::from(thread * per_thread + index),
                        scope: "sandbox".to_owned(),
                        metrics: attrs(json!({ "n": thread, "i": index })),
                    });
                    sink.append(&record).expect("append");
                }
            });
        }
    });

    let contents = fs::read_to_string(&path).expect("read log");
    let lines: Vec<&str> = contents.lines().collect();
    assert_eq!(lines.len() as i32, threads * per_thread, "no lines lost");
    for line in lines {
        serde_json::from_str::<Record>(line).expect("each line parses intact (not interleaved)");
    }

    let _ = fs::remove_dir_all(path.parent().expect("parent"));
}

#[test]
fn over_cap_span_truncates_attrs_in_place() {
    let path = temp_log("trunc-span");
    let sink = Sink::new(path.clone(), MAX_LINE_BYTES);
    let blob = "x".repeat(MAX_LINE_BYTES);
    sink.append(&Record::Span(Span {
        ts: 1,
        trace: "t".to_owned(),
        span: "d-0".to_owned(),
        parent: None,
        name: Cow::Borrowed("command.exec"),
        dur_ms: 0.0,
        status: SpanStatus::Completed,
        attrs: attrs(json!({ "blob": blob })),
    }))
    .expect("append");

    let contents = fs::read_to_string(&path).expect("read log");
    let lines: Vec<&str> = contents.lines().collect();
    assert_eq!(lines.len(), 1, "one line, never split");
    let value: Value = serde_json::from_str(lines[0]).expect("parses");
    assert!(
        value["attrs"]["_truncated"].is_number(),
        "Span marker nested under attrs"
    );
    assert!(
        value["attrs"].get("blob").is_none(),
        "oversized attr dropped wholesale"
    );
    assert!(lines[0].len() < MAX_LINE_BYTES, "truncated line is small");

    let _ = fs::remove_dir_all(path.parent().expect("parent"));
}

#[test]
fn over_cap_sample_truncates_metrics_at_top_level() {
    let path = temp_log("trunc-sample");
    let sink = Sink::new(path.clone(), MAX_LINE_BYTES);
    let blob = "x".repeat(MAX_LINE_BYTES);
    sink.append(&Record::Sample(Sample {
        ts: 1,
        scope: "sandbox".to_owned(),
        metrics: attrs(json!({ "blob": blob })),
    }))
    .expect("append");

    let contents = fs::read_to_string(&path).expect("read log");
    let value: Value =
        serde_json::from_str(contents.lines().next().expect("line")).expect("parses");
    assert!(
        value["_truncated"].is_number(),
        "flattened Sample marker lands at the top level"
    );
    assert!(value.get("blob").is_none());

    let _ = fs::remove_dir_all(path.parent().expect("parent"));
}

#[test]
fn oversized_record_writes_one_bounded_marker_for_escaped_utf8() {
    let path = temp_log("trunc-escaped-utf8");
    let max_line = 512;
    let sink = Sink::with_budget(path.clone(), max_line, 4_096);
    let blob = format!("{}{}", "\\\"\n".repeat(400), "🦀".repeat(400));

    sink.append(&sample(7, blob)).expect("append");

    let bytes = fs::read(&path).expect("read marker");
    assert!(bytes.len() <= max_line, "newline is included in the cap");
    assert_eq!(bytes.iter().filter(|byte| **byte == b'\n').count(), 1);
    let value: Value = serde_json::from_slice(&bytes).expect("marker parses");
    assert!(value["_truncated"].as_u64().is_some());
    assert!(value.get("blob").is_none());
    assert_eq!(value["kind"], "sample");

    let _ = fs::remove_dir_all(path.parent().expect("parent"));
}

#[test]
fn recovery_accounts_one_oversized_escaped_utf8_line() {
    let path = temp_log("recover-oversized-escaped-utf8");
    fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
    let max_line = 512;
    let budget = 4_096;
    let oversized = serde_json::to_vec(&sample(
        6,
        format!("{}{}", "\\\"\n".repeat(400), "🦀".repeat(400)),
    ))
    .expect("serialize fixture");
    assert!(oversized.len() > max_line);
    assert!(
        oversized.len() as u64 > budget / 2,
        "legacy recovery is entered only when a segment exceeds its half-budget"
    );
    let mut fixture = oversized;
    fixture.push(b'\n');
    fs::write(&path, fixture).expect("write oversized fixture");

    let sink = Sink::with_budget(path.clone(), max_line, budget);
    sink.append(&sample(7, "recovered".to_owned()))
        .expect("recovery append");

    assert_eq!(sink.stats().dropped_oversized, 1);
    let contents = fs::read_to_string(&path).expect("read recovered store");
    let lines: Vec<&str> = contents.lines().collect();
    assert_eq!(lines.len(), 1);
    serde_json::from_str::<Record>(lines[0]).expect("recovered line parses");
    assert!(lines[0].len() < max_line);

    let _ = fs::remove_dir_all(path.parent().expect("parent"));
}

#[test]
fn marker_that_cannot_fit_is_dropped_once() {
    let path = temp_log("marker-drop");
    let sink = Sink::with_budget(path.clone(), 8, 4_096);
    sink.append(&sample(1, "x".repeat(100)))
        .expect("policy drop");
    assert!(!path.exists());
    assert_eq!(sink.stats().dropped_oversized, 1);
    assert_eq!(sink.stats().truncated_records, 0);
    let _ = fs::remove_dir_all(path.parent().expect("parent"));
}

#[test]
fn crash_during_rotation_recovers_within_budget() {
    let budget = 4_096;
    let max_line = 1_024;
    let complete = serde_json::to_vec(&sample(1, "old".repeat(40))).expect("serialize");
    let mut line = complete.clone();
    line.push(b'\n');

    for state in [
        "before-remove-old",
        "after-remove-old",
        "after-rename",
        "partial-append",
        "stale-compaction",
        "legacy-over-cap",
    ] {
        let path = temp_log(state);
        let sibling = rotated(&path);
        fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
        match state {
            "before-remove-old" => {
                fs::write(&sibling, &line).expect("old rotated");
                fs::write(&path, &line).expect("old active");
            }
            "after-remove-old" => {
                fs::write(&path, &line).expect("active only");
            }
            "after-rename" => {
                fs::write(&sibling, &line).expect("renamed active");
            }
            "partial-append" => {
                fs::write(&sibling, &line).expect("rotated");
                let mut partial = line.clone();
                partial.extend_from_slice(b"{\"kind\":\"sample\"");
                fs::write(&path, partial).expect("partial active");
            }
            "stale-compaction" => {
                fs::write(&path, &line).expect("active");
                fs::write(
                    format!("{}.compact.{}", path.display(), std::process::id()),
                    b"partial temporary",
                )
                .expect("stale temporary");
            }
            "legacy-over-cap" => {
                fs::write(&path, line.repeat(40)).expect("oversized legacy active");
                fs::write(&sibling, line.repeat(40)).expect("oversized legacy rotated");
            }
            _ => unreachable!(),
        }

        let sink = Sink::with_budget(path.clone(), max_line, budget);
        sink.append(&sample(2, "recovered".to_owned()))
            .expect("recovery append");
        assert_store(&path, budget);
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }
}

#[cfg(target_os = "linux")]
#[test]
fn enospc_is_counted_once_without_retry() {
    use std::os::unix::fs::symlink;

    let path = temp_log("enospc");
    fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
    symlink("/dev/full", &path).expect("link active segment to /dev/full");
    let sink = Sink::with_budget(path.clone(), MAX_LINE_BYTES, 4_096);
    let error = sink
        .append(&sample(1, "business-continues".to_owned()))
        .expect_err("/dev/full returns ENOSPC");
    assert_eq!(error.raw_os_error(), Some(28));
    assert_eq!(sink.stats().dropped_storage, 1, "one attempt, one drop");
    let _ = fs::remove_dir_all(path.parent().expect("parent"));
}
