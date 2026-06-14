//! Adapter-side `commit_to_git` tests: stable wire response keys over the
//! typed [`operation::checkpoint::CommitOutcome`] and the checkpoint-to-daemon error
//! mapping. Pipeline behavior lives in `operation`'s checkpoint tests.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use layerstack::{LayerChange, LayerStack};
use operation::checkpoint::contract::CommitInput;
use operation::OpRequest;
use protocol::catalog::BuiltinOp;
use serde_json::json;

use super::*;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[test]
fn commit_to_git_response_shape_for_committed_and_noop() -> TestResult {
    let fixture = Fixture::new("shape")?;
    LayerStack::open(fixture.root.clone())?.publish_layer(&[LayerChange::Write {
        path: layerstack::LayerPath::parse("checkpoint/included.txt")?,
        content: b"included\n".to_vec(),
    }])?;
    let args = json!({
        "layer_stack_root": fixture.root,
        "workspace_root": fixture.workspace,
        "paths": ["checkpoint/included.txt"],
        "message": "checkpoint shape",
    });

    // committed = true: a fresh path projects, stages, and commits.
    let committed = commit_to_git(parse_commit_input(args.clone()), DispatchContext::empty())?;
    assert_response_keys(&committed);
    assert_eq!(committed["success"], json!(true));
    assert_eq!(committed["committed"], json!(true));
    assert!(committed["commit_sha"].is_string(), "commit_sha present");
    assert_eq!(committed["manifest_version"], json!(2));
    assert!(committed["manifest_root_hash"].is_string());
    assert_eq!(committed["paths"], json!(["checkpoint/included.txt"]));
    assert!(matches!(
        committed["worktree_mode"].as_str(),
        Some("overlay" | "projection")
    ));
    assert!(
        committed.get("timings").is_none(),
        "commit_to_git timings live in trace/meta, not the result payload"
    );

    // committed = false: re-committing the same staged paths is a no-op that
    // still reports the prior HEAD and the full response shape.
    let noop = commit_to_git(parse_commit_input(args), DispatchContext::empty())?;
    assert_response_keys(&noop);
    assert_eq!(noop["success"], json!(true));
    assert_eq!(noop["committed"], json!(false));
    assert_eq!(noop["commit_sha"], committed["commit_sha"]);
    Ok(())
}

fn assert_response_keys(response: &serde_json::Value) {
    let keys: std::collections::BTreeSet<&str> = response
        .as_object()
        .expect("commit_to_git response is an object")
        .keys()
        .map(String::as_str)
        .collect();
    let expected: std::collections::BTreeSet<&str> = [
        "success",
        "committed",
        "commit_sha",
        "manifest_version",
        "manifest_root_hash",
        "paths",
        "worktree_mode",
    ]
    .into_iter()
    .collect();
    assert_eq!(keys, expected, "commit_to_git response keys are stable");
}

#[test]
fn commit_to_git_rejects_git_pathspecs() -> TestResult {
    let fixture = Fixture::new("reject-git")?;
    let response = commit_to_git(
        parse_commit_input(json!({
            "layer_stack_root": fixture.root,
            "workspace_root": fixture.workspace,
            "paths": [".git/config"],
            "message": "bad checkpoint",
        })),
        DispatchContext::empty(),
    );

    assert!(matches!(response, Err(DaemonError::Forbidden(_))));
    Ok(())
}

#[test]
fn commit_to_git_records_checkpoint_trace_events() -> TestResult {
    let fixture = Fixture::new("trace-events")?;
    LayerStack::open(fixture.root.clone())?.publish_layer(&[LayerChange::Write {
        path: layerstack::LayerPath::parse("checkpoint/included.txt")?,
        content: b"included\n".to_vec(),
    }])?;
    let sink = crate::trace::RequestTraceEventSink::default();
    let context = DispatchContext::empty().with_trace_events(sink.clone());

    let response = commit_to_git(
        parse_commit_input(json!({
            "layer_stack_root": fixture.root,
            "workspace_root": fixture.workspace,
            "paths": ["checkpoint/included.txt"],
            "message": "checkpoint trace events",
        })),
        context,
    )?;

    assert_response_keys(&response);
    let events = sink.drain();
    assert!(
        events.iter().any(|event| event.module == "workspace.route"
            && event.name == "route_selected"
            && event.details["kind"] == "fast_path"
            && event.details["reason"] == "commit_to_git_uses_layerstack_worktree"),
        "checkpoint route event recorded"
    );
    assert!(
        events.iter().any(|event| event.module == "checkpoint"
            && event.name == "git_command_finished"
            && event.details["argv_summary"] == "git commit -m <message>"
            && event.details["exit_code"] == 0),
        "checkpoint commit event recorded"
    );
    Ok(())
}

fn parse_commit_input(args: serde_json::Value) -> CommitInput {
    match OpRequest::parse(BuiltinOp::CommitToGit, &args, "checkpoint-test-invocation")
        .expect("valid commit input")
    {
        OpRequest::CommitToGit(input) => input,
        _ => unreachable!("commit op parses to commit input"),
    }
}

struct Fixture {
    base: PathBuf,
    root: PathBuf,
    workspace: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> TestResult<Self> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let base = std::env::temp_dir().join(format!(
            "eosd-commit-to-git-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("layer-stack");
        let workspace = base.join("workspace");
        let layer = root.join("layers").join("B000001-base");
        std::fs::create_dir_all(&layer)?;
        std::fs::create_dir_all(root.join("staging"))?;
        std::fs::create_dir_all(&workspace)?;
        std::fs::write(layer.join("README.md"), "# README\n")?;
        std::fs::write(
            root.join("manifest.json"),
            serde_json::to_string_pretty(&json!({
                "schema_version": 1,
                "version": 1,
                "layers": [{"layer_id": "B000001-base", "path": "layers/B000001-base"}],
            }))?,
        )?;
        std::fs::write(
            root.join("workspace.json"),
            serde_json::to_string_pretty(&json!({
                "workspace_root": workspace,
                "layer_stack_root": root,
                "active_manifest_version": 1,
                "active_root_hash": "root",
                "base_manifest_version": 1,
                "base_root_hash": "base",
            }))?,
        )?;
        run_git_init(&workspace)?;
        Ok(Self {
            base,
            root,
            workspace,
        })
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn run_git_init(workspace: &Path) -> TestResult {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .arg("init")
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "git init failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into())
    }
}
