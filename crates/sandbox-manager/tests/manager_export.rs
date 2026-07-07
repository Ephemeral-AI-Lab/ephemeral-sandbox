//! Manager export surface: catalog + SPECS↔OPERATIONS parity (spec H6),
//! the forward loop against a fake AND a hostile daemon, apply semantics
//! (winners, deletions, opaque clears, dotfile-under-opaque ordering — spec
//! inv 2, skip-unchanged, idempotent re-run, archive atomicity), and the
//! host boundary (spec inv 9): `..`/absolute/hardlink rejection,
//! symlink-then-traverse containment, whiteout-target validation after the
//! prefix strip, the dest deny-list, and the decompression / entry-count
//! caps.

use std::collections::VecDeque;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use sandbox_manager::{
    CreateSandboxRequest, CreateSandboxResult, ManagerError, ManagerServices, SandboxDaemonClient,
    SandboxDaemonEndpoint, SandboxDaemonInstaller, SandboxId, SandboxRecord, SandboxRuntime,
    SandboxState, SandboxStore, StartedDaemon,
};
use sandbox_protocol::{error_kind, CliOperationScope, Request, Response};
use serde_json::{json, Value};

const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];

struct FakeRuntime;

impl SandboxRuntime for FakeRuntime {
    fn create_sandbox(
        &self,
        _request: &CreateSandboxRequest,
    ) -> Result<CreateSandboxResult, ManagerError> {
        Ok(CreateSandboxResult {
            id: sandbox_id("container-1"),
        })
    }

    fn destroy_sandbox(&self, _record: &SandboxRecord) -> Result<(), ManagerError> {
        Ok(())
    }
}

struct FakeInstaller;

impl SandboxDaemonInstaller for FakeInstaller {
    fn install_daemon(&self, _record: &SandboxRecord) -> Result<(), ManagerError> {
        Ok(())
    }

    fn start_daemon(&self, record: &SandboxRecord) -> Result<StartedDaemon, ManagerError> {
        Ok(StartedDaemon {
            daemon: SandboxDaemonEndpoint::new(
                "127.0.0.1",
                7000,
                format!("token-{}", record.id.as_str()),
            ),
            daemon_http: None,
        })
    }

    fn stop_daemon(&self, _record: &SandboxRecord) -> Result<(), ManagerError> {
        Ok(())
    }

    fn check_daemon(
        &self,
        _record: &SandboxRecord,
        _endpoint: &SandboxDaemonEndpoint,
    ) -> Result<(), ManagerError> {
        Ok(())
    }
}

/// A scriptable export daemon. `export_layerstack` pops scripted start
/// values; `read_export_chunk` serves scripted chunk values when queued,
/// else pages the honest `stream` bytes at the requested offset.
#[derive(Default)]
struct ExportDaemon {
    starts: Mutex<VecDeque<Value>>,
    scripted_chunks: Mutex<VecDeque<Value>>,
    stream: Mutex<Option<Vec<u8>>>,
    invocations: Mutex<Vec<(String, Value)>>,
}

impl ExportDaemon {
    fn push_start(&self, value: Value) {
        self.starts.lock().expect("starts lock").push_back(value);
    }

    fn set_stream(&self, bytes: Vec<u8>) {
        *self.stream.lock().expect("stream lock") = Some(bytes);
    }

    fn invocations(&self) -> Vec<(String, Value)> {
        self.invocations.lock().expect("invocations lock").clone()
    }
}

const HONEST_CHUNK_BYTES: usize = 48;

impl SandboxDaemonClient for ExportDaemon {
    fn invoke_with_timeout(
        &self,
        _endpoint: &SandboxDaemonEndpoint,
        request: Request,
        _timeout: Duration,
    ) -> Result<Response, ManagerError> {
        self.invocations
            .lock()
            .expect("invocations lock")
            .push((request.op.clone(), request.args.clone()));
        match request.op.as_str() {
            "export_layerstack" => {
                let value = self
                    .starts
                    .lock()
                    .expect("starts lock")
                    .pop_front()
                    .unwrap_or_else(|| start_value(2, &["L000002-a"], (0, 0, 0, 0), 0, None));
                Ok(Response::ok(value))
            }
            "read_export_chunk" => {
                if let Some(scripted) = self
                    .scripted_chunks
                    .lock()
                    .expect("chunks lock")
                    .pop_front()
                {
                    return Ok(Response::ok(scripted));
                }
                let stream = self.stream.lock().expect("stream lock");
                let bytes = stream.as_deref().unwrap_or(&[]);
                let offset = request.args["offset"].as_u64().unwrap_or(0) as usize;
                let end = bytes.len().min(offset + HONEST_CHUNK_BYTES);
                let chunk = bytes.get(offset..end).unwrap_or(&[]);
                Ok(Response::ok(json!({
                    "chunk": base64::engine::general_purpose::STANDARD.encode(chunk),
                    "offset": offset,
                    "len": chunk.len(),
                    "total": bytes.len(),
                    "eof": end >= bytes.len(),
                })))
            }
            other => Ok(Response::ok(json!({ "forwarded": other }))),
        }
    }
}

fn start_value(
    version: i64,
    layers: &[&str],
    entries: (u64, u64, u64, u64),
    spool_bytes: u64,
    live_sessions: Option<&[&str]>,
) -> Value {
    let mut value = json!({
        "export_id": "exp-test",
        "manifest_version": version,
        "layers_exported": layers,
        "entries": {
            "files": entries.0,
            "symlinks": entries.1,
            "whiteouts": entries.2,
            "opaques": entries.3,
        },
        "spool_bytes": spool_bytes,
    });
    if let Some(live) = live_sessions {
        value["live_workspace_sessions"] = json!(live);
    }
    value
}

struct Env {
    services: Arc<ManagerServices>,
    daemon: Arc<ExportDaemon>,
    base: PathBuf,
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn temp_base(label: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let base = std::env::temp_dir().join(format!(
        "manager-export-{label}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).expect("create test base");
    base
}

fn env_with_store(label: &str, store: Arc<SandboxStore>) -> Env {
    let daemon = Arc::new(ExportDaemon::default());
    let services = Arc::new(ManagerServices::new(
        Arc::clone(&store),
        Arc::new(FakeRuntime),
        Arc::new(FakeInstaller),
        Arc::clone(&daemon) as Arc<dyn SandboxDaemonClient>,
    ));
    store
        .insert(SandboxRecord {
            id: sandbox_id("sbox-1"),
            workspace_root: PathBuf::from("/testbed"),
            state: SandboxState::Ready,
            daemon: Some(SandboxDaemonEndpoint::new("127.0.0.1", 7000, "token")),
            daemon_http: None,
            shared_base: None,
        })
        .expect("insert sandbox");
    Env {
        services,
        daemon,
        base: temp_base(label),
    }
}

fn env(label: &str) -> Env {
    env_with_store(label, Arc::new(SandboxStore::new()))
}

fn sandbox_id(value: &str) -> SandboxId {
    SandboxId::new(value).expect("valid sandbox id")
}

fn export_request(sandbox_id: &str, dest: &Path, format: Option<&str>) -> Request {
    let mut args = json!({
        "sandbox_id": sandbox_id,
        "dest": dest.to_string_lossy(),
    });
    if let Some(format) = format {
        args["format"] = json!(format);
    }
    Request::new(
        "export_changes",
        "req-export",
        CliOperationScope::System,
        args,
    )
}

fn dispatch(env: &Env, request: &Request) -> Value {
    sandbox_manager::dispatch_operation(&env.services, request).into_json_value()
}

type StreamBuilder<'a> = tar::Builder<zstd::stream::write::Encoder<'a, Vec<u8>>>;

fn build_stream(build: impl FnOnce(&mut StreamBuilder<'_>)) -> Vec<u8> {
    let encoder = zstd::stream::write::Encoder::new(Vec::new(), 3).expect("encoder");
    let mut builder = tar::Builder::new(encoder);
    build(&mut builder);
    let encoder = builder.into_inner().expect("finish tar");
    encoder.finish().expect("finish zstd")
}

fn add_dir(builder: &mut StreamBuilder<'_>, name: &str, mode: u32, mtime: u64) {
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Directory);
    header.set_size(0);
    header.set_mode(mode);
    header.set_mtime(mtime);
    builder
        .append_data(&mut header, name, std::io::empty())
        .expect("append dir");
}

fn add_file(builder: &mut StreamBuilder<'_>, name: &str, content: &[u8], mode: u32, mtime: u64) {
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_size(content.len() as u64);
    header.set_mode(mode);
    header.set_mtime(mtime);
    builder
        .append_data(&mut header, name, content)
        .expect("append file");
}

fn add_symlink(builder: &mut StreamBuilder<'_>, name: &str, target: &str) {
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Symlink);
    header.set_size(0);
    header.set_mode(0o777);
    header.set_mtime(0);
    builder
        .append_link(&mut header, name, target)
        .expect("append symlink");
}

fn add_marker(builder: &mut StreamBuilder<'_>, name: &str) {
    add_file(builder, name, b"", 0o644, 0);
}

/// Craft an entry with a raw header name (and optional raw link target),
/// bypassing tar-rs path validation — the honest daemon cannot author these.
fn add_raw(
    builder: &mut StreamBuilder<'_>,
    raw_name: &str,
    kind: tar::EntryType,
    content: &[u8],
    raw_link: Option<&str>,
) {
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(kind);
    header.set_size(content.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(0);
    {
        let gnu = header.as_gnu_mut().expect("gnu header");
        gnu.name[..raw_name.len()].copy_from_slice(raw_name.as_bytes());
        if let Some(link) = raw_link {
            gnu.linkname[..link.len()].copy_from_slice(link.as_bytes());
        }
    }
    header.set_cksum();
    builder.append(&header, content).expect("append raw entry");
}

fn honest_delta_stream() -> Vec<u8> {
    build_stream(|builder| {
        add_dir(builder, "src/", 0o755, 1_750_000_000);
        add_file(builder, "src/a.rs", b"v2\n", 0o640, 1_750_000_123);
        add_marker(builder, "src/.wh.b.rs");
    })
}

fn read_to_string(path: &Path) -> String {
    std::fs::read_to_string(path).expect("read file")
}

fn error_message(value: &Value) -> String {
    value["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

fn dir_entries(path: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(path)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

// ---------------------------------------------------------------- catalog

#[test]
fn export_changes_spec_is_in_the_catalog() {
    let spec = sandbox_manager_operations::cli_operation_specs()
        .iter()
        .find(|spec| spec.name == "export_changes")
        .expect("export_changes spec");
    assert_eq!(spec.family, "management");
    let arg_names: Vec<&str> = spec.args.iter().map(|arg| arg.name).collect();
    assert_eq!(arg_names, ["sandbox_id", "dest", "format"]);
    let cli = spec.cli.as_ref().expect("cli spec");
    assert!(cli.usage.contains("--sandbox-id"));
    assert!(cli.usage.contains("--dest"));
    assert!(cli.usage.contains("--format"));
    let squash = sandbox_manager_operations::cli_operation_specs()
        .iter()
        .find(|spec| spec.name == "checkpoint_squash")
        .expect("checkpoint_squash spec");
    assert!(
        squash.related.contains(&"export_changes"),
        "checkpoint_squash relates to export_changes"
    );
}

// SPECS↔OPERATIONS parity (spec H6): every catalog spec must have a live
// dispatcher — a spec whose op dispatches as unknown_op is drift.
#[test]
fn every_catalog_spec_has_a_dispatcher() {
    let env = env("parity");
    for spec in sandbox_manager_operations::cli_operation_specs() {
        let response = dispatch(
            &env,
            &Request::new(
                spec.name,
                "req-parity",
                CliOperationScope::System,
                json!({}),
            ),
        );
        assert_ne!(
            response["error"]["kind"],
            json!("unknown_op"),
            "catalog spec {} has no dispatcher",
            spec.name
        );
    }
}

// ------------------------------------------------------------- dir apply

#[test]
fn dir_apply_reproduces_winners_deletions_and_opaque_clears() {
    let env = env("dir-apply");
    let dest = env.base.join("dest");
    std::fs::create_dir_all(dest.join("src")).expect("seed src");
    std::fs::write(dest.join("src/a.rs"), "v1\n").expect("seed a.rs");
    std::fs::write(dest.join("src/b.rs"), "B\n").expect("seed b.rs");
    std::fs::create_dir_all(dest.join("cfg")).expect("seed cfg");
    std::fs::write(dest.join("cfg/dev.yml"), "D\n").expect("seed dev.yml");

    env.daemon.push_start(start_value(
        3,
        &["L000003-b", "L000002-a"],
        (2, 1, 1, 1),
        64,
        None,
    ));
    env.daemon.set_stream(build_stream(|builder| {
        add_dir(builder, "cfg/", 0o755, 1_750_000_000);
        add_marker(builder, "cfg/.wh..wh..opq");
        add_file(builder, "cfg/prod.yml", b"P2\n", 0o644, 1_750_000_200);
        add_symlink(builder, "link.md", "src/a.rs");
        add_dir(builder, "src/", 0o755, 1_750_000_000);
        add_file(builder, "src/a.rs", b"v2\n", 0o640, 1_750_000_123);
        add_marker(builder, "src/.wh.b.rs");
    }));

    let result = dispatch(&env, &export_request("sbox-1", &dest, None));
    assert!(result.get("error").is_none(), "export failed: {result}");
    assert_eq!(result["format"], json!("dir"));
    assert_eq!(result["manifest_version"], json!(3));
    assert_eq!(result["layers_exported"], json!(["L000003-b", "L000002-a"]));
    assert_eq!(result["files_written"], json!(2));
    assert_eq!(result["symlinks_written"], json!(1));
    assert_eq!(result["deletes_applied"], json!(1));
    assert_eq!(result["opaque_clears"], json!(1));
    assert_eq!(result["skipped_unchanged"], json!(0));
    assert_eq!(result["bytes_written"], json!(6));

    assert_eq!(read_to_string(&dest.join("src/a.rs")), "v2\n");
    assert!(!dest.join("src/b.rs").exists(), "whiteout deleted b.rs");
    assert!(
        !dest.join("cfg/dev.yml").exists(),
        "opaque clear removed base-origin dev.yml"
    );
    assert_eq!(read_to_string(&dest.join("cfg/prod.yml")), "P2\n");
    let link = std::fs::read_link(dest.join("link.md")).expect("symlink");
    assert_eq!(link, PathBuf::from("src/a.rs"));
    assert!(
        !dest.join("src/.wh.b.rs").exists() && !dest.join("cfg/.wh..wh..opq").exists(),
        "no literal markers land on the host"
    );

    use std::os::unix::fs::MetadataExt as _;
    let meta = std::fs::metadata(dest.join("src/a.rs")).expect("meta");
    assert_eq!(meta.mode() & 0o7777, 0o640, "mode carried");
    assert_eq!(meta.mtime(), 1_750_000_123, "second-granular mtime stamped");
}

// Spec inv 2 / adversarial C2: a dotfile winner sorts before its
// directory's opaque marker in the stream; a blind tar-order applier writes
// it then clears it away. The three-pass apply must keep it.
#[test]
fn dotfile_winner_survives_its_directorys_opaque_clear() {
    let env = env("opaque-ordering");
    let dest = env.base.join("dest");
    std::fs::create_dir_all(dest.join("cfg")).expect("seed cfg");
    std::fs::write(dest.join("cfg/dev.yml"), "D\n").expect("seed dev.yml");

    env.daemon
        .push_start(start_value(2, &["L000002-a"], (2, 0, 0, 1), 64, None));
    env.daemon.set_stream(build_stream(|builder| {
        add_dir(builder, "cfg/", 0o755, 0);
        add_file(builder, "cfg/.env", b"E\n", 0o600, 1_750_000_001);
        add_marker(builder, "cfg/.wh..wh..opq");
        add_file(builder, "cfg/prod.yml", b"P\n", 0o644, 1_750_000_002);
    }));

    let result = dispatch(&env, &export_request("sbox-1", &dest, None));
    assert!(result.get("error").is_none(), "export failed: {result}");
    assert_eq!(
        read_to_string(&dest.join("cfg/.env")),
        "E\n",
        "the dotfile winner survives the opaque clear"
    );
    assert_eq!(read_to_string(&dest.join("cfg/prod.yml")), "P\n");
    assert!(!dest.join("cfg/dev.yml").exists());
}

#[test]
fn rerun_skips_unchanged_file_winners_and_recounts_deletions() {
    let env = env("idempotent");
    let dest = env.base.join("dest");
    for _ in 0..2 {
        env.daemon
            .push_start(start_value(3, &["L000002-a"], (1, 0, 1, 0), 64, None));
    }
    env.daemon.set_stream(honest_delta_stream());

    let first = dispatch(&env, &export_request("sbox-1", &dest, None));
    assert!(first.get("error").is_none(), "first export failed: {first}");
    assert_eq!(first["files_written"], json!(1));
    assert_eq!(first["skipped_unchanged"], json!(0));

    let second = dispatch(&env, &export_request("sbox-1", &dest, None));
    assert!(
        second.get("error").is_none(),
        "second export failed: {second}"
    );
    assert_eq!(second["files_written"], json!(0), "zero content writes");
    assert_eq!(second["bytes_written"], json!(0));
    assert_eq!(second["skipped_unchanged"], json!(1));
    assert_eq!(
        second["deletes_applied"],
        json!(1),
        "whiteouts re-apply on re-run"
    );
    assert_eq!(read_to_string(&dest.join("src/a.rs")), "v2\n");
}

#[test]
fn dir_result_contract_keys_are_exact() {
    let env = env("contract");
    let dest = env.base.join("dest");
    env.daemon
        .push_start(start_value(3, &["L000002-a"], (1, 0, 1, 0), 64, None));
    env.daemon.set_stream(honest_delta_stream());

    let result = dispatch(&env, &export_request("sbox-1", &dest, None));
    let mut keys: Vec<&str> = result
        .as_object()
        .expect("result object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "bytes_written",
            "deletes_applied",
            "files_written",
            "format",
            "layers_exported",
            "manifest_version",
            "opaque_clears",
            "skipped_unchanged",
            "symlinks_written",
        ],
        "exact dir contract; live_workspace_sessions only when non-empty"
    );
}

#[test]
fn live_workspace_sessions_ride_the_result_when_reported() {
    let env = env("live-sessions");
    let dest = env.base.join("dest");
    env.daemon.push_start(start_value(
        3,
        &["L000002-a"],
        (1, 0, 1, 0),
        64,
        Some(&["ws-7"]),
    ));
    env.daemon.set_stream(honest_delta_stream());

    let result = dispatch(&env, &export_request("sbox-1", &dest, None));
    assert_eq!(result["live_workspace_sessions"], json!(["ws-7"]));
}

#[test]
fn empty_delta_applies_as_a_clean_no_op() {
    let env = env("empty-delta");
    let dest = env.base.join("dest");
    std::fs::create_dir_all(&dest).expect("dest");
    std::fs::write(dest.join("keep.txt"), "K\n").expect("seed");
    env.daemon
        .push_start(start_value(1, &[], (0, 0, 0, 0), 13, None));
    env.daemon.set_stream(build_stream(|_| {}));

    let result = dispatch(&env, &export_request("sbox-1", &dest, None));
    assert!(result.get("error").is_none(), "export failed: {result}");
    assert_eq!(result["manifest_version"], json!(1));
    assert_eq!(result["layers_exported"], json!([]));
    assert_eq!(result["files_written"], json!(0));
    assert_eq!(result["deletes_applied"], json!(0));
    assert_eq!(result["opaque_clears"], json!(0));
    assert_eq!(result["bytes_written"], json!(0));
    assert_eq!(read_to_string(&dest.join("keep.txt")), "K\n");
    assert_eq!(dir_entries(&dest), ["keep.txt"]);
}

// MED-06 twin: a symlink at a directory position is replaced, never
// followed — its old target directory stays untouched.
#[test]
fn directory_winner_replaces_a_dest_symlink_without_following_it() {
    let env = env("symlink-replace");
    let dest = env.base.join("dest");
    let elsewhere = env.base.join("elsewhere");
    std::fs::create_dir_all(&dest).expect("dest");
    std::fs::create_dir_all(&elsewhere).expect("elsewhere");
    std::os::unix::fs::symlink(&elsewhere, dest.join("d")).expect("planted symlink");

    env.daemon
        .push_start(start_value(2, &["L000002-a"], (1, 0, 0, 0), 64, None));
    env.daemon.set_stream(build_stream(|builder| {
        add_dir(builder, "d/", 0o755, 0);
        add_file(builder, "d/file.txt", b"F\n", 0o644, 1_750_000_500);
    }));

    let result = dispatch(&env, &export_request("sbox-1", &dest, None));
    assert!(result.get("error").is_none(), "export failed: {result}");
    let meta = std::fs::symlink_metadata(dest.join("d")).expect("meta");
    assert!(
        meta.is_dir(),
        "the symlink was replaced by a real directory"
    );
    assert_eq!(read_to_string(&dest.join("d/file.txt")), "F\n");
    assert_eq!(
        dir_entries(&elsewhere),
        Vec::<String>::new(),
        "the old symlink target was never written through"
    );
}

// ---------------------------------------------------------------- archives

#[test]
fn tar_zst_archive_is_written_as_received_with_atomic_rename() {
    let env = env("tar-zst");
    let dest = env.base.join("out/delta.tar.zst");
    std::fs::create_dir_all(dest.parent().expect("parent")).expect("mkdir out");
    let stream = honest_delta_stream();
    env.daemon.push_start(start_value(
        3,
        &["L000002-a"],
        (1, 0, 1, 0),
        stream.len() as u64,
        None,
    ));
    env.daemon.set_stream(stream.clone());

    let result = dispatch(&env, &export_request("sbox-1", &dest, Some("tar-zst")));
    assert!(result.get("error").is_none(), "export failed: {result}");
    assert_eq!(result["format"], json!("tar-zst"));
    assert_eq!(result["files_written"], json!(1));
    assert_eq!(result["whiteouts_emitted"], json!(1));
    assert_eq!(result["bytes_written"], json!(stream.len()));
    assert!(
        result.get("deletes_applied").is_none(),
        "no apply-side fields"
    );
    assert!(result.get("skipped_unchanged").is_none());

    assert_eq!(std::fs::read(&dest).expect("archive"), stream);
    assert_eq!(
        dir_entries(dest.parent().expect("parent")),
        ["delta.tar.zst"],
        "no temp sibling left behind"
    );
}

#[test]
fn tar_archive_is_decompressed_plain_tar() {
    let env = env("tar-plain");
    let dest = env.base.join("out/delta.tar");
    std::fs::create_dir_all(dest.parent().expect("parent")).expect("mkdir out");
    env.daemon
        .push_start(start_value(3, &["L000002-a"], (1, 0, 1, 0), 64, None));
    env.daemon.set_stream(honest_delta_stream());

    let result = dispatch(&env, &export_request("sbox-1", &dest, Some("tar")));
    assert!(result.get("error").is_none(), "export failed: {result}");

    let bytes = std::fs::read(&dest).expect("archive");
    assert_ne!(&bytes[..4], &ZSTD_MAGIC, "plain tar, not zstd");
    let mut archive = tar::Archive::new(&bytes[..]);
    let names: Vec<String> = archive
        .entries()
        .expect("entries")
        .map(|entry| {
            let mut entry = entry.expect("entry");
            let name = String::from_utf8(entry.path_bytes().into_owned()).expect("utf8");
            let mut sink = Vec::new();
            entry.read_to_end(&mut sink).expect("drain");
            name
        })
        .collect();
    assert_eq!(names, ["src/", "src/a.rs", "src/.wh.b.rs"]);
}

// ------------------------------------------------------------ dest guard

#[test]
fn relative_dest_is_rejected_before_any_forward() {
    let env = env("relative-dest");
    let request = Request::new(
        "export_changes",
        "req-export",
        CliOperationScope::System,
        json!({ "sandbox_id": "sbox-1", "dest": "./relative" }),
    );
    let result = dispatch(&env, &request);
    assert_eq!(result["error"]["kind"], json!(error_kind::INVALID_REQUEST));
    assert!(error_message(&result).contains("absolute"));
    assert!(
        env.daemon.invocations().is_empty(),
        "no forward happened: {:?}",
        env.daemon.invocations()
    );
}

#[test]
fn deny_list_rejects_root_home_state_dir_and_export_spool_paths() {
    let registry_base = temp_base("deny-registry");
    let registry_path = registry_base.join("state/registry.json");
    let store = Arc::new(SandboxStore::load(registry_path.clone()).expect("load store"));
    let env = env_with_store("deny-list", store);

    let mut denied: Vec<PathBuf> = vec![
        PathBuf::from("/"),
        registry_path.clone(),
        registry_path.parent().expect("state dir").to_path_buf(),
        PathBuf::from("/var/eos/.export/spool.tar.zst"),
    ];
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            denied.push(PathBuf::from(home));
        }
    }
    for dest in denied {
        let result = dispatch(&env, &export_request("sbox-1", &dest, None));
        assert_eq!(
            result["error"]["kind"],
            json!(error_kind::INVALID_REQUEST),
            "deny-list must reject {}: {result}",
            dest.display()
        );
        assert!(
            env.daemon.invocations().is_empty(),
            "deny-list rejects before any forward"
        );
    }
    let _ = std::fs::remove_dir_all(&registry_base);
}

#[test]
fn format_and_dest_shape_violations_are_rejected() {
    let env = env("shape");
    let dir_dest = env.base.join("some-dir");
    std::fs::create_dir_all(&dir_dest).expect("mkdir");
    let file_dest = env.base.join("some-file");
    std::fs::write(&file_dest, "x").expect("file");

    let bad_format = dispatch(&env, &export_request("sbox-1", &dir_dest, Some("zip")));
    assert_eq!(
        bad_format["error"]["kind"],
        json!(error_kind::INVALID_REQUEST)
    );

    let tar_onto_dir = dispatch(&env, &export_request("sbox-1", &dir_dest, Some("tar")));
    assert_eq!(
        tar_onto_dir["error"]["kind"],
        json!(error_kind::INVALID_REQUEST)
    );

    let dir_onto_file = dispatch(&env, &export_request("sbox-1", &file_dest, None));
    assert_eq!(
        dir_onto_file["error"]["kind"],
        json!(error_kind::INVALID_REQUEST)
    );

    let orphan_parent = dispatch(
        &env,
        &export_request("sbox-1", &env.base.join("missing/delta.tar"), Some("tar")),
    );
    assert_eq!(
        orphan_parent["error"]["kind"],
        json!(error_kind::INVALID_REQUEST)
    );

    assert!(env.daemon.invocations().is_empty());
}

#[test]
fn non_ready_sandbox_is_rejected_by_the_forward_gate_with_dest_untouched() {
    let env = env("non-ready");
    env.services
        .store
        .insert(SandboxRecord {
            id: sandbox_id("sbox-creating"),
            workspace_root: PathBuf::from("/testbed"),
            state: SandboxState::Creating,
            daemon: Some(SandboxDaemonEndpoint::new("127.0.0.1", 7000, "token")),
            daemon_http: None,
            shared_base: None,
        })
        .expect("insert");
    let dest = env.base.join("untouched");

    let creating = dispatch(&env, &export_request("sbox-creating", &dest, None));
    assert_eq!(
        creating["error"]["kind"],
        json!(error_kind::INVALID_REQUEST)
    );
    let missing = dispatch(&env, &export_request("sbox-missing", &dest, None));
    assert_eq!(missing["error"]["kind"], json!(error_kind::INVALID_REQUEST));
    assert!(!dest.exists(), "dest is never created on a gate reject");
}

// -------------------------------------------------------- hostile daemon

fn hostile_apply(env: &Env, dest: &Path, stream: Vec<u8>) -> Value {
    env.daemon.push_start(start_value(
        9,
        &["L000009-x"],
        (1, 0, 0, 0),
        stream.len() as u64,
        None,
    ));
    env.daemon.set_stream(stream);
    dispatch(env, &export_request("sbox-1", dest, None))
}

#[test]
fn traversal_entries_are_rejected_with_nothing_applied() {
    let env = env("tar-slip");
    let dest = env.base.join("dest");
    let sentinel = env.base.join("escape.txt");
    std::fs::write(&sentinel, "untouched\n").expect("sentinel");

    let dotdot = hostile_apply(
        &env,
        &dest,
        build_stream(|builder| {
            add_file(builder, "ok.txt", b"OK\n", 0o644, 0);
            add_raw(
                builder,
                "../escape.txt",
                tar::EntryType::Regular,
                b"pwn\n",
                None,
            );
        }),
    );
    assert_eq!(dotdot["error"]["kind"], json!(error_kind::OPERATION_FAILED));
    assert!(
        error_message(&dotdot).contains("'..'"),
        "error names the rejection: {dotdot}"
    );
    assert_eq!(read_to_string(&sentinel), "untouched\n");
    assert_eq!(
        dir_entries(&dest),
        Vec::<String>::new(),
        "no valid entries partially applied"
    );
}

#[test]
fn absolute_entry_names_are_rejected_with_nothing_outside_dest() {
    let env = env("absolute-slip");
    let dest = env.base.join("dest");
    let sentinel = env.base.join("abs-escape.txt");
    std::fs::write(&sentinel, "untouched\n").expect("sentinel");

    let absolute = hostile_apply(
        &env,
        &dest,
        build_stream(|builder| {
            add_raw(
                builder,
                &sentinel.to_string_lossy(),
                tar::EntryType::Regular,
                b"pwn\n",
                None,
            );
        }),
    );
    assert_eq!(
        absolute["error"]["kind"],
        json!(error_kind::OPERATION_FAILED)
    );
    assert!(
        error_message(&absolute).contains("absolute"),
        "error names the rejection: {absolute}"
    );
    assert_eq!(read_to_string(&sentinel), "untouched\n");
}

#[test]
fn hardlink_entries_are_rejected() {
    let env = env("hardlink");
    let dest = env.base.join("dest");

    let hardlink = hostile_apply(
        &env,
        &dest,
        build_stream(|builder| {
            add_file(builder, "victim.txt", b"V\n", 0o644, 0);
            add_raw(
                builder,
                "link.txt",
                tar::EntryType::Link,
                b"",
                Some("victim.txt"),
            );
        }),
    );
    assert_eq!(
        hardlink["error"]["kind"],
        json!(error_kind::OPERATION_FAILED)
    );
    assert!(
        error_message(&hardlink).contains("hardlink"),
        "error names the rejection: {hardlink}"
    );
    assert_eq!(
        dir_entries(&dest),
        Vec::<String>::new(),
        "a hardlink anywhere in the stream aborts the whole apply"
    );
}

// Spec inv 9 (O_NOFOLLOW fd walk): a symlink planted by an earlier entry is
// replaced by a real directory when a later entry traverses through it — the
// write never follows the symlink out of dest.
#[test]
fn symlink_then_traverse_is_contained_in_dest() {
    let env = env("symlink-traverse");
    let dest = env.base.join("dest");
    let evil = env.base.join("evil_dir");
    std::fs::create_dir_all(&evil).expect("evil dir");

    let result = hostile_apply(
        &env,
        &dest,
        build_stream(|builder| {
            add_symlink(builder, "x", &evil.to_string_lossy());
            add_file(builder, "x/passwd", b"pwn\n", 0o644, 1_750_000_000);
        }),
    );
    assert!(result.get("error").is_none(), "apply failed: {result}");
    let meta = std::fs::symlink_metadata(dest.join("x")).expect("meta");
    assert!(
        meta.is_dir(),
        "the planted symlink was replaced by a real directory"
    );
    assert_eq!(read_to_string(&dest.join("x/passwd")), "pwn\n");
    assert!(
        !evil.join("passwd").exists(),
        "no write followed the symlink out of dest"
    );
    assert_eq!(
        dir_entries(&evil),
        Vec::<String>::new(),
        "the symlink target directory stays empty"
    );
}

// Spec inv 9: whiteout targets are validated AFTER the .wh. prefix strip.
// A marker whose stripped target normalizes outside its directory (here the
// stripped name is `..`) is rejected; a sentinel outside dest is untouched.
#[test]
fn whiteout_target_escaping_dest_is_rejected() {
    let env = env("wh-escape");
    let dest = env.base.join("dest");
    let victim = env.base.join("victim");
    std::fs::write(&victim, "present\n").expect("victim");

    let after_strip = hostile_apply(
        &env,
        &dest,
        build_stream(|builder| add_raw(builder, ".wh...", tar::EntryType::Regular, b"", None)),
    );
    assert_eq!(
        after_strip["error"]["kind"],
        json!(error_kind::OPERATION_FAILED)
    );
    assert!(
        error_message(&after_strip).contains("whiteout"),
        "error names the whiteout rejection: {after_strip}"
    );

    let parent_escape = hostile_apply(
        &env,
        &dest,
        build_stream(|builder| {
            add_raw(builder, "../.wh.victim", tar::EntryType::Regular, b"", None)
        }),
    );
    assert_eq!(
        parent_escape["error"]["kind"],
        json!(error_kind::OPERATION_FAILED)
    );
    assert_eq!(
        read_to_string(&victim),
        "present\n",
        "no remove_path escaped dest"
    );
}

// Defense in depth for inv 10: a reserved `.wh.` component anywhere but a
// final marker position is rejected (publish already fail-closes on these,
// but the host applier does not trust the daemon).
#[test]
fn reserved_wh_path_component_is_rejected() {
    let env = env("wh-component");
    let dest = env.base.join("dest");
    let result = hostile_apply(
        &env,
        &dest,
        build_stream(|builder| {
            add_raw(
                builder,
                ".wh.sneaky/child.txt",
                tar::EntryType::Regular,
                b"x",
                None,
            )
        }),
    );
    assert_eq!(result["error"]["kind"], json!(error_kind::OPERATION_FAILED));
    assert!(error_message(&result).contains(".wh."));
}

// Serialize the two cap tests: each lowers a global cap via env var for the
// duration of its apply, and no honest test decompresses past 64 KiB or
// carries more than a handful of entries, so a concurrent apply is unaffected.
fn cap_guard() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|poison| poison.into_inner())
}

#[test]
fn decompression_bomb_is_capped() {
    let _guard = cap_guard();
    std::env::set_var("EOS_EXPORT_MAX_DECOMPRESSED_BYTES", "65536");
    let env = env("zstd-bomb");
    let dest = env.base.join("dest");
    let bomb = build_stream(|builder| {
        add_file(builder, "big.bin", &vec![0_u8; 131_072], 0o644, 0);
    });
    let result = hostile_apply(&env, &dest, bomb);
    std::env::remove_var("EOS_EXPORT_MAX_DECOMPRESSED_BYTES");

    assert_eq!(result["error"]["kind"], json!(error_kind::OPERATION_FAILED));
    assert!(
        error_message(&result).contains("decompressed"),
        "error names the cap: {result}"
    );
    assert!(
        !dest.join("big.bin").exists(),
        "the bomb never lands on disk"
    );
}

#[test]
fn entry_count_bomb_is_capped() {
    let _guard = cap_guard();
    std::env::set_var("EOS_EXPORT_MAX_ENTRIES", "1000");
    let env = env("entry-bomb");
    let dest = env.base.join("dest");
    let bomb = build_stream(|builder| {
        for index in 0..1500 {
            add_file(builder, &format!("f{index}.txt"), b"", 0o644, 0);
        }
    });
    let result = hostile_apply(&env, &dest, bomb);
    std::env::remove_var("EOS_EXPORT_MAX_ENTRIES");

    assert_eq!(result["error"]["kind"], json!(error_kind::OPERATION_FAILED));
    assert!(
        error_message(&result).contains("entry-count cap"),
        "error names the cap: {result}"
    );
}

// ---------------------------------------------------------- forward loop

// The manager pages the whole compressed stream across many bounded
// read_export_chunk forwards, each reusing the request id, then applies the
// reassembled bytes.
#[test]
fn forward_loop_pages_a_multi_chunk_stream() {
    let env = env("forward-loop");
    let dest = env.base.join("dest");
    let stream = honest_delta_stream();
    assert!(
        stream.len() > HONEST_CHUNK_BYTES,
        "fixture must span several chunks"
    );
    env.daemon.push_start(start_value(
        3,
        &["L000002-a"],
        (1, 0, 1, 0),
        stream.len() as u64,
        None,
    ));
    env.daemon.set_stream(stream.clone());

    let result = dispatch(&env, &export_request("sbox-1", &dest, None));
    assert!(result.get("error").is_none(), "export failed: {result}");
    assert_eq!(read_to_string(&dest.join("src/a.rs")), "v2\n");

    let invocations = env.daemon.invocations();
    assert_eq!(invocations[0].0, "export_layerstack");
    let chunk_calls = invocations
        .iter()
        .filter(|(op, _)| op == "read_export_chunk")
        .count();
    let expected = stream.len().div_ceil(HONEST_CHUNK_BYTES);
    assert_eq!(chunk_calls, expected, "one forward per bounded chunk");
    assert!(
        invocations
            .iter()
            .all(|(_, args)| args.get("export_id").is_none()
                || args["export_id"] == json!("exp-test")),
        "every chunk forward pins the same export_id"
    );
}

// A daemon that does not know export_layerstack (unknown_op) surfaces as a
// clean operation_failed telling the operator to recreate the sandbox.
#[test]
fn stale_daemon_unknown_op_is_translated() {
    let env = env("stale-daemon");
    let dest = env.base.join("dest");
    env.daemon.push_start(json!({
        "error": { "kind": "unknown_op", "message": "unknown operation", "details": {} }
    }));

    let result = dispatch(&env, &export_request("sbox-1", &dest, None));
    assert_eq!(result["error"]["kind"], json!(error_kind::OPERATION_FAILED));
    assert!(
        error_message(&result).contains("export_changes"),
        "error tells the operator to recreate: {result}"
    );
}
