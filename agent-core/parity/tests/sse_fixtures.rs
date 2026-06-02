// AC-workspace-06: the SSE fixture corpus exists, is non-empty, and parses as
// `event:` / `data:` framed UTF-8. Replay assertions against these byte streams
// are an eos-llm-client (Phase 2) obligation; here we only guard their presence
// and framing.

use std::fs;
use std::path::{Path, PathBuf};

fn sse_files(provider: &str) -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("sse")
        .join(provider);
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|_| panic!("read sse/{provider} dir"))
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "sse"))
        .collect();
    files.sort();
    files
}

fn assert_framed(path: &Path) {
    // read_to_string fails on non-UTF-8, which is the UTF-8 guard.
    let content = fs::read_to_string(path).expect("sse fixture is utf-8");
    assert!(
        !content.trim().is_empty(),
        "empty sse fixture: {}",
        path.display()
    );
    let mut data_lines = 0usize;
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let framed = line.starts_with("event:")
            || line.starts_with("data:")
            || line.starts_with("id:")
            || line.starts_with("retry:")
            || line.starts_with(':');
        assert!(framed, "unframed sse line in {}: {line:?}", path.display());
        if line.starts_with("data:") {
            data_lines += 1;
        }
    }
    assert!(data_lines > 0, "no data: frames in {}", path.display());
}

#[test]
fn anthropic_fixtures_are_framed() {
    let files = sse_files("anthropic");
    assert!(!files.is_empty(), "no anthropic sse fixtures");
    for path in &files {
        assert_framed(path);
    }
}

#[test]
fn openai_fixtures_are_framed() {
    let files = sse_files("openai");
    assert!(!files.is_empty(), "no openai sse fixtures");
    for path in &files {
        assert_framed(path);
    }
}
