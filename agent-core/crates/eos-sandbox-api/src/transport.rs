//! The `SandboxTransport` DIP seam (anchor §6).
//!
//! One async RPC boundary to the sandbox daemon. This crate declares the trait;
//! `eos-sandbox-host` implements the daemon-backed concrete (`DaemonSandboxTransport`)
//! and stamps the wire-level protocol version; `eos-runtime` injects it as
//! `Arc<dyn SandboxTransport>`. The `tool_api` helpers depend only on
//! `&dyn SandboxTransport`, never on a concrete client.

use async_trait::async_trait;
use eos_types::{JsonObject, SandboxId};

use crate::error::SandboxApiError;
use crate::ops::DaemonOp;

/// One sandbox RPC boundary. Implemented in `eos-sandbox-host` by the daemon
/// client and (in tests) by an in-memory mock.
///
/// Uses `#[async_trait]` because it is stored as `Arc<dyn SandboxTransport>` at
/// the composition root; it is intentionally **not** sealed (`eos-sandbox-host`
/// is an external implementor by design).
#[async_trait]
pub trait SandboxTransport: Send + Sync {
    /// Call one sandbox RPC. The implementor stamps a wire-level protocol
    /// version and reuses any `invocation_id` already present in `payload` for
    /// engine/daemon in-flight correlation.
    async fn call(
        &self,
        sandbox_id: &SandboxId,
        op: DaemonOp,
        payload: JsonObject,
        timeout_s: u32,
    ) -> Result<JsonObject, SandboxApiError>;
}

#[cfg(test)]
pub(crate) mod mock {
    //! An in-memory `SandboxTransport` returning a canned outcome, used by the
    //! `tool_api` conflict tests.

    use super::{async_trait, DaemonOp, JsonObject, SandboxApiError, SandboxId, SandboxTransport};

    pub(crate) struct MockTransport {
        outcome: Result<JsonObject, SandboxApiError>,
    }

    impl MockTransport {
        pub(crate) fn err(error: SandboxApiError) -> Self {
            Self {
                outcome: Err(error),
            }
        }
    }

    #[async_trait]
    impl SandboxTransport for MockTransport {
        async fn call(
            &self,
            _sandbox_id: &SandboxId,
            _op: DaemonOp,
            _payload: JsonObject,
            _timeout_s: u32,
        ) -> Result<JsonObject, SandboxApiError> {
            self.outcome.clone()
        }
    }
}
