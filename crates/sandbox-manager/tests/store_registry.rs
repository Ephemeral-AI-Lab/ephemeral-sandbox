use std::path::{Path, PathBuf};

use sandbox_manager::{
    ManagerError, SandboxDaemonEndpoint, SandboxHttpEndpoint, SandboxId, SandboxRecord,
    SandboxState, SandboxStore, SharedBaseMount,
};

struct RegistryDir {
    root: PathBuf,
}

impl RegistryDir {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "sandbox-store-registry-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time after epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("create registry dir");
        Self { root }
    }

    fn snapshot_path(&self) -> PathBuf {
        self.root.join("registry").join("sandboxes.json")
    }
}

impl Drop for RegistryDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn id(value: &str) -> SandboxId {
    SandboxId::new(value).expect("valid sandbox id")
}

fn ready_record(name: &str, port: u16) -> SandboxRecord {
    let mut record = SandboxRecord::new(
        id(name),
        PathBuf::from("/tmp/workspaces").join(name),
        SandboxState::Ready,
    );
    record.daemon = Some(SandboxDaemonEndpoint::new("127.0.0.1", port, "token"));
    record.daemon_http = Some(SandboxHttpEndpoint::new("127.0.0.1", port + 1));
    record.shared_base = Some(SharedBaseMount {
        source: PathBuf::from("/tmp/shared-base"),
        target: PathBuf::from("/workspace/base"),
        root_hash: "hash".to_owned(),
        readonly: true,
    });
    record
}

fn load(path: &Path) -> SandboxStore {
    SandboxStore::load(path.to_path_buf()).expect("load registry")
}

#[test]
fn ready_ids_are_filtered_and_sorted() {
    let store = SandboxStore::new();
    store
        .insert(ready_record("zulu", 7100))
        .expect("insert zulu");
    store
        .create(id("creating"), PathBuf::from("/tmp/workspaces/creating"))
        .expect("create non-ready sandbox");
    store
        .insert(ready_record("alpha", 7200))
        .expect("insert alpha");

    assert_eq!(
        store.ready_ids().expect("list ready ids"),
        vec![id("alpha"), id("zulu")]
    );
    assert!(store.is_ready(&id("zulu")).expect("inspect ready state"));
    assert!(!store
        .is_ready(&id("creating"))
        .expect("inspect creating state"));
    assert!(store.is_ready(&id("missing")).is_err());
}

#[test]
fn mutations_survive_reload() {
    let dir = RegistryDir::new("reload");
    let path = dir.snapshot_path();

    let store = load(&path);
    store
        .create(id("alpha"), PathBuf::from("/tmp/workspaces/alpha"))
        .expect("create alpha");
    store
        .update_endpoints(
            &id("alpha"),
            Some(SandboxDaemonEndpoint::new("127.0.0.1", 7100, "token")),
            Some(SandboxHttpEndpoint::new("127.0.0.1", 7101)),
        )
        .expect("update endpoints");
    store
        .transition_state(&id("alpha"), SandboxState::Creating, SandboxState::Ready)
        .expect("transition to ready");
    store
        .insert(ready_record("beta", 7200))
        .expect("insert beta");
    drop(store);

    let reloaded = load(&path);
    let records = reloaded.list().expect("list records");
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].id, id("alpha"));
    assert_eq!(records[0].state, SandboxState::Ready);
    assert_eq!(
        records[0].daemon,
        Some(SandboxDaemonEndpoint::new("127.0.0.1", 7100, "token"))
    );
    assert_eq!(records[1], ready_record("beta", 7200));
}

#[test]
fn activity_revision_is_monotonic_and_survives_reload() {
    let dir = RegistryDir::new("activity-revision");
    let path = dir.snapshot_path();
    let store = load(&path);
    store
        .insert(ready_record("alpha", 7100))
        .expect("insert alpha");

    assert_eq!(
        store
            .advance_activity_revision(&id("alpha"))
            .expect("first revision")
            .activity_revision,
        1
    );
    assert_eq!(
        store
            .advance_activity_revision(&id("alpha"))
            .expect("second revision")
            .activity_revision,
        2
    );
    drop(store);

    assert_eq!(
        load(&path)
            .inspect(&id("alpha"))
            .expect("reloaded alpha")
            .activity_revision,
        2
    );
}

#[test]
fn registry_without_activity_revision_loads_as_revision_zero() {
    let dir = RegistryDir::new("legacy-activity-revision");
    let path = dir.snapshot_path();
    std::fs::create_dir_all(path.parent().expect("snapshot parent")).expect("create parent");
    std::fs::write(
        &path,
        br#"[{"id":"legacy","workspace_root":"/tmp/ws","state":"ready","daemon":null,"daemon_http":null,"shared_base":null}]"#,
    )
    .expect("write legacy snapshot");

    let store = load(&path);
    assert_eq!(
        store
            .inspect(&id("legacy"))
            .expect("legacy record")
            .activity_revision,
        0
    );
    assert_eq!(
        store
            .advance_activity_revision(&id("legacy"))
            .expect("advance migrated revision")
            .activity_revision,
        1
    );
}

#[test]
fn remove_is_persisted() {
    let dir = RegistryDir::new("remove");
    let path = dir.snapshot_path();

    let store = load(&path);
    store
        .insert(ready_record("alpha", 7100))
        .expect("insert alpha");
    store
        .insert(ready_record("beta", 7200))
        .expect("insert beta");
    store.remove(&id("alpha")).expect("remove alpha");
    drop(store);

    let records = load(&path).list().expect("list records");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].id, id("beta"));
}

#[test]
fn reconcile_adopts_runtime_records_and_marks_orphans_failed() {
    let dir = RegistryDir::new("reconcile");
    let path = dir.snapshot_path();

    let store = load(&path);
    store
        .insert(ready_record("survivor", 7100))
        .expect("insert survivor");
    store
        .insert(ready_record("orphan", 7200))
        .expect("insert orphan");
    drop(store);

    // The runtime reports the survivor on a new port plus a container the
    // registry has never seen; the orphan's container is gone.
    let reloaded = load(&path);
    let orphaned = reloaded
        .reconcile(vec![
            ready_record("survivor", 7300),
            ready_record("adopted", 7400),
        ])
        .expect("reconcile");
    assert_eq!(orphaned, vec![id("orphan")]);

    let records = load(&path).list().expect("list records");
    assert_eq!(records.len(), 3);
    assert_eq!(records[0], ready_record("adopted", 7400));
    assert_eq!(records[1].id, id("orphan"));
    assert_eq!(records[1].state, SandboxState::Failed);
    assert_eq!(records[2], ready_record("survivor", 7300));
}

#[test]
fn reconcile_reports_already_failed_orphans_only_once() {
    let dir = RegistryDir::new("refail");
    let path = dir.snapshot_path();

    let store = load(&path);
    store
        .insert(ready_record("orphan", 7100))
        .expect("insert orphan");
    let first = store.reconcile(Vec::new()).expect("first reconcile");
    assert_eq!(first, vec![id("orphan")]);
    let second = store.reconcile(Vec::new()).expect("second reconcile");
    assert!(second.is_empty());
}

#[test]
fn load_rejects_corrupt_snapshot() {
    let dir = RegistryDir::new("corrupt");
    let path = dir.snapshot_path();
    std::fs::create_dir_all(path.parent().expect("snapshot parent")).expect("create parent");
    std::fs::write(&path, b"not json").expect("write corrupt snapshot");

    let error = SandboxStore::load(path).expect_err("corrupt snapshot must fail");
    assert!(matches!(error, ManagerError::RegistryPersistFailed { .. }));
}

#[test]
fn load_rejects_invalid_sandbox_id() {
    let dir = RegistryDir::new("badid");
    let path = dir.snapshot_path();
    std::fs::create_dir_all(path.parent().expect("snapshot parent")).expect("create parent");
    std::fs::write(
        &path,
        br#"[{"id":"bad id!","workspace_root":"/tmp/ws","state":"ready","daemon":null,"daemon_http":null,"shared_base":null}]"#,
    )
    .expect("write snapshot");

    let error = SandboxStore::load(path).expect_err("invalid id must fail");
    assert!(matches!(error, ManagerError::RegistryPersistFailed { .. }));
}

#[test]
fn snapshot_is_not_left_behind_as_temp_file() {
    let dir = RegistryDir::new("atomic");
    let path = dir.snapshot_path();

    let store = load(&path);
    store
        .insert(ready_record("alpha", 7100))
        .expect("insert alpha");

    assert!(path.is_file());
    let staged = path.with_file_name("sandboxes.json.tmp");
    assert!(!staged.exists());
}
