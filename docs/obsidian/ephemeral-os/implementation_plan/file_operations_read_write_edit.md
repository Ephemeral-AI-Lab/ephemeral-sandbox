---
title: Runtime File Operations — read / write / edit
tags:
  - ephemeral-os
  - layerstack
  - sandbox
  - runtime
  - file
  - namespace
  - implementation-plan
status: implementation_plan
updated: 2026-07-02
---

# Runtime File Operations — read / write / edit

## Goal

Add `read`, `write`, and `edit` runtime operations to the sandbox `file`
domain. They must be **signature-symmetric** to the `ephemeral-agent` local-os
tools of the same name, plus one optional argument: `workspace_session_id`,
which behaves exactly like `exec_command` — resolve that session when present,
operate against the layerstack snapshot when absent.

Target shape:

```text
file_read  --path P [--offset N] [--limit N] [--workspace-session-id ID]
file_write --path P  content     [--workspace-session-id ID]
file_edit  --path P  edits[]      [--workspace-session-id ID]

workspace_session_id present -> run the op INSIDE the session namespace (through the
                                mounted overlay the shell also writes through); DO NOT publish
workspace_session_id absent  -> read from latest snapshot; write/edit publish a layer
```

The three impls ship next to `blame` as `impl FileService` blocks:

```text
crates/sandbox-runtime/operation/src/file/service/impls/
  blame.rs   (exists)
  read.rs    (new)
  write.rs   (new)
  edit.rs    (new)
```

## Symmetry Contract

The local-os reference lives at
`ephemeral-agent/packages/ephai-agent/src/tools/workspace/local/{read,write,edit}.ts`.
Match argument names and meaning; add `workspace_session_id`.

| Op | Args (symmetric) | Added |
|---|---|---|
| read | `file_path`, `offset?` (1-indexed), `limit?` (default 2000) | `workspace_session_id?` |
| write | `file_path`, `content` | `workspace_session_id?` |
| edit | `file_path`, `edits: [{ old_string, new_string, replace_all? }]` | `workspace_session_id?` |

Output fields keep the local-os names that are meaningful in the sandbox; drop
host-only fields (`mtime_ms`, `previous_mtime_ms`) that a layerstack publish has
no faithful analog for.

```text
read  -> { file_path, content, start_line, num_lines, total_lines,
           bytes_read, total_bytes, next_offset, truncated }
write -> { type: "create" | "update", file_path, bytes_written }
edit  -> { type: "edit", file_path, edits_applied, replacements, bytes_written }
```

Read validation and text shaping also follow the local-os tool: `limit` defaults
to 2000 and must be `1..=2000`; `offset <= 1` starts at line 1; UTF-8 text drops
a leading BOM and normalizes `\r\n` / `\r` to `\n` before line windowing. Large
reads stream through the file and cap selected output bytes; they do **not**
reject a file merely because the whole file exceeds the output cap.

Intentional sandbox-specific divergence: no extension-based binary denylist.
Sandbox reads reject non-UTF-8 content at decode time. Symlinks and directories
are rejected as invalid request on both backends instead of being followed or
encoded as bytes; for session writes this includes symlink parent components,
not just the final path.

## Operation Matrix

The two axes are the operation and whether a `workspace_session_id` was given.
Every cell reuses existing runtime primitives; the only layerstack change is a
classified read/inspect API, with no storage-format change.

| | no `workspace_session_id` | `workspace_session_id` present |
|---|---|---|
| **read** | `LayerStackService::head` + `read_manifest_text_window` over the active manifest | resolve session; run a namespace read-window against the session's mounted workspace |
| **write** | `LayerChange::Write` published on head as `operation:<request_id>`; blame attributed inside publish | run a namespace file write against the session's mounted workspace; **no publish** — attributed later on session capture |
| **edit** | read merged → apply ordered edits → publish on head (OCC retry re-reads and re-applies) | run namespace file read → apply ordered edits → namespace file write; **no publish** |

This mirrors `exec_command`: in-session mutations stay in the session overlay and
are attributed to `workspace_session:<id>` when the session is later captured;
sessionless mutations publish immediately and are attributed to
`operation:<id>`.

### Sessionless vs session — two backends

| | sessionless (layerstack backend) | session (namespace backend) |
|---|---|---|
| touches | layerstack CAS only — **no namespace, no mount, no fork** | the session's live overlay, **through the mount**, via a one-shot setns runner |
| source of truth | latest published snapshot (`head`); the host `workspace_root` bind is detached after base build (`services.rs`), so the snapshot — not any host path — is authoritative | the merged overlay view the shell also sees |
| write result | a published layer, immediately | an `upperdir` change, captured later |
| attribution | `operation:<request_id>` at publish | `workspace_session:<id>` at capture |
| sees a live session's uncommitted edits? | no — by design (isolation) | yes — its own |

The asymmetry with `exec_command` is deliberate: a sessionless *command* still
needs a one-shot namespace because it runs arbitrary code, but a sessionless
*file op* needs no namespace at all — the snapshot is sufficient, so it stays on
the cheap layerstack path. The namespace is entered **only** when a
`workspace_session_id` pins a live overlay whose coherence must be preserved.

## Architecture

### Why the impls take collaborators as parameters

`FileService` today owns only the append-only auditability store (`blame` +
`record_layer_publish`). Critically, `LayerStackService` **already holds**
`Arc<FileService>` so it can write blame events after each commit:

```text
LayerStackService  --owns Arc-->  FileService(audit store)
```

`read`/`write`/`edit` need the layerstack and the workspace-session service. If
`FileService` owned those back, construction would be an unbreakable cycle
(`layerstack` needs `file` at build time, so `file` cannot need `layerstack`).

Therefore the new impls receive their collaborators **by parameter**, not as
fields. `FileService` stays audit-only and does not gain `workspace_root`.
Path mapping uses the session handle's `workspace_root` when a session is
present; sessionless absolute-path mapping reads the existing layerstack
workspace binding.

```rust
impl FileService {
    pub fn read(
        &self,
        layerstack: &LayerStackService,
        workspace_session: &WorkspaceSessionService,
        input: ReadInput,
    ) -> Result<ReadOutput, FileOperationError>;

    pub fn write(
        &self,
        layerstack: &LayerStackService,
        workspace_session: &WorkspaceSessionService,
        input: WriteInput,
    ) -> Result<WriteOutput, FileOperationError>;

    pub fn edit(
        &self,
        layerstack: &LayerStackService,
        workspace_session: &WorkspaceSessionService,
        input: EditInput,
    ) -> Result<EditOutput, FileOperationError>;
}
```

The dispatch layer already holds every service on `SandboxRuntimeOperations`, so
it passes them through with no new wiring:

```rust
operations.file.read(
    operations.layerstack.as_ref(),
    operations.workspace_session.as_ref(),
    input,
)
```

Dependency direction stays acyclic: `file ops -> {layerstack, workspace_session,
namespace-execution}` and `layerstack -> file(audit)`. The namespace file
runner is a private helper, not a new top-level service and not a
`SandboxRuntimeOperations` field.

### Module layout

```text
file/
  error.rs        + FileOperationError (peer to FileError)
  mod.rs          re-export FileOperationError + DTOs
  audit.rs        (unchanged) record_layer_publish
  service.rs      + mod dto / support / namespace; re-export DTOs
  service/
    core.rs       unchanged: FileService stays auditability-store only
    store.rs      (unchanged)
    dto.rs        (new) Read/Write/Edit In & Out, EditOp
    support.rs    (new) resolve_layer_path + publish_on_head + windowing helpers
    namespace.rs  (new) run in-session file ops through the mount namespace
    impls/
      mod.rs      + mod read; mod write; mod edit;
      blame.rs    (unchanged)
      read.rs     (new) impl FileService::read
      write.rs    (new) impl FileService::write
      edit.rs     (new) impl FileService::edit
```

## Path Semantics

The protocol field is `file_path` to match the local-os tools. The CLI may keep
`--path` as a flag alias, but dispatch maps it to `file_path`.

`file_path` is accepted as **either** an absolute path under the configured
workspace root **or** a repo-relative path, then normalized through `LayerPath`
(the same normalization `blame` uses, where `./src/x` == `src/x`).

```text
if file_path is absolute and under workspace_root -> strip prefix -> LayerPath::parse
else                                               -> LayerPath::parse(file_path as-is)
absolute but outside workspace_root                -> InvalidPath (LayerPath rejects leading '/')
```

The workspace root source is contextual:

```text
session present -> handler.handle.workspace_root
session absent  -> layerstack workspace binding
```

Do not store this on `FileService`.

```rust
// file/service/support.rs
pub(super) fn resolve_layer_path(
    workspace_root: &Path,
    file_path: &str,
) -> Result<LayerPath, FileOperationError> {
    let candidate = match Path::new(file_path).strip_prefix(workspace_root) {
        Ok(rel) => rel
            .to_str()
            .ok_or_else(|| FileOperationError::InvalidPath(file_path.to_owned()))?
            .to_owned(),
        Err(_) => file_path.to_owned(),
    };
    LayerPath::parse(&candidate)
        .map_err(|_| FileOperationError::InvalidPath(file_path.to_owned()))
}
```

## Session Filesystem Access (workspace_session_id present)

### Required target

A live session's merged workspace exists inside the session mount namespace.
Session file operations must therefore run inside that namespace against
`handle.workspace_root`. Do not mutate `entry.upperdir` from the host.

Host-side `capture_changes` may read `upperdir` after command execution, but
that does not make host-side writes to a mounted overlay correct. The runtime
also supports still-running commands in caller-owned sessions, so turn-based
non-concurrency is not an invariant.

### Unified namespace runner (shell + file)

A session file op and `exec_command` are the **same shape**: `setns` into the
session's holder namespaces, run a body, return a result. We add the file body to
the **existing ns-runner harness** rather than building a parallel mechanism. The
runner is deliberately two primitives; `edit` composes them in the file domain:

```text
FileRunnerOp =
  Read  { rel, max_bytes }
  Write { rel, content }

FileRunnerResult =
  Read  { bytes, existed, total_bytes }
  Write { existed, bytes_written }
```

The dispatch seam already exists. `sandbox-daemon ns-runner` selects a body by
mode flag, and every mode shares one `setns` join and one request/result
protocol — the file body is the third body next to shell and overlay-mount:

```text
sandbox-daemon ns-runner (--shell-exec | --mount-overlay | --file-op) --request-fd FD --result-fd FD
  --shell-exec    -> daemon/src/runner/shell.rs         -> runner::run           (setns → shell, interactive/PTY)
  --mount-overlay -> daemon/src/runner/mount_overlay.rs -> setns_overlay_mount   (setns → mount, one-shot)
  --file-op (NEW) -> daemon/src/runner/file_op.rs       -> runner::run_file_op   (setns → file,  one-shot)

enum NsRunnerOperation { ShellExec, MountOverlay, FileOp }   // Run -> ShellExec (rename); FileOp is new
```

The mode taxonomy becomes **explicit and symmetric** — three named modes, no
implicit default:

- Rename the module-private `NsRunnerOperation::Run` -> `ShellExec` (3 references
  in `daemon/src/runner/mod.rs`; internal only, no wire impact).
- Add `NsRunnerOperation::FileOp`, its `--file-op` parser arm, and dispatch to
  `file_op::run`.
- Add the `--shell-exec` parser arm and drop the `mode.unwrap_or(Run)` default so
  every spawn names its mode (`mode.ok_or_else(...)`).
- Split the launcher's overloaded `mode_flag: Option<&str>` — today it doubles as
  both the argv flag and the wait discipline (`None` -> unbounded interactive
  shell; `Some` -> setup-timeout one-shot). After the split it carries an
  always-present mode flag plus a separate wait discipline
  (`Interactive | OneShot { setup_timeout }`, already implied by `spawn_pty` vs the
  one-shot spawn), so an explicit `--shell-exec` no longer inherits the setup
  timeout.

`ns-runner` is an internal same-binary subcommand, so the flip needs no
compatibility window. Sequencing: `--file-op` is one-shot and does **not** depend
on the shell rename or the wait-discipline split — it can land first with
`Some("--file-op")` on the existing launcher, and the `--shell-exec`/explicit-mode
cleanup can follow (or land together).

Shared and unchanged:

- `setns_user_mnt` (`runner/setns/namespaces.rs`): `setns(user)` then
  `setns(mnt)` — enough for filesystem ownership and the mounted overlay view.
- `NamespaceRunnerRequest { request_id, args, workspace_root, ns_fds, … }` in,
  `RunResult { exit_code, payload }` out (`runner/protocol.rs`). The file body
  reuses `workspace_root` + `ns_fds` already in the request and needs **no PTY and
  no transcript**, so its launch is the non-interactive `mount_overlay` shape, not
  the shell shape.

New pieces, each a sibling of an existing one — note the launch path is the
**`mount_overlay` peer**, driven by the workspace `NamespaceRuntime`, not the
command engine (`WorkspaceSessionService` holds `Arc<WorkspaceRuntimeService>`, not
the exec engine):

```text
namespace-process   runner/setns/file_op.rs   setns_user_mnt then Read/Write at workspace_root/rel   (peer of setns_overlay_mount)
namespace-process   runner::run_file_op        entry mirroring runner::run
namespace-execution engine file-op launch      one-shot, awaits RunResult                             (peer of engine.mount_overlay)
sandbox-daemon      runner/file_op.rs           --file-op body + dispatch in runner/mod.rs             (peer of runner/mount_overlay.rs)
workspace runtime   NamespaceRuntime file-op    launch the runner for a resolved entry                (peer of its mount_overlay)
workspace-session   run_file_op(&handler, op)   resolve entry, delegate to the workspace runtime      (peer of resolve_session / capture)
```

The runner executes **after `setns`**, so whiteouts, opaque dirs, copy-up, and
cache coherence are the mounted overlay's job, not reimplemented in the operation
crate. The file-type policy is explicit and shared with sessionless reads:
regular files are supported; absent paths return `existed=false`; directories,
symlinks, and other non-regular files are invalid request errors. The `file`
domain calls exactly one method — `workspace_session.run_file_op(...)` — and
never learns `setns` or overlay detail (boundary law). `edit` issues one `Read`
and one `Write` through this path with `apply_edits` in between; the two-spawn
read-modify-write is non-atomic under a concurrent in-session writer, the same
non-atomicity the sessionless read→publish path already has.

`Read` checks metadata before loading bytes; a regular file larger than
`max_bytes` returns `FileTooLarge` with the file size and limit.

`Write` must still be atomic inside the namespace: inspect the existing path
with `symlink_metadata`, reject directories/symlinks/non-regular files, create
the parent directory when absent, write a same-directory temp file, fsync it,
preserve the existing mode when updating a regular file, then `rename` over the
target. The result's `existed` flag is the pre-write regular-file existence.

### Pseudo code — read / write / edit

```text
resolve_session_target(workspace_session, file_path, id):
    handler = workspace_session.resolve_session(id)          # Err NotFound -> WorkspaceSessionNotFound
    entry   = handler.handle.entry()                         # ns_fds + mounted workspace_root
    rel     = resolve_layer_path(handler.handle.workspace_root, file_path)
    return (rel, entry)

resolve_layer_target(layerstack, file_path):
    workspace_root = layerstack.workspace_root()
    rel = resolve_layer_path(workspace_root, file_path)
    return rel
```

Read:

```text
read(input):
    if input.workspace_session_id:
        (rel, entry) = resolve_session_target(
            workspace_session, input.file_path, input.workspace_session_id)
        current = namespace_read(entry, rel, MAX_READ_BYTES)
        if !current.existed: return Err(NotFound(rel))
        bytes = current.bytes
        total_bytes = current.total_bytes
    else:
        rel = resolve_layer_target(layerstack, input.file_path)
        (manifest, _) = layerstack.head()
        read = layerstack.read_manifest_file(manifest, rel, MAX_READ_BYTES)
        if read is Absent: return Err(NotFound(rel))
        if read is NotRegular(kind): return Err(NotRegular{rel, kind})
        if read is TooLarge(size, limit): return Err(FileTooLarge{rel, size, limit})
        bytes = read.bytes
        total_bytes = read.total_bytes

    text = utf8(bytes) else Err(NotUtf8(rel))
    return window(text, input.offset, input.limit, rel, total_bytes)
```

Write:

```text
write(input):
    if input.workspace_session_id:
        (rel, entry) = resolve_session_target(
            workspace_session, input.file_path, input.workspace_session_id)
        result = namespace_write(entry, rel, bytes(input.content))
        # NO publish; attributed to workspace_session:<id> later, on session capture
        return { type: result.existed ? "update" : "create",
                 file_path: rel, bytes_written: result.bytes_written }

    rel = resolve_layer_target(layerstack, input.file_path)
    owner = "operation:" + input.request_id
    result = publish_on_head(layerstack, rel, owner, manifest =>
        status = layerstack.inspect_manifest_path(manifest, rel)
        if status is NotRegular(kind): return Err(NotRegular{rel, kind})
        HeadAttempt {
            content: bytes(input.content),
            existed_before: status is File,
        })                                                   # last-writer-wins

    return { type: result.existed_before ? "update" : "create",
             file_path: rel, bytes_written: result.bytes_written }
```

Edit:

```text
edit(input):
    if input.edits is empty: return Err(NoEdits)

    if input.workspace_session_id:
        (rel, entry) = resolve_session_target(
            workspace_session, input.file_path, input.workspace_session_id)
        current = namespace_read(entry, rel, MAX_EDIT_BYTES)
        if !current.existed: return Err(NotFound(rel))
        text = utf8(current.bytes) else Err(NotUtf8(rel))
        (edited, replacements) = apply_edits(text, input.edits, rel)
        result = namespace_write(entry, rel, bytes(edited))
        # NO publish
        return { type: "edit", file_path: rel,
                 edits_applied: len(input.edits), replacements,
                 bytes_written: result.bytes_written }

    rel = resolve_layer_target(layerstack, input.file_path)
    owner = "operation:" + input.request_id
    replacements = 0
    result = publish_on_head(layerstack, rel, owner, manifest =>
        read = layerstack.read_manifest_file(manifest, rel, MAX_EDIT_BYTES)
        if read is Absent: return Err(NotFound(rel))
        if read is NotRegular(kind): return Err(NotRegular{rel, kind})
        if read is TooLarge(size, limit): return Err(FileTooLarge{rel, size, limit})
        text = utf8(read.bytes) else Err(NotUtf8(rel))
        (edited, count) = apply_edits(text, input.edits, rel)
        replacements = count
        HeadAttempt { content: bytes(edited), existed_before: true })

    return { type: "edit", file_path: rel,
             edits_applied: len(input.edits), replacements,
             bytes_written: result.bytes_written }
```

Shared edit rules:

```text
apply_edits(text, edits, path):
    cur = normalize_line_endings(text)
    original = cur
    for e in edits:
        old = normalize_line_endings(e.old_string)
        new = normalize_line_endings(e.new_string)
        if old == "":  return Err(EditNotFound{path})
        if old == new: return Err(NoChanges{path})
        count = occurrences(cur, old)
        if count == 0:                      return Err(EditNotFound{path, snippet(old)})
        if count > 1 and not e.replace_all: return Err(EditNotUnique{path, count, snippet})
        if e.replace_all: cur = replace_all(cur, old, new); replacements += count
        else:             cur = replace_first(cur, old, new); replacements += 1
    if cur == original: return Err(NoChanges{path})
    return (restore_original_line_endings(cur, text), replacements)
```

## Layerstack Access (no workspace_session_id)

Three thin primitives are added to `LayerStackService` so raw `LayerStack`
handling stays inside the layerstack service (boundary law). The read/inspect
results must preserve file-type classification; `Option<Vec<u8>>` is not enough
because the existing projection can distinguish absent paths, regular files,
symlinks, directories, and oversized files.

```rust
// layerstack/service/impls/read.rs
impl LayerStackService {
    pub fn head(&self)
        -> Result<(Manifest, LayerStackRevision), LayerStackServiceError>;

    pub fn read_manifest_file(&self, manifest: &Manifest, rel: &str, max_bytes: usize)
        -> Result<ManifestFileRead, LayerStackServiceError>;

    pub fn inspect_manifest_path(&self, manifest: &Manifest, rel: &str)
        -> Result<ManifestPathStatus, LayerStackServiceError>;

    pub fn workspace_root(&self) -> Result<PathBuf, LayerStackServiceError>;
}

pub enum ManifestPathStatus {
    Absent,
    File { total_bytes: u64 },
    NotRegular { kind: FileEntryKind },
}

pub enum ManifestFileRead {
    Absent,
    File { bytes: Vec<u8>, total_bytes: u64 },
    NotRegular { kind: FileEntryKind },
    TooLarge { size: u64, limit: usize },
}

pub enum FileEntryKind {
    Directory,
    Symlink,
    Other,
}
```

- `head` opens the stack, reads the active manifest, and returns it with its
  revision (`revision_from_manifest`), so a caller can publish against it.
- `read_manifest_file` reads one path from a given manifest via `MergedView` (no
  writer lock needed — a manifest pins immutable layers) and maps symlink,
  directory, and other non-regular entries to `NotRegular`. Oversized regular
  files are returned as `TooLarge`, not as generic layerstack failures.
- `inspect_manifest_path` uses the same classified projection without reading
  file bytes; `write` uses it to compute `create` vs `update` without loading a
  large existing file.
- `workspace_root` reads the existing layerstack workspace binding. It does not
  add a field to `FileService`.

Sessionless `read` is `head` then `read_manifest_file`; `Absent` maps to
`NotFound`, `NotRegular` maps to invalid request, and `File` continues through
UTF-8 normalization and windowing. The underlying layerstack crate needs to make
the projection's classified read result available publicly or add an equivalent
public helper; do not infer type from `read_bytes_limited`.

### publish_on_head (write + edit)

The read-modify-write policy lives in the file domain and reuses the existing
`LayerStackService::publish_changes` (which maps `owner` to blame after commit).
Optimistic concurrency: on a base conflict, re-read head, re-apply, re-publish.

```rust
// file/service/support.rs
pub(super) fn publish_on_head(
    layerstack: &LayerStackService,
    rel: &LayerPath,
    owner: &str,
    make_attempt: impl FnMut(&Manifest) -> Result<HeadAttempt, FileOperationError>,
) -> Result<HeadPublish, FileOperationError> {
    // loop, bounded ~8 attempts:
    //   (base_manifest, expected_base) = layerstack.head()
    //   attempt = make_attempt(&base_manifest)
    //   publish_changes {
    //     expected_base, base_manifest, protected_drops: Vec::new(),
    //     changes: [Write{rel, attempt.content}], owner
    //   }
    //   retry on base-conflict / source-conflict errors, else return
}
```

`HeadAttempt` carries `{ content, existed_before }`; `HeadPublish` returns that
`existed_before` value from the successful attempt plus `bytes_written`. `write`
calls `inspect_manifest_path` inside its closure, rejects `NotRegular`, and
records whether the successful attempt saw `Absent` or `File` so the output type
is correct. `edit` calls `read_manifest_file` inside its closure, maps `Absent`
to `NotFound`, rejects `NotRegular`, and re-applies the edits to the fresh
content on every retry rather than clobbering a concurrent change.

Retry classification (all other errors surface immediately):

```text
retriable:
  LayerStackServiceError::InvalidBaseRevision { .. }
  LayerStackServiceError::LayerStack { error: ManifestConflict { .. }, .. }
  LayerStackServiceError::PublishRejected { rejection }
    where rejection.reason is InvalidBaseRevision
    or SourceConflict
```

## Owner Attribution & Auditability

The owner string is the existing convention documented on
`PublishChangesRequest`:

```text
sessionless write/edit -> owner = "operation:<request_id>"   (request_id from Request)
session write/edit      -> no publish now; attributed to
                           "workspace_session:<id>" when the session is captured
```

`request_id` already exists on `sandbox_protocol::Request`, so no id generation
is required. Sessionless publishes flow through `publish_changes ->
record_layer_publish`, so `file_blame` reflects the new owner with no extra work.

## Error Model

Add `FileOperationError` as a peer to the blame-only `FileError` (keep blame's
error narrow; do not overload it).

```rust
pub enum FileOperationError {
    NotFound(String),
    InvalidPath(String),
    NotUtf8(String),
    NotRegular { path: String, kind: FileEntryKind },
    FileTooLarge { path: String, size: u64, limit: usize },
    EditNotFound { path: String, snippet: String },
    EditNotUnique { path: String, count: usize, snippet: String },
    NoEdits,
    NoChanges(String),
    WorkspaceSessionNotFound(String),
    WorkspaceSession(String),
    LayerStack(#[from] LayerStackServiceError),
    Io { path: String, source: std::io::Error },
}
```

Dispatch → `Response` mapping:

| Variant | kind |
|---|---|
| `NotFound`, `WorkspaceSessionNotFound` | `not_found` (+ details) |
| `InvalidPath`, `NotUtf8`, `NotRegular`, `FileTooLarge`, `EditNotFound`, `EditNotUnique`, `NoEdits`, `NoChanges` | `invalid_request` |
| `WorkspaceSession`, `LayerStack`, `Io` | `operation_failed` |

`edit` mirrors local-os semantics: every `old_string` must be found; it must be
unique unless `replace_all` is set; empty edit arrays and no-op edits are
rejected; edits apply in array order with local-os line-ending normalization.
Classified read `TooLarge` maps to `FileOperationError::FileTooLarge`, not to
the catch-all `LayerStack` variant.

## CLI Definitions

Add three specs and dispatchers to
`crates/sandbox-runtime/operation/src/cli_definition/file_operations.rs`, and
register them in `OPERATIONS`. Refresh the family `description` (blame is no
longer the only member).

```text
sandbox-cli runtime file_read  --path FILE [--offset N] [--limit N] [--workspace-session-id ID]
sandbox-cli runtime file_write --path FILE --content TEXT [--workspace-session-id ID]
sandbox-cli runtime file_edit  --path FILE --edits JSON   [--workspace-session-id ID]
```

The protocol/request field is `file_path`; `--path` is only the CLI flag name.

`ArgKind` has no array variant, so `edits` is declared as a string in the spec
and the dispatcher accepts **both** a real JSON array (the programmatic agent
path, `request.args.edits`) and a JSON string (CLI ergonomics). `request_id` is
read from `request.request_id`; `workspace_session_id` is parsed exactly as
`exec_command` does (empty ⇒ `None`).

## File-by-File Change Plan

```text
EDIT sandbox-runtime-layerstack projection   expose classified read/inspect result (no storage format change)
NEW  layerstack/service/impls/read.rs        head() + read_manifest_file() + inspect_manifest_path()
EDIT layerstack/service/core.rs              expose workspace_root() via binding
EDIT layerstack/service/impls/mod.rs         + mod read;

EDIT file/service/core.rs                    unchanged constructor/signature
EDIT file/error.rs                           + FileOperationError
NEW  file/service/dto.rs                     Read/Write/Edit In & Out, EditOp
NEW  file/service/support.rs                 resolve_layer_path + publish_on_head + text windowing
NEW  file/service/namespace.rs               call namespace file runner for session ops
NEW  file/service/impls/read.rs              impl FileService::read
NEW  file/service/impls/write.rs             impl FileService::write
NEW  file/service/impls/edit.rs              impl FileService::edit
EDIT file/service/impls/mod.rs               + mod read/write/edit;
EDIT file/service.rs                         + mod dto/support/namespace; re-export DTOs
EDIT file/mod.rs                             re-export FileOperationError + DTOs

# unified ns-runner: explicit modes NsRunnerOperation { ShellExec, MountOverlay, FileOp }
NEW  namespace-process  runner/setns/file_op.rs   setns_user_mnt + classified Read + atomic Write at workspace_root/rel
EDIT namespace-process  runner/{mod,setns}.rs     + run_file_op entry (peer of run_setns)
NEW  namespace-execution engine file-op launch    one-shot, non-PTY (peer of engine.mount_overlay)
EDIT namespace-execution launcher                 split mode_flag Option into {mode flag, wait discipline}; pass --shell-exec for shell
NEW  sandbox-daemon      runner/file_op.rs         --file-op body (peer of runner/mount_overlay.rs)
EDIT sandbox-daemon      runner/mod.rs             rename Run->ShellExec; + FileOp variant; + --shell-exec/--file-op arms; require explicit mode
EDIT workspace          NamespaceRuntime + WorkspaceRuntimeService  file-op launch (peer of mount_overlay)
NEW  workspace_session  service/impls/run_file_op.rs  resolve entry, delegate to workspace runtime
EDIT services.rs                             no FileService::open signature change
EDIT cli_definition/file_operations.rs       FILE_READ/WRITE/EDIT specs + dispatch + register

NEW  tests/file_operations.rs                four-quadrant coverage (see Verification)
```

`FileService::open` does not grow an argument.

## Verification

Build and unit checks:

```sh
cargo build
cargo test -p sandbox-runtime
cargo clippy --all-targets
cargo fmt
```

New integration test `tests/file_operations.rs` must cover:

```text
read  sessionless      -> content from the snapshot; offset/limit windowing; bytes/next/truncated fields
read  session          -> namespace write is visible on a subsequent read
read  missing          -> NotFound on both backends, never empty content
read  text normalize   -> BOM and CRLF/CR normalize before windowing
read  validation       -> limit 0 and limit >2000 are invalid_request
write sessionless      -> type=create then type=update; file_blame owner = operation:<id>
write session          -> lands in the session overlay; NOT visible via a sessionless snapshot read
write atomic session   -> parent dirs created, regular-file mode preserved on update, no partial direct write
edit  sessionless      -> replacements; EditNotFound / EditNotUnique errors
edit  session          -> read-modify-write against the live overlay
file  not-regular      -> symlink, directory, and special file rejected on both backends
path  accept-both      -> absolute-under-workspace-root and repo-relative resolve equal
path  reject escape    -> absolute outside root, `..`, empty, and NUL paths rejected
publish OCC            -> concurrent SourceConflict retries for both write and edit
session concurrency    -> file_write during a running in-session command is visible in that namespace
```

Live sandbox checks with `sandbox-cli`:

```sh
bin/start-sandbox-docker-gateway --rebuild-binary

# sessionless: publish then observe ownership
bin/sandbox-cli runtime file_write --path notes/hello.txt --content "hi"
bin/sandbox-cli runtime file_read  --path notes/hello.txt
bin/sandbox-cli runtime file_edit  --path notes/hello.txt \
  --edits '[{"old_string":"hi","new_string":"bye"}]'
bin/sandbox-cli runtime file_blame --path notes/hello.txt   # owner = operation:<id>

# session: mutate the live overlay without publishing
ws=$(bin/sandbox-cli runtime create_workspace_session --json | jq -r .workspace_session_id)
bin/sandbox-cli runtime file_write --workspace-session-id "$ws" --path a.txt --content "x"
bin/sandbox-cli runtime file_read  --workspace-session-id "$ws" --path a.txt   # -> x
bin/sandbox-cli runtime file_read  --path a.txt                                # snapshot: absent

# coherence: mutate through the namespace while a command is still alive
cmd=$(bin/sandbox-cli runtime exec_command --workspace-session-id "$ws" --yield-time-ms 0 "sleep 30" | jq -r .command_session_id)
bin/sandbox-cli runtime file_write --workspace-session-id "$ws" --path b.txt --content "y"
bin/sandbox-cli runtime exec_command --workspace-session-id "$ws" "cat b.txt" # -> y
```

Pass criteria:

```text
sessionless write/edit publish and are visible to file_read and file_blame
session write/edit are visible only inside that session
a session edit does not create a layerstack layer
session file ops go through the namespace, not host-side upperdir mutation
absolute-under-workspace-root and repo-relative paths address the same file
clippy and fmt are clean
```

## Safety Rules

- `read` is a pure read; it never mounts, never publishes, never mutates.
- Session write/edit run inside the target session namespace; never host-write
  `upperdir`, never touch a lower layer, never another session, never the shared
  base.
- Session write/edit are unavailable until the namespace file-op runner exists;
  host-side `upperdir` mutation is not an acceptable fallback.
- Sessionless write/edit go through `publish_changes` only, so every published
  line is attributed and auditable by `file_blame`.
- Path resolution rejects `..` and absolute paths outside the workspace root
  (`LayerPath` invariants); no path escapes the repo tree.
- `FileService` stays `&self`; no new locks. Layerstack concurrency is handled by
  its existing writer lock; sessionless OCC is handled by bounded retry.
- The blame/`record_layer_publish` edge is untouched; no existing publish
  behavior changes.

## Non-Goals

- No host-side overlay merge/write implementation in the file service.
- No `delete`/`move`/`stat`/`ls` operations in this pass — only `read`, `write`,
  `edit`.
- No new field on `SandboxRuntimeOperations` and no new top-level service.
- No change to the local-os tools; this is the sandbox side of the symmetry.
- No binary-file handling beyond a UTF-8 guard (`NotUtf8`); no image/PDF policy.
- No symlink following in this pass. Symlinks are rejected consistently on both
  backends until a workspace-confined symlink-following policy is designed.

## Open Risks

- **Namespace file runner scope.** Keep it to read/write bytes only. Do not add
  delete/move/stat/list until those operations exist.
- **Whiteout fidelity.** Session ops delegate to the mounted overlay namespace;
  sessionless ops delegate to the layerstack projection. Do not duplicate
  whiteout logic in the operation crate. The projection must expose classified
  path results so directories/symlinks are rejected instead of being misread.
- **Sessionless OCC churn.** Under heavy concurrent publishing, `write` and
  `edit` may retry. The bound (~8) trades a rare failure for no unbounded spin.
  Tune only if a real workload contends.
</content>
</invoke>
