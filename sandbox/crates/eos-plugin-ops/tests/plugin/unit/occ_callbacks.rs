use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use eos_layerstack::LayerStack;
use serde_json::Value;

use super::*;

type TestError = Box<dyn Error + Send + Sync + 'static>;
type TestResult = Result<(), TestError>;

#[test]
fn occ_callback_applies_changeset_through_daemon_writer() -> TestResult {
    let fixture = Fixture::new("apply")?;
    let reply = handle_callback_for_root(
        &fixture.root,
        PpcMessage {
            message_id: "callback-1".to_owned(),
            direction: PpcDirection::Request,
            op: OCC_APPLY_CHANGESET_OP.to_owned(),
            body: serde_json::to_string(&json!({
                "layer_stack_root": fixture.root,
                "snapshot_version": null,
                "changes": [{
                    "kind": "write",
                    "path": "src/main.py",
                    "content_utf8": "print('ok')\n"
                }]
            }))?,
        },
    )?;

    assert_eq!(reply.message_id, "callback-1");
    assert_eq!(reply.direction, PpcDirection::Reply);
    let body: Value = serde_json::from_str(&reply.body)?;
    assert_eq!(body["success"], true);
    assert_eq!(body["files"][0]["path"], "src/main.py");
    assert_eq!(body["files"][0]["status"], "committed");
    assert_eq!(read_text(&fixture.root, "src/main.py")?, "print('ok')\n");
    Ok(())
}

#[test]
fn occ_callback_rejects_unknown_callback_op() -> TestResult {
    let fixture = Fixture::new("unknown")?;
    let err = callback_error(
        handle_callback_for_root(
            &fixture.root,
            PpcMessage {
                message_id: "callback-unknown".to_owned(),
                direction: PpcDirection::Request,
                op: "daemon.unknown".to_owned(),
                body: "{}".to_owned(),
            },
        ),
        "unknown callback unexpectedly succeeded",
    )?;

    assert!(err
        .to_string()
        .contains("unknown daemon plugin callback op"));
    Ok(())
}

#[test]
fn occ_callback_rejects_ambiguous_write_content() -> TestResult {
    let fixture = Fixture::new("ambiguous")?;
    let err = callback_error(
        handle_callback_for_root(
            &fixture.root,
            PpcMessage {
                message_id: "callback-bad-content".to_owned(),
                direction: PpcDirection::Request,
                op: OCC_APPLY_CHANGESET_OP.to_owned(),
                body: serde_json::to_string(&json!({
                    "layer_stack_root": fixture.root,
                    "changes": [{
                        "kind": "write",
                        "path": "src/main.py",
                        "content_utf8": "text",
                        "content_bytes": [116, 101, 120, 116]
                    }]
                }))?,
            },
        ),
        "ambiguous write content unexpectedly succeeded",
    )?;

    assert!(err.to_string().contains("only one of content_utf8"));
    Ok(())
}

#[test]
fn occ_callback_rejects_wrong_layer_stack_root() -> TestResult {
    let fixture = Fixture::new("expected")?;
    let other = Fixture::new("other")?;
    let err = callback_error(
        handle_callback_for_root(
            &fixture.root,
            PpcMessage {
                message_id: "callback-wrong-root".to_owned(),
                direction: PpcDirection::Request,
                op: OCC_APPLY_CHANGESET_OP.to_owned(),
                body: serde_json::to_string(&json!({
                    "layer_stack_root": other.root,
                    "changes": [{
                        "kind": "write",
                        "path": "src/main.py",
                        "content_utf8": "text"
                    }]
                }))?,
            },
        ),
        "wrong layer stack root unexpectedly succeeded",
    )?;

    assert!(err.to_string().contains("did not match service root"));
    Ok(())
}

fn callback_error(
    result: Result<PpcMessage, PluginRuntimeError>,
    context: &'static str,
) -> Result<PluginRuntimeError, TestError> {
    match result {
        Ok(_) => Err(std::io::Error::other(context).into()),
        Err(err) => Ok(err),
    }
}

fn read_text(root: &Path, path: &str) -> Result<String, PluginRuntimeError> {
    Ok(LayerStack::open(root.to_path_buf())?.read_text(path)?.0)
}

struct Fixture {
    base: PathBuf,
    root: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Result<Self, std::io::Error> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let base = std::env::temp_dir().join(format!(
            "eos-plugin-occ-callback-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("layer-stack");
        std::fs::create_dir_all(&root)?;
        Ok(Self { base, root })
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}
