//! Emit-stream unit suite: tar-zst spool round-trip, logical whiteout/opaque
//! encodings, mode + second-granular mtime fidelity, deterministic order,
//! and the empty-delta archive.

use std::collections::BTreeMap;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::stack::{emit_delta_stream, DeltaStreamStats, DeltaWinner};
use crate::LayerPath;

use super::test_fixture::unique_suffix;

const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];

struct StreamFixture {
    base: PathBuf,
}

impl StreamFixture {
    fn new(label: &str) -> Self {
        let base =
            std::env::temp_dir().join(format!("layerstack-emit-{label}-{}", unique_suffix()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("create fixture base");
        Self { base }
    }

    fn spool(&self) -> PathBuf {
        self.base.join("spool.tar.zst")
    }
}

impl Drop for StreamFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

struct DecodedEntry {
    name: String,
    kind: tar::EntryType,
    mode: u32,
    mtime: u64,
    size: u64,
    content: Vec<u8>,
    link_target: Option<String>,
}

fn decode_spool(spool: &Path) -> Vec<DecodedEntry> {
    let file = std::fs::File::open(spool).expect("open spool");
    let decoder = zstd::stream::read::Decoder::new(file).expect("zstd decoder");
    let mut archive = tar::Archive::new(decoder);
    let mut decoded = Vec::new();
    for entry in archive.entries().expect("entries") {
        let mut entry = entry.expect("entry");
        let name = String::from_utf8(entry.path_bytes().into_owned()).expect("utf8 name");
        let header = entry.header();
        let kind = header.entry_type();
        let mode = header.mode().expect("mode");
        let mtime = header.mtime().expect("mtime");
        let size = header.size().expect("size");
        let link_target = header
            .link_name()
            .expect("link name")
            .map(|target| target.to_string_lossy().into_owned());
        let mut content = Vec::new();
        entry.read_to_end(&mut content).expect("content");
        decoded.push(DecodedEntry {
            name,
            kind,
            mode,
            mtime,
            size,
            content,
            link_target,
        });
    }
    decoded
}

fn path(p: &str) -> LayerPath {
    LayerPath::parse(p).expect("layer path")
}

#[test]
fn emit_round_trips_winners_with_mode_and_second_mtime() {
    let fixture = StreamFixture::new("round-trip");
    let source_dir = fixture.base.join("layer/src");
    std::fs::create_dir_all(&source_dir).expect("mkdir");
    std::fs::set_permissions(&source_dir, std::fs::Permissions::from_mode(0o700))
        .expect("dir mode");
    let source_file = source_dir.join("a.rs");
    std::fs::write(&source_file, "v2\n").expect("write");
    std::fs::set_permissions(&source_file, std::fs::Permissions::from_mode(0o640))
        .expect("file mode");
    let stamp = SystemTime::UNIX_EPOCH + Duration::from_secs(1_750_000_123);
    let handle = std::fs::File::options()
        .write(true)
        .open(&source_file)
        .expect("open for times");
    handle
        .set_times(std::fs::FileTimes::new().set_modified(stamp))
        .expect("set mtime");
    drop(handle);
    let link = fixture.base.join("layer/link.md");
    std::os::unix::fs::symlink("src/a.rs", &link).expect("symlink");

    let mut winners = BTreeMap::new();
    winners.insert(
        path("src"),
        DeltaWinner::Directory {
            source: source_dir.clone(),
        },
    );
    winners.insert(
        path("src/a.rs"),
        DeltaWinner::File {
            source: source_file.clone(),
        },
    );
    winners.insert(path("link.md"), DeltaWinner::Symlink { source: link });

    let stats = emit_delta_stream(&winners, &fixture.spool(), 3).expect("emit");
    assert_eq!(stats.files, 1);
    assert_eq!(stats.symlinks, 1);
    assert_eq!(stats.whiteouts, 0);
    assert_eq!(stats.opaques, 0);

    let mut magic = [0_u8; 4];
    std::fs::File::open(fixture.spool())
        .expect("open spool")
        .read_exact(&mut magic)
        .expect("magic");
    assert_eq!(magic, ZSTD_MAGIC, "spool is zstd-framed");

    let entries = decode_spool(&fixture.spool());
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        vec!["link.md", "src/", "src/a.rs"],
        "deterministic winner-map order; directories carry a trailing slash"
    );
    let link_entry = &entries[0];
    assert_eq!(link_entry.kind, tar::EntryType::Symlink);
    assert_eq!(link_entry.link_target.as_deref(), Some("src/a.rs"));
    let dir_entry = &entries[1];
    assert_eq!(dir_entry.kind, tar::EntryType::Directory);
    assert_eq!(dir_entry.mode, 0o700);
    let file_entry = &entries[2];
    assert_eq!(file_entry.kind, tar::EntryType::Regular);
    assert_eq!(file_entry.mode, 0o640);
    assert_eq!(file_entry.mtime, 1_750_000_123);
    assert_eq!(file_entry.size, 3);
    assert_eq!(file_entry.content, b"v2\n");
}

#[test]
fn emit_encodes_deletions_and_opaques_logically() {
    let fixture = StreamFixture::new("markers");
    let cfg_dir = fixture.base.join("layer/cfg");
    std::fs::create_dir_all(&cfg_dir).expect("mkdir");

    let mut winners = BTreeMap::new();
    winners.insert(path("cfg"), DeltaWinner::OpaqueDir { source: cfg_dir });
    winners.insert(path("src/b.rs"), DeltaWinner::Delete);
    winners.insert(path("top.txt"), DeltaWinner::Delete);

    let stats = emit_delta_stream(&winners, &fixture.spool(), 3).expect("emit");
    assert_eq!(stats.whiteouts, 2);
    assert_eq!(stats.opaques, 1);
    assert_eq!(stats.files, 0);

    let entries = decode_spool(&fixture.spool());
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        vec!["cfg/", "cfg/.wh..wh..opq", "src/.wh.b.rs", ".wh.top.txt"],
        "logical OCI encodings, never kernel whiteouts"
    );
    for marker in &entries[1..] {
        assert_eq!(marker.kind, tar::EntryType::Regular);
        assert_eq!(marker.size, 0);
        assert_eq!(marker.mtime, 0);
    }
}

#[test]
fn emit_empty_winner_map_is_a_valid_empty_archive() {
    let fixture = StreamFixture::new("empty");
    let winners = BTreeMap::new();

    let stats: DeltaStreamStats = emit_delta_stream(&winners, &fixture.spool(), 3).expect("emit");
    assert_eq!(stats, DeltaStreamStats::default());

    let spool_bytes = std::fs::metadata(fixture.spool())
        .expect("spool meta")
        .len();
    assert!(
        spool_bytes > 0,
        "an empty archive still has tar EOF framing"
    );
    assert!(decode_spool(&fixture.spool()).is_empty());
}
