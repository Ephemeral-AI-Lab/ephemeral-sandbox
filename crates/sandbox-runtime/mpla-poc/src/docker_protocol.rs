use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{evidence, ArtifactStatus, PocResult, QualificationReceipt};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DockerResponse {
    Qualification(Box<QualificationReceipt>),
    Failure {
        status: ArtifactStatus,
        message: String,
    },
}

impl DockerResponse {
    pub fn failed(message: impl Into<String>) -> Self {
        Self::Failure {
            status: ArtifactStatus::Failed,
            message: message.into(),
        }
    }

    pub fn cancelled(message: impl Into<String>) -> Self {
        Self::Failure {
            status: ArtifactStatus::Cancelled,
            message: message.into(),
        }
    }

    pub fn incomplete(message: impl Into<String>) -> Self {
        Self::Failure {
            status: ArtifactStatus::Incomplete,
            message: message.into(),
        }
    }

    pub fn status(&self) -> ArtifactStatus {
        match self {
            Self::Qualification(receipt) => receipt.status,
            Self::Failure { status, .. } => *status,
        }
    }

    pub fn write_atomic(&self, path: &Path) -> PocResult<()> {
        evidence::write_atomic_json(path, self)
    }

    pub fn encode_line(&self) -> PocResult<Vec<u8>> {
        let mut encoded = serde_json::to_vec(self)?;
        encoded.push(b'\n');
        Ok(encoded)
    }

    pub fn decode_line(line: &[u8]) -> PocResult<Self> {
        serde_json::from_slice(line).map_err(Into::into)
    }
}
