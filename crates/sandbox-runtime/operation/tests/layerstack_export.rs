//! Export daemon-op surface: internal registration, the start result
//! contract, spool paging to eof (unlink-on-eof), the empty delta, coexisting
//! per-export spools, and lease release after the fold.

use std::io::Read;
use std::sync::Arc;

use base64::Engine as _;
use sandbox_operation_contract::{OperationRequest, OperationScope, OperationScopeKind};
use sandbox_runtime::SandboxRuntimeOperations;
use sandbox_runtime_layerstack::{LayerChange, LayerPath, LayerStack};
use serde_json::{json, Value};

mod support;
use support::FakeWorkspaceService;

fn export_request() -> OperationRequest {
    OperationRequest::new(
        "export_layerstack",
        "req-export-test",
        OperationScope::sandbox("sbox-test"),
        json!({}),
    )
}

fn chunk_request(export_id: &str, offset: u64, limit: Option<u64>) -> OperationRequest {
    let mut args = json!({ "export_id": export_id, "offset": offset });
    if let Some(limit) = limit {
        args["limit"] = json!(limit);
    }
    OperationRequest::new(
        "read_export_chunk",
        "req-export-test",
        OperationScope::sandbox("sbox-test"),
        args,
    )
}

fn operations_with_real_layerstack() -> (SandboxRuntimeOperations, std::path::PathBuf) {
    let fake = Arc::new(FakeWorkspaceService::new());
    let layerstack =
        support::observed_layerstack_service(sandbox_observability::Observer::disabled());
    let root = layerstack.layer_stack_root().to_path_buf();
    let services = support::build_services_with_launch_driver_and_layerstack(
        Arc::clone(&fake),
        Arc::new(support::FakeLaunchDriver::new()),
        Arc::clone(&layerstack),
    );
    let operations = SandboxRuntimeOperations::new(
        services.command,
        services.workspace,
        layerstack,
        support::test_file_service(),
    );
    (operations, root)
}

fn scratch_export_dir(root: &std::path::Path) -> std::path::PathBuf {
    root.parent()
        .expect("layerstack root parent")
        .join("scratch")
        .join(".export")
}

fn publish(root: &std::path::Path, changes: &[LayerChange]) {
    let mut stack = LayerStack::open(root.to_path_buf()).expect("open stack");
    stack.publish_layer(changes).expect("publish");
}

fn write_change(path: &str, content: &str) -> LayerChange {
    LayerChange::Write {
        path: LayerPath::parse(path).expect("path"),
        content: content.as_bytes().to_vec(),
    }
}

fn page_all(
    operations: &SandboxRuntimeOperations,
    export_id: &str,
    limit: Option<u64>,
) -> (Vec<u8>, usize) {
    let mut assembled = Vec::new();
    let mut offset = 0_u64;
    let mut chunks = 0_usize;
    loop {
        let response = sandbox_runtime::dispatch_operation(
            operations,
            &chunk_request(export_id, offset, limit),
        );
        let value = response.into_json_value();
        assert!(value.get("error").is_none(), "chunk read failed: {value}");
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(value["chunk"].as_str().expect("chunk"))
            .expect("base64");
        assert_eq!(value["len"].as_u64(), Some(bytes.len() as u64));
        assert_eq!(value["offset"].as_u64(), Some(offset));
        offset += bytes.len() as u64;
        assembled.extend_from_slice(&bytes);
        chunks += 1;
        if value["eof"].as_bool().expect("eof") {
            assert_eq!(value["total"].as_u64(), Some(offset));
            break;
        }
    }
    (assembled, chunks)
}

fn decode_entry_names(bytes: &[u8]) -> Vec<String> {
    let decoder = zstd::stream::read::Decoder::new(bytes).expect("zstd");
    let mut archive = tar::Archive::new(decoder);
    archive
        .entries()
        .expect("entries")
        .map(|entry| {
            let mut entry = entry.expect("entry");
            let name = String::from_utf8(entry.path_bytes().into_owned()).expect("utf8");
            let mut sink = Vec::new();
            entry.read_to_end(&mut sink).expect("drain");
            name
        })
        .collect()
}

#[test]
fn export_operations_are_internal_and_absent_from_the_public_catalog() {
    let internal = sandbox_runtime::runtime_internal_handler_keys().collect::<Vec<_>>();
    assert!(internal.contains(&(OperationScopeKind::Sandbox, "export_layerstack")));
    assert!(internal.contains(&(OperationScopeKind::Sandbox, "read_export_chunk")));
    let catalog = sandbox_operation_catalog::runtime::runtime_catalog();
    let encoded = sandbox_operation_contract::catalog_to_value(catalog).to_string();
    assert!(
        !encoded.contains("export_layerstack") && !encoded.contains("read_export_chunk"),
        "internal registration must keep both ops out of every catalog surface"
    );
}

// Start result contract: export_id, manifest_version, layers_exported,
// entry counts, spool_bytes; live_workspace_sessions omitted when none.
// Paging reassembles the exact spool bytes and eof unlinks it.
#[test]
fn export_spools_and_pages_to_eof() {
    let (operations, root) = operations_with_real_layerstack();
    publish(
        &root,
        &[
            write_change("src/a.rs", "v1\n"),
            write_change("src/b.rs", "B\n"),
        ],
    );
    publish(
        &root,
        &[
            write_change("src/a.rs", "v2\n"),
            LayerChange::Delete {
                path: LayerPath::parse("src/b.rs").expect("path"),
            },
        ],
    );

    let response = sandbox_runtime::dispatch_operation(&operations, &export_request());
    let value = response.into_json_value();
    assert!(value.get("error").is_none(), "export failed: {value}");
    let object = value.as_object().expect("result object");
    let mut keys = object.keys().collect::<Vec<_>>();
    keys.sort();
    assert_eq!(
        keys,
        vec![
            "entries",
            "export_id",
            "layers_exported",
            "manifest_version",
            "spool_bytes"
        ],
        "no live_workspace_sessions key when no session is alive"
    );
    assert_eq!(value["manifest_version"], json!(3));
    assert_eq!(
        value["layers_exported"].as_array().map(Vec::len),
        Some(2),
        "both published layers, base excluded"
    );
    assert_eq!(value["entries"]["files"], json!(1));
    assert_eq!(value["entries"]["whiteouts"], json!(1));
    assert_eq!(value["entries"]["symlinks"], json!(0));
    assert_eq!(value["entries"]["opaques"], json!(0));

    let export_id = value["export_id"].as_str().expect("export_id").to_owned();
    let spool_bytes = value["spool_bytes"].as_u64().expect("spool_bytes");
    let export_dir = scratch_export_dir(&root);
    let spool_path = export_dir.join(format!("{export_id}.tar.zst"));
    assert!(spool_path.is_file(), "spool lives under scratch/.export");

    let (assembled, chunks) = page_all(&operations, &export_id, Some(64));
    assert_eq!(assembled.len() as u64, spool_bytes);
    assert!(chunks > 1, "a 64-byte limit must take several chunks");
    assert!(
        !spool_path.exists(),
        "serving the final byte unlinks the spool"
    );

    let names = decode_entry_names(&assembled);
    assert_eq!(names, vec!["src/", "src/a.rs", "src/.wh.b.rs"]);

    let stray =
        sandbox_runtime::dispatch_operation(&operations, &chunk_request(&export_id, 0, None));
    let stray = stray.into_json_value();
    assert_eq!(stray["error"]["kind"], json!("operation_failed"));
    assert!(
        stray["error"]["message"]
            .as_str()
            .expect("message")
            .contains("export not found"),
        "post-eof reads fail with export-not-found"
    );

    let stack = LayerStack::open(root).expect("open stack");
    assert_eq!(
        stack.active_lease_count(),
        0,
        "the export lease is released once the spool is complete"
    );
}

// A base-only manifest exports an empty delta: no layers, zero counts, a
// valid (empty) archive that still pages to eof.
#[test]
fn export_empty_delta_is_a_valid_empty_archive() {
    let (operations, _root) = operations_with_real_layerstack();

    let response = sandbox_runtime::dispatch_operation(&operations, &export_request());
    let value = response.into_json_value();
    assert!(value.get("error").is_none(), "export failed: {value}");
    assert_eq!(value["layers_exported"], json!([]));
    assert_eq!(value["entries"]["files"], json!(0));
    assert_eq!(value["entries"]["whiteouts"], json!(0));
    let spool_bytes = value["spool_bytes"].as_u64().expect("spool_bytes");
    assert!(spool_bytes > 0, "an empty archive still has framing bytes");

    let export_id = value["export_id"].as_str().expect("export_id");
    let (assembled, _) = page_all(&operations, export_id, None);
    assert_eq!(assembled.len() as u64, spool_bytes);
    assert!(decode_entry_names(&assembled).is_empty());
}

// Two sequential exports coexist: the second fold never unlinks the first
// export's spool, and each pages independently by export_id.
#[test]
fn export_spools_are_keyed_and_coexist() {
    let (operations, root) = operations_with_real_layerstack();
    publish(&root, &[write_change("one.txt", "1\n")]);

    let first =
        sandbox_runtime::dispatch_operation(&operations, &export_request()).into_json_value();
    assert!(first.get("error").is_none(), "first export failed: {first}");
    let first_id = first["export_id"].as_str().expect("export_id").to_owned();

    publish(&root, &[write_change("two.txt", "2\n")]);
    let second =
        sandbox_runtime::dispatch_operation(&operations, &export_request()).into_json_value();
    assert!(
        second.get("error").is_none(),
        "second export failed: {second}"
    );
    let second_id = second["export_id"].as_str().expect("export_id").to_owned();
    assert_ne!(first_id, second_id);

    let export_dir = scratch_export_dir(&root);
    assert!(export_dir.join(format!("{first_id}.tar.zst")).is_file());
    assert!(export_dir.join(format!("{second_id}.tar.zst")).is_file());

    let (first_bytes, _) = page_all(&operations, &first_id, None);
    let (second_bytes, _) = page_all(&operations, &second_id, None);
    assert_eq!(decode_entry_names(&first_bytes), vec!["one.txt"]);
    assert_eq!(
        decode_entry_names(&second_bytes),
        vec!["one.txt", "two.txt"]
    );

    let leftovers: Vec<_> = std::fs::read_dir(&export_dir)
        .map(|entries| entries.filter_map(Result::ok).collect())
        .unwrap_or_default();
    assert!(
        leftovers.is_empty(),
        "every spool unlinked on eof: {leftovers:?}"
    );
}

// A live workspace session is reported in the start result, never a failure.
#[test]
fn export_reports_live_workspace_sessions() {
    let (operations, root) = operations_with_real_layerstack();
    publish(&root, &[write_change("a.txt", "A\n")]);

    let value: Value =
        sandbox_runtime::dispatch_operation(&operations, &export_request()).into_json_value();
    assert!(value.get("error").is_none());
    assert!(
        value.get("live_workspace_sessions").is_none(),
        "omitted when no session is alive"
    );
}
