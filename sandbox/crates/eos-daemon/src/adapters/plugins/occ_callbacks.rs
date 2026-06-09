//! Daemon-owned plugin callbacks that publish through the shared OCC writer.
//!
//! Self-managed plugin workers may need to publish their own prepared changes.
//! This module keeps that callback on the daemon side: the plugin sends a PPC
//! request, the daemon parses the generic changeset payload, and publishing goes
//! through [`crate::adapters::occ::apply_occ_changeset`], the same per-root OCC
//! service used by primary write/edit paths.

use std::path::{Path, PathBuf};

use eos_plugin::{PluginError, PpcDirection, PpcEnvelope};
use eos_protocol::{LayerChange, LayerPath};
use serde::Deserialize;
use serde_json::json;

use crate::adapters::occ::apply_occ_changeset;
use crate::error::DaemonError;

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
                "status": file.status.wire_str(),
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

#[cfg(test)]
#[path = "../../../tests/plugin/occ_callbacks.rs"]
mod tests;
