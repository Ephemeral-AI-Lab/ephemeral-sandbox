---
title: File Operation Test Cases
tags:
  - ephemeral-os
  - sandbox
  - runtime
  - file
  - testing
status: draft
updated: 2026-07-02
---

# File Operation Test Cases

These are the required tests for `file_read`, `file_write`, and `file_edit`.
Primary coverage belongs in
`crates/sandbox-runtime/operation/tests/file_operations.rs`; lower-level unit
tests are only for helpers that are awkward to drive through the runtime API.

## Must-Hold Contract

- [ ] Sessionless `file_read` reads the latest published layerstack snapshot.
- [ ] Sessionless `file_write` and `file_edit` publish through `amend_path` under
      the layerstack writer lock.
- [ ] Session `file_read`, `file_write`, and `file_edit` operate through the
      live session namespace and mounted workspace.
- [ ] Session writes/edits do not publish immediately and are attributed only
      when the session is captured.
- [ ] File operations never read from or write to the detached host workspace as
      the source of truth.
- [ ] Session writes/edits never mutate `entry.upperdir` directly from the host.
- [ ] The sandbox protocol field is `path`; CLI `--path` maps directly to it.

## Read Cases

### Sessionless Read

- [ ] Reads a file that exists in the current snapshot.
      Expected: returns `content`, `start_line`, `num_lines`, `total_lines`,
      `bytes_read`, `total_bytes`, `next_offset`, and `truncated`.
- [ ] Missing file.
      Expected: `not_found`, not empty content.
- [ ] Reads after a sessionless write.
      Expected: new published content is visible.
- [ ] Does not see uncommitted changes in a live session.
      Expected: `not_found` or previous snapshot content.
- [ ] Reads a large file with a small `offset`/`limit` window.
      Expected: succeeds if the selected window is within output limits; it
      must not fail only because the whole file is large.
- [ ] Selected read output exceeds the max response bytes.
      Expected: invalid request / `OutputTooLarge`, not `FileTooLarge`.
- [ ] `limit` omitted.
      Expected: default is 2000 lines.
- [ ] `limit = 0`.
      Expected: invalid request.
- [ ] `limit > 2000`.
      Expected: invalid request.
- [ ] `offset <= 1`.
      Expected: starts at line 1.
- [ ] `offset` past EOF.
      Expected: empty content window with correct totals, not `not_found`.
- [ ] UTF-8 with leading BOM.
      Expected: BOM is removed before windowing.
- [ ] CRLF and CR-only line endings.
      Expected: normalized to `\n` before windowing.
- [ ] Invalid UTF-8 bytes.
      Expected: invalid request / not UTF-8.
- [ ] Directory path.
      Expected: invalid request / not regular.
- [ ] Symlink path.
      Expected: invalid request / not regular; symlink is not followed.
- [ ] Symlink parent path.
      Expected: invalid request; no parent symlink traversal.
- [ ] Special file path, where test fixture can create one.
      Expected: invalid request / not regular.

### Session Read

- [ ] Reads a file created by an in-session shell command.
      Expected: namespace read sees the live overlay content.
- [ ] Reads a file created by session `file_write`.
      Expected: content is visible in the same session.
- [ ] Reads while a command is still alive in the same session.
      Expected: sees changes made through the mounted namespace.
- [ ] Missing file in the session overlay.
      Expected: `not_found`.
- [ ] Large session read with a small `offset`/`limit` window.
      Expected: runner returns only the requested window; it does not transfer
      the full file to the operation layer.
- [ ] BOM, CRLF/CR, `offset`, and `limit`.
      Expected: same shaping as sessionless read.
- [ ] Directory, symlink, symlink parent, and special file in the live overlay.
      Expected: invalid request / not regular.

## Write Cases

### Sessionless Write

- [ ] Create a new file.
      Expected: returns `type = create`; subsequent sessionless read sees it.
- [ ] Update an existing file.
      Expected: returns `type = update`; subsequent sessionless read sees it.
- [ ] Blame after create/update.
      Expected: owner is `operation:<request_id>` for changed lines.
- [ ] Identical-content write.
      Expected: final content is unchanged; blame behavior is explicit and stable.
- [ ] Missing parent directories.
      Expected: parents are created for the new file.
- [ ] Existing parent is a file.
      Expected: invalid request.
- [ ] Existing parent is a symlink.
      Expected: invalid request; no symlink traversal.
- [ ] Final target is a directory.
      Expected: invalid request / not regular.
- [ ] Final target is a symlink.
      Expected: invalid request / not regular; symlink is not followed.
- [ ] Final target is a special file.
      Expected: invalid request / not regular.
- [ ] Concurrent sessionless writes to one path.
      Expected: operations serialize under `amend_path`; final content is one
      complete write and no partial/stale publish is observed.
- [ ] Partial write failure injected before publish.
      Expected: no new layer is committed.

### Session Write

- [ ] Create a new file through `workspace_session_id`.
      Expected: visible inside the session and not visible in sessionless read.
- [ ] Update an existing session file.
      Expected: visible inside the session.
- [ ] Capture the session after a session write.
      Expected: later blame attribution is `workspace_session:<id>`.
- [ ] Write while a command is still alive in the same session.
      Expected: command can observe the new content through the mounted overlay.
- [ ] Verify storage target.
      Expected: write goes through `WorkspaceSessionService::run_file_op` and
      the namespace runner against the mounted workspace, not by host-side
      mutation of `entry.upperdir`.
- [ ] Missing parent directories.
      Expected: parents are created through the mounted overlay.
- [ ] Existing parent is a file.
      Expected: invalid request.
- [ ] Existing parent is a symlink.
      Expected: invalid request; no escape through symlink parents.
- [ ] Final target is a symlink, directory, or special file.
      Expected: invalid request / not regular.
- [ ] Update preserves regular-file mode.
      Expected: existing executable/readable mode is preserved.
- [ ] Simulated write failure before rename.
      Expected: no partially written target file.
- [ ] Temp file cleanup.
      Expected: no durable temp artifacts after success or expected failure.

## Edit Cases

### Sessionless Edit

- [ ] Empty `edits` array.
      Expected: invalid request / no edits.
- [ ] `old_string == new_string`.
      Expected: invalid request / no changes.
- [ ] `old_string` not found.
      Expected: invalid request / edit not found.
- [ ] `old_string` appears more than once and `replace_all` is false or absent.
      Expected: invalid request / edit not unique.
- [ ] `old_string` appears more than once and `replace_all` is true.
      Expected: all occurrences are replaced and replacement count is correct.
- [ ] Multiple edit entries.
      Expected: applied in array order against the evolving content.
- [ ] Later edit depends on an earlier edit.
      Expected: succeeds if the earlier edit creates the later target.
- [ ] Earlier edit removes a later target.
      Expected: later edit reports not found.
- [ ] Edit preserves normalized line-ending semantics.
      Expected: matching follows local-os normalization rules.
- [ ] Edit target exceeds `MAX_EDIT_BYTES`.
      Expected: invalid request / `FileTooLarge` before loading the whole file.
- [ ] Edit target contains invalid UTF-8.
      Expected: invalid request / not UTF-8.
- [ ] Concurrent sessionless edits/writes to one path.
      Expected: operations serialize under `amend_path`; edit applies to the
      current head while the lock is held, with no OCC retry loop.
- [ ] Partial publish failure.
      Expected: no new layer is committed.

### Session Edit

- [ ] Edit a file created in the session.
      Expected: read-modify-write happens against the live overlay.
- [ ] Edit a file modified by an in-session shell command.
      Expected: sees current namespace content, not the snapshot version.
- [ ] Sessionless read after session edit.
      Expected: does not see the uncommitted session edit.
- [ ] Capture after session edit.
      Expected: captured layer contains edited content and blame owner is
      `workspace_session:<id>`.
- [ ] Same replacement semantics as sessionless edit.
      Expected: no edits, no changes, not found, not unique, and `replace_all`
      all behave the same.
- [ ] Concurrent shell write races with session edit.
      Expected: documented last-writer-wins behavior; the final rename is atomic
      and no partial file is observed.
- [ ] Symlink parent or symlink target.
      Expected: invalid request; no symlink traversal.

## Path Cases

- [ ] Runtime request uses `path`, not `file_path`.
      Expected: direct sandbox requests accept `path`; any local-os adapter
      translation happens before the runtime call.
- [ ] Repo-relative path such as `src/file.txt`.
      Expected: resolves to the same layer path on both backends.
- [ ] Absolute path under workspace root.
      Expected: strips the workspace root and resolves to the same layer path as
      the repo-relative form.
- [ ] Absolute path outside workspace root.
      Expected: invalid path.
- [ ] Empty path.
      Expected: invalid path.
- [ ] Path containing NUL.
      Expected: invalid path.
- [ ] Path containing `..`.
      Expected: invalid path.
- [ ] Path with `.` components.
      Expected: normalization matches `LayerPath` behavior.
- [ ] Existing parent path is a whiteout or opaque directory case in layerstack.
      Expected: sessionless read/write/edit classify the merged view correctly.
- [ ] Parent path is hidden by a whiteout.
      Expected: treated as absent or invalid according to the merged manifest,
      never resolved to a lower-layer object.

## Runner And Protocol Cases

- [ ] Namespace runner mode.
      Expected: `--shell`, `--mount-overlay`, and `--file-op` are the only valid
      modes; no mode and multiple modes are invalid.
- [ ] Shell runner launch.
      Expected: `spawn_pty` passes `--shell`, keeps existing PTY/cgroup/cancel
      behavior, and does not apply a setup timeout.
- [ ] Request/result runner launch.
      Expected: mount-overlay and file-op pass request/result fds, start
      draining result output before waiting on the child, cap result bytes, and
      use the setup timeout.
- [ ] Session operation boundary.
      Expected: file service calls `WorkspaceSessionService::run_file_op`; it
      does not construct namespace entries or call namespace execution directly.
- [ ] Runner error mapping.
      Expected: missing paths map to `not_found`; invalid paths, UTF-8 errors,
      and not-regular files map to `invalid_request`; internal launch/I/O errors
      map to `operation_failed`.
- [ ] Runner body performs read/windowing inside the namespace.
      Expected: large session reads do not transfer the full file just to return
      a small line window.
- [ ] Hook/test backend support.
      Expected: session file-op tests either run against the real live runner or
      have an explicit file-op hook; tests must not pass by bypassing namespace
      semantics.

## Layerstack Helper Cases

- [ ] `read_current_window` classifies absent, file, directory, symlink, special
      file, invalid UTF-8, and selected-output-too-large cases.
- [ ] `read_current_window` does not reject a regular file solely because the
      whole file is larger than the output cap.
- [ ] `amend_path` write with `max_bytes = 0`.
      Expected: classifies existing target without loading large existing bytes.
- [ ] `amend_path` edit with `MAX_EDIT_BYTES`.
      Expected: rejects oversized edit input before transform.
- [ ] `amend_path` transform error.
      Expected: no commit and no blame record.
- [ ] `amend_path` commit success.
      Expected: `record_layer_publish` runs and `file_blame` shows the new owner.

## Live E2E Matrix (110 Cases)

AGENTS.md requires rebuilding the Docker sandbox gateway binary before live
sandbox checks:

```sh
bin/start-sandbox-docker-gateway --rebuild-binary
```

Use `sandbox-cli` for manual sandbox operations.

Live e2e coverage is a 110-case matrix against a real sandbox: 39 session
cases and 71 sessionless cases. Cases prefixed `[complex]` (33 of 110) are
larger-volume or multi-step scenarios. Breakdown:

| Group                                    | Total   | Session | Sessionless | Complex |
| ---------------------------------------- | ------- | ------- | ----------- | ------- |
| Baseline smoke (read/write/edit)         | 15      | 4       | 11          | 0       |
| Session-only (Linux)                     | 5       | 5       | 0           | 0       |
| Concurrent operations                    | 26      | 9       | 17          | 12      |
| Correctness: layerstack, mount, conflict | 27      | 9       | 18          | 9       |
| File ops + exec ops                      | 27      | 8       | 19          | 9       |
| File blame                               | 10      | 4       | 6           | 3       |
| **Total**                                | **110** | **39**  | **71**      | **33**  |

Standing correctness rule: every case whose operations publish content
(sessionless `file_write`/`file_edit`, one-shot exec capture, or session
capture) must assert `file_blame` ownership of the touched lines as part of
its expected outcome — in addition to the content and layerstack checks, and
even where the checklist entry does not spell the blame assertion out.

The test implementation lives in `cli-operation-e2e-live-test/runtime/file/`
(pytest; the gateway/sandbox lifecycle fixtures come from
`cli-operation-e2e-live-test/conftest.py`). One module per checklist group:

```
cli-operation-e2e-live-test/runtime/file/
├── README.md                            # layout + run instructions
├── helpers.py                           # file-op CLI wrappers + blame assertions
├── smoke/
│   ├── test_read_smoke.py               # Read Smoke (5)
│   ├── test_write_smoke.py              # Write Smoke (5)
│   ├── test_edit_smoke.py               # Edit Smoke (5)
│   └── test_session_only_linux.py       # Session-Only Cases (5, Docker/Linux sandbox)
├── concurrent/
│   ├── test_concurrent_sessionless.py   # Concurrent Operations — Sessionless (17)
│   └── test_concurrent_session.py       # Concurrent Operations — Session (9)
├── correctness/
│   ├── test_correctness_sessionless.py  # Correctness — Sessionless (18)
│   └── test_correctness_session.py      # Correctness — Session (9)
├── file_exec/
│   ├── test_file_exec_sessionless.py    # File Ops + Exec Ops — Sessionless (19)
│   └── test_file_exec_session.py        # File Ops + Exec Ops — Session (8)
└── blame/
    ├── test_blame_sessionless.py        # File Blame — Sessionless (6)
    └── test_blame_session.py            # File Blame — Session (4)
```

### Test Runner Instructions

Verified live against `ubuntu:24.04` on 2026-07-02 (sessionless
write/read/edit/blame, missing-file `not_found`, one-shot exec capture +
blame attribution, session write/read visibility, in-session exec through the
mount, destroy-discards — all matched the contract above).

Run through pytest (preferred — fixtures own gateway bring-up and
sandbox/session teardown):

```sh
cd cli-operation-e2e-live-test
pytest runtime/file                      # whole file-op matrix
pytest runtime/file/blame                # one group
pytest runtime/file -m "not slow"        # skip [complex] cases
```

- The gateway is started automatically by the `gateway_up` fixture and reused
  if one is already answering; do not restart a running gateway out from
  under parallel workers. Force a cold rebuild with `E2E_REBUILD_BINARY=1`
  (default) or run `bin/start-sandbox-docker-gateway --rebuild-binary` once
  from the repo root.
- Sandboxes default to `ubuntu:24.04` (`E2E_IMAGE` overrides) with a
  bind-mounted host workspace root (`E2E_WORKSPACE_VARIANT`, default
  `repo/testbed`).
- Use the `sandbox` fixture (or `manager/management/helpers.py`) so leaked
  sandboxes are drained on session teardown.

Manual spot-checks use the same verified command shapes:

```sh
export PATH="$PWD/bin:$PATH"
sandbox-cli manager create_sandbox --image ubuntu:24.04 --workspace-root <host-dir>
sandbox-cli runtime --sandbox-id <id> file_write --path notes.txt --content 'hello'
sandbox-cli runtime --sandbox-id <id> file_read  --path notes.txt [--offset N --limit N]
sandbox-cli runtime --sandbox-id <id> file_edit  --path notes.txt \
    --edits '[{"old_string":"a","new_string":"b","replace_all":false}]'
sandbox-cli runtime --sandbox-id <id> file_blame --path notes.txt
sandbox-cli runtime --sandbox-id <id> exec_command 'sed -i "s/a/b/" notes.txt'
sandbox-cli runtime --sandbox-id <id> create_workspace_session
sandbox-cli runtime --sandbox-id <id> file_write --path draft.txt --content 'x' \
    --workspace-session-id <ws-id>
sandbox-cli runtime --sandbox-id <id> destroy_workspace_session --workspace-session-id <ws-id>
sandbox-cli manager destroy_sandbox --sandbox-id <id>
```

The CLI prints one JSON line per operation — to stdout on success, to stderr
with exit 1 on an error response (so expected faults like `not_found` exit
nonzero; assert on the JSON `error.kind`, not the exit code). Always destroy
what you create; other agents share the gateway.

### Read Smoke

- [x] Sessionless read of a file created by sessionless `file_write`.
- [x] Session read of a file created by session `file_write`.
- [x] Sessionless read with `offset` and `limit` over a multi-line file.
- [x] Sessionless read of a missing file returns `not_found`.
- [x] Sessionless read rejects an absolute path outside the workspace root.

### Write Smoke

- [x] Sessionless write creates a new file and sessionless read sees it.
- [x] Sessionless write updates an existing file and `file_blame` shows
      `operation:<request_id>`.
- [x] Session write is visible with `workspace_session_id` and invisible to
      sessionless read before capture.
- [x] Session write creates missing parent directories.
- [x] Write to an existing directory is rejected.

### Edit Smoke

- [x] Sessionless edit performs one unique replacement and sessionless read sees
      the result.
- [x] Sessionless edit with `replace_all=true` replaces multiple occurrences.
- [x] Sessionless edit with missing `old_string` returns edit-not-found.
- [x] Session edit is visible with `workspace_session_id` and invisible to
      sessionless read before capture.
- [x] Ordered multi-edit applies against evolving content.

### Session-Only Cases (Docker/Linux sandbox)

These run through the live Docker/Linux namespace runner, so the file-operation
matrix must execute them against the sandbox gateway instead of treating the
host operating system as the source of truth. They are proven only here; record
a transcript for each.

- [x] Session write updates an existing executable file and preserves its mode.
- [x] Session write to an in-session directory is rejected as invalid request /
      not regular.
- [x] Session write to an in-session symlink is rejected as invalid request /
      not regular; the symlink is not followed.
- [x] Session write to an in-session symlink parent is rejected as invalid
      request; no symlink-parent traversal.
- [x] Session edit to an in-session symlink or symlink parent is rejected as
      invalid request; no symlink traversal.

### Concurrent Operations — Sessionless (17)

- [x] Two concurrent sessionless `file_write` requests to the same path with
      different multi-line contents (parallel `sandbox-cli runtime file_write`
      invocations).
      Expected: both return `type = create`/`update` with no `operation_failed`;
      the writes serialize under the `amend_path` exclusive writer lock, so a
      final `file_read` returns exactly one writer's complete content (never
      interleaved bytes) and `file_blame` tiles every line with the single
      `operation:<request_id>` owner of the last-committed writer.
- [x] Concurrent sessionless `file_write` (rewriting a seeded `alpha\nbeta` file
      to `alpha\nGAMMA`) and `file_edit` (`alpha` → `ALPHA`) on the same path.
      Expected: each op either succeeds or the edit returns `invalid_request`
      (edit not found); final content is one of the complete serialized outcomes
      (`alpha\nGAMMA` or `ALPHA\nGAMMA`), and `file_blame` owners per line match
      exactly the ops whose text survives — no torn state, no `operation_failed`.
- [x] Two concurrent sessionless `file_edit` requests targeting the same unique
      `old_string` on one path (`replace_all` absent).
      Expected: exactly one edit returns `type = edit` with `replacements = 1`;
      the loser reads the post-edit head under the writer lock (no OCC retry)
      and returns `invalid_request` / edit not found; final `file_read` shows a
      single replacement.
- [x] Two concurrent sessionless `file_write` requests with byte-identical
      content to one new path.
      Expected: both return ok (one `type = create`, one `type = update` by
      serialization order); the second publish dedupes against the identical
      head layer (no-op), `observability layerstack` shows `manifest_version`
      advanced by exactly 1, and `file_blame` owner is the first committer's
      `operation:<request_id>`.
- [x] Five concurrent sessionless `file_read` requests of a seeded path racing
      one sessionless `file_write` that replaces its content.
      Expected: every read returns a complete published snapshot —
      `content`/`total_bytes` match either the whole old content or the whole
      new content, never a mix, never `operation_failed`.
- [x] Sessionless `file_read` racing a sessionless `file_write` that creates a
      brand-new path.
      Expected: the read returns either `not_found` or the complete new content
      with correct `total_lines`/`total_bytes`; an empty-content success is
      never observed.
- [x] Two concurrent sessionless `file_write` requests to two disjoint paths.
      Expected: both publish independently; each `file_read` returns its full
      content, each `file_blame` shows only its own `operation:<request_id>`,
      and `observability layerstack` shows `manifest_version` advanced by
      exactly 2.
- [x] `file_blame` racing a sessionless `file_write` to the same path.
      Expected: blame returns a fully tiled `ranges` set
      (`start_line`/`line_count`/`owner` covering the whole file) for either the
      pre-write or post-write state — owners drawn from
      `original`/`operation:<request_id>` — never a partially updated tiling.
- [x] Sessionless `file_edit` changing line 2 of a seeded 4-line file racing a
      one-shot `exec_command` (no `--workspace-session-id`) whose shell command
      sed-edits line 4, so the exec-owned session capture publish three-way
      merges with the amend commit.
      Expected: regardless of commit order both changes land; final `file_read`
      shows line 2 and line 4 changed; `file_blame` shows line 2 owned by
      `operation:<request_id>`, line 4 owned by
      `workspace_session:<one-shot-id>`, and untouched lines `original`.
- [x] Sessionless `file_write` wholesale-rewriting a seeded single-line file
      racing a one-shot `exec_command` that wholesale-rewrites the same file
      with different content (merge-ineligible capture).
      Expected: the amend commit always wins — if the capture publishes second
      its three-way merge conflicts and the publish is dropped as
      `source_conflict` (never surfaced; `exec_command` still reports
      `status = ok`); final content is the sessionless write's payload in both
      orderings and `file_blame` shows only `operation:<request_id>`.
- [x] [complex] 20+ concurrent sessionless `file_write` requests to one path,
      each with a unique multi-line payload.
      Expected: all 20 return ok; final `file_read` returns exactly one writer's
      complete payload, `file_blame` tiles the file with that single
      `operation:<request_id>`, and `observability layerstack` shows
      `manifest_version` advanced by exactly 20 (every serialized commit is a
      full layer, no partial publish).
- [x] [complex] 100 concurrent sessionless `file_write` requests, each creating
      its own distinct path under one directory (`fanout/file-<i>.txt`).
      Expected: all 100 return `type = create`; 100 subsequent `file_read` calls
      each return the correct full content, per-path `file_blame` owner matches
      each writer's `operation:<request_id>`, and layer count in
      `observability layerstack` grows by exactly 100 with the daemon still
      serving requests.
- [x] [complex] Mixed fan-out on one hot path: 10 sessionless writers (unique
      disjoint payloads), 10 sessionless readers, and 10 `file_blame` calls all
      launched concurrently.
      Expected: every read returns one complete committed payload
      (`total_bytes` matches exactly one payload), every blame response is a
      complete single-owner tiling drawn from `original` or one writer's
      `operation:<request_id>`, no request returns `operation_failed`, and the
      final state matches the last-committed writer.
- [x] [complex] Two concurrent sessionless `file_write` requests each carrying a
      distinct ~1 MiB multi-line payload to the same path, racing five windowed
      `file_read` requests (`--offset`/`--limit` 50-line windows).
      Expected: every windowed read succeeds (no `OutputTooLarge` for the small
      window) and reports `total_bytes` equal to exactly one full payload's size
      with window content from that same payload — never bytes from both; the
      final read and wholesale `file_blame` owner match one writer.
- [x] [complex] Seed one file with 20 unique tokens on separate lines, then run
      20 concurrent sessionless `file_edit` requests, each replacing only its
      own token.
      Expected: all 20 return `replacements = 1` (each edit applies to the
      evolving head under the writer lock and its token is untouched by the
      other edits); final content contains all 20 replacements, `file_blame`
      shows each edited line owned by its own `operation:<request_id>`, and
      `manifest_version` advances by exactly 20.
- [x] [complex] 40-way disjoint race: 20 one-shot `exec_command` invocations
      each shell-writing its own path concurrently with 20 sessionless
      `file_write` requests to 20 other paths.
      Expected: all 40 paths are subsequently readable with complete content;
      `file_blame` owners split by origin — `workspace_session:<id>` (20
      distinct one-shot session owners) for exec-created paths and
      `operation:<request_id>` for file-op paths; every capture publish commits
      (disjoint paths, no `source_conflict`).
- [x] [complex] Sustained hot-path churn: 5 concurrent workers each issue 10
      sequential sessionless `file_write` requests with unique contents to one
      path (50 writes) while a poller repeatedly calls
      `sandbox-cli observability layerstack --sandbox-id ID`.
      Expected: all 50 writes return ok; the poller's `manifest_version` samples
      are strictly non-decreasing and finish at baseline + 50; the final
      `file_read` and `file_blame` owner match exactly one of the 50 request
      ids.

### Concurrent Operations — Session (9)

- [x] Inside one live workspace session (`create_workspace_session`), race a
      session `file_write` (rewriting `alpha\nbeta` to `alpha\nGAMMA`) against a
      session `file_edit` (`alpha` → `ALPHA`) on the same path via
      `--workspace-session-id`.
      Expected: each op returns ok or the edit returns
      `invalid_request`/`not_found`; the final session `file_read` returns one
      complete last-writer-wins variant (edit is read-modify-write, write lands
      via atomic `renameat`) — never interleaved bytes; sessionless `file_read`
      of the path still returns `not_found` (session ops never publish).
- [x] Two concurrent session `file_write` requests with different contents to
      the same path in one live session.
      Expected: both return ok; final session `file_read` returns exactly one
      writer's complete content; a follow-up
      `exec_command --workspace-session-id ID "ls -a <dir>"` shows no leftover
      `.<name>.tmp.<pid>` temp artifacts from the atomic-rename path.
- [x] Session `file_read` requests looping concurrently with an in-session shell
      command (`exec_command --workspace-session-id ID`) that repeatedly writes
      a temp file and atomically `mv`s it over the target, embedding the same
      generation marker on the first and last line.
      Expected: every session read returns a self-consistent complete generation
      (first and last line markers match, `num_lines`/`total_bytes` coherent);
      no read observes a torn or empty intermediate state through the mounted
      namespace.
- [x] `destroy_workspace_session` racing an `exec_command` still running in that
      same session.
      Expected: the session-lifecycle lock serializes admission — destroy either
      returns `operation_failed` with
      `error.details.active_command_session_ids` listing the running command, or
      succeeds with `destroyed = true` only after the command reached terminal
      state; session `file_read` returns `not_found` for the
      `workspace_session_id` only once destroy succeeded.
- [x] [complex] Inside one long-running session, launch 20+ concurrent session
      `file_write` requests to 20 disjoint paths while an interactive in-session
      shell (started with `--yield-time-ms 0`, driven via
      `write_command_stdin`/`read_command_lines`) concurrently `cat`s the same
      paths.
      Expected: all writes return ok; each session `file_read` and the shell
      transcript show complete per-path content through the live overlay; all 20
      paths remain `not_found` to sessionless `file_read`;
      `destroy_workspace_session` then discards everything without any layer
      being published (`manifest_version` unchanged).
- [x] [complex] Hot-file storm inside one session: 10 concurrent session writes
      with unique payloads, 10 concurrent session reads, and a live shell `cat`
      loop, all on one path.
      Expected: every reader (file op and shell transcript via
      `read_command_lines`) observes exactly one complete payload per read
      (atomic rename, last-writer-wins); the final session read matches one
      writer; `ls -a` in the session shows no `.tmp` artifacts; nothing
      publishes to the layerstack.
- [x] [complex] Same-path conflicting captures: 5 concurrent one-shot
      `exec_command` invocations each wholesale-rewriting the same seeded
      single-line file with unique content, so their exec-owned session captures
      publish in completion order against a moving head.
      Expected: the first capture commits; every later capture three-way merge
      conflicts and is dropped (`source_conflict`, never surfaced — all 5
      `exec_command` responses report `status = ok`); final content equals
      exactly one session's payload, `file_blame` shows one single
      `workspace_session:<id>` owner, and `manifest_version` advances by
      exactly 1.
- [x] [complex] Capture-order independence: seed a 10-line file, then run 5
      concurrent one-shot `exec_command` invocations, each sed-editing only its
      own line, so 5 capture publishes three-way merge in arbitrary completion
      order.
      Expected: all 5 captures commit cleanly (line-disjoint merges); the final
      sessionless `file_read` contains all 5 modifications; `file_blame` shows 5
      distinct `workspace_session:<id>` owners on the 5 edited lines with
      `original` on untouched lines; `manifest_version` advances by exactly 5.
- [x] [complex] Capture racing sessionless writers: a one-shot `exec_command`
      runs a ~5 s script that keeps rewriting a seeded one-line file while 20
      concurrent sessionless `file_write` requests hammer the same path with
      unique one-line payloads.
      Expected: all 20 sessionless writes serialize and return ok; the terminal
      capture publish either merge-conflicts and is dropped or commits before
      any write, so the final `file_read` matches exactly one of the 20
      payloads, `file_blame` shows a single `operation:<request_id>` owner
      consistent with that content, `exec_command` reports `status = ok`, and
      `manifest_version` equals baseline + 20 (capture dropped) or + 21 (capture
      committed first) with no other value.

### Correctness: Layerstack, Mount, Conflict — Sessionless (18)

- [x] Three sessionless `file_write` calls to three distinct new paths, then
      read each back.
      Expected: `observability layerstack` shows `manifest_version` advanced by
      exactly 3 and three new `layer_id` entries prepended (newest first); each
      `file_read` returns the written `content` with correct
      `total_lines`/`total_bytes`.
- [x] Blame ladder on one path: sessionless `file_write` creates a 3-line file
      (request A), then `file_edit` rewrites line 2 (request B), then
      `file_edit` rewrites line 3 (request C).
      Expected: `file_blame` tiles line 1 to `operation:<A>`, line 2 to
      `operation:<B>`, line 3 to `operation:<C>` as three coalesced ranges;
      `manifest_version` advanced by exactly 3.
- [x] Identical-content `file_write` immediately repeated on the same path
      (digest matches the head layer).
      Expected: second write returns `type = update` but publishes no layer —
      `manifest_version` and `root_hash` unchanged, layer list identical, and
      `file_blame` owners unchanged.
- [x] Identical-content rewrite not at head: write X to `a.txt` (request A),
      write to `b.txt`, then rewrite `a.txt` with byte-identical X (request C).
      Expected: the third write commits a new layer (`manifest_version` +1)
      because the head digest differs, but `file_blame a.txt` still shows every
      line owned by `operation:<A>` (all lines resolve as inherited/active, none
      as command lines of C).
- [x] Delete via exec then re-create via file op: one-shot
      `exec_command "rm f.txt"` publishes a whiteout layer; then sessionless
      `file_write` re-creates `f.txt`.
      Expected: after the exec, `file_read f.txt` faults `not_found`; the write
      returns `type = create`; `file_read` then returns the new content (upper
      layer resolves before the whiteout) and `file_blame` shows all lines owned
      by the new `operation:<request_id>`.
- [x] Parent hidden by whiteout: one-shot `exec_command "rm -rf dir"` (dir has
      files in lower layers), then `file_write dir/new.txt`.
      Expected: `file_read dir/old.txt` is `not_found` (never the lower-layer
      object); the write succeeds as `type = create` with parents re-created in
      the new layer; `file_read dir/new.txt` returns the content while
      `file_read dir/old.txt` stays `not_found`.
- [x] Opaque directory: one-shot
      `exec_command "rm -rf dir && mkdir dir && echo fresh > dir/only.txt"` over
      a dir with lower-layer children.
      Expected: one captured layer carries `OpaqueDir(dir)` plus the write;
      `file_read dir/only.txt` returns `fresh`, every pre-existing `dir/*` path
      reads `not_found` through the merged manifest, and a
      `file_write dir/only.txt` update classifies the target as an existing
      regular file (`type = update`).
- [x] [complex] Deep whiteout/opaque hierarchy: seed `a/b/c/d` with files at
      each depth (one exec layer), publish `rm -rf a/b` plus a re-created
      `a/b/x/new.txt` via a second exec, then sessionless
      `file_write a/b/c/d/deep.txt`.
      Expected: reads under the old `a/b/c` subtree are `not_found` at every
      depth; `a/keep.txt` and `a/b/x/new.txt` read correctly; the final write
      returns `type = create` and only the explicitly re-created paths are
      visible in the merged view.
- [x] [complex] Deep layer stack, multi-path: 60 sequential sessionless
      `file_write` calls cycling over 20 paths (3 generations each).
      Expected: `manifest_version` advanced by exactly 60 with 60 new layers;
      `file_read` of every path returns its last-written generation;
      `file_blame` of each path shows all lines owned by the
      `operation:<request_id>` of its final write.
- [x] [complex] Deep layer stack, single path: `file_write` a 50-line file of
      unique markers, then 50 sequential `file_edit` calls, each replacing
      exactly one marker line.
      Expected: all 50 publishes commit (`manifest_version` +51 total); the
      final `file_read` shows all replacements applied cumulatively;
      `file_blame` maps each line to the specific `operation:<request_id>` that
      last edited it.
- [x] [complex] Large file windowed reads over a layer boundary: generate a
      ~15,000-line file with ~200-byte lines in one exec-published layer, then
      sessionless `file_edit` a unique marker near line 10,000 (new layer).
      Expected: `file_read` with default `limit` faults `invalid_request`
      (selected output over the 256 KiB `MAX_OUTPUT_BYTES` cap, not
      `FileTooLarge`); `--offset 1 --limit 500` and a window over line 10,000
      succeed with correct `start_line`/`num_lines`/`next_offset`/`total_lines`,
      the edited window shows the replacement while early windows are
      byte-identical to the pre-edit read, and `file_blame` shows the edited
      line as `operation:<request_id>` with surrounding lines owned by the
      exec's `workspace_session:<id>`.
- [x] [complex] Hundreds of files in one captured layer: a one-shot exec creates
      300 files across nested directories, then sessionless `file_write` updates
      3 of them.
      Expected: the exec adds exactly one layer (`manifest_version` +1, not
      +300); spot `file_read` of 10 files across the tree returns exec content;
      the 3 updates add 3 layers, and `file_blame` shows updated files owned by
      `operation:<request_id>` while untouched files stay
      `workspace_session:<id>`.
- [x] [complex] Mixed-history blame at scale: on one seeded 20-line base file,
      alternate 5 sessionless `file_edit` single-line replacements with 5
      one-shot exec single-line rewrites (10 publishes, disjoint lines).
      Expected: final `file_read` shows all 10 modified lines; `file_blame`
      tiles each modified line to its exact actor (`operation:<request_id>` or
      `workspace_session:<id>` respectively) and every untouched line to
      `original`.
- [x] First sessionless `file_edit` of a file that shipped in the workspace
      base, replacing one unique line.
      Expected: `file_blame` shows the changed line owned by
      `operation:<request_id>` and all untouched lines owned by `original`.
- [x] Blame survives deletion: sessionless `file_write` a file (audit event
      recorded), then one-shot `exec_command "rm <path>"` publishes the
      whiteout.
      Expected: `file_read` faults `not_found`, but `file_blame` (a pure store
      read) still returns the pre-delete ranges owned by
      `operation:<request_id>` — a delete appends no audit event.
- [x] Forbidden publishes: sessionless `file_write` to `layers/evil.txt` and
      `manifest.json` (layerstack-internal paths).
      Expected: both fault `operation_failed` with publish rejection
      `protected_path`; `manifest_version`/`root_hash` are unchanged and no
      layer is added. (`.git` is no longer special-cased — see the git-policy
      suite.)
- [x] Gitignored route: with `logs/` in the base `.gitignore`, a one-shot exec
      writes a multi-line `logs/app.log`.
      Expected: the file is committed on the `ignored` route and
      `file_read logs/app.log` returns its content, but `file_blame` returns a
      single wholesale range (`start_line = 1`, `line_count = 1`) owned by
      `workspace_session:<id>` rather than per-line tiling.
- [x] No-change capture: one-shot `exec_command "true"` completes.
      Expected: the capture publish is a `no_op` — `manifest_version`,
      `root_hash`, and the layer list are unchanged after the command reaches
      `status = ok`, and `active_lease_count` returns to its prior value once
      the one-shot workspace is destroyed.

### Correctness: Layerstack, Mount, Conflict — Session (9)

- [x] Frozen snapshot mount: `create_workspace_session`, then sessionless
      `file_write` a new file and update an existing base file.
      Expected: session `file_read --workspace-session-id` of the new path is
      `not_found` and of the updated path returns the pre-update snapshot
      content; an in-session `exec_command cat` agrees;
      `observability layerstack --workspace-id` shows the session's `mounts`
      exclude the newly published `layer_id` even though the sandbox-wide view
      lists it.
- [x] [complex] Session overlay at scale through the mount: 100 session
      `file_write` calls (`--workspace-session-id`) across nested directories in
      a caller-owned session.
      Expected: an in-session `exec_command "find . -type f | wc -l"` counts all
      100 through the mounted overlay; session `file_read` spot-checks match;
      `observability layerstack --workspace-id` reports `upper_bytes > 0`; the
      sandbox `manifest_version` is unchanged (session file ops never publish).
- [x] Destroy discards the overlay: session `file_write` several files in a
      caller-owned session, then `destroy_workspace_session`.
      Expected: destroy returns `destroyed: true`; `active_lease_count`
      decrements; sessionless `file_read` of every session path is `not_found`;
      a fresh `create_workspace_session` also reads them `not_found`;
      `manifest_version` unchanged.
- [x] One-shot capture end state: a single one-shot `exec_command` creates one
      file, modifies one seeded base file, and deletes another base file.
      Expected: exactly one new layer (`manifest_version` +1); sessionless
      `file_read` shows the new file and the modification, and `not_found` for
      the deleted path; `file_blame` shows created/changed lines owned by
      `workspace_session:<id>` and untouched lines `original`.
- [x] Capture after the base advanced — clean auto-merge: start a one-shot exec
      blocked on stdin (`read x; printf "tail\n" >> notes.txt`), sessionless
      `file_edit` line 1 of `notes.txt` while it waits, then
      `write_command_stdin` to release it.
      Expected: the capture publishes via three-way merge (no rejection): final
      sessionless `file_read notes.txt` contains both the edited line 1 and the
      appended tail; `file_blame` shows line 1 owned by
      `operation:<edit request_id>` and the tail line by
      `workspace_session:<id>`.
- [x] Capture after the base advanced — overlapping conflict: same stdin-gated
      one-shot pattern, but the in-session command rewrites the same line the
      sessionless `file_edit` changed, with different content.
      Expected: the command still ends `status = ok`, but the capture publish is
      rejected with `source_conflict` and discarded — `file_read` returns only
      the sessionless edit's content, `manifest_version` reflects only the
      sessionless publish, and `file_blame` still shows
      `operation:<request_id>` on the contested line.
- [x] Session delete vs sessionless modify: a stdin-gated one-shot session
      deletes `shared.txt` (and also creates `unrelated.txt`) after a
      sessionless `file_write` updated `shared.txt` post-session-start.
      Expected: the capture is rejected atomically (`source_conflict` on the
      delete, which cannot merge): `file_read shared.txt` returns the
      sessionless content, `file_read unrelated.txt` is `not_found` (no partial
      changeset escapes), and no layer is added for the capture.
- [x] [complex] Two sessions from one base captured in sequence: start two
      stdin-gated one-shot execs back-to-back on the same `manifest_version`;
      each creates ~100 distinct files and edits a disjoint region of one shared
      file; release session 1, then session 2.
      Expected: both captures publish (`manifest_version` +2), the second via
      clean merge against the advanced head; the final merged view contains both
      file sets and both shared-file regions; `file_blame` on the shared file
      maps each region to its own `workspace_session:<id>`.
- [x] [complex] Capture with hundreds of changed files: one one-shot exec
      creates 300 files, modifies 50 seeded base files, and deletes 20 others
      across a nested tree.
      Expected: exactly one new layer (`manifest_version` +1); spot sessionless
      `file_read` confirms creations and modifications, deleted paths are
      `not_found` through published whiteouts, and `file_blame` spot-checks
      attribute changed lines to `workspace_session:<id>` with untouched lines
      `original`.

### File Ops + Exec Ops — Sessionless (19)

- [x] One-shot `exec_command` creates a new file
      (`printf 'alpha\nbeta' > exec/made.txt`), then sessionless `file_read` and
      `file_blame` inspect it.
      Expected: exec returns `status = ok`, `exit_code = 0`; the one-shot
      capture publishes on completion, so `file_read` returns
      `content = "alpha\nbeta"` with correct window fields, and `file_blame`
      shows a single range with owner `workspace_session:<one-shot id>`.
- [x] Sessionless `file_write` creates a 3-line file (`operation:<request A>`),
      then a one-shot `exec_command` runs `sed -i` replacing only line 2, then
      `file_read` + `file_blame`.
      Expected: `file_read` shows the sed result; `file_blame` shows line 2
      owned by `workspace_session:<one-shot id>` while lines 1 and 3 keep owner
      `operation:<request A>`.
- [x] Sessionless `file_write` publishes `victim.txt`; a one-shot `exec_command`
      runs `rm victim.txt` (exit 0); then sessionless `file_read` and
      `file_edit` target the path.
      Expected: the published `Delete` whiteout makes both `file_read` and
      `file_edit` return kind `not_found` (`file not found: victim.txt`), not
      stale content.
- [x] One-shot exec creates `reports/daily/r1.txt`, a second one-shot exec runs
      `rm -rf reports`, then sessionless `file_write` creates
      `reports/daily/r2.txt` and a third exec runs `cat reports/daily/r2.txt`.
      Expected: after the `rm -rf`, `file_read reports/daily/r1.txt` is
      `not_found` (parent hidden by whiteout, never resolved to the old object);
      the `file_write` returns `type = create` with parents recreated; the final
      exec exits 0 printing r2's content, and r1 stays `not_found`.
- [x] One-shot exec runs `mv old.txt new.txt` over a previously published file,
      then sessionless reads and blame on both paths.
      Expected: `file_read old.txt` → `not_found`; `file_read new.txt` returns
      the moved content; `file_blame new.txt` attributes its lines to
      `workspace_session:<one-shot id>`.
- [x] Sessionless `file_write` creates `scripts/hello.sh` (echo + side-effect
      write to `out/result.txt`), then one one-shot `exec_command` runs
      `chmod +x scripts/hello.sh && ./scripts/hello.sh`.
      Expected: exec `status = ok`, `exit_code = 0` with the script's stdout in
      `output`; sessionless `file_read out/result.txt` returns the side-effect
      content written by the script.
- [x] One-shot exec creates an executable in one command
      (`printf '#!/bin/sh\necho tool-v1' > tool.sh && chmod +x tool.sh`); then
      sessionless `file_read tool.sh` and a second one-shot exec runs
      `./tool.sh`.
      Expected: `file_read` returns the script source; the executable bit
      survives capture/publish/projection, so the second exec returns
      `exit_code = 0` with `tool-v1` in `output`.
- [x] One-shot exec creates `ln -s real.txt link.txt` next to a published
      `real.txt`; then sessionless `file_read link.txt` and
      `file_write link.txt`.
      Expected: the symlink is published as a symlink entry; both operations
      fail `invalid_request` with `path is not a regular file (Symlink)`; the
      symlink is not followed and `file_read real.txt` still returns the
      original content.
- [x] One-shot exec creates a symlinked directory
      (`mkdir realdir; printf x > realdir/inner.txt; ln -s realdir linkdir`);
      then sessionless `file_read linkdir/inner.txt` and
      `file_write linkdir/new.txt`.
      Expected: both are rejected as `invalid_request` (symlink parent; no
      parent-symlink traversal); `file_read realdir/inner.txt` through the real
      parent succeeds.
- [x] One-shot exec runs `mkfifo pipe.fifo && printf ok > note.txt`; then
      sessionless `file_read` of both paths.
      Expected: exec `exit_code = 0`; capture drops the FIFO as a protected drop
      (`unsupported_special_file`) so `file_read pipe.fifo` → `not_found`, while
      `note.txt` is published and readable — the special file never reaches the
      layerstack.
- [x] One-shot exec writes a file with a UTF-8 BOM and CRLF endings
      (`printf '\xef\xbb\xbfline1\r\nline2\r\n' > mixed.txt`); then sessionless
      `file_read mixed.txt`.
      Expected: BOM removed and CRLF normalized before windowing —
      `content = "line1\nline2"`, `start_line = 1`, `total_lines = 2`, no `\r`
      bytes in content.
- [x] One-shot exec writes non-UTF-8 bytes
      (`head -c 64 /dev/urandom > blob.bin`); then sessionless
      `file_read blob.bin` and `file_edit blob.bin`.
      Expected: both fail `invalid_request` with
      `file is not valid UTF-8: blob.bin`; neither returns partial bytes.
- [x] Sessionless `file_write` creates a 10-line `data/data.csv`, then a
      one-shot exec runs `wc -l < data/data.csv && grep -c ',' data/data.csv`.
      Expected: exec `status = ok`, `exit_code = 0`, `output` shows the expected
      line and match counts — the exec consumes exactly the published
      `file_write` content.
- [x] [complex] Long sessionless interleave: 12 rounds where round *i* runs a
      one-shot exec appending `exec-i` to `journal.txt` followed by a
      sessionless `file_edit` rewriting that line to `edited-i`; finish with a
      one-shot exec `cat journal.txt`.
      Expected: every round's exec sees all prior published rounds (layer
      ordering); final `file_read` and the exec `output` both show the 12
      `edited-i` lines in order; `file_blame` shows each line owned by its
      round's `operation:<edit request_id>` (no `workspace_session:` owners
      survive on edited lines).
- [x] [complex] One-shot exec generates 400 files (`src/f001.txt`…`f400.txt`,
      each with a unique known body) and packs `tar czf bundle/src.tgz src`; a
      second one-shot exec extracts it to `unpacked/` (`tar xzf`).
      Expected: both execs `exit_code = 0`; sampled sessionless `file_read`s of
      `unpacked/src/f001.txt`, `f200.txt`, `f400.txt` return the exact bodies;
      `file_blame` on a sampled file shows one range owned by the extracting
      exec's `workspace_session:<id>`; a missing index like `f401.txt` is
      `not_found`.
- [x] [complex] 200 sessionless `file_write`s create
      `parts/part_001.txt`…`part_200.txt` (2 known lines each) plus a `build.sh`
      that concatenates and validates them; one-shot exec runs `sh build.sh`.
      Expected: exec `status = ok`, `exit_code = 0`, `output` reports the
      expected `400` total lines / PASS marker; sessionless `file_read` of the
      exec-produced `all.txt` returns the concatenation, and
      `file_blame all.txt` shows owner `workspace_session:<one-shot id>`.
- [x] [complex] One-shot exec generates a multi-MB file
      (`seq 1 500000 > big/seq.txt`, ~3.4 MB); then windowed sessionless reads.
      Expected: `file_read --offset 250000 --limit 100` returns
      `start_line = 250000`, `num_lines = 100`, lines `250000..250099`,
      `total_lines = 500000`, `truncated = true`, `next_offset = 250100`;
      `--offset 600000` (past EOF) returns an empty content window with correct
      totals, not `not_found`; the read never fails merely because the whole
      file is large.
- [x] [complex] One-shot exec generates a single ~300 KB line
      (`head -c 300000 /dev/zero | tr '\0' x > wide.txt`); then sessionless
      `file_read wide.txt` and a follow-up one-shot exec `wc -c wide.txt`.
      Expected: `file_read` fails `invalid_request` with
      `selected read output exceeds the maximum of 262144 bytes`
      (`OutputTooLarge`, not `FileTooLarge`); the follow-up exec exits 0
      reporting 300000 bytes, proving the file itself published intact.
- [x] [complex] One-shot exec generates a >4 MiB text file
      (`yes padding-line | head -n 500000 > big/pad.txt`); then sessionless
      `file_edit` on it and a small windowed `file_read`.
      Expected: `file_edit` fails `invalid_request` with `file is too large`
      (`MAX_EDIT_BYTES` = 4 MiB, rejected before transform, no layer committed);
      `file_read --offset 1 --limit 5` on the same file still succeeds with
      `total_lines = 500000`.

### File Ops + Exec Ops — Session (8)

- [x] In a created workspace session, session exec
      (`exec_command --workspace-session-id`) writes `s/notes.txt`; session
      `file_edit` rewrites it; session exec `cat s/notes.txt` re-reads it; then
      a sessionless `file_read` of the same path.
      Expected: session `file_read` sees the shell-created content, the edit
      returns `replacements = 1`, the `cat` exec `output` shows the edited text
      (live overlay round-trip), and the sessionless read is `not_found`
      (nothing published).
- [x] Start a long-lived interactive shell in a session
      (`exec_command --workspace-session-id --yield-time-ms 0 "sh"` →
      `status = running` with `command_session_id`); session `file_write`
      creates `live.txt`; `write_command_stdin "cat live.txt\n"`; session
      `file_edit` changes it; `cat` again; then `exit`.
      Expected: `read_command_lines`/yield output shows the written content and
      then the edited content while the command is still alive (mounted-overlay
      visibility); final stdin `exit` yields `status = ok`, `exit_code = 0`.
- [x] Session exec creates `d/x.txt`, session `file_read` confirms it, session
      exec runs `rm d/x.txt && rmdir d`; then session `file_read d/x.txt`,
      session `file_write d/sub/y.txt`, and session exec `cat d/sub/y.txt`.
      Expected: after the removal the session read is `not_found`; the session
      write returns `type = create` recreating parents through the mounted
      overlay; the final exec exits 0 printing y's content.
- [x] Session exec runs `mkdir p && mkfifo p/f.fifo` (exit 0); then session
      `file_read p/f.fifo` and session `file_write p/f.fifo`.
      Expected: both fail `invalid_request` with
      `path is not a regular file (Other)` — the in-namespace runner classifies
      the FIFO by stat and never opens or overwrites it.
- [x] Session `file_write` creates `run.sh` printing `v1`; session exec runs
      `chmod +x run.sh && ./run.sh` (exit 0, `v1`); session `file_write` updates
      the script body to print `v2`; session exec runs `./run.sh` again without
      re-chmodding.
      Expected: the session write preserves the existing executable mode (runner
      `fchmod` with the prior `st_mode`), so the second exec returns
      `exit_code = 0` with `v2` in `output`.
- [x] [complex] One-shot capture lifecycle: start a sessionless
      `exec_command --yield-time-ms 0 "printf shell-line > mix.txt && sh"`
      (still `running`); discover its one-shot workspace id via
      `observability snapshot` (`workspaces[].workspace_id` with an
      `active_namespace_executions` entry for `exec_command`); run session
      `file_write mix2.txt` and `file_edit mix.txt` with that
      `--workspace-session-id`; then `write_command_stdin "exit\n"`.
      Expected: on terminal `status = ok` the one-shot finalize captures and
      publishes the combined shell + file-op changes, so sessionless `file_read`
      sees both `mix.txt` (edited) and `mix2.txt`; `file_blame` on both shows
      owner `workspace_session:<that one-shot id>`; a session `file_read` with
      the now-destroyed workspace id fails `not_found`
      (`workspace session not found`).
- [x] [complex] Long interleaved session then destroy: in one caller-owned
      session run 15 alternating rounds of session exec append to `journal.txt`
      + session `file_edit` of that line + session `file_read` verification,
      plus 20 session `file_write`s under `notes/` checked by session exec
      `ls notes | wc -l` (`20`); attempt `destroy_workspace_session` while an
      interactive shell is still running, then exit it and destroy for real.
      Expected: every in-session read shows the combined shell+file-op state;
      sessionless `file_read journal.txt` is `not_found` the whole time; the
      first destroy fails `operation_failed` listing the running
      `active_command_session_ids`; after exit, destroy returns
      `destroyed = true` with `evicted_upperdir_bytes > 0`, and sessionless
      reads remain `not_found` — uncaptured caller-owned session changes are
      discarded, never published.
- [x] [complex] Large in-session volume: session exec generates 300 files
      (`gen/f001..f300`, unique bodies) and a ~3 MB `gen/big.txt`
      (`seq 1 400000`) plus one ~300 KB single-line `gen/wide.txt`; then
      windowed session `file_read`s.
      Expected: sampled session reads of `f001`/`f150`/`f300` match exactly;
      `file_read --workspace-session-id --offset 200000 --limit 50` on `big.txt`
      returns lines `200000..200049` with `total_lines = 400000` and
      `truncated = true` (the runner windows inside the namespace instead of
      shipping the whole file); session read of `wide.txt` fails
      `invalid_request` `OutputTooLarge`; sessionless `file_read` of any `gen/`
      path is `not_found` before capture.

### File Blame — Sessionless (6)

- [x] Blame key resolution and unaudited paths: sessionless `file_write`
      creates `src/notes.txt` (request A), then `file_blame` is called with
      `--path src/notes.txt` and `--path ./src/notes.txt`, plus on a base file
      that shipped in the workspace and was never touched, on the directory
      `src`, and on a path containing `..`.
      Expected: both spellings of the written path return byte-identical
      responses — one range
      `{start_line = 1, line_count = N, owner = "operation:<A>"}` (the audit
      key normalizes through `LayerPath`); the never-touched base file (whose
      `file_read` succeeds), the directory, and the `..` path each fault kind
      `not_found` with `no auditability record for path: <path>` — blame is a
      pure store read, never a snapshot read.
- [x] Same-owner coalescing: sessionless `file_write` seeds a 6-line file
      (request A), then one sessionless `file_edit` (request B) carries two
      `edits` entries rewriting the adjacent lines 3 and 4 in the same request.
      Expected: `file_blame` returns exactly three ranges —
      `{1, 2, operation:<A>}`, `{3, 2, operation:<B>}` (the two adjacent
      same-owner lines coalesce into one range, not two),
      `{5, 2, operation:<A>}`; ranges are contiguous, non-overlapping, and
      their `line_count` sums to the file's `total_lines` from `file_read`.
- [x] Line-shift on insert and delete: seed a 6-line file via `file_write`
      (request A); `file_edit` (request B) replaces the one-line `old_string`
      at line 3 with a three-line `new_string`; then `file_edit` (request C)
      replaces those three lines with a single new line.
      Expected: after B, blame tiles 8 lines as `{1,2,operation:<A>}`,
      `{3,3,operation:<B>}`, `{6,3,operation:<A>}` — untouched trailing lines
      shift down but keep owner A; after C, blame tiles 6 lines as
      `{1,2,operation:<A>}`, `{3,1,operation:<C>}`, `{4,3,operation:<A>}` —
      the shrink shifts trailing ownership up without reassigning any
      untouched line.
- [x] `replace_all` multi-site attribution: seed a 9-line file (request A)
      whose lines 2, 5, and 8 each contain the same unique token; one
      `file_edit` with `replace_all = true` (request B) rewrites all three
      occurrences (`replacements = 3`).
      Expected: blame shows lines 2, 5, and 8 each owned by the single
      `operation:<B>` as three separate one-line ranges (non-adjacent
      same-owner lines never falsely coalesce across the intervening
      `operation:<A>` lines), and all other lines remain `operation:<A>`; the
      tiling still covers lines 1–9 exactly once.
- [x] [complex] 50-round insert/delete ladder from many actors: seed a 10-line
      file (request A), then run 50 sequential sessionless `file_edit`
      requests — odd rounds insert a uniquely-tokened line (one-line
      `old_string` → two-line `new_string`), even rounds delete one previously
      inserted token line — while an oracle script mirrors the same operations
      locally to compute expected per-line owners; call `file_blame` after
      every 10th round and at the end.
      Expected: every sampled blame response exactly matches the oracle — full
      tiling with correct shifted `start_line` values after 50 geometry
      changes, each surviving inserted line owned by its inserting
      `operation:<request_id>` (50 distinct ids in play), and all 10 seed
      lines still owned by `operation:<A>`; no line ever reports `unknown`.
- [x] [complex] Owner turnover across 30 wholesale generations: issue 30
      sequential sessionless `file_write` requests to one path where
      generation *i* has a distinct line count (alternating 5, 40, and 12
      lines) and every line's text is unique across all generations (so the
      publish diff marks every line `Command`); call `file_blame` after each
      generation.
      Expected: after every generation, blame returns exactly one range
      `{start_line = 1, line_count = <that generation's line count>,
      owner = "operation:<that write's request_id>"}` — no residue from any of
      the 29 prior owners, `line_count` tracks each grow/shrink exactly, and
      the final response still matches after 30 audit events of history on the
      path.

### File Blame — Session (4)

- [x] Live-session changes are invisible to blame: after a sessionless
      `file_write` seeds `seed.txt` (request A), `create_workspace_session`,
      then session `file_write` creates `s/draft.txt` and session `file_edit`
      rewrites a line of `seed.txt` via `--workspace-session-id`; call
      `file_blame` on both paths before and after
      `destroy_workspace_session`.
      Expected: `file_blame s/draft.txt` faults `not_found` with
      `no auditability record for path: s/draft.txt` at every point
      (file_blame takes only `path` — there is no session-scoped blame), and
      `file_blame seed.txt` returns byte-identical ranges owned by
      `operation:<A>` before, during, and after the session — uncaptured
      session ops append no audit events.
- [x] Capture insertion shifts without reassigning: sessionless `file_write`
      seeds a 5-line file (request A), then a one-shot `exec_command` runs
      `sed -i '3i marker'` on it so the capture publish inserts one line
      mid-file.
      Expected: `file_blame` tiles 6 lines as exactly three ranges —
      `{1, 2, operation:<A>}`, `{3, 1, workspace_session:<one-shot id>}`,
      `{4, 3, operation:<A>}`; only the inserted line carries the session
      owner and the shifted lines 4–6 retain `operation:<A>` at their new
      positions.
- [x] Capture deletion mints no ownership: seed a 4-line file via `file_write`
      (request A), then `file_edit` rewrites line 4 (request B); a one-shot
      `exec_command` runs `sed -i '2d'` so its capture publishes a pure line
      deletion.
      Expected: `file_blame` tiles 3 lines as `{1, 2, operation:<A>}`,
      `{3, 1, operation:<B>}` — the deleting exec's `workspace_session:<id>`
      appears on no line (a deletion introduces no `Command` lines), and B's
      ownership shifts from line 4 to line 3 intact.
- [x] [complex] Deep prepend history across 20 captures: seed a 5-line file
      via `file_write` (request A), then run 20 sequential one-shot
      `exec_command` invocations where exec *i* runs `sed -i "1i gen-<i>"` on
      the file, each capture three-way merging against the advanced head;
      spot-check `file_blame` after exec 10 and after exec 20.
      Expected: after exec 20, blame tiles 25 lines as 21 ranges — lines 1..20
      are twenty one-line ranges owned by 20 distinct `workspace_session:<id>`
      owners in reverse capture order (line 1 = exec 20's session, line 20 =
      exec 1's), and lines 21–25 remain one coalesced
      `{21, 5, operation:<A>}` range; the exec-10 spot check shows the same
      structure at 15 lines, proving inherited owners survive repeated
      shift-and-merge across a deep capture history.
