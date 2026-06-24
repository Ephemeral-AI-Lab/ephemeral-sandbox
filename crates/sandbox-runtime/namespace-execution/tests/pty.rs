//! PtyMaster I/O over a real `openpt` pair — exercised on darwin with no child.

use std::io::{Read, Write};
use std::thread;
use std::time::{Duration, Instant};

include!("support/pty_src.rs");

use crate::pty::{open_pty_pair, terminate_process_group_for_test, PtyMaster};

#[test]
fn reader_drains_slave_output_into_the_transcript() {
    let _teardown_hook: fn(i32) = terminate_process_group_for_test();
    let (master, mut slave) = open_pty_pair().expect("openpt pair");
    let pty = PtyMaster::spawn(master, None, None, Box::new(|| {})).expect("pty master");

    slave.write_all(b"hello pty\n").expect("write to slave");

    assert!(wait_for_len(&pty, 10) >= 10);
}

#[test]
fn write_stdin_reaches_the_slave() {
    let (master, mut slave) = open_pty_pair().expect("openpt pair");
    let pty = PtyMaster::spawn(master, None, None, Box::new(|| {})).expect("pty master");

    pty.write_stdin(b"to-slave\n").expect("write stdin");

    let mut buf = [0_u8; 64];
    let read = slave.read(&mut buf).expect("read slave");
    assert!(String::from_utf8_lossy(&buf[..read]).contains("to-slave"));
}

#[test]
fn file_backed_reader_appends_timestamp_prefixed_transcript() {
    let dir = std::env::temp_dir().join(format!(
        "ns-exec-pty-file-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).expect("create transcript dir");
    let path = dir.join("transcript.log");
    let _ = std::fs::remove_file(&path);

    let (master, mut slave) = open_pty_pair().expect("openpt pair");
    let pty =
        PtyMaster::spawn(master, None, Some(path.clone()), Box::new(|| {})).expect("pty master");

    slave.write_all(b"file line\n").expect("write to slave");

    let contents = wait_for_file_contains(&path, "file line");
    assert_eq!(
        pty.output_len(),
        std::fs::metadata(&path).expect("transcript metadata").len()
    );
    assert!(contents.contains("file line"), "got {contents:?}");
    assert!(
        contents.starts_with('[') && contents.contains("] "),
        "expected timestamp prefix, got {contents:?}"
    );
}

fn wait_for_len(pty: &PtyMaster, len: u64) -> u64 {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let observed = pty.output_len();
        if observed >= len || Instant::now() >= deadline {
            return observed;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_file_contains(path: &std::path::Path, needle: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let contents = std::fs::read_to_string(path).unwrap_or_default();
        if contents.contains(needle) || Instant::now() >= deadline {
            return contents;
        }
        thread::sleep(Duration::from_millis(10));
    }
}
