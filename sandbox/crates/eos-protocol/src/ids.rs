//! Shared identity newtypes for the sandbox wire vocabulary.
//!
//! These wrap the daemon-supplied identity strings so the workspace runtime
//! modules share one definition instead of redeclaring divergent local newtypes.
//! Each is a transparent single-field tuple struct, so serde encodes it as the
//! bare inner string and the wire shape is identical to a plain `String`.

use serde::{Deserialize, Serialize};

/// Caller identity supplied by the daemon for one operation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct CallerId(pub String);

/// Tool invocation identity supplied by the daemon for one operation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct InvocationId(pub String);

/// Stable per-workspace handle id (the isolated enter/exit key and scratch seed).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct WorkspaceHandleId(pub String);
