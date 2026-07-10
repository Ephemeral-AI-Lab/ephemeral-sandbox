"""Live e2e: Correctness: Layerstack, Mount, Conflict — Session (9 cases)."""

import pytest

from runtime.file.helpers import (
    assert_blame_owners,
    assert_blame_tiling,
    assert_content,
    assert_error,
    assert_manifest_delta,
    assert_ok,
    assert_single_owner,
    create_workspace_session,
    destroy_workspace_session,
    edit,
    exec_command,
    file_edit,
    file_read,
    file_write,
    layer_ids,
    layerstack,
    owners_by_line,
    sandbox_from_workspace,
    write_command_stdin,
)


def _exec_ok(sandbox, command, **kwargs):
    kwargs.setdefault("yield_time_ms", 30_000)
    result = exec_command(sandbox, command, **kwargs)
    assert result["status"] == "ok", result
    assert result["exit_code"] == 0, result
    return result


def _start_gated_command(sandbox, command):
    result = exec_command(sandbox, command, yield_time_ms=0, timeout_ms=120_000)
    assert result["status"] == "running", result
    assert result["command_session_id"], result
    return result["command_session_id"]


def _release_gated_command(sandbox, command_session_id):
    result = write_command_stdin(
        sandbox,
        command_session_id,
        "go\n",
        yield_time_ms=5_000,
        timeout=240,
    )
    assert result["status"] == "ok", result
    assert result["exit_code"] == 0, result
    return result


def _assert_stack_unchanged(sandbox, before):
    after = layerstack(sandbox)
    assert after["manifest_version"] == before["manifest_version"], after
    assert after["root_hash"] == before["root_hash"], after
    assert layer_ids(sandbox) == [layer["layer_id"] for layer in before["layers"]]
    return after


def _created_path(index):
    return f"capture/new/d{index % 10:02d}/file-{index}.txt"


def test_frozen_snapshot_mount_excludes_later_published_layers(tmp_path):
    """Frozen snapshot mount: `create_workspace_session`, then sessionless
    `file_write` a new file and update an existing base file.
    Expected: session `file_read --workspace-session-id` of the new path is
    `not_found` and of the updated path returns the pre-update snapshot
    content; an in-session `exec_command cat` agrees;
    `observability layerstack --workspace-id` shows the session's `mounts`
    exclude the newly published `layer_id` even though the sandbox-wide view
    lists it."""
    files = {"base/existing.txt": "before"}
    with sandbox_from_workspace(tmp_path, files=files) as sandbox:
        session_id = create_workspace_session(sandbox)
        try:
            session_view_before = layerstack(sandbox, workspace_id=session_id)
            mounted_before = [mount["layer_id"] for mount in session_view_before["mounts"]]
            stack_before = layerstack(sandbox)

            assert_ok(file_write(sandbox, "session-frozen/new.txt", "new"))
            assert_single_owner(sandbox, "session-frozen/new.txt", prefix="operation:")
            assert_ok(file_write(sandbox, "base/existing.txt", "after"))
            assert_single_owner(sandbox, "base/existing.txt", prefix="operation:")

            assert_error(
                file_read(
                    sandbox,
                    "session-frozen/new.txt",
                    workspace_session_id=session_id,
                ),
                "not_found",
            )
            assert_content(
                file_read(
                    sandbox,
                    "base/existing.txt",
                    workspace_session_id=session_id,
                ),
                "before",
            )
            result = _exec_ok(
                sandbox,
                "cat base/existing.txt; test ! -e session-frozen/new.txt",
                workspace_session_id=session_id,
            )
            assert result["output"] == "before", result

            stack_after = assert_manifest_delta(sandbox, stack_before, 2)
            before_layer_ids = {layer["layer_id"] for layer in stack_before["layers"]}
            new_layers = [
                layer["layer_id"]
                for layer in stack_after["layers"]
                if layer["layer_id"] not in before_layer_ids
            ]
            assert len(new_layers) == 2, stack_after
            session_view_after = layerstack(sandbox, workspace_id=session_id)
            mounted_after = [mount["layer_id"] for mount in session_view_after["mounts"]]
            assert mounted_after == mounted_before, session_view_after
            assert set(new_layers).isdisjoint(mounted_after), session_view_after
        finally:
            destroy_workspace_session(sandbox, session_id, grace_s=1)


@pytest.mark.slow
def test_complex_session_overlay_at_scale_through_mount(tmp_path):
    """[complex] Session overlay at scale through the mount: 100 session
    `file_write` calls (`--workspace-session-id`) across nested directories in
    a caller-owned session.
    Expected: an in-session `exec_command "find . -type f | wc -l"` counts all
    100 through the mounted overlay; session `file_read` spot-checks match;
    `observability layerstack --workspace-id` reports `upper_bytes > 0`; the
    sandbox `manifest_version` is unchanged (session file ops never publish)."""
    with sandbox_from_workspace(tmp_path) as sandbox:
        before = layerstack(sandbox)
        session_id = create_workspace_session(sandbox)
        try:
            for index in range(1, 101):
                path = f"session-scale/dir{index % 10:02d}/file-{index:03d}.txt"
                result = file_write(
                    sandbox,
                    path,
                    f"session-{index:03d}",
                    workspace_session_id=session_id,
                )
                assert result["type"] == "create", result

            count = _exec_ok(
                sandbox,
                "find . -type f | wc -l",
                workspace_session_id=session_id,
            )
            assert int(count["output"].strip()) == 100, count
            for index in [1, 25, 50, 75, 100]:
                path = f"session-scale/dir{index % 10:02d}/file-{index:03d}.txt"
                assert_content(
                    file_read(sandbox, path, workspace_session_id=session_id),
                    f"session-{index:03d}",
                )
            session_view = layerstack(sandbox, workspace_id=session_id)
            assert session_view["upper_bytes"] > 0, session_view
            _assert_stack_unchanged(sandbox, before)
        finally:
            destroy_workspace_session(sandbox, session_id, grace_s=1)


def test_destroy_discards_the_overlay(tmp_path):
    """Destroy discards the overlay: session `file_write` several files in a
    caller-owned session, then `destroy_workspace_session`.
    Expected: destroy returns `destroyed: true`; `active_lease_count`
    decrements; sessionless `file_read` of every session path is `not_found`;
    a fresh `create_workspace_session` also reads them `not_found`;
    `manifest_version` unchanged."""
    paths = [f"discard/file-{index}.txt" for index in range(1, 4)]
    with sandbox_from_workspace(tmp_path) as sandbox:
        before = layerstack(sandbox)
        session_id = create_workspace_session(sandbox)
        for path in paths:
            assert_ok(
                file_write(
                    sandbox,
                    path,
                    f"draft {path}\n",
                    workspace_session_id=session_id,
                )
            )
        with_session = layerstack(sandbox)
        assert with_session["active_lease_count"] == before["active_lease_count"] + 1
        destroy = assert_ok(destroy_workspace_session(sandbox, session_id, grace_s=1))
        assert destroy["destroyed"] is True, destroy
        after_destroy = _assert_stack_unchanged(sandbox, before)
        assert after_destroy["active_lease_count"] == before["active_lease_count"]

        for path in paths:
            assert_error(file_read(sandbox, path), "not_found")

        fresh = create_workspace_session(sandbox)
        try:
            for path in paths:
                assert_error(
                    file_read(sandbox, path, workspace_session_id=fresh),
                    "not_found",
                )
        finally:
            destroy_workspace_session(sandbox, fresh, grace_s=1)


def test_one_shot_capture_end_state(tmp_path):
    """One-shot capture end state: a single one-shot `exec_command` creates one
    file, modifies one seeded base file, and deletes another base file.
    Expected: exactly one new layer (`manifest_version` +1); sessionless
    `file_read` shows the new file and the modification, and `not_found` for
    the deleted path; `file_blame` shows created/changed lines owned by
    `workspace_session:<id>` and untouched lines `original`."""
    files = {
        "capture/base.txt": "keep\nchange-me",
        "capture/delete.txt": "delete",
    }
    with sandbox_from_workspace(tmp_path, files=files) as sandbox:
        before = layerstack(sandbox)
        _exec_ok(
            sandbox,
            "mkdir -p capture/new && "
            "printf 'created' > capture/new/file.txt && "
            "sed -i '2s/.*/changed/' capture/base.txt && "
            "rm capture/delete.txt",
        )
        assert_manifest_delta(sandbox, before, 1)
        assert_content(file_read(sandbox, "capture/new/file.txt"), "created")
        created_owner = assert_single_owner(
            sandbox,
            "capture/new/file.txt",
            prefix="workspace_session:",
        )
        assert_content(file_read(sandbox, "capture/base.txt"), "keep\nchanged")
        owners = owners_by_line(assert_blame_tiling(sandbox, "capture/base.txt"))
        assert owners[0] == "original", owners
        assert owners[1] == created_owner or owners[1].startswith("workspace_session:")
        assert_error(file_read(sandbox, "capture/delete.txt"), "not_found")


def test_capture_after_base_advanced_clean_auto_merge(tmp_path):
    """Capture after the base advanced — clean auto-merge: start a one-shot exec
    blocked on stdin (`read x; printf "tail\n" >> notes.txt`), sessionless
    `file_edit` line 1 of `notes.txt` while it waits, then
    `write_command_stdin` to release it.
    Expected: the capture publishes via three-way merge (no rejection): final
    sessionless `file_read notes.txt` contains both the edited line 1 and the
    appended tail; `file_blame` shows line 1 owned by
    `operation:<edit request_id>` and the tail line by
    `workspace_session:<id>`."""
    with sandbox_from_workspace(tmp_path, files={"notes.txt": "one\ntwo\n"}) as sandbox:
        before = layerstack(sandbox)
        command_session_id = _start_gated_command(
            sandbox,
            "read x; printf 'tail\\n' >> notes.txt",
        )
        assert_ok(file_edit(sandbox, "notes.txt", [edit("one", "ONE")]))
        edit_owner = owners_by_line(assert_blame_tiling(sandbox, "notes.txt"))[0]
        assert edit_owner.startswith("operation:")

        _release_gated_command(sandbox, command_session_id)
        assert_content(file_read(sandbox, "notes.txt"), "ONE\ntwo\ntail")
        owners = owners_by_line(assert_blame_tiling(sandbox, "notes.txt"))
        assert owners[0] == edit_owner, owners
        assert owners[1] == "original", owners
        assert owners[2].startswith("workspace_session:"), owners
        assert_manifest_delta(sandbox, before, 2)


def test_capture_after_base_advanced_overlapping_conflict(tmp_path):
    """Capture after the base advanced — overlapping conflict: same stdin-gated
    one-shot pattern, but the in-session command rewrites the same line the
    sessionless `file_edit` changed, with different content.
    Expected: the command still ends `status = ok`, but the capture publish is
    rejected with `source_conflict` and discarded — `file_read` returns only
    the sessionless edit's content, `manifest_version` reflects only the
    sessionless publish, and `file_blame` still shows
    `operation:<request_id>` on the contested line."""
    with sandbox_from_workspace(tmp_path, files={"notes.txt": "one\ntwo"}) as sandbox:
        before = layerstack(sandbox)
        command_session_id = _start_gated_command(
            sandbox,
            "read x; sed -i '1s/.*/SESSION/' notes.txt",
        )
        assert_ok(file_edit(sandbox, "notes.txt", [edit("one", "OPERATION")]))
        edit_owner = owners_by_line(assert_blame_tiling(sandbox, "notes.txt"))[0]

        result = write_command_stdin(
            sandbox,
            command_session_id,
            "go\n",
            yield_time_ms=5_000,
            timeout=240,
        )
        assert result["status"] == "ok", result
        assert result["exit_code"] == 0, result
        assert_content(file_read(sandbox, "notes.txt"), "OPERATION\ntwo")
        assert_blame_owners(sandbox, "notes.txt", [edit_owner, "original"])
        assert_manifest_delta(sandbox, before, 1)


def test_session_delete_vs_sessionless_modify_rejects_atomically(tmp_path):
    """Session delete vs sessionless modify: a stdin-gated one-shot session
    deletes `shared.txt` (and also creates `unrelated.txt`) after a
    sessionless `file_write` updated `shared.txt` post-session-start.
    Expected: the capture is rejected atomically (`source_conflict` on the
    delete, which cannot merge): `file_read shared.txt` returns the
    sessionless content, `file_read unrelated.txt` is `not_found` (no partial
    changeset escapes), and no layer is added for the capture."""
    with sandbox_from_workspace(tmp_path, files={"shared.txt": "base"}) as sandbox:
        before = layerstack(sandbox)
        command_session_id = _start_gated_command(
            sandbox,
            "read x; rm shared.txt; printf 'unrelated' > unrelated.txt",
        )
        assert_ok(file_write(sandbox, "shared.txt", "sessionless"))
        owner = assert_single_owner(sandbox, "shared.txt", prefix="operation:")

        result = write_command_stdin(
            sandbox,
            command_session_id,
            "go\n",
            yield_time_ms=5_000,
            timeout=240,
        )
        assert result["status"] == "ok", result
        assert result["exit_code"] == 0, result
        assert_content(file_read(sandbox, "shared.txt"), "sessionless")
        assert_single_owner(sandbox, "shared.txt", owner=owner)
        assert_error(file_read(sandbox, "unrelated.txt"), "not_found")
        assert_manifest_delta(sandbox, before, 1)


@pytest.mark.slow
def test_complex_two_sessions_from_one_base_captured_in_sequence(tmp_path):
    """[complex] Two sessions from one base captured in sequence: start two
    stdin-gated one-shot execs back-to-back on the same `manifest_version`;
    each creates ~100 distinct files and edits a disjoint region of one shared
    file; release session 1, then session 2.
    Expected: both captures publish (`manifest_version` +2), the second via
    clean merge against the advanced head; the final merged view contains both
    file sets and both shared-file regions; `file_blame` on the shared file
    maps each region to its own `workspace_session:<id>`."""
    shared = "\n".join(f"base-{index:03d}" for index in range(1, 201))
    with sandbox_from_workspace(tmp_path, files={"shared.txt": shared}) as sandbox:
        before = layerstack(sandbox)
        cmd1 = _start_gated_command(
            sandbox,
            "read x; mkdir -p session-one; "
            "for i in $(seq 1 100); do "
            "printf 'one-%03d' \"$i\" > session-one/file-$i.txt; done; "
            "for i in $(seq 1 10); do sed -i \"${i}s/.*/one-${i}/\" shared.txt; done",
        )
        cmd2 = _start_gated_command(
            sandbox,
            "read x; mkdir -p session-two; "
            "for i in $(seq 1 100); do "
            "printf 'two-%03d' \"$i\" > session-two/file-$i.txt; done; "
            "for i in $(seq 101 110); do sed -i \"${i}s/.*/two-${i}/\" shared.txt; done",
        )

        _release_gated_command(sandbox, cmd1)
        _release_gated_command(sandbox, cmd2)
        assert_manifest_delta(sandbox, before, 2)
        for index in [1, 50, 100]:
            assert_content(
                file_read(sandbox, f"session-one/file-{index}.txt"),
                f"one-{index:03d}",
            )
            assert_content(
                file_read(sandbox, f"session-two/file-{index}.txt"),
                f"two-{index:03d}",
            )
        shared_read = assert_ok(file_read(sandbox, "shared.txt", offset=1, limit=120))
        assert "one-1" in shared_read["content"], shared_read
        assert "two-101" in shared_read["content"], shared_read
        owners = owners_by_line(assert_blame_tiling(sandbox, "shared.txt"))
        owner_one = owners[0]
        owner_two = owners[100]
        assert owner_one.startswith("workspace_session:"), owners[:12]
        assert owner_two.startswith("workspace_session:"), owners[98:112]
        assert owner_one != owner_two
        assert owners[:10] == [owner_one] * 10, owners[:12]
        assert owners[100:110] == [owner_two] * 10, owners[98:112]


@pytest.mark.slow
def test_complex_capture_with_hundreds_of_changed_files(tmp_path):
    """[complex] Capture with hundreds of changed files: one one-shot exec
    creates 300 files, modifies 50 seeded base files, and deletes 20 others
    across a nested tree.
    Expected: exactly one new layer (`manifest_version` +1); spot sessionless
    `file_read` confirms creations and modifications, deleted paths are
    `not_found` through published whiteouts, and `file_blame` spot-checks
    attribute changed lines to `workspace_session:<id>` with untouched lines
    `original`."""
    files = {}
    for index in range(1, 51):
        files[f"capture/mod/file-{index}.txt"] = f"keep-{index}\nold-{index}"
    for index in range(1, 21):
        files[f"capture/delete/file-{index}.txt"] = f"delete-{index}"

    with sandbox_from_workspace(tmp_path, files=files) as sandbox:
        before = layerstack(sandbox)
        _exec_ok(
            sandbox,
            "for i in $(seq 1 300); do "
            "d=$(printf 'capture/new/d%02d' $((i % 10))); "
            "mkdir -p \"$d\"; "
            "printf 'new-%03d' \"$i\" > \"$d/file-$i.txt\"; "
            "done; "
            "for i in $(seq 1 50); do "
            "sed -i \"2s/.*/mod-${i}/\" capture/mod/file-$i.txt; "
            "done; "
            "for i in $(seq 1 20); do rm -f capture/delete/file-$i.txt; done",
            timeout=240,
        )
        assert_manifest_delta(sandbox, before, 1)

        for index in [1, 99, 150, 225, 300]:
            path = _created_path(index)
            assert_content(file_read(sandbox, path), f"new-{index:03d}")
            assert_single_owner(sandbox, path, prefix="workspace_session:")

        for index in [1, 17, 33, 50]:
            path = f"capture/mod/file-{index}.txt"
            assert_content(file_read(sandbox, path), f"keep-{index}\nmod-{index}")
            owners = owners_by_line(assert_blame_tiling(sandbox, path))
            assert owners[0] == "original", owners
            assert owners[1].startswith("workspace_session:"), owners

        for index in [1, 7, 20]:
            assert_error(file_read(sandbox, f"capture/delete/file-{index}.txt"), "not_found")
