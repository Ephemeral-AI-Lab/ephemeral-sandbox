// AC-workspace-05: the committed Pydantic schema JSON under `schemas/` is pinned
// by an insta snapshot. Editing a fixture (or the upstream Pydantic model
// without re-capturing) fails the snapshot until reviewed and accepted.

use std::fs;
use std::path::{Path, PathBuf};

fn schema_files() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("schemas");
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .expect("read schemas dir")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
        .collect();
    files.sort();
    files
}

#[test]
fn schemas_match_snapshots() {
    let files = schema_files();
    assert!(!files.is_empty(), "no schema fixtures captured");
    for path in files {
        // "message.schema.json" -> file_stem "message.schema" -> name "message".
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .expect("utf-8 file stem");
        let name = stem.strip_suffix(".schema").unwrap_or(stem).to_owned();
        let raw = fs::read_to_string(&path).expect("read schema fixture");
        let value: serde_json::Value =
            serde_json::from_str(&raw).expect("schema fixture is valid json");
        insta::assert_json_snapshot!(name, value);
    }
}
