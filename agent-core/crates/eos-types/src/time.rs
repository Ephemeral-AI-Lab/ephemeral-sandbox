//! `UtcDateTime` timestamp wrapper and the `Clock` trait seam.
//!
//! `UtcDateTime` wraps `OffsetDateTime` with the invariant that the offset is
//! always UTC, so Anthropic/OpenAI/DB timestamps are substitutable (LSP). RFC
//! 3339 is the single wire format, matching Python's `datetime.now(UTC)`
//! isoformat persistence. `Clock` is the DIP seam: inject it instead of reading
//! the global wall clock so tests are deterministic.

use std::sync::RwLock;

use ::time::format_description::well_known::Rfc3339;
use ::time::{OffsetDateTime, UtcOffset};

/// A UTC instant. Wraps `OffsetDateTime`, guaranteeing the offset is always UTC.
#[repr(transparent)]
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
)]
#[serde(transparent)]
pub struct UtcDateTime(
    #[serde(
        serialize_with = "::time::serde::rfc3339::serialize",
        deserialize_with = "deserialize_rfc3339_utc"
    )]
    #[schemars(schema_with = "rfc3339_schema")]
    OffsetDateTime,
);

impl UtcDateTime {
    /// The current instant from the system clock, normalized to UTC.
    #[must_use]
    pub fn now() -> Self {
        Self(OffsetDateTime::now_utc())
    }

    /// Wrap an `OffsetDateTime`, normalizing any offset to UTC so the wrapper's
    /// invariant (offset == UTC) holds for every value.
    #[must_use]
    pub fn from_offset(dt: OffsetDateTime) -> Self {
        Self(dt.to_offset(UtcOffset::UTC))
    }

    /// Format as an RFC 3339 string (the canonical wire form). Infallible
    /// because the workspace `time` pin omits `large-dates`, constraining years
    /// to `0..=9999` (the RFC 3339 range).
    #[must_use]
    pub fn to_rfc3339(self) -> String {
        self.0
            .format(&Rfc3339)
            .expect("rfc3339 format is infallible without the time `large-dates` feature")
    }

    /// Parse an RFC 3339 string, normalizing the result to UTC.
    pub fn parse_rfc3339(s: &str) -> Result<Self, crate::error::CoreError> {
        Ok(Self::from_offset(OffsetDateTime::parse(s, &Rfc3339)?))
    }

    /// Consume the wrapper, returning the inner UTC `OffsetDateTime`.
    #[must_use]
    pub fn into_inner(self) -> OffsetDateTime {
        self.0
    }
}

/// Schema override for the inner timestamp field: a JSON `string` with
/// `format: date-time`. Needed because the field serializes as an RFC 3339
/// string and `schemars` has no `time` integration to infer that.
fn rfc3339_schema(_gen: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
    schemars::schema::SchemaObject {
        instance_type: Some(schemars::schema::InstanceType::String.into()),
        format: Some("date-time".to_owned()),
        ..Default::default()
    }
    .into()
}

/// Deserialize an RFC 3339 timestamp and normalize it to UTC, preserving the
/// `UtcDateTime` invariant (offset == UTC) on the wire-input path. The default
/// `time::serde::rfc3339` deserialize keeps the encoded offset, so a value like
/// `...+02:00` would otherwise survive non-normalized.
fn deserialize_rfc3339_utc<'de, D>(deserializer: D) -> Result<OffsetDateTime, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let dt = ::time::serde::rfc3339::deserialize(deserializer)?;
    Ok(dt.to_offset(UtcOffset::UTC))
}

/// Source of the current wall-clock instant. Inject instead of calling the
/// global clock so tests are deterministic (`test-mock-traits`).
pub trait Clock: Send + Sync {
    /// Current instant, normalized to UTC.
    fn now(&self) -> UtcDateTime;
}

/// Production clock backed by the system wall clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> UtcDateTime {
        UtcDateTime::now()
    }
}

/// Test clock with a settable instant for deterministic tests. Reads dominate,
/// so the instant lives behind an `RwLock` (`own-rwlock-readers`).
#[derive(Debug)]
pub struct TestClock {
    instant: RwLock<UtcDateTime>,
}

impl TestClock {
    /// Create a test clock fixed at `instant`.
    #[must_use]
    pub fn new(instant: UtcDateTime) -> Self {
        Self {
            instant: RwLock::new(instant),
        }
    }

    /// Overwrite the instant returned by [`Clock::now`].
    pub fn set(&self, instant: UtcDateTime) {
        *self.instant.write().expect("test clock lock not poisoned") = instant;
    }
}

impl Clock for TestClock {
    fn now(&self) -> UtcDateTime {
        // No `.await` in this crate, so the guard never spans one; copy the
        // `Copy` value out and drop the guard before returning.
        *self.instant.read().expect("test clock lock not poisoned")
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // unwrap is permitted in tests (err-no-unwrap-prod)
    use super::*;
    use std::sync::Arc;
    use std::thread;

    // AC-types-03: RFC 3339 roundtrip + UTC normalization.
    #[test]
    fn utc_datetime_rfc3339_roundtrip() {
        let s = "2026-06-02T19:47:00Z";
        let dt = UtcDateTime::parse_rfc3339(s).expect("parse");
        // `time` emits the UTC offset as `Z`; Python isoformat uses `+00:00`.
        // Both are valid RFC 3339 and parse to the same instant.
        let formatted = dt.to_rfc3339();
        let reparsed = UtcDateTime::parse_rfc3339(&formatted).expect("reparse");
        assert_eq!(dt, reparsed);

        // A non-UTC offset is normalized to UTC on construction.
        let plus2 = OffsetDateTime::parse("2026-06-02T21:47:00+02:00", &Rfc3339).unwrap();
        let normalized = UtcDateTime::from_offset(plus2);
        assert_eq!(normalized.into_inner().offset(), UtcOffset::UTC);
        // Same instant as the `Z` value above.
        assert_eq!(normalized, dt);
    }

    // AC-types-04: Clock injection is deterministic and thread-shareable.
    #[test]
    fn test_clock_is_settable() {
        let t0 = UtcDateTime::parse_rfc3339("2020-01-01T00:00:00Z").unwrap();
        let t1 = UtcDateTime::parse_rfc3339("2030-12-31T23:59:59Z").unwrap();
        let clock: Arc<dyn Clock> = Arc::new(TestClock::new(t0));
        assert_eq!(clock.now(), t0);

        // Shared across threads, the same handle yields identical reads.
        let a = Arc::clone(&clock);
        let b = Arc::clone(&clock);
        let ha = thread::spawn(move || a.now());
        let hb = thread::spawn(move || b.now());
        assert_eq!(ha.join().unwrap(), hb.join().unwrap());

        // Downcast not needed: set through the concrete handle.
        let concrete = TestClock::new(t0);
        concrete.set(t1);
        assert_eq!(concrete.now(), t1);
    }

    // AC-types-06 (timestamp portion): UtcDateTime schemas as string/date-time.
    #[test]
    fn json_schema_utc_datetime_is_date_time_string() {
        let schema = serde_json::to_value(schemars::schema_for!(UtcDateTime)).unwrap();
        assert_eq!(schema["type"], serde_json::json!("string"));
        assert_eq!(schema["format"], serde_json::json!("date-time"));
    }

    // The serde wire form is a bare RFC 3339 string (transparent). The exact
    // bytes are the canonical UTC `Z` form; full byte-parity with Python's
    // variable-precision `+00:00` isoformat is a cutover/Phase-0-harness concern.
    #[test]
    fn serde_is_transparent_rfc3339_string() {
        let dt = UtcDateTime::parse_rfc3339("2026-06-02T19:47:00Z").unwrap();
        let value = serde_json::to_value(dt).unwrap();
        assert_eq!(value, serde_json::json!("2026-06-02T19:47:00Z"));
        let back: UtcDateTime = serde_json::from_value(value).unwrap();
        assert_eq!(back, dt);
    }

    // Blocker fix: the transparent deserialize path must normalize a non-UTC
    // offset to UTC, preserving the type's invariant (spec §8).
    #[test]
    fn deserialize_normalizes_non_utc_offset() {
        let dt: UtcDateTime =
            serde_json::from_value(serde_json::json!("2026-06-02T21:47:00+02:00")).unwrap();
        assert_eq!(dt.into_inner().offset(), UtcOffset::UTC);
        let z: UtcDateTime =
            serde_json::from_value(serde_json::json!("2026-06-02T19:47:00Z")).unwrap();
        assert_eq!(dt, z);
        // Re-serialization emits the canonical UTC (`Z`) form, not `+02:00`.
        assert_eq!(
            serde_json::to_value(dt).unwrap(),
            serde_json::json!("2026-06-02T19:47:00Z")
        );
    }
}
