#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);

pub struct TestRoot {
    path: PathBuf,
}

impl TestRoot {
    pub fn new(label: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "eos-benchmark-{label}-{}-{nonce}-{}",
            std::process::id(),
            NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).expect("create isolated test root");
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn join(&self, path: impl AsRef<Path>) -> PathBuf {
        self.path.join(path)
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        make_tree_writable(&self.path);
        let _ = fs::remove_dir_all(&self.path);
    }
}

pub fn create_fake_repository(path: &Path) {
    fs::create_dir_all(path.join("crates")).expect("create fake repository crates directory");
    fs::create_dir_all(path.join("benchmark")).expect("create fake repository benchmark directory");
    fs::write(path.join("Cargo.toml"), b"[workspace]\n").expect("write fake repository manifest");
}

fn make_tree_writable(root: &Path) {
    let Ok(metadata) = fs::symlink_metadata(root) else {
        return;
    };
    if metadata.file_type().is_symlink() {
        return;
    }
    set_writable(root, &metadata);
    if !metadata.is_dir() {
        return;
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        make_tree_writable(&entry.path());
    }
}

#[cfg(unix)]
fn set_writable(path: &Path, metadata: &fs::Metadata) {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = metadata.permissions();
    permissions.set_mode(permissions.mode() | 0o700);
    let _ = fs::set_permissions(path, permissions);
}

#[cfg(not(unix))]
fn set_writable(path: &Path, metadata: &fs::Metadata) {
    let mut permissions = metadata.permissions();
    permissions.set_readonly(false);
    let _ = fs::set_permissions(path, permissions);
}
