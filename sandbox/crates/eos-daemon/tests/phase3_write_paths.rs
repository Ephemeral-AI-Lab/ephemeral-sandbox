use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

// Integration test crates receive every normal `eos-daemon` dependency even
// when the test only drives public daemon APIs. These imports keep
// `unused_crate_dependencies` meaningful without suppressing it crate-wide.
use eos_daemon::wire::Request;
use eos_daemon::OpTable;
use eos_layerstack as _;
use eos_occ as _;
use eos_overlay as _;
use eos_plugin as _;
use eos_workspace_runtime as _;
use serde as _;
use serde_json::{json, Value};
use sha2 as _;
use thiserror as _;
use tokio as _;
use tokio_util as _;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[test]
fn dispatches_layerstack_write_file_and_reads_published_bytes() -> TestResult {
    let fixture = seed_layer_stack("write_file")?;
    let write = Request {
        op: "api.v1.write_file".to_owned(),
        invocation_id: "inv-write".to_owned(),
        args: json!({
            "layer_stack_root": &fixture.root,
            "path": fixture.workspace.join("new.txt"),
            "content": "hello\n",
        }),
    };
    let table = OpTable::with_builtins();

    let response = table.dispatch(&write);

    assert_eq!(response["success"], Value::Bool(true));
    assert_eq!(response["workspace"], Value::String("ephemeral".to_owned()));
    assert_eq!(response["changed_paths"], json!(["new.txt"]));
    assert_eq!(response["changed_path_kinds"], json!({"new.txt": "write"}));
    assert_eq!(
        response["mutation_source"],
        Value::String("api_write".to_owned())
    );
    assert_eq!(response["status"], Value::String("committed".to_owned()));
    assert!(response["timings"]["api.write.occ_apply_s"]
        .as_f64()
        .is_some());

    let read = Request {
        op: "api.v1.read_file".to_owned(),
        invocation_id: "inv-read".to_owned(),
        args: json!({
            "layer_stack_root": &fixture.root,
            "path": fixture.workspace.join("new.txt"),
        }),
    };
    let response = table.dispatch(&read);
    assert_eq!(response["success"], Value::Bool(true));
    assert_eq!(response["content"], Value::String("hello\n".to_owned()));
    assert_eq!(response["exists"], Value::Bool(true));
    Ok(())
}

#[test]
fn write_file_missing_content_is_invalid_envelope() -> TestResult {
    let fixture = seed_layer_stack("write_missing_content")?;
    let request = Request {
        op: "api.v1.write_file".to_owned(),
        invocation_id: "inv-write".to_owned(),
        args: json!({
            "layer_stack_root": &fixture.root,
            "path": fixture.workspace.join("new.txt"),
        }),
    };

    let response = OpTable::with_builtins().dispatch(&request);

    assert_eq!(response["success"], Value::Bool(false));
    assert_eq!(
        response["error"]["kind"],
        Value::String("invalid_envelope".to_owned())
    );
    assert!(response["error"]["message"]
        .as_str()
        .is_some_and(|message| message.contains("content is required")));
    Ok(())
}

#[test]
fn write_file_create_only_existing_returns_guarded_conflict() -> TestResult {
    let fixture = seed_layer_stack("write_create_only")?;
    let request = Request {
        op: "api.v1.write_file".to_owned(),
        invocation_id: "inv-write".to_owned(),
        args: json!({
            "layer_stack_root": &fixture.root,
            "path": fixture.workspace.join("README.md"),
            "content": "replacement\n",
            "overwrite": false,
        }),
    };

    let response = OpTable::with_builtins().dispatch(&request);

    assert_eq!(response["success"], Value::Bool(false));
    assert_eq!(response["changed_paths"], json!([]));
    assert_eq!(response["status"], Value::String("rejected".to_owned()));
    assert_eq!(
        response["conflict"],
        json!({
            "reason": "create_only_existing",
            "conflict_file": "README.md",
            "message": "file already exists",
        })
    );
    Ok(())
}

#[test]
fn write_file_git_path_is_dropped_by_occ_routing() -> TestResult {
    let fixture = seed_layer_stack("write_git_drop")?;
    let table = OpTable::with_builtins();
    let write = Request {
        op: "api.v1.write_file".to_owned(),
        invocation_id: "inv-write".to_owned(),
        args: json!({
            "layer_stack_root": &fixture.root,
            "path": fixture.workspace.join(".git/config"),
            "content": "ignored\n",
        }),
    };

    let response = table.dispatch(&write);

    assert_eq!(response["success"], Value::Bool(true));
    assert_eq!(response["status"], Value::String("committed".to_owned()));
    assert_eq!(response["changed_paths"], json!([]));
    assert_eq!(
        response["timings"]["resource.command_exec.changed_path_count"],
        json!(0.0)
    );

    let read = Request {
        op: "api.v1.read_file".to_owned(),
        invocation_id: "inv-read".to_owned(),
        args: json!({
            "layer_stack_root": &fixture.root,
            "path": fixture.workspace.join(".git/config"),
        }),
    };
    let response = table.dispatch(&read);
    assert_eq!(response["success"], Value::Bool(true));
    assert_eq!(response["exists"], Value::Bool(false));
    Ok(())
}

#[test]
fn dispatches_layerstack_edit_file_and_reads_published_bytes() -> TestResult {
    let fixture = seed_layer_stack("edit_file")?;
    let edit = Request {
        op: "api.v1.edit_file".to_owned(),
        invocation_id: "inv-edit".to_owned(),
        args: json!({
            "layer_stack_root": &fixture.root,
            "path": fixture.workspace.join("README.md"),
            "edits": [{"old_text": "README", "new_text": "NOTES", "replace_all": false}],
        }),
    };
    let table = OpTable::with_builtins();

    let response = table.dispatch(&edit);

    assert_eq!(response["success"], Value::Bool(true));
    assert_eq!(response["changed_paths"], json!(["README.md"]));
    assert_eq!(
        response["mutation_source"],
        Value::String("api_edit".to_owned())
    );
    assert_eq!(response["applied_edits"], json!(1));

    let read = Request {
        op: "api.v1.read_file".to_owned(),
        invocation_id: "inv-read".to_owned(),
        args: json!({
            "layer_stack_root": &fixture.root,
            "path": fixture.workspace.join("README.md"),
        }),
    };
    let response = table.dispatch(&read);
    assert_eq!(response["content"], Value::String("# NOTES\n".to_owned()));
    Ok(())
}

#[test]
fn identical_head_write_is_idempotent() -> TestResult {
    let fixture = seed_layer_stack("write_idempotent")?;
    let table = OpTable::with_builtins();
    let request = Request {
        op: "api.v1.write_file".to_owned(),
        invocation_id: "inv-write".to_owned(),
        args: json!({
            "layer_stack_root": &fixture.root,
            "path": fixture.workspace.join("new.txt"),
            "content": "same\n",
        }),
    };

    assert_eq!(table.dispatch(&request)["success"], Value::Bool(true));
    assert_eq!(table.dispatch(&request)["success"], Value::Bool(true));

    let metrics = Request {
        op: "api.layer_metrics".to_owned(),
        invocation_id: "inv-metrics".to_owned(),
        args: request.args,
    };
    let response = table.dispatch(&metrics);
    assert_eq!(response["manifest_depth"], json!(2));
    Ok(())
}

struct Fixture {
    base: PathBuf,
    root: PathBuf,
    workspace: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn seed_layer_stack(label: &str) -> TestResult<Fixture> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let base = std::env::temp_dir().join(format!(
        "eosd-p3-{label}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&base);
    let workspace = base.join("workspace");
    let root = base.join("layer-stack");
    let layer = root.join("layers").join("B000001-base");
    std::fs::create_dir_all(&workspace)?;
    std::fs::create_dir_all(&layer)?;
    std::fs::create_dir_all(root.join("staging"))?;
    std::fs::write(layer.join("README.md"), "# README\n")?;
    write_json(
        &root.join("manifest.json"),
        &json!({
            "schema_version": 1,
            "version": 1,
            "layers": [{"layer_id": "B000001-base", "path": "layers/B000001-base"}],
        }),
    )?;
    write_json(
        &root.join("workspace.json"),
        &json!({
            "workspace_root": workspace,
            "layer_stack_root": root,
            "active_manifest_version": 1,
            "active_root_hash": "root",
            "base_manifest_version": 1,
            "base_root_hash": "base",
        }),
    )?;
    Ok(Fixture {
        base,
        root,
        workspace,
    })
}

fn write_json(path: &Path, value: &Value) -> TestResult {
    let encoded = serde_json::to_string_pretty(value)?;
    std::fs::write(path, encoded)?;
    Ok(())
}
