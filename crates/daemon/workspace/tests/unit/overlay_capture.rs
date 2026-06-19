use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use layerstack::{LayerChange, LayerPath};
use workspace::overlay::capture::capture_upperdir;
use workspace::{ProtectedPathDrop, ProtectedPathDropReason};

type TestResult<T = ()> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[cfg(unix)]
#[test]
fn captures_upperdir_files_whiteouts_symlinks_and_opaque_markers() -> TestResult {
    let fixture = Fixture::new("capture_upperdir")?;
    std::fs::create_dir_all(fixture.base.join("dir"))?;
    std::fs::write(fixture.base.join("dir/file.txt"), b"hello")?;
    std::fs::write(fixture.base.join(".wh.old.txt"), b"")?;
    std::fs::write(fixture.base.join("dir/.wh..wh..opq"), b"")?;
    std::os::unix::fs::symlink("../target", fixture.base.join("link"))?;

    let captured = capture_upperdir(&fixture.base)?;

    assert!(captured.changes.contains(&LayerChange::Write {
        path: LayerPath::parse("dir/file.txt")?,
        content: b"hello".to_vec(),
    }));
    assert!(captured.changes.contains(&LayerChange::Delete {
        path: LayerPath::parse("old.txt")?,
    }));
    assert!(captured.changes.contains(&LayerChange::Symlink {
        path: LayerPath::parse("link")?,
        source_path: "../target".to_owned(),
    }));
    assert!(captured.changes.contains(&LayerChange::OpaqueDir {
        path: LayerPath::parse("dir")?,
    }));
    Ok(())
}

#[cfg(unix)]
#[test]
fn captures_unsupported_special_files_as_workspace_protected_drops() -> TestResult {
    let fixture = Fixture::new("capture_unsupported_special_file")?;
    let fifo_path = fixture.base.join("run.fifo");
    let status = std::process::Command::new("mkfifo")
        .arg(&fifo_path)
        .status()?;
    assert!(status.success(), "mkfifo failed with status {status}");
    std::fs::write(fixture.base.join("file.txt"), b"regular")?;

    let captured = capture_upperdir(&fixture.base)?;

    assert!(captured.changes.contains(&LayerChange::Write {
        path: LayerPath::parse("file.txt")?,
        content: b"regular".to_vec(),
    }));
    assert!(
        captured
            .changes
            .iter()
            .all(|change| change.path().as_str() != "run.fifo"),
        "unsupported FIFO must not become a layer payload"
    );
    assert_eq!(
        captured.protected_drops,
        vec![ProtectedPathDrop {
            path: "run.fifo".to_owned(),
            reason: ProtectedPathDropReason::UnsupportedSpecialFile,
        }]
    );
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn captures_non_utf8_layer_paths_as_invalid_layer_path_drops() -> TestResult {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let fixture = Fixture::new("capture_non_utf8_layer_path")?;
    let bad_name = OsString::from_vec(vec![b'b', 0xff, b'd']);
    std::fs::write(fixture.base.join(bad_name), b"invalid")?;
    std::fs::write(fixture.base.join("file.txt"), b"regular")?;

    let captured = capture_upperdir(&fixture.base)?;

    assert_eq!(
        captured.changes,
        vec![LayerChange::Write {
            path: LayerPath::parse("file.txt")?,
            content: b"regular".to_vec(),
        }]
    );
    assert_eq!(captured.protected_drops.len(), 1);
    assert_eq!(
        captured.protected_drops[0].reason,
        ProtectedPathDropReason::InvalidLayerPath
    );
    assert!(
        captured.protected_drops[0]
            .path
            .starts_with(".invalid-layer-path/"),
        "invalid layer path drops use a stable representable placeholder: {:?}",
        captured.protected_drops[0]
    );
    Ok(())
}

struct Fixture {
    base: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> TestResult<Self> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let base = std::env::temp_dir().join(format!(
            "workspace-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base)?;
        Ok(Self { base })
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}
