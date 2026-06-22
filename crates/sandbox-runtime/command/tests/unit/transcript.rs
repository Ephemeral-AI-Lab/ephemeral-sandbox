use super::*;

fn epoch() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(0).expect("unix epoch is valid")
}

#[test]
fn formats_utc_timestamp_prefix() {
    assert_eq!(
        format_timestamp_prefix_at(epoch()),
        "[1970-01-01T00:00:00.000Z] "
    );
}

#[test]
fn prefixes_each_line() {
    let mut prefixer = TranscriptTimestampPrefixer::new();

    let output = prefixer.prefix_at(b"hello\nworld", epoch());

    assert_eq!(
        String::from_utf8(output).expect("utf8"),
        "[1970-01-01T00:00:00.000Z] hello\n[1970-01-01T00:00:00.000Z] world"
    );
}

#[test]
fn preserves_line_state_across_chunks() {
    let mut prefixer = TranscriptTimestampPrefixer::new();

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

#[test]
fn transcript_reads_are_bounded_to_a_suffix() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!(
        "command-transcript-bounded-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    std::fs::create_dir_all(&root)?;
    let path = root.join("transcript.log");
    let mut bytes = b"begin\n".to_vec();
    bytes.extend(vec![b'x'; MAX_TRANSCRIPT_READ_BYTES as usize]);
    bytes.extend_from_slice(b"end\n");
    std::fs::write(&path, bytes)?;

    let output = read_transcript_since(&path, 0);

    assert!(
        output.starts_with(TRANSCRIPT_TRUNCATED_NOTICE),
        "bounded read should report skipped prefix"
    );
    assert!(
        output.len() <= TRANSCRIPT_TRUNCATED_NOTICE.len() + MAX_TRANSCRIPT_READ_BYTES as usize,
        "bounded read should not materialize the full transcript"
    );
    assert!(
        !output.contains("begin\n"),
        "bounded read should skip oldest bytes"
    );
    assert!(
        output.ends_with("end\n"),
        "bounded read should retain suffix"
    );
    let _ = std::fs::remove_dir_all(root);
    Ok(())
}

#[test]
fn full_transcript_read_preserves_prefix_and_suffix() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!(
        "command-transcript-full-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    std::fs::create_dir_all(&root)?;
    let path = root.join("transcript.log");
    let mut bytes = b"begin\n".to_vec();
    bytes.extend(vec![b'x'; MAX_TRANSCRIPT_READ_BYTES as usize]);
    bytes.extend_from_slice(b"end\n");
    std::fs::write(&path, bytes)?;

    let output = read_full_transcript_stdout(&path);

    assert!(output.starts_with("begin\n"));
    assert!(output.ends_with("end\n"));
    assert!(!output.starts_with(TRANSCRIPT_TRUNCATED_NOTICE));
    let _ = std::fs::remove_dir_all(root);
    Ok(())
}
