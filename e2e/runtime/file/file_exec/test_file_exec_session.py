"""Live e2e: File Ops + Exec Ops - Session (8 cases)."""

import time

import pytest

from runtime.file.helpers import (
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
    layerstack,
    read_command_lines,
    snapshot,
    workspace_session,
    write_command_stdin,
)


def _exec_ok(sandbox, command, *, workspace_session_id=None, **kwargs):
    kwargs.setdefault("yield_time_ms", 30_000)
    result = exec_command(
        sandbox,
        command,
        workspace_session_id=workspace_session_id,
        **kwargs,
    )
    assert result["status"] == "ok", result
    assert result["exit_code"] == 0, result
    return result


def _start_shell(sandbox, workspace_session_id=None):
    result = exec_command(
        sandbox,
        "sh",
        workspace_session_id=workspace_session_id,
        yield_time_ms=0,
        timeout_ms=120_000,
    )
    assert result["status"] == "running", result
    assert result["command_session_id"], result
    return result["command_session_id"]


def _find_running_exec_workspace(sandbox, command_session_id):
    for _ in range(30):
        snap = snapshot(sandbox)
        for workspace in snap.get("workspaces", []):
            for execution in workspace.get("active_namespace_executions", []):
                if (
                    execution.get("namespace_execution_id") == command_session_id
                    and execution.get("operation") == "exec_command"
                ):
                    return workspace["workspace_id"]
        time.sleep(0.1)
    pytest.fail(f"workspace for command session {command_session_id} not found")


def _wait_for_session_file(sandbox, workspace_session_id, path):
    for _ in range(30):
        result = file_read(sandbox, path, workspace_session_id=workspace_session_id)
        if "error" not in result:
            return result
        time.sleep(0.1)
    pytest.fail(f"{path} never became visible in {workspace_session_id}")


def test_session_exec_file_edit_exec_round_trip_stays_unpublished(sandbox, workspace_session):
    """In a created workspace session, session exec
    (`exec_command --workspace-session-id`) writes `s/notes.txt`; session
    `file_edit` rewrites it; session exec `cat s/notes.txt` re-reads it; then
    a sessionless `file_read` of the same path.
    Expected: session `file_read` sees the shell-created content, the edit
    returns `replacements = 1`, the `cat` exec `output` shows the edited text
    (live overlay round-trip), and the sessionless read is `not_found`
    (nothing published)."""
    before = layerstack(sandbox)
    _exec_ok(
        sandbox,
        "mkdir -p s && printf 'one\\ntwo' > s/notes.txt",
        workspace_session_id=workspace_session,
    )
    assert_content(
        file_read(sandbox, "s/notes.txt", workspace_session_id=workspace_session),
        "one\ntwo",
    )
    result = assert_ok(
        file_edit(
            sandbox,
            "s/notes.txt",
            [edit("two", "TWO")],
            workspace_session_id=workspace_session,
        )
    )
    assert result["replacements"] == 1, result
    output = _exec_ok(sandbox, "cat s/notes.txt", workspace_session_id=workspace_session)
    assert "one\nTWO" in output["output"], output
    assert_error(file_read(sandbox, "s/notes.txt"), "not_found")
    assert_manifest_delta(sandbox, before, 0)


def test_interactive_session_shell_sees_file_ops_while_running(sandbox, workspace_session):
    """Start a long-lived interactive shell in a session
    (`exec_command --workspace-session-id --yield-time-ms 0 "sh"` ->
    `status = running` with `command_session_id`); session `file_write`
    creates `live.txt`; `write_command_stdin "cat live.txt\n"`; session
    `file_edit` changes it; `cat` again; then `exit`.
    Expected: `read_command_lines`/yield output shows the written content and
    then the edited content while the command is still alive (mounted-overlay
    visibility); final stdin `exit` yields `status = ok`, `exit_code = 0`."""
    before = layerstack(sandbox)
    command_session_id = _start_shell(sandbox, workspace_session)
    try:
        assert_ok(
            file_write(
                sandbox,
                "live.txt",
                "draft",
                workspace_session_id=workspace_session,
            )
        )
        first = write_command_stdin(
            sandbox,
            command_session_id,
            "cat live.txt\n",
            yield_time_ms=500,
        )
        assert first["status"] == "running", first
        assert "draft" in first["output"], first

        assert_ok(
            file_edit(
                sandbox,
                "live.txt",
                [edit("draft", "edited")],
                workspace_session_id=workspace_session,
            )
        )
        second = write_command_stdin(
            sandbox,
            command_session_id,
            "cat live.txt\n",
            yield_time_ms=500,
        )
        assert second["status"] == "running", second
        transcript = read_command_lines(sandbox, command_session_id, start_offset=0, limit=100)
        assert "draft" in second["output"] or "draft" in transcript["output"], transcript
        assert "edited" in second["output"], second

        final = write_command_stdin(
            sandbox,
            command_session_id,
            "exit\n",
            yield_time_ms=30_000,
        )
        command_session_id = None
        assert final["status"] == "ok", final
        assert final["exit_code"] == 0, final
    finally:
        if command_session_id is not None:
            write_command_stdin(sandbox, command_session_id, "exit\n", yield_time_ms=30_000)

    assert_error(file_read(sandbox, "live.txt"), "not_found")
    assert_manifest_delta(sandbox, before, 0)


def test_session_exec_remove_then_session_write_recreates_parent(sandbox, workspace_session):
    """Session exec creates `d/x.txt`, session `file_read` confirms it, session
    exec runs `rm d/x.txt && rmdir d`; then session `file_read d/x.txt`,
    session `file_write d/sub/y.txt`, and session exec `cat d/sub/y.txt`.
    Expected: after the removal the session read is `not_found`; the session
    write returns `type = create` recreating parents through the mounted
    overlay; the final exec exits 0 printing y's content."""
    before = layerstack(sandbox)
    _exec_ok(
        sandbox,
        "mkdir d && printf x > d/x.txt",
        workspace_session_id=workspace_session,
    )
    assert_content(file_read(sandbox, "d/x.txt", workspace_session_id=workspace_session), "x")
    _exec_ok(sandbox, "rm d/x.txt && rmdir d", workspace_session_id=workspace_session)
    assert_error(
        file_read(sandbox, "d/x.txt", workspace_session_id=workspace_session),
        "not_found",
    )
    result = assert_ok(
        file_write(
            sandbox,
            "d/sub/y.txt",
            "y",
            workspace_session_id=workspace_session,
        )
    )
    assert result["type"] == "create", result
    output = _exec_ok(sandbox, "cat d/sub/y.txt", workspace_session_id=workspace_session)
    assert "y" in output["output"], output
    assert_error(file_read(sandbox, "d/sub/y.txt"), "not_found")
    assert_manifest_delta(sandbox, before, 0)


def test_session_fifo_is_rejected_by_session_file_ops(sandbox, workspace_session):
    """Session exec runs `mkdir p && mkfifo p/f.fifo` (exit 0); then session
    `file_read p/f.fifo` and session `file_write p/f.fifo`.
    Expected: both fail `invalid_request` with
    `path is not a regular file (Other)` - the in-namespace runner classifies
    the FIFO by stat and never opens or overwrites it."""
    before = layerstack(sandbox)
    _exec_ok(sandbox, "mkdir p && mkfifo p/f.fifo", workspace_session_id=workspace_session)
    assert_error(
        file_read(sandbox, "p/f.fifo", workspace_session_id=workspace_session),
        "invalid_request",
        "not a regular file",
    )
    assert_error(
        file_write(
            sandbox,
            "p/f.fifo",
            "bad",
            workspace_session_id=workspace_session,
        ),
        "invalid_request",
        "not a regular file",
    )
    assert_manifest_delta(sandbox, before, 0)


def test_session_write_preserves_existing_executable_mode(sandbox, workspace_session):
    """Session `file_write` creates `run.sh` printing `v1`; session exec runs
    `chmod +x run.sh && ./run.sh` (exit 0, `v1`); session `file_write` updates
    the script body to print `v2`; session exec runs `./run.sh` again without
    re-chmodding.
    Expected: the session write preserves the existing executable mode (runner
    `fchmod` with the prior `st_mode`), so the second exec returns
    `exit_code = 0` with `v2` in `output`."""
    before = layerstack(sandbox)
    assert_ok(
        file_write(
            sandbox,
            "run.sh",
            "#!/bin/sh\necho v1\n",
            workspace_session_id=workspace_session,
        )
    )
    first = _exec_ok(
        sandbox,
        "chmod +x run.sh && ./run.sh",
        workspace_session_id=workspace_session,
    )
    assert "v1" in first["output"], first
    assert_ok(
        file_write(
            sandbox,
            "run.sh",
            "#!/bin/sh\necho v2\n",
            workspace_session_id=workspace_session,
        )
    )
    second = _exec_ok(sandbox, "./run.sh", workspace_session_id=workspace_session)
    assert "v2" in second["output"], second
    assert_error(file_read(sandbox, "run.sh"), "not_found")
    assert_manifest_delta(sandbox, before, 0)


@pytest.mark.slow
def test_complex_one_shot_capture_lifecycle_accepts_file_ops_before_exit(sandbox):
    """[complex] One-shot capture lifecycle: start a sessionless
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
    (`workspace session not found`)."""
    before = layerstack(sandbox)
    result = exec_command(
        sandbox,
        "printf shell-line > mix.txt && sh",
        yield_time_ms=0,
        timeout_ms=120_000,
    )
    assert result["status"] == "running", result
    command_session_id = result["command_session_id"]
    workspace_id = _find_running_exec_workspace(sandbox, command_session_id)
    try:
        _wait_for_session_file(sandbox, workspace_id, "mix.txt")
        assert_ok(file_write(sandbox, "mix2.txt", "file-op", workspace_session_id=workspace_id))
        assert_ok(
            file_edit(
                sandbox,
                "mix.txt",
                [edit("shell-line", "edited-line")],
                workspace_session_id=workspace_id,
            )
        )
        final = write_command_stdin(sandbox, command_session_id, "exit\n", yield_time_ms=30_000)
        command_session_id = None
        assert final["status"] == "ok", final
        assert final["exit_code"] == 0, final
    finally:
        if command_session_id is not None:
            write_command_stdin(sandbox, command_session_id, "exit\n", yield_time_ms=30_000)

    assert_manifest_delta(sandbox, before, 1)
    assert_content(file_read(sandbox, "mix.txt"), "edited-line")
    assert_content(file_read(sandbox, "mix2.txt"), "file-op")
    assert_single_owner(sandbox, "mix.txt", owner=f"workspace_session:{workspace_id}")
    assert_single_owner(sandbox, "mix2.txt", owner=f"workspace_session:{workspace_id}")
    assert_error(
        file_read(sandbox, "mix.txt", workspace_session_id=workspace_id),
        "not_found",
        "workspace session not found",
    )


@pytest.mark.slow
def test_complex_long_interleaved_session_destroy_discards_all_changes(sandbox):
    """[complex] Long interleaved session then destroy: in one caller-owned
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
    reads remain `not_found` - uncaptured caller-owned session changes are
    discarded, never published."""
    before = layerstack(sandbox)
    session_id = create_workspace_session(sandbox)
    command_session_id = None
    destroyed = False
    try:
        command_session_id = _start_shell(sandbox, session_id)
        lines = []
        for index in range(1, 16):
            _exec_ok(
                sandbox,
                f"printf 'exec-{index}\\n' >> journal.txt",
                workspace_session_id=session_id,
            )
            lines.append(f"exec-{index}")
            assert_ok(
                file_edit(
                    sandbox,
                    "journal.txt",
                    [edit(f"exec-{index}", f"edited-{index}")],
                    workspace_session_id=session_id,
                )
            )
            lines[-1] = f"edited-{index}"
            assert_content(
                file_read(sandbox, "journal.txt", workspace_session_id=session_id),
                "\n".join(lines),
            )
            assert_error(file_read(sandbox, "journal.txt"), "not_found")

        for index in range(1, 21):
            assert_ok(
                file_write(
                    sandbox,
                    f"notes/note-{index:02d}.txt",
                    f"note-{index:02d}",
                    workspace_session_id=session_id,
                )
            )
        output = _exec_ok(
            sandbox,
            "ls notes | wc -l",
            workspace_session_id=session_id,
        )
        assert "20" in output["output"], output

        rejection = assert_error(destroy_workspace_session(sandbox, session_id), "operation_failed")
        active = rejection.get("details", {}).get("active_command_session_ids", [])
        assert command_session_id in active, rejection

        final = write_command_stdin(sandbox, command_session_id, "exit\n", yield_time_ms=30_000)
        command_session_id = None
        assert final["status"] == "ok", final
        assert final["exit_code"] == 0, final
        destroyed_result = assert_ok(destroy_workspace_session(sandbox, session_id))
        destroyed = True
        assert destroyed_result["destroyed"] is True, destroyed_result
        assert destroyed_result["evicted_upperdir_bytes"] > 0, destroyed_result
    finally:
        if command_session_id is not None:
            write_command_stdin(sandbox, command_session_id, "exit\n", yield_time_ms=30_000)
        if not destroyed:
            destroy_workspace_session(sandbox, session_id, grace_s=1)

    assert_error(file_read(sandbox, "journal.txt"), "not_found")
    assert_error(file_read(sandbox, "notes/note-01.txt"), "not_found")
    assert_manifest_delta(sandbox, before, 0)


@pytest.mark.slow
def test_complex_large_in_session_volume_supports_windowed_reads(sandbox, workspace_session):
    """[complex] Large in-session volume: session exec generates 300 files
    (`gen/f001..f300`, unique bodies) and a ~3 MB `gen/big.txt`
    (`seq 1 400000`) plus one ~300 KB single-line `gen/wide.txt`; then
    windowed session `file_read`s.
    Expected: sampled session reads of `f001`/`f150`/`f300` match exactly;
    `file_read --workspace-session-id --offset 200000 --limit 50` on `big.txt`
    returns lines `200000..200049` with `total_lines = 400000` and
    `truncated = true` (the runner windows inside the namespace instead of
    shipping the whole file); session read of `wide.txt` fails
    `invalid_request` `OutputTooLarge`; sessionless `file_read` of any `gen/`
    path is `not_found` before capture."""
    before = layerstack(sandbox)
    _exec_ok(
        sandbox,
        "mkdir -p gen && "
        "for i in $(seq -w 1 300); do printf 'body-%s\\n' \"$i\" > gen/f$i.txt; done && "
        "seq 1 400000 > gen/big.txt && "
        "head -c 300000 /dev/zero | tr '\\0' x > gen/wide.txt",
        workspace_session_id=workspace_session,
        timeout=240,
        yield_time_ms=120_000,
    )

    for index in ("001", "150", "300"):
        assert_content(
            file_read(
                sandbox,
                f"gen/f{index}.txt",
                workspace_session_id=workspace_session,
            ),
            f"body-{index}",
        )
    window = assert_ok(
        file_read(
            sandbox,
            "gen/big.txt",
            offset=200000,
            limit=50,
            workspace_session_id=workspace_session,
        )
    )
    assert window["start_line"] == 200000, window
    assert window["num_lines"] == 50, window
    assert window["total_lines"] == 400000, window
    assert window["truncated"] is True, window
    assert window["content"].splitlines() == [str(i) for i in range(200000, 200050)]
    assert_error(
        file_read(sandbox, "gen/wide.txt", workspace_session_id=workspace_session),
        "invalid_request",
        "selected read output exceeds the maximum of 262144 bytes",
    )
    assert_error(file_read(sandbox, "gen/f001.txt"), "not_found")
    assert_manifest_delta(sandbox, before, 0)
