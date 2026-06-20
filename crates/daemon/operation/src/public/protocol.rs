//! Public daemon operation protocol boundary.
//!
//! Operation families define their domain-specific payloads locally. The
//! generic request/response carrier is shared through `sandbox_protocol`.

pub use sandbox_protocol::{
    OwnedRequest, Request as OperationRequest, Response as OperationResponse,
};
