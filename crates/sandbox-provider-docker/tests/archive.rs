use std::collections::HashMap;
use std::io::Read as _;
use std::path::Path;

use flate2::read::GzDecoder;

#[allow(dead_code)]
#[path = "../src/archive.rs"]
mod archive;

#[test]
fn install_archive_is_gzip_with_exact_paths_modes_and_bytes() {
    let daemon = b"daemon-payload";
    let config = b"runtime:\n  workspace: {}\n";
    let archive = archive::build_install_archive(
        Path::new("/eos/bin/sandbox-daemon"),
        daemon,
        Path::new("/eos/config/daemon.yml"),
        config,
    )
    .expect("build install archive");

    assert_eq!(&archive[..2], &[0x1f, 0x8b]);
    let files = archive_files(&archive);
    assert_eq!(
        files.get("eos/bin/sandbox-daemon"),
        Some(&(0o755, daemon.to_vec()))
    );
    assert_eq!(
        files.get("eos/config/daemon.yml"),
        Some(&(0o644, config.to_vec()))
    );
}

fn archive_files(archive: &[u8]) -> HashMap<String, (u32, Vec<u8>)> {
    let decoder = GzDecoder::new(archive);
    let mut archive = tar::Archive::new(decoder);
    let mut files = HashMap::new();
    for entry in archive.entries().expect("read archive entries") {
        let mut entry = entry.expect("read archive entry");
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry
            .path()
            .expect("read archive path")
            .to_string_lossy()
            .into_owned();
        let mode = entry.header().mode().expect("read archive mode");
        let mut contents = Vec::new();
        entry.read_to_end(&mut contents).expect("read archive file");
        files.insert(path, (mode, contents));
    }
    files
}
