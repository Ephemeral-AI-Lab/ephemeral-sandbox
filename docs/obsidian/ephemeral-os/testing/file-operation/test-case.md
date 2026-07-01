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

These cases are the implementation guardrails for `file_read`, `file_write`,
and `file_edit`. They are intentionally written as a coverage checklist rather
than a full test implementation.

## Must-Hold Contract

- [ ] Sessionless `file_read` reads the latest published layerstack snapshot.
- [ ] Sessionless `file_write` and `file_edit` publish a new layer on head.
- [ ] Session `file_read`, `file_write`, and `file_edit` operate through the
      live session namespace and mounted workspace.
- [ ] Session writes/edits do not publish immediately and are attributed only
      when the session is captured.
- [ ] File operations never read from or write to the detached host workspace as
      the source of truth.
- [ ] Session writes/edits never mutate `entry.upperdir` directly from the host.

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
      Expected: invalid request / too large for selected output.
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
- [ ] Same text/windowing cases as sessionless read.
      Expected: BOM, CRLF/CR, `offset`, `limit`, and large-window behavior match.
- [ ] Directory, symlink, and special file in the live overlay.
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
      Expected: either no-op behavior is explicit or published attribution stays
      consistent with the final implementation contract.
- [ ] Missing parent directories.
      Expected: parents are created if the operation contract allows it.
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
- [ ] Content exceeds write size cap.
      Expected: rejected before publish.
- [ ] Concurrent publish conflict on head.
      Expected: retry on classified OCC conflict, then publish on latest head or
      return a clear conflict after bounded retries.
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
      Expected: write happens through the namespace runner against the mounted
      workspace, not by host-side mutation of `entry.upperdir`.
- [ ] Missing parent directories.
      Expected: parents are created through the mounted overlay if allowed.
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
- [ ] Final content exceeds write size cap.
      Expected: invalid request before publish.
- [ ] Edit target contains invalid UTF-8.
      Expected: invalid request / not UTF-8.
- [ ] Concurrent publish conflict on head.
      Expected: re-read, re-apply edits, then publish or return a bounded retry
      failure; no stale-content overwrite.
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
      Expected: either detected as a clear conflict before rename or last-writer
      wins according to documented filesystem behavior; no partial file.
- [ ] Symlink parent or symlink target.
      Expected: invalid request; no symlink traversal.

## Path Cases

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

- [ ] Namespace file-op request uses compact binary-safe encoding for content.
      Expected: no raw JSON `Vec<u8>` arrays for large file payloads.
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

## Live Smoke Checklist

AGENTS.md requires rebuilding the Docker sandbox gateway binary before live
sandbox checks:

```sh
bin/start-sandbox-docker-gateway --rebuild-binary
```

Use `sandbox-cli` for manual sandbox operations.

```sh
# sessionless publish path
bin/sandbox-cli runtime file_write --path notes/hello.txt --content "hi"
bin/sandbox-cli runtime file_read --path notes/hello.txt
bin/sandbox-cli runtime file_edit --path notes/hello.txt \
  --edits '[{"old_string":"hi","new_string":"bye"}]'
bin/sandbox-cli runtime file_blame --path notes/hello.txt

# session namespace path
ws=$(bin/sandbox-cli runtime create_workspace_session --json | jq -r .workspace_session_id)
bin/sandbox-cli runtime file_write --workspace-session-id "$ws" --path a.txt --content "x"
bin/sandbox-cli runtime file_read --workspace-session-id "$ws" --path a.txt
bin/sandbox-cli runtime file_read --path a.txt

# coherence with a live command in the same session
bin/sandbox-cli runtime exec_command --workspace-session-id "$ws" --yield-time-ms 0 "sleep 30"
bin/sandbox-cli runtime file_write --workspace-session-id "$ws" --path b.txt --content "y"
bin/sandbox-cli runtime exec_command --workspace-session-id "$ws" "cat b.txt"
```
