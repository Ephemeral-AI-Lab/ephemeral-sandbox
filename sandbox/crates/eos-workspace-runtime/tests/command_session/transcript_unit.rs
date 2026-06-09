use super::*;

fn epoch() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(0).expect("unix epoch is valid")
}

#[test]
fn formats_utc_timestamp_prefix() {
    let timezone = TranscriptTimestampTimezone::parse("UTC").expect("timezone");

    assert_eq!(
        timezone.format_prefix_at(epoch()),
        "[1970-01-01T00:00:00.000Z] "
    );
}

#[test]
fn formats_fixed_offset_timestamp_prefix() {
    let timezone = TranscriptTimestampTimezone::parse("+08:00").expect("timezone");

    assert_eq!(
        timezone.format_prefix_at(epoch()),
        "[1970-01-01T08:00:00.000+08:00] "
    );
}

#[test]
fn prefixes_each_line() {
    let mut prefixer = TranscriptTimestampPrefixer::new("UTC").expect("prefixer");

    let output = prefixer.prefix_at(b"hello\nworld", epoch());

    assert_eq!(
        String::from_utf8(output).expect("utf8"),
        "[1970-01-01T00:00:00.000Z] hello\n[1970-01-01T00:00:00.000Z] world"
    );
}

#[test]
fn preserves_line_state_across_chunks() {
    let mut prefixer = TranscriptTimestampPrefixer::new("UTC").expect("prefixer");

    let first = prefixer.prefix_at(b"hello", epoch());
    let second = prefixer.prefix_at(b"\nworld", epoch());

    assert_eq!(
        format!(
            "{}{}",
            String::from_utf8(first).expect("utf8"),
            String::from_utf8(second).expect("utf8")
        ),
        "[1970-01-01T00:00:00.000Z] hello\n[1970-01-01T00:00:00.000Z] world"
    );
}
