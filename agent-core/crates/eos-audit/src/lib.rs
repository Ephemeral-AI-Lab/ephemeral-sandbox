//! `eos-audit` — the agent-core write-only audit side channel.
//!
//! This crate owns the structured event envelope ([`AuditEvent`] +
//! correlation [`AuditNode`]), the [`AuditSink`] seam, and the append-only
//! `JSONL` writers ([`JsonlSink`] and [`BufferedJsonlSink`] +
//! [`BufferedAuditShutdown`]).
//!
//! It depends only on `eos-types`. It does **not** own lifecycle policy (when
//! events fire is producer/engine policy), does not import any downstream
//! crate's stream types, and does no buffering/lane-routing beyond the single
//! bounded writer thread.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod error;
mod event;
mod jsonl;
mod node;
mod sink;

pub use error::AuditError;
pub use event::{AuditEvent, AuditSource, SCHEMA_VERSION};
pub use jsonl::{BufferedAuditShutdown, BufferedJsonlSink, JsonlSink};
pub use node::{AuditNode, AuditNodeBuilder};
pub use sink::{AuditSink, NoopAuditSink};
