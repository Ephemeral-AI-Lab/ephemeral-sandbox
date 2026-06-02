//! The single cross-crate error enum for the value primitives in this crate.
//!
//! Per spec-conventions §8 each crate owns exactly one `thiserror` enum.
//! `CoreError` is deliberately tiny: it covers only the two failures these
//! primitives can raise (an empty id string and a malformed RFC 3339
//! timestamp). Richer errors belong to the crate that owns the failing
//! operation.

/// Errors raised by the shared `eos-types` primitives.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CoreError {
    /// An identifier `FromStr` rejected an empty string. `kind` is the newtype
    /// name (e.g. `"TaskId"`) so callers can report which id failed.
    #[error("empty {kind} identifier")]
    EmptyId {
        /// The newtype kind whose `FromStr` received the empty string.
        kind: &'static str,
    },
    /// An RFC 3339 timestamp string failed to parse into a `UtcDateTime`.
    #[error("invalid utc timestamp")]
    Timestamp(#[from] time::error::Parse),
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // unwrap is permitted in tests (err-no-unwrap-prod)
    use super::*;

    // AC-types-05: CoreError is std::error::Error, converts time::error::Parse
    // via `?`/#[from], and Display is lowercase with no trailing punctuation.
    #[test]
    fn core_error_from_and_display() {
        fn is_std_error<E: std::error::Error>() {}
        is_std_error::<CoreError>();

        // The static template fragments are lowercase with no trailing
        // punctuation; the interpolated `kind` is a deliberate type name.
        let empty = CoreError::EmptyId { kind: "TaskId" };
        let msg = empty.to_string();
        assert_eq!(msg, "empty TaskId identifier");
        assert!(!msg.ends_with('.'));

        // A variant with no interpolation must be fully lowercase, no period.
        let ts = CoreError::Timestamp(
            time::OffsetDateTime::parse("nope", &time::format_description::well_known::Rfc3339)
                .unwrap_err(),
        );
        let ts_msg = ts.to_string();
        assert_eq!(ts_msg, "invalid utc timestamp");
        assert_eq!(ts_msg, ts_msg.to_lowercase());
        assert!(!ts_msg.ends_with('.'));

        // `?` conversion from time::error::Parse via #[from].
        fn parse(s: &str) -> Result<time::OffsetDateTime, CoreError> {
            let dt =
                time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339)?;
            Ok(dt)
        }
        let err = parse("not-a-timestamp").unwrap_err();
        assert!(matches!(err, CoreError::Timestamp(_)));
        assert!(std::error::Error::source(&err).is_some());
    }
}
