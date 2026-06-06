//! `BackendStore`: open `backend.db`, run migrations, and hand out the typed
//! repositories. Also holds the shared column codecs the repositories reuse.

use std::path::Path;
use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::SqlitePool;
use time::OffsetDateTime;

use eos_types::{CoreError, UtcDateTime};

use crate::audit_cursor::AuditCursorRepo;
use crate::event_log::EventLogRepo;
use crate::obs::{ObsEventRepo, SandboxCallCorrelationRepo};
use crate::run_meta::RunMetaRepo;

/// Errors raised by the `backend.db` persistence layer.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    /// An underlying `sqlx` error (connection, query, constraint violation).
    #[error("database error")]
    Sqlx(#[from] sqlx::Error),
    /// A migration failed to apply.
    #[error("migration failed")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    /// A filesystem error creating the database's parent directory.
    #[error("filesystem error preparing the database path")]
    Io(#[from] std::io::Error),
    /// A JSON column failed to encode.
    #[error("failed to encode json column")]
    JsonEncode(#[source] serde_json::Error),
    /// A JSON column failed to decode.
    #[error("failed to decode json column")]
    JsonDecode(#[source] serde_json::Error),
    /// A TEXT column held an invalid typed id.
    #[error("invalid id in column {column}")]
    InvalidId {
        /// The offending column.
        column: &'static str,
        /// The id-parse failure.
        #[source]
        source: CoreError,
    },
    /// A TEXT column held a value outside the expected enum vocabulary.
    #[error("invalid {field} value {value:?}")]
    InvalidEnum {
        /// The domain field being parsed.
        field: &'static str,
        /// The offending raw value.
        value: String,
    },
}

/// Owns the pool and one instance of each repository. Cloning is cheap (every
/// repository holds a `SqlitePool` clone).
#[derive(Debug, Clone)]
pub struct BackendStore {
    pool: SqlitePool,
    run_meta: RunMetaRepo,
    event_log: EventLogRepo,
    obs_events: ObsEventRepo,
    correlations: SandboxCallCorrelationRepo,
    audit_cursors: AuditCursorRepo,
}

impl BackendStore {
    /// Open the `backend.db` file at `db_path`, applying WAL + busy-timeout
    /// PRAGMAs, creating the parent directory, and running migrations.
    ///
    /// # Errors
    /// [`StoreError`] on a filesystem, connection, or migration failure.
    pub async fn open(db_path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let db_path = db_path.as_ref();
        if let Some(parent) = db_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let options = SqliteConnectOptions::new()
            .filename(db_path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(Duration::from_millis(5000));
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await?;
        sqlx::migrate!().run(&pool).await?;
        Ok(Self::with_pool(pool))
    }

    fn with_pool(pool: SqlitePool) -> Self {
        Self {
            run_meta: RunMetaRepo::new(pool.clone()),
            event_log: EventLogRepo::new(pool.clone()),
            obs_events: ObsEventRepo::new(pool.clone()),
            correlations: SandboxCallCorrelationRepo::new(pool.clone()),
            audit_cursors: AuditCursorRepo::new(pool.clone()),
            pool,
        }
    }

    /// The `run_meta` repository.
    #[must_use]
    pub fn run_meta(&self) -> &RunMetaRepo {
        &self.run_meta
    }

    /// The `event_log` repository.
    #[must_use]
    pub fn event_log(&self) -> &EventLogRepo {
        &self.event_log
    }

    /// The `obs_event` repository.
    #[must_use]
    pub fn obs_events(&self) -> &ObsEventRepo {
        &self.obs_events
    }

    /// The `sandbox_call_correlation` repository.
    #[must_use]
    pub fn correlations(&self) -> &SandboxCallCorrelationRepo {
        &self.correlations
    }

    /// The `audit_cursor` repository.
    #[must_use]
    pub fn audit_cursors(&self) -> &AuditCursorRepo {
        &self.audit_cursors
    }

    /// The underlying connection pool.
    #[must_use]
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

// --- shared column codecs (reused by every repository) ---

/// Bind form for a UTC timestamp column (sqlx `time` encodes it as TEXT).
pub(crate) fn ts_in(dt: UtcDateTime) -> OffsetDateTime {
    dt.into_inner()
}

/// Decode a UTC timestamp column.
pub(crate) fn ts_out(dt: OffsetDateTime) -> UtcDateTime {
    UtcDateTime::from_offset(dt)
}

/// Parse a non-null TEXT id column into a typed id.
pub(crate) fn id_in<T>(column: &'static str, raw: String) -> Result<T, StoreError>
where
    T: TryFrom<String, Error = CoreError>,
{
    T::try_from(raw).map_err(|source| StoreError::InvalidId { column, source })
}

/// Parse a nullable TEXT id column into an optional typed id.
pub(crate) fn opt_id_in<T>(column: &'static str, raw: Option<String>) -> Result<Option<T>, StoreError>
where
    T: TryFrom<String, Error = CoreError>,
{
    raw.map(|raw| id_in(column, raw)).transpose()
}

/// Encode a JSON value to its TEXT-column form.
pub(crate) fn json_encode(value: &serde_json::Value) -> Result<String, StoreError> {
    serde_json::to_string(value).map_err(StoreError::JsonEncode)
}

/// Decode a TEXT JSON column.
pub(crate) fn json_decode(text: &str) -> Result<serde_json::Value, StoreError> {
    serde_json::from_str(text).map_err(StoreError::JsonDecode)
}
