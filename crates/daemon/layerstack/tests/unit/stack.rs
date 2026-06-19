use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use super::*;
use crate::fs::{remove_path, write_manifest};
use crate::workspace_base::build_workspace_base;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[test]
fn active_manifest_reads_wait_for_exclusive_storage_replacement() -> TestResult {
    let fixture = CommitFixture::new("read-blocks-replace")?;
    std::fs::write(fixture.workspace.join("tracked.txt"), "base\n")?;
    build_workspace_base(&fixture.root, &fixture.workspace, false)?;
    let stack = LayerStack::open(fixture.root.clone())?;
    let exclusive = stack.writer_lock.exclusive()?;
    remove_path(&fixture.root.join(ACTIVE_MANIFEST_FILE))?;

    let (version_tx, version_rx) = mpsc::channel();
    let root = fixture.root.clone();
    let reader = std::thread::spawn(move || -> TestResult {
        let version = LayerStack::open(root)?.read_active_manifest()?.version;
        version_tx.send(version)?;
        Ok(())
    });

    assert!(
        version_rx.recv_timeout(Duration::from_millis(50)).is_err(),
        "active manifest read observed transient storage state while exclusive replacement was held"
    );
    let manifest = Manifest::new(7, Vec::new(), crate::model::MANIFEST_SCHEMA_VERSION)?;
    write_manifest(fixture.root.join(ACTIVE_MANIFEST_FILE), &manifest)?;
    drop(exclusive);

    assert_eq!(version_rx.recv_timeout(Duration::from_secs(1))?, 7);
    reader
        .join()
        .map_err(|_| std::io::Error::other("reader thread panicked"))??;
    Ok(())
}

struct CommitFixture {
    root: PathBuf,
    workspace: PathBuf,
}

impl CommitFixture {
    fn new(label: &str) -> TestResult<Self> {
        let base = std::env::temp_dir().join(format!(
            "layerstack-commit-{label}-{}-{}",
            std::process::id(),
            NEXT_COMMIT_TEST.fetch_add(1, Ordering::Relaxed)
        ));
        let root = base.join("layer-stack");
        let workspace = base.join("workspace");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&workspace)?;
        Ok(Self { root, workspace })
    }
}

impl Drop for CommitFixture {
    fn drop(&mut self) {
        if let Some(base) = self.root.parent() {
            let _ = std::fs::remove_dir_all(base);
        }
    }
}

static NEXT_COMMIT_TEST: AtomicU64 = AtomicU64::new(0);
