//! Test helper for recognizing daemon-reported CAS hashes by shape.

/// A daemon-reported hash looks like a 64-char lowercase hex SHA-256.
#[must_use]
pub fn looks_like_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
