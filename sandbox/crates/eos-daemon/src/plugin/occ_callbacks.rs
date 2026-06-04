//! Daemon-owned plugin callbacks that publish through the shared OCC writer.
//!
//! Self-managed plugin workers may need to publish their own prepared changes.
//! This module keeps that callback on the daemon side: the plugin sends a PPC
//! request, the daemon parses the generic changeset payload, and publishing goes
//! through [`crate::occ_writer::apply_occ_changeset`], the same per-root OCC
//! service used by primary write/edit paths.

use std::path::{Path, PathBuf};

use eos_plugin::{PluginError, PpcDirection, PpcEnvelope};
use eos_protocol::{LayerChange, LayerPath};
use serde::Deserialize;
use serde_json::json;

use crate::error::DaemonError;
use crate::occ_writer::apply_occ_changeset;

pub(super) const OCC_APPLY_CHANGESET_OP: &str = "daemon.occ.apply_changeset";

pub(super) fn handle_callback_for_root(
    expected_root: &Path,
    request: PpcEnvelope,
) -> Result<PpcEnvelope, DaemonError> {
    if request.direction != PpcDirection::Request {
        return Err(PluginError::Ppc(
            "daemon plugin callback handling requires a request envelope".to_owned(),
        )
        .into());
    }
    match request.op.as_str() {
        OCC_APPLY_CHANGESET_OP => handle_apply_changeset(expected_root, request),
        op => Err(PluginError::Ppc(format!("unknown daemon plugin callback op: {op}")).into()),
    }
}

fn handle_apply_changeset(
    expected_root: &Path,
    request: PpcEnvelope,
) -> Result<PpcEnvelope, DaemonError> {
    let body: ApplyChangesetRequest = serde_json::from_str(&request.body)
        .map_err(|err| PluginError::Ppc(format!("invalid OCC callback payload: {err}")))?;
    let root = require_absolute_root(&body.layer_stack_root)?;
    if root != expected_root {
        return Err(PluginError::Ppc(format!(
            "OCC callback layer_stack_root {} did not match service root {}",
            root.display(),
            expected_root.display()
        ))
        .into());
    }
    let changes = body
        .changes
        .into_iter()
        .map(CallbackLayerChange::into_layer_change)
        .collect::<Result<Vec<_>, PluginError>>()?;
    let base_hashes = body
        .base_hashes
        .into_iter()
        .map(|base| {
            Ok((
                parse_layer_path(&base.path)?,
                empty_string_as_none(base.hash),
            ))
        })
        .collect::<Result<Vec<_>, PluginError>>()?;
    let result = apply_occ_changeset(
        &root,
        body.snapshot_version,
        &changes,
        base_hashes.as_slice(),
    )?;
    let files = result
        .files
        .iter()
        .map(|file| {
            json!({
                "path": file.path.as_str(),
                "status": occ_status_wire(file.status),
                "message": file.message,
            })
        })
        .collect::<Vec<_>>();
    let response_body = json!({
        "success": result.success(),
        "files": files,
        "published_manifest_version": result.published_manifest_version,
        "timings": result.timings,
    });
    Ok(PpcEnvelope {
        message_id: request.message_id,
        direction: PpcDirection::Reply,
        op: "reply".to_owned(),
        body: serde_json::to_string(&response_body)
            .map_err(|err| PluginError::Ppc(err.to_string()))?,
    })
}

#[derive(Debug, Deserialize)]
struct ApplyChangesetRequest {
    layer_stack_root: String,
    #[serde(default)]
    snapshot_version: Option<u64>,
    changes: Vec<CallbackLayerChange>,
    #[serde(default)]
    base_hashes: Vec<BaseHashPayload>,
}

#[derive(Debug, Deserialize)]
struct BaseHashPayload {
    path: String,
    #[serde(default)]
    hash: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum CallbackLayerChange {
    Write {
        path: String,
        #[serde(default)]
        content_utf8: Option<String>,
        #[serde(default)]
        content_bytes: Option<Vec<u8>>,
    },
    Delete {
        path: String,
    },
    Symlink {
        path: String,
        source_path: String,
    },
    OpaqueDir {
        path: String,
    },
}

impl CallbackLayerChange {
    fn into_layer_change(self) -> Result<LayerChange, PluginError> {
        match self {
            Self::Write {
                path,
                content_utf8,
                content_bytes,
            } => Ok(LayerChange::Write {
                path: parse_layer_path(&path)?,
                content: write_content(content_utf8, content_bytes)?,
            }),
            Self::Delete { path } => Ok(LayerChange::Delete {
                path: parse_layer_path(&path)?,
            }),
            Self::Symlink { path, source_path } => Ok(LayerChange::Symlink {
                path: parse_layer_path(&path)?,
                source_path,
            }),
            Self::OpaqueDir { path } => Ok(LayerChange::OpaqueDir {
                path: parse_layer_path(&path)?,
            }),
        }
    }
}

fn require_absolute_root(root: &str) -> Result<PathBuf, PluginError> {
    let root = PathBuf::from(root);
    if root.is_absolute() {
        Ok(root)
    } else {
        Err(PluginError::Ppc(
            "OCC callback layer_stack_root must be absolute".to_owned(),
        ))
    }
}

fn parse_layer_path(path: &str) -> Result<LayerPath, PluginError> {
    LayerPath::parse(path).map_err(|err| PluginError::Ppc(err.to_string()))
}

fn write_content(
    content_utf8: Option<String>,
    content_bytes: Option<Vec<u8>>,
) -> Result<Vec<u8>, PluginError> {
    match (content_utf8, content_bytes) {
        (Some(content), None) => Ok(content.into_bytes()),
        (None, Some(content)) => Ok(content),
        (Some(_), Some(_)) => Err(PluginError::Ppc(
            "write callback must provide only one of content_utf8 or content_bytes".to_owned(),
        )),
        (None, None) => Err(PluginError::Ppc(
            "write callback requires content_utf8 or content_bytes".to_owned(),
        )),
    }
}

fn empty_string_as_none(value: Option<String>) -> Option<String> {
    value.and_then(|value| if value.is_empty() { None } else { Some(value) })
}

const fn occ_status_wire(status: eos_occ::OccStatus) -> &'static str {
    match status {
        eos_occ::OccStatus::Accepted => "accepted",
        eos_occ::OccStatus::Committed => "committed",
        eos_occ::OccStatus::AbortedVersion => "aborted_version",
        eos_occ::OccStatus::AbortedOverlap => "aborted_overlap",
        eos_occ::OccStatus::Dropped => "dropped",
        eos_occ::OccStatus::Rejected => "rejected",
        _ => "failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eos_layerstack::LayerStack;
    use serde_json::Value;
    use std::error::Error;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    type TestError = Box<dyn Error + Send + Sync + 'static>;
    type TestResult = Result<(), TestError>;

    #[test]
    fn occ_callback_applies_changeset_through_daemon_writer() -> TestResult {
        let fixture = Fixture::new("apply")?;
        let reply = handle_callback_for_root(
            &fixture.root,
            PpcEnvelope {
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
                PpcEnvelope {
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
                PpcEnvelope {
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
                PpcEnvelope {
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
        result: Result<PpcEnvelope, DaemonError>,
        context: &'static str,
    ) -> Result<DaemonError, TestError> {
        match result {
            Ok(_) => Err(std::io::Error::other(context).into()),
            Err(err) => Ok(err),
        }
    }

    fn read_text(root: &Path, path: &str) -> Result<String, DaemonError> {
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
}
