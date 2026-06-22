use std::path::PathBuf;

use thiserror::Error;

use super::service::model::LayerStackRevision;

#[derive(Debug, Error)]
pub enum LayerStackServiceError {
    #[error("initialize layerstack service at {layer_stack_root:?}: {error}")]
    Init {
        layer_stack_root: PathBuf,
        error: String,
    },

    #[error("invalid base revision: expected {expected:?}, base {base:?}")]
    InvalidBaseRevision {
        expected: LayerStackRevision,
        base: LayerStackRevision,
    },

    #[error("publish rejected: {rejection:?}")]
    PublishRejected {
        rejection: Box<sandbox_runtime_layerstack::PublishReject>,
    },

    #[error("layerstack {operation} failed: {error}")]
    LayerStack {
        operation: &'static str,
        error: sandbox_runtime_layerstack::LayerStackError,
    },
}

impl LayerStackServiceError {
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Self::Init { .. } => "init",
            Self::InvalidBaseRevision { .. } => "invalid_base_revision",
            Self::PublishRejected { .. } => "publish_rejected",
            Self::LayerStack { operation, .. } => match *operation {
                "open" => "open",
                "publish" => "publish",
                "squash" => "squash",
                _ => "layerstack",
            },
        }
    }
}
