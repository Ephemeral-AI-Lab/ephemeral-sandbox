//! Command workspace policy boundary for ephemeral command sessions.

mod finalize;
mod policy;
mod prepare;
pub mod types;

pub use policy::EphemeralCommandPolicy;
