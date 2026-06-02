//! [`ToolIntent`] — the tool-classification intent (read-only / write / lifecycle).
//!
//! `eos-tools` owns this enum (anchor §5). It shares the three values of
//! `eos_sandbox_api::Intent` (the foreground sandbox-call intent) but is a
//! distinct, locally-owned contract; the sandbox boundary converts via
//! [`From`]/[`Into`] rather than aliasing another crate's type (GC: avoids an
//! unrecorded cross-crate ownership inversion). The lifecycle-batch predicate
//! (`dispatch.rs`) and sandbox routing both read this.

use eos_sandbox_api::Intent;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// How a tool is classified for batch-dispatch policy and sandbox routing.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ToolIntent {
    /// Read-only operation (no mutations) — `Intent.READ_ONLY`.
    ReadOnly,
    /// Operation permitted to mutate the workspace — `Intent.WRITE_ALLOWED`.
    WriteAllowed,
    /// Workspace lifecycle operation — `Intent.LIFECYCLE`. Drives the
    /// lifecycle-batch policy (§6.6): a lifecycle call executes solo so later
    /// calls observe new routing state.
    Lifecycle,
}

impl ToolIntent {
    /// The wire string for this intent (the serde `snake_case` form).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ToolIntent::ReadOnly => "read_only",
            ToolIntent::WriteAllowed => "write_allowed",
            ToolIntent::Lifecycle => "lifecycle",
        }
    }
}

impl From<Intent> for ToolIntent {
    fn from(intent: Intent) -> Self {
        match intent {
            Intent::ReadOnly => ToolIntent::ReadOnly,
            Intent::WriteAllowed => ToolIntent::WriteAllowed,
            Intent::Lifecycle => ToolIntent::Lifecycle,
        }
    }
}

impl From<ToolIntent> for Intent {
    fn from(intent: ToolIntent) -> Self {
        match intent {
            ToolIntent::ReadOnly => Intent::ReadOnly,
            ToolIntent::WriteAllowed => Intent::WriteAllowed,
            ToolIntent::Lifecycle => Intent::Lifecycle,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // unwrap permitted in tests (err-no-unwrap-prod)
    use super::*;

    #[test]
    fn round_trips_with_sandbox_intent() {
        for intent in [
            ToolIntent::ReadOnly,
            ToolIntent::WriteAllowed,
            ToolIntent::Lifecycle,
        ] {
            let sandbox: Intent = intent.into();
            assert_eq!(ToolIntent::from(sandbox), intent);
            assert_eq!(intent.as_str(), sandbox.as_wire());
        }
    }

    #[test]
    fn wire_values_match_python() {
        assert_eq!(
            serde_json::to_value(ToolIntent::ReadOnly).unwrap(),
            "read_only"
        );
        assert_eq!(
            serde_json::to_value(ToolIntent::WriteAllowed).unwrap(),
            "write_allowed"
        );
        assert_eq!(
            serde_json::to_value(ToolIntent::Lifecycle).unwrap(),
            "lifecycle"
        );
    }
}
