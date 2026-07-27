use std::fs;
use std::path::PathBuf;

use sandbox_runtime_mpla_poc::report::{seal_manifest, verify_manifest};
use sandbox_runtime_mpla_poc::{evidence, PocError};
use uuid::Uuid;

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("mpla-manifest-{}", Uuid::new_v4()));
        fs::create_dir(&path).expect("test directory");
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn manifest_covers_and_verifies_every_regular_artifact() {
    let temp = TestDirectory::new();
    evidence::write_atomic_bytes(&temp.0.join("a.json"), b"{\"a\":1}\n").expect("a");
    evidence::write_atomic_bytes(&temp.0.join("nested/b.json"), b"{\"b\":2}\n").expect("b");
    let sealed = seal_manifest(&temp.0).expect("seal");
    assert!(sealed.verified);
    assert_eq!(sealed.entries.len(), 2);
    let verified = verify_manifest(&temp.0).expect("verify");
    assert_eq!(verified.entries, sealed.entries);
}

#[test]
fn manifest_rejects_tamper_and_unreported_artifact() {
    let temp = TestDirectory::new();
    evidence::write_atomic_bytes(&temp.0.join("a"), b"original").expect("artifact");
    seal_manifest(&temp.0).expect("seal");
    evidence::write_atomic_bytes(&temp.0.join("a"), b"tampered").expect("tamper");
    assert!(matches!(
        verify_manifest(&temp.0),
        Err(PocError::Integrity(_))
    ));

    seal_manifest(&temp.0).expect("reseal");
    evidence::write_atomic_bytes(&temp.0.join("unreported"), b"new").expect("new artifact");
    assert!(matches!(
        verify_manifest(&temp.0),
        Err(PocError::Integrity(_))
    ));
}
