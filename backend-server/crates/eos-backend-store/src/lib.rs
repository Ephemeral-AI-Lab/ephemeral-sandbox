//! `eos-backend-store` — `backend.db` persistence.
//!
//! Owns the `SQLite` pool (WAL + busy-timeout PRAGMAs), the versioned
//! `migrations/`, and one concrete repository per table: `run_meta`,
//! `event_log`, `obs_event`, `sandbox_call_correlation`, and `audit_cursor`.
//! [`BackendStore::open`] is the single constructor.
//!
//! Repositories are concrete (no trait objects): backend-server is the only
//! consumer and no alternate backend substitution is load-bearing here. The
//! shared column codecs (typed ids, JSON, UTC timestamps) live in [`db`].
#![warn(missing_docs)]

mod audit_cursor;
mod db;
mod event_log;
mod obs;
mod run_meta;

pub use audit_cursor::AuditCursorRepo;
pub use db::{BackendStore, StoreError};
pub use event_log::EventLogRepo;
pub use obs::{ObsEventRepo, SandboxCallCorrelationRepo};
pub use run_meta::RunMetaRepo;
