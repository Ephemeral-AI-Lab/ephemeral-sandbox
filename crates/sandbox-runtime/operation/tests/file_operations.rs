//! Runtime file-operation coverage (spec §Verification). Sessionless ops run
//! against a real `LayerStackService` over a fixture snapshot; session ops run
//! through the explicit `run_file_op` hook (namespace semantics preserved, not
//! bypassed); path, layerstack-helper, and runner-placement cases round it out.

mod support;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use sandbox_observability::{Observer, SpanRegistry};
use sandbox_protocol::{CliOperationScope, Request};
use sandbox_runtime::file::{
    EditInput, EditOp, FileEntryKind, FileOperationError, FileService, ReadInput, ReadOutput,
    WriteInput,
};
use sandbox_runtime::layerstack::{LayerStackService, ManifestReadWindow};
use sandbox_runtime::workspace_session::WorkspaceSessionService;
use sandbox_runtime::{CommandOperationService, SandboxRuntimeOperations};
use sandbox_runtime_layerstack::LayerPath;
use sandbox_runtime_namespace_execution::{NamespaceExecutionEngine, NamespaceTarget};
use sandbox_runtime_namespace_process::runner::protocol::NsFds;
use sandbox_runtime_workspace::{
    run_result_err, run_result_ok, FileRunnerEntryKind, FileRunnerError, FileRunnerOp,
    FileRunnerResult, NetworkProfile, WorkspaceSessionId,
};
use serde_json::json;

use support::{
    fake_workspace_runtime, workspace_handle, FakeLauncher, FakeRunnerScript, FakeWorkspaceService,
};

const MAX_OUTPUT_BYTES: usize = 256 * 1024;

fn uniq() -> u64 {
    static C: AtomicU64 = AtomicU64::new(0);
    C.fetch_add(1, Ordering::Relaxed)
}

fn temp(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "file-ops-{label}-{}-{}",
        std::process::id(),
        uniq()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

struct Env {
    file: Arc<FileService>,
    layerstack: Arc<LayerStackService>,
    workspace_session: Arc<WorkspaceSessionService>,
    fake: Arc<FakeWorkspaceService>,
    workspace_root: PathBuf,
}

/// Build a snapshot from a fixture workspace, plus the audit store, layerstack,
/// and a hook-backed workspace-session service.
fn env() -> Env {
    let base = temp("env");
    let root = base.join("layer-stack");
    let workspace = base.join("workspace");
    std::fs::create_dir_all(workspace.join("sub")).expect("mkdir");
    write_fixture(&workspace.join("readme.txt"), b"line1\nline2\nline3\n");
    write_fixture(&workspace.join("sub/nested.txt"), b"a\nb\n");
    write_fixture(
        &workspace.join("bom.txt"),
        "\u{feff}hello\nworld\n".as_bytes(),
    );
    write_fixture(&workspace.join("crlf.txt"), b"a\r\nb\rc\n");
    write_fixture(&workspace.join("binary.dat"), &[0xff, 0xfe, 0x00, b'x']);
    write_fixture(&workspace.join("big.txt"), big_file(5000).as_bytes());
    write_fixture(&workspace.join("wide.txt"), &vec![b'x'; 300_000]);
    write_fixture(
        &workspace.join("huge.txt"),
        &vec![b'x'; 4 * 1024 * 1024 + 1],
    );
    std::os::unix::fs::symlink("readme.txt", workspace.join("link.txt")).expect("symlink");
    std::os::unix::fs::symlink("sub", workspace.join("linkdir")).expect("symlink dir");

    sandbox_runtime_layerstack::build_workspace_base(&root, &workspace, false).expect("build base");
    let file = Arc::new(FileService::open(temp("audit")).expect("audit store"));
    let layerstack = Arc::new(
        LayerStackService::new(root, Observer::disabled(), Arc::clone(&file))
            .expect("layerstack service"),
    );
    let fake = Arc::new(FakeWorkspaceService::new());
    let workspace_session = Arc::new(WorkspaceSessionService::new(
        fake_workspace_runtime(Arc::clone(&fake)),
        Arc::clone(&layerstack),
        Observer::disabled(),
    ));
    Env {
        file,
        layerstack,
        workspace_session,
        fake,
        workspace_root: workspace,
    }
}

fn write_fixture(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir parent");
    }
    std::fs::write(path, bytes).expect("write fixture");
}

fn big_file(lines: usize) -> String {
    (0..lines)
        .map(|i| format!("row{i}"))
        .collect::<Vec<_>>()
        .join("\n")
}

impl Env {
    fn read(&self, input: ReadInput) -> Result<ReadOutput, FileOperationError> {
        self.file
            .read(&self.layerstack, &self.workspace_session, input)
    }
    fn write(
        &self,
        input: WriteInput,
    ) -> Result<sandbox_runtime::file::WriteOutput, FileOperationError> {
        self.file
            .write(&self.layerstack, &self.workspace_session, input)
    }
    fn edit(
        &self,
        input: EditInput,
    ) -> Result<sandbox_runtime::file::EditOutput, FileOperationError> {
        self.file
            .edit(&self.layerstack, &self.workspace_session, input)
    }
    fn create_session(&self) -> WorkspaceSessionId {
        self.fake.push_create_result(Ok(workspace_handle(
            "ws-1",
            "lease-1",
            PathBuf::from("/workspace/session"),
            NetworkProfile::Shared,
        )));
        self.workspace_session
            .create_workspace_session(support::create_request())
            .expect("session created")
            .workspace_session_id
    }
}

fn read_of(path: &str) -> ReadInput {
    ReadInput {
        path: path.into(),
        offset: None,
        limit: None,
        workspace_session_id: None,
    }
}

fn write_of(path: &str, content: &str, id: &str) -> WriteInput {
    WriteInput {
        path: path.into(),
        content: content.into(),
        request_id: id.into(),
        workspace_session_id: None,
    }
}

fn edit_of(path: &str, edits: Vec<EditOp>, id: &str) -> EditInput {
    EditInput {
        path: path.into(),
        edits,
        request_id: id.into(),
        workspace_session_id: None,
    }
}

fn edit_op(old: &str, new: &str, replace_all: bool) -> EditOp {
    EditOp {
        old_string: old.into(),
        new_string: new.into(),
        replace_all,
    }
}

// ---------- sessionless read ----------

#[test]
fn sessionless_read_returns_window_fields() {
    let env = env();
    let out = env.read(read_of("readme.txt")).expect("read");
    assert_eq!(out.content, "line1\nline2\nline3");
    assert_eq!(out.start_line, 1);
    assert_eq!(out.num_lines, 3);
    assert_eq!(out.total_lines, 3);
    assert_eq!(out.bytes_read, out.content.len());
    assert_eq!(out.total_bytes, 18);
    assert_eq!(out.next_offset, None);
    assert!(!out.truncated);
}

#[test]
fn sessionless_read_offset_limit_windows_and_paginates() {
    let env = env();
    let out = env
        .read(ReadInput {
            path: "readme.txt".into(),
            offset: Some(2),
            limit: Some(1),
            workspace_session_id: None,
        })
        .expect("read");
    assert_eq!(out.content, "line2");
    assert_eq!(out.start_line, 2);
    assert_eq!(out.num_lines, 1);
    assert_eq!(out.next_offset, Some(3));
    assert!(out.truncated);
}

#[test]
fn sessionless_read_offset_past_eof_is_empty_not_missing() {
    let env = env();
    let out = env
        .read(ReadInput {
            path: "readme.txt".into(),
            offset: Some(99),
            limit: None,
            workspace_session_id: None,
        })
        .expect("read");
    assert_eq!(out.content, "");
    assert_eq!(out.num_lines, 0);
    assert_eq!(out.total_lines, 3);
    assert!(!out.truncated);
}

#[test]
fn sessionless_read_missing_is_not_found() {
    let env = env();
    assert!(matches!(
        env.read(read_of("nope.txt")),
        Err(FileOperationError::NotFound(_))
    ));
}

#[test]
fn sessionless_read_normalizes_bom_and_line_endings() {
    let env = env();
    assert_eq!(
        env.read(read_of("bom.txt")).expect("bom").content,
        "hello\nworld"
    );
    let crlf = env.read(read_of("crlf.txt")).expect("crlf");
    assert_eq!(crlf.content, "a\nb\nc");
    assert_eq!(crlf.total_lines, 3);
}

#[test]
fn sessionless_read_invalid_utf8_is_rejected() {
    let env = env();
    assert!(matches!(
        env.read(read_of("binary.dat")),
        Err(FileOperationError::NotUtf8(_))
    ));
}

#[test]
fn sessionless_read_large_file_returns_small_window() {
    let env = env();
    let out = env
        .read(ReadInput {
            path: "big.txt".into(),
            offset: Some(1),
            limit: Some(5),
            workspace_session_id: None,
        })
        .expect("large read");
    assert_eq!(out.num_lines, 5);
    assert_eq!(out.total_lines, 5000);
    assert!(out.truncated);
}

#[test]
fn sessionless_read_output_over_cap_is_output_too_large() {
    let env = env();
    match env.read(read_of("wide.txt")) {
        Err(FileOperationError::OutputTooLarge { limit, .. }) => {
            assert_eq!(limit, MAX_OUTPUT_BYTES);
        }
        other => panic!("expected OutputTooLarge, got {other:?}"),
    }
}

#[test]
fn sessionless_read_directory_is_not_regular() {
    let env = env();
    match env.read(read_of("sub")) {
        Err(FileOperationError::NotRegular {
            kind: FileEntryKind::Directory,
            ..
        }) => {}
        other => panic!("expected NotRegular(Directory), got {other:?}"),
    }
}

#[test]
fn sessionless_read_symlink_is_not_regular_not_followed() {
    let env = env();
    match env.read(read_of("link.txt")) {
        Err(FileOperationError::NotRegular {
            kind: FileEntryKind::Symlink,
            ..
        }) => {}
        // If the base builder dropped the symlink, it must be absent, never followed.
        Err(FileOperationError::NotFound(_)) => {}
        other => panic!("expected NotRegular(Symlink) or NotFound, got {other:?}"),
    }
}

#[test]
fn sessionless_read_symlink_parent_is_not_followed() {
    let env = env();
    // linkdir -> sub is a symlinked parent directory. Classification treats a
    // symlink ancestor as blocking and never joins through it, so the read
    // surfaces as NotRegular(Symlink), never not_found or the target.
    assert!(matches!(
        env.read(read_of("linkdir/nested.txt")),
        Err(FileOperationError::NotRegular {
            kind: FileEntryKind::Symlink,
            ..
        })
    ));
    assert!(matches!(
        env.layerstack
            .read_current_window(&layer_path("linkdir/nested.txt"), 1, 10, MAX_OUTPUT_BYTES)
            .expect("read window"),
        ManifestReadWindow::Symlink
    ));
}

// ---------- sessionless write ----------

#[test]
fn sessionless_write_create_then_update_with_blame() {
    let env = env();
    let created = env
        .write(write_of("new/file.txt", "x\ny\n", "req-a"))
        .expect("create");
    assert_eq!(created.kind.as_str(), "create");
    assert_eq!(created.bytes_written, 4);
    assert_eq!(
        env.read(read_of("new/file.txt")).expect("read").content,
        "x\ny"
    );

    let updated = env
        .write(write_of("new/file.txt", "z\ny\n", "req-b"))
        .expect("update");
    assert_eq!(updated.kind.as_str(), "update");

    let ranges = env.file.blame("new/file.txt").expect("blame");
    assert_eq!(
        ranges[0].owner, "operation:req-b",
        "changed line -> new owner"
    );
    assert!(
        ranges.iter().any(|r| r.owner == "operation:req-a"),
        "unchanged line keeps prior owner: {ranges:?}"
    );
}

#[test]
fn sessionless_write_to_directory_is_not_regular() {
    let env = env();
    match env.write(write_of("sub", "x", "req")) {
        Err(FileOperationError::NotRegular {
            kind: FileEntryKind::Directory,
            ..
        }) => {}
        other => panic!("expected NotRegular(Directory), got {other:?}"),
    }
}

#[test]
fn sessionless_concurrent_writes_serialize_without_lost_update() {
    let env = env();
    let layerstack = Arc::clone(&env.layerstack);
    let file = Arc::clone(&env.file);
    let workspace_session = Arc::clone(&env.workspace_session);
    let handles: Vec<_> = (0..8)
        .map(|i| {
            let (file, layerstack, workspace_session) = (
                Arc::clone(&file),
                Arc::clone(&layerstack),
                Arc::clone(&workspace_session),
            );
            thread::spawn(move || {
                file.write(
                    &layerstack,
                    &workspace_session,
                    write_of("race.txt", &format!("writer-{i}\n"), &format!("req-{i}")),
                )
                .expect("concurrent write")
            })
        })
        .collect();
    for handle in handles {
        handle.join().expect("thread");
    }
    // Final content is exactly one complete write, never interleaved.
    let content = env.read(read_of("race.txt")).expect("read").content;
    assert!(
        (0..8).any(|i| content == format!("writer-{i}")),
        "final content must be one complete write, got {content:?}"
    );
}

#[test]
fn sessionless_write_partial_failure_commits_no_layer() {
    let env = env();
    // Arm the layerstack publish failpoint for the next commit.
    let marker_dir = env.layerstack.layer_stack_root().join(".layer-metadata");
    std::fs::create_dir_all(&marker_dir).expect("mkdir metadata");
    std::fs::write(marker_dir.join("fail-next-publish"), b"").expect("arm failpoint");
    std::env::set_var("SANDBOX_LAYERSTACK_ENABLE_TEST_FAILPOINTS", "1");

    let result = env.write(write_of("wont-commit.txt", "nope", "req-fail"));
    std::env::remove_var("SANDBOX_LAYERSTACK_ENABLE_TEST_FAILPOINTS");

    assert!(result.is_err(), "injected failure must fail the write");
    assert!(
        matches!(
            env.read(read_of("wont-commit.txt")),
            Err(FileOperationError::NotFound(_))
        ),
        "no layer should be committed"
    );
}

// ---------- sessionless edit ----------

#[test]
fn sessionless_edit_unique_replacement() {
    let env = env();
    let out = env
        .edit(edit_of(
            "readme.txt",
            vec![edit_op("line2", "LINE2", false)],
            "e1",
        ))
        .expect("edit");
    assert_eq!(out.replacements, 1);
    assert_eq!(out.edits_applied, 1);
    assert_eq!(
        env.read(read_of("readme.txt")).expect("read").content,
        "line1\nLINE2\nline3"
    );
}

#[test]
fn sessionless_edit_errors() {
    let env = env();
    assert!(matches!(
        env.edit(edit_of("readme.txt", vec![], "e")),
        Err(FileOperationError::NoEdits)
    ));
    assert!(matches!(
        env.edit(edit_of(
            "readme.txt",
            vec![edit_op("same", "same", false)],
            "e"
        )),
        Err(FileOperationError::NoChanges(_))
    ));
    assert!(matches!(
        env.edit(edit_of(
            "readme.txt",
            vec![edit_op("absent", "x", false)],
            "e"
        )),
        Err(FileOperationError::EditNotFound { .. })
    ));
    // "line" appears on all three lines -> not unique without replace_all.
    assert!(matches!(
        env.edit(edit_of(
            "readme.txt",
            vec![edit_op("line", "L", false)],
            "e"
        )),
        Err(FileOperationError::EditNotUnique { count: 3, .. })
    ));
}

#[test]
fn sessionless_edit_replace_all_and_ordered_edits() {
    let env = env();
    let out = env
        .edit(edit_of(
            "readme.txt",
            vec![edit_op("line", "L", true)],
            "ea",
        ))
        .expect("replace_all");
    assert_eq!(out.replacements, 3);
    assert_eq!(
        env.read(read_of("readme.txt")).expect("read").content,
        "L1\nL2\nL3"
    );

    // Ordered: second edit depends on the first's output.
    let out = env
        .edit(edit_of(
            "readme.txt",
            vec![edit_op("L1", "A", false), edit_op("A\nL2", "A2", false)],
            "eb",
        ))
        .expect("ordered");
    assert_eq!(out.replacements, 2);
    assert_eq!(
        env.read(read_of("readme.txt")).expect("read").content,
        "A2\nL3"
    );
}

#[test]
fn sessionless_edit_invalid_utf8_is_rejected() {
    let env = env();
    assert!(matches!(
        env.edit(edit_of("binary.dat", vec![edit_op("x", "y", false)], "e")),
        Err(FileOperationError::NotUtf8(_))
    ));
}

#[test]
fn sessionless_edit_over_max_edit_bytes_is_file_too_large() {
    let env = env();
    // huge.txt is one byte over MAX_EDIT_BYTES (4 MiB); the classify pass in
    // amend_path must reject it as FileTooLarge before loading the whole file.
    match env.edit(edit_of("huge.txt", vec![edit_op("x", "y", false)], "r")) {
        Err(FileOperationError::FileTooLarge { limit, .. }) => {
            assert_eq!(limit, 4 * 1024 * 1024);
        }
        other => panic!("expected FileTooLarge, got {other:?}"),
    }
}

#[test]
fn sessionless_edit_line_ending_only_edit_mixed_with_real_edit_is_accepted() {
    // edit.ts symmetry (P1 Option A): a per-edit no-op is gated on the *raw*
    // strings, so a line-ending-only edit ("beta\r\n" -> "beta\n", normalized
    // equal) is allowed when batched with a real edit. The net change is governed
    // by the final current == original check, not a per-edit normalized guard.
    let env = env();
    env.write(write_of("mix.txt", "alpha\nbeta\n", "w"))
        .expect("write");
    let out = env
        .edit(edit_of(
            "mix.txt",
            vec![
                edit_op("alpha", "ALPHA", false),
                edit_op("beta\r\n", "beta\n", false),
            ],
            "e",
        ))
        .expect("mixed batch is a net change and is accepted");
    assert_eq!(out.replacements, 2);
    assert_eq!(
        env.read(read_of("mix.txt")).expect("read").content,
        "ALPHA\nbeta"
    );
}

// ---------- path handling ----------

#[test]
fn path_absolute_under_root_equals_repo_relative() {
    let env = env();
    let abs = env.workspace_root.join("readme.txt");
    let via_abs = env
        .read(read_of(abs.to_str().expect("utf8 path")))
        .expect("abs read");
    let via_rel = env.read(read_of("readme.txt")).expect("rel read");
    assert_eq!(via_abs.content, via_rel.content);
    assert_eq!(via_abs.path, "readme.txt");
}

#[test]
fn path_escapes_are_invalid() {
    let env = env();
    for bad in ["/etc/passwd", "../escape", "", "a\0b", "../../x"] {
        assert!(
            matches!(
                env.read(read_of(bad)),
                Err(FileOperationError::InvalidPath(_))
            ),
            "path {bad:?} should be InvalidPath"
        );
    }
}

#[test]
fn path_dot_components_normalize() {
    let env = env();
    assert_eq!(
        env.read(read_of("./readme.txt")).expect("dot read").path,
        "readme.txt"
    );
}

// ---------- dispatch (parse-time validation) ----------

#[test]
fn dispatch_file_read_limit_out_of_range_is_invalid_request() {
    let env = env();
    let operations = dispatch_operations(&env);
    // The 1..=2000 limit range is enforced only at the dispatch/parse seam, which
    // the direct-ReadInput tests bypass; both out-of-range bounds must reject.
    for limit in [0_i64, 2001] {
        let response = sandbox_runtime::dispatch_operation(
            &operations,
            &runtime_request("file_read", json!({ "path": "readme.txt", "limit": limit })),
        )
        .into_json_value();
        assert_eq!(
            response["error"]["kind"], "invalid_request",
            "limit {limit} must be invalid_request, got {response}"
        );
    }
}

fn dispatch_operations(env: &Env) -> SandboxRuntimeOperations {
    let command = Arc::new(CommandOperationService::new(
        Arc::clone(&env.workspace_session),
        sandbox_runtime::command::CommandConfig::default(),
        Observer::disabled(),
    ));
    SandboxRuntimeOperations::new(
        command,
        Arc::clone(&env.workspace_session),
        Arc::clone(&env.layerstack),
        Arc::clone(&env.file),
    )
}

fn runtime_request(op: &str, args: serde_json::Value) -> Request {
    Request::new(op, "req-test", CliOperationScope::system(), args)
}

// ---------- layerstack helpers ----------

#[test]
fn read_current_window_classifies_entries() {
    let env = env();
    let cap = MAX_OUTPUT_BYTES;
    assert!(matches!(
        env.layerstack
            .read_current_window(&layer_path("nope"), 1, 10, cap)
            .expect("read window"),
        ManifestReadWindow::Absent
    ));
    assert!(matches!(
        env.layerstack
            .read_current_window(&layer_path("sub"), 1, 10, cap)
            .expect("read window"),
        ManifestReadWindow::Directory
    ));
    assert!(matches!(
        env.layerstack
            .read_current_window(&layer_path("binary.dat"), 1, 10, cap)
            .expect("read window"),
        ManifestReadWindow::NotUtf8
    ));
    assert!(matches!(
        env.layerstack
            .read_current_window(&layer_path("wide.txt"), 1, 2000, cap)
            .expect("read window"),
        ManifestReadWindow::OutputTooLarge { .. }
    ));
    assert!(matches!(
        env.layerstack
            .read_current_window(&layer_path("readme.txt"), 1, 10, cap)
            .expect("read window"),
        ManifestReadWindow::Text { total_lines: 3, .. }
    ));
}

#[test]
fn read_current_window_large_file_not_rejected_for_total_size() {
    let env = env();
    // big.txt is large in total but a small window is fine.
    let window = env
        .layerstack
        .read_current_window(&layer_path("big.txt"), 1, 3, MAX_OUTPUT_BYTES)
        .expect("read window");
    assert!(matches!(
        window,
        ManifestReadWindow::Text { num_lines: 3, .. }
    ));
}

fn layer_path(path: &str) -> LayerPath {
    LayerPath::parse(path).expect("layer path")
}

// ---------- session (explicit run_file_op hook) ----------

#[test]
fn session_write_then_read_visible_and_not_published() {
    let env = env();
    let id = env.create_session();

    env.fake
        .push_run_file_op_result(Ok(run_result_ok(&FileRunnerResult::Write {
            existed: false,
            bytes_written: 5,
        })));
    let write = env
        .write(WriteInput {
            path: "s.txt".into(),
            content: "hello".into(),
            request_id: "r".into(),
            workspace_session_id: Some(id.clone()),
        })
        .expect("session write");
    assert_eq!(write.kind.as_str(), "create");

    // Session write does not publish: sessionless read cannot see it.
    assert!(matches!(
        env.read(read_of("s.txt")),
        Err(FileOperationError::NotFound(_))
    ));
    assert!(
        env.file.blame("s.txt").is_err(),
        "session write must not publish"
    );

    // A subsequent session read sees the namespace content (scripted).
    env.fake
        .push_run_file_op_result(Ok(run_result_ok(&FileRunnerResult::ReadWindow {
            existed: true,
            content: "hello".into(),
            start_line: 1,
            num_lines: 1,
            total_lines: 1,
            bytes_read: 5,
            total_bytes: 5,
            next_offset: None,
            truncated: false,
        })));
    let read = env
        .read(ReadInput {
            path: "s.txt".into(),
            offset: None,
            limit: None,
            workspace_session_id: Some(id.clone()),
        })
        .expect("session read");
    assert_eq!(read.content, "hello");

    // Boundary: the file service went through run_file_op, not the namespace directly.
    let ops = env.fake.run_file_op_calls();
    assert_eq!(ops.len(), 2, "one write + one read op reached the runner");
}

#[test]
fn session_edit_is_read_modify_write() {
    let env = env();
    let id = env.create_session();
    env.fake
        .push_run_file_op_result(Ok(run_result_ok(&FileRunnerResult::ReadFile {
            existed: true,
            bytes_b64: STANDARD.encode("foo\nbar\n"),
            total_bytes: 8,
        })));
    env.fake
        .push_run_file_op_result(Ok(run_result_ok(&FileRunnerResult::Write {
            existed: true,
            bytes_written: 8,
        })));
    let out = env
        .edit(EditInput {
            path: "s.txt".into(),
            edits: vec![edit_op("foo", "FOO", false)],
            request_id: "r".into(),
            workspace_session_id: Some(id),
        })
        .expect("session edit");
    assert_eq!(out.replacements, 1);
    let last_write = env
        .fake
        .run_file_op_calls()
        .into_iter()
        .find_map(|(_, op)| match op {
            FileRunnerOp::Write { content, .. } => Some(content),
            _ => None,
        })
        .expect("edit issued a Write");
    assert_eq!(last_write, "FOO\nbar\n");
}

#[test]
fn session_missing_and_runner_errors_map_correctly() {
    let env = env();
    let id = env.create_session();

    // ReadWindow existed=false -> NotFound.
    env.fake
        .push_run_file_op_result(Ok(run_result_ok(&FileRunnerResult::ReadWindow {
            existed: false,
            content: String::new(),
            start_line: 1,
            num_lines: 0,
            total_lines: 0,
            bytes_read: 0,
            total_bytes: 0,
            next_offset: None,
            truncated: false,
        })));
    assert!(matches!(
        env.read(read_session("gone.txt", &id)),
        Err(FileOperationError::NotFound(_))
    ));

    // NotRegular(Symlink) -> invalid_request NotRegular.
    env.fake
        .push_run_file_op_result(Ok(run_result_err(&FileRunnerError::NotRegular {
            kind: FileRunnerEntryKind::Symlink,
        })));
    assert!(matches!(
        env.read(read_session("alink", &id)),
        Err(FileOperationError::NotRegular {
            kind: FileEntryKind::Symlink,
            ..
        })
    ));

    // Runner Io (transport) -> operation_failed (WorkspaceSession).
    env.fake
        .push_run_file_op_result(Ok(run_result_err(&FileRunnerError::Io {
            path: "s".into(),
            message: "boom".into(),
        })));
    assert!(matches!(
        env.read(read_session("x", &id)),
        Err(FileOperationError::WorkspaceSession(_))
    ));
}

#[test]
fn session_unknown_session_is_not_found() {
    let env = env();
    assert!(matches!(
        env.read(read_session("x", &WorkspaceSessionId("missing".into()))),
        Err(FileOperationError::WorkspaceSessionNotFound(_))
    ));
}

fn read_session(path: &str, id: &WorkspaceSessionId) -> ReadInput {
    ReadInput {
        path: path.into(),
        offset: None,
        limit: None,
        workspace_session_id: Some(id.clone()),
    }
}

// ---------- runner placement (engine + fake launcher) ----------

#[test]
fn file_op_launch_uses_cgroup_and_setup_timeout_overlay_uses_none() {
    let launcher = FakeLauncher::new();
    launcher.push_script(FakeRunnerScript::completes(run_result_ok(
        &FileRunnerResult::Write {
            existed: false,
            bytes_written: 3,
        },
    )));
    let spans = Arc::new(SpanRegistry::new(Observer::disabled()));
    let engine =
        NamespaceExecutionEngine::<()>::with_launcher(Box::new(launcher.clone()), spans, 8, 7.5);

    let cgroup = PathBuf::from("/sys/fs/cgroup/workspace-ws-1/cgroup.procs");
    let file_op = engine
        .run_file_op(
            target(),
            engine.allocate_id(),
            serde_json::json!({ "op": "write", "rel": "a.txt", "content": "abc" }),
            Some(cgroup.clone()),
        )
        .expect("spawn file op");
    file_op.wait().expect("file op result");

    // Overlay mount launches with no cgroup placement (spawn records it; the
    // fake child parks, so we do not wait on it).
    let _overlay = engine
        .mount_overlay(target(), engine.allocate_id())
        .expect("spawn overlay");

    assert_eq!(launcher.file_op_setup_timeouts(), vec![7.5]);
    assert_eq!(
        launcher.recorded_cgroup_procs_paths(),
        vec![Some(cgroup), None],
        "file op passes the session cgroup.procs; overlay mount passes none"
    );
}

fn target() -> NamespaceTarget {
    NamespaceTarget {
        workspace_root: PathBuf::from("/workspace/session"),
        layer_paths: Vec::new(),
        upperdir: None,
        workdir: None,
        ns_fds: NsFds {
            user: None,
            mnt: None,
            pid: None,
            net: None,
        },
    }
}
