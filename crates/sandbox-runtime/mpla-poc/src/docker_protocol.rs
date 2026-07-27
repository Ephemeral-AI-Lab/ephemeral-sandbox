use serde::{Deserialize, Serialize};

use crate::{ArtifactStatus, QualificationReceipt};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DockerResponse {
    Qualification(Box<QualificationReceipt>),
    Failure {
        status: ArtifactStatus,
        message: String,
    },
}
