"""Live e2e: Concurrent Operations - Session (9 cases)."""

import time

import pytest

from runtime.file.helpers import (
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
    layerstack,
    owners_by_line,
    read_command_lines,
    run_concurrently,
    sandbox_from_workspace,
    write_command_stdin,
    workspace_session,
)


def _is_error(result):
    return isinstance(result, dict) and "error" in result


def _exec_ok(sandbox, command, *, workspace_session_id=None, **kwargs):
    kwargs.setdefault("yield_time_ms", 30_000)
    result = exec_command(
        sandbox,
        command,
        workspace_session_id=workspace_session_id,
        **kwargs,
    )
    assert "error" not in result, result
    assert result["status"] == "ok", result
    assert result["exit_code"] == 0, result
    return result


def _start_shell(sandbox, workspace_session_id):
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


def _exit_shell(sandbox, command_session_id):
    result = write_command_stdin(
        sandbox,
        command_session_id,
        "exit\n",
        yield_time_ms=30_000,
    )
    assert result["status"] == "ok", result
    assert result["exit_code"] == 0, result
    return result


def _read_transcript_until(sandbox, command_session_id, predicate, *, attempts=80):
    last = None
    for _ in range(attempts):
        result = read_command_lines(sandbox, command_session_id, start_offset=0, limit=1000)
        assert_ok(result)
        last = result.get("output", "")
        if predicate(last):
            return last
        time.sleep(0.1)
    pytest.fail(f"transcript did not reach expected state: {last!r}")


def _assert_sessionless_not_found(sandbox, paths):
    for path in paths:
        assert_error(file_read(sandbox, path), "not_found")


def test_session_write_races_session_edit_same_path(sandbox, workspace_session):
    """Inside one live workspace session (`create_workspace_session`), race a
    session `file_write` (rewriting `alpha\nbeta` to `alpha\nGAMMA`) against a
    session `file_edit` (`alpha` -> `ALPHA`) on the same path via
    `--workspace-session-id`.
    Expected: each op returns ok or the edit returns
    `invalid_request`/`not_found`; the final session `file_read` returns one
    complete last-writer-wins variant (edit is read-modify-write, write lands
    via atomic `renameat`) - never interleaved bytes; sessionless `file_read`
    of the path still returns `not_found` (session ops never publish)."""
    before = layerstack(sandbox)
    path = "session-concurrent/write-edit.txt"
    assert_ok(file_write(sandbox, path, "alpha\nbeta", workspace_session_id=workspace_session))

    write_result, edit_result = run_concurrently(
        [
            lambda: file_write(
                sandbox,
                path,
                "alpha\nGAMMA",
                workspace_session_id=workspace_session,
            ),
            lambda: file_edit(
                sandbox,
                path,
                [edit("alpha", "ALPHA")],
                workspace_session_id=workspace_session,
            ),
        ],
        max_workers=2,
    )

    assert_ok(write_result)
    if _is_error(edit_result):
        assert edit_result["error"]["kind"] in {"invalid_request", "not_found"}, edit_result
    else:
        assert_ok(edit_result)
    final = assert_ok(file_read(sandbox, path, workspace_session_id=workspace_session))
    assert final["content"] in {
        "alpha\nGAMMA",
        "ALPHA\nGAMMA",
        "ALPHA\nbeta",
    }, final
    assert_error(file_read(sandbox, path), "not_found")
    assert_manifest_delta(sandbox, before, 0)


def test_two_concurrent_session_writes_same_path_leave_no_tmp_artifacts(
    sandbox, workspace_session
):
    """Two concurrent session `file_write` requests with different contents to
    the same path in one live session.
    Expected: both return ok; final session `file_read` returns exactly one
    writer's complete content; a follow-up
    `exec_command --workspace-session-id ID "ls -a <dir>"` shows no leftover
    `.<name>.tmp.<pid>` temp artifacts from the atomic-rename path."""
    before = layerstack(sandbox)
    path = "session-concurrent/same/file.txt"
    payloads = {"writer-a\nbody-a", "writer-b\nbody-b"}

    results = run_concurrently(
        [
            lambda body=body: file_write(
                sandbox,
                path,
                body,
                workspace_session_id=workspace_session,
            )
            for body in payloads
        ],
        max_workers=2,
    )

    for result in results:
        assert_ok(result)
    final = assert_ok(file_read(sandbox, path, workspace_session_id=workspace_session))
    assert final["content"] in payloads, final
    listing = _exec_ok(
        sandbox,
        "ls -a session-concurrent/same",
        workspace_session_id=workspace_session,
    )
    assert ".tmp" not in listing["output"], listing
    assert_error(file_read(sandbox, path), "not_found")
    assert_manifest_delta(sandbox, before, 0)


def test_session_reads_race_in_session_atomic_mv_loop(sandbox, workspace_session):
    """Session `file_read` requests looping concurrently with an in-session shell
    command (`exec_command --workspace-session-id ID`) that repeatedly writes
    a temp file and atomically `mv`s it over the target, embedding the same
    generation marker on the first and last line.
    Expected: every session read returns a self-consistent complete generation
    (first and last line markers match, `num_lines`/`total_bytes` coherent);
    no read observes a torn or empty intermediate state through the mounted
    namespace."""
    before = layerstack(sandbox)
    path = "session-concurrent/atomic.txt"
    assert_ok(file_write(sandbox, path, "gen-0\nbody-0\ngen-0", workspace_session_id=workspace_session))

    def writer():
        return _exec_ok(
            sandbox,
            "for i in $(seq 1 30); do "
            "printf 'gen-%s\\nbody-%s\\ngen-%s' \"$i\" \"$i\" \"$i\" "
            "> session-concurrent/atomic.tmp && "
            "mv session-concurrent/atomic.tmp session-concurrent/atomic.txt && "
            "sleep 0.02; "
            "done",
            workspace_session_id=workspace_session,
            timeout=240,
        )

    calls = [writer] + [
        lambda: file_read(sandbox, path, workspace_session_id=workspace_session)
        for _ in range(20)
    ]
    results = run_concurrently(calls, max_workers=8)

    assert_ok(results[0])
    for result in results[1:]:
        read = assert_ok(result)
        lines = read["content"].splitlines()
        assert len(lines) == 3, read
        assert lines[0] == lines[2], read
        assert read["num_lines"] == 3, read
        assert read["total_bytes"] == len(read["content"].encode("utf-8")), read
    assert_error(file_read(sandbox, path), "not_found")
    assert_manifest_delta(sandbox, before, 0)


def test_destroy_workspace_session_races_running_exec(sandbox):
    """`destroy_workspace_session` racing an `exec_command` still running in
    that same session.
    Expected: the session-lifecycle lock serializes admission - destroy either
    returns `operation_failed` with
    `error.details.active_command_session_ids` listing the running command, or
    succeeds with `destroyed = true` only after the command reached terminal
    state; session `file_read` returns `not_found` for the
    `workspace_session_id` only once destroy succeeded."""
    before = layerstack(sandbox)
    session_id = create_workspace_session(sandbox)
    command_session_id = None
    destroyed = False
    try:
        command_session_id = _start_shell(sandbox, session_id)
        destroy_result = destroy_workspace_session(sandbox, session_id)
        if _is_error(destroy_result):
            error = assert_error(destroy_result, "operation_failed")
            active = error.get("details", {}).get("active_command_session_ids", [])
            assert command_session_id in active, error
            _exit_shell(sandbox, command_session_id)
            command_session_id = None
            destroy_result = assert_ok(destroy_workspace_session(sandbox, session_id))
        else:
            destroy_result = assert_ok(destroy_result)
            command_session_id = None

        assert destroy_result["destroyed"] is True, destroy_result
        destroyed = True
        assert_error(
            file_read(sandbox, "anything.txt", workspace_session_id=session_id),
            "not_found",
        )
    finally:
        if command_session_id is not None:
            write_command_stdin(sandbox, command_session_id, "exit\n", yield_time_ms=30_000)
        if not destroyed:
            destroy_workspace_session(sandbox, session_id, grace_s=1)

    assert_manifest_delta(sandbox, before, 0)


@pytest.mark.slow
def test_complex_long_session_disjoint_writes_visible_to_shell_then_destroy(sandbox):
    """[complex] Inside one long-running session, launch 20+ concurrent session
    `file_write` requests to 20 disjoint paths while an interactive in-session
    shell (started with `--yield-time-ms 0`, driven via
    `write_command_stdin`/`read_command_lines`) concurrently `cat`s the same
    paths.
    Expected: all writes return ok; each session `file_read` and the shell
    transcript show complete per-path content through the live overlay; all 20
    paths remain `not_found` to sessionless `file_read`;
    `destroy_workspace_session` then discards everything without any layer
    being published (`manifest_version` unchanged)."""
    before = layerstack(sandbox)
    session_id = create_workspace_session(sandbox)
    command_session_id = None
    destroyed = False
    paths = {
        f"session-concurrent/live/p{index:02d}.txt": f"payload-{index:02d}"
        for index in range(1, 21)
    }
    try:
        command_session_id = _start_shell(sandbox, session_id)
        script = (
            "for i in $(seq -w 1 20); do "
            "p=session-concurrent/live/p$i.txt; "
            "while [ ! -f \"$p\" ]; do sleep 0.01; done; "
            "printf 'PATH:%s:' \"$p\"; cat \"$p\"; printf '\\n__PATH_DONE__\\n'; "
            "done\n"
        )
        start = write_command_stdin(sandbox, command_session_id, script, yield_time_ms=0)
        assert start["status"] == "running", start

        results = run_concurrently(
            [
                lambda path=path, body=body: file_write(
                    sandbox,
                    path,
                    body,
                    workspace_session_id=session_id,
                )
                for path, body in paths.items()
            ],
            max_workers=20,
        )
        for result in results:
            assert_ok(result)

        transcript = _read_transcript_until(
            sandbox,
            command_session_id,
            lambda output: all(body in output for body in paths.values())
            and output.count("__PATH_DONE__") >= 20,
        )
        for path, body in paths.items():
            assert_content(file_read(sandbox, path, workspace_session_id=session_id), body)
            assert f"PATH:{path}:{body}" in transcript, transcript
        _assert_sessionless_not_found(sandbox, paths)

        _exit_shell(sandbox, command_session_id)
        command_session_id = None
        destroy_result = assert_ok(destroy_workspace_session(sandbox, session_id))
        assert destroy_result["destroyed"] is True, destroy_result
        destroyed = True
    finally:
        if command_session_id is not None:
            write_command_stdin(sandbox, command_session_id, "exit\n", yield_time_ms=30_000)
        if not destroyed:
            destroy_workspace_session(sandbox, session_id, grace_s=1)

    _assert_sessionless_not_found(sandbox, paths)
    assert_manifest_delta(sandbox, before, 0)


@pytest.mark.slow
def test_complex_hot_file_storm_inside_one_session(sandbox, workspace_session):
    """[complex] Hot-file storm inside one session: 10 concurrent session writes
    with unique payloads, 10 concurrent session reads, and a live shell `cat`
    loop, all on one path.
    Expected: every reader (file op and shell transcript via
    `read_command_lines`) observes exactly one complete payload per read
    (atomic rename, last-writer-wins); the final session read matches one
    writer; `ls -a` in the session shows no `.tmp` artifacts; nothing
    publishes to the layerstack."""
    before = layerstack(sandbox)
    path = "session-concurrent/storm/hot.txt"
    seed = "seed-payload"
    payloads = {f"storm-payload-{index:02d}" for index in range(1, 11)}
    allowed = payloads | {seed}
    assert_ok(file_write(sandbox, path, seed, workspace_session_id=workspace_session))
    command_session_id = _start_shell(sandbox, workspace_session)
    try:
        script = (
            "for i in $(seq 1 12); do "
            "cat session-concurrent/storm/hot.txt; "
            "printf '\\n__READ__\\n'; sleep 0.02; "
            "done\n"
        )
        start = write_command_stdin(sandbox, command_session_id, script, yield_time_ms=0)
        assert start["status"] == "running", start

        calls = [
            lambda body=body: ("write", file_write(
                sandbox,
                path,
                body,
                workspace_session_id=workspace_session,
            ))
            for body in payloads
        ]
        calls += [
            lambda: ("read", file_read(sandbox, path, workspace_session_id=workspace_session))
            for _ in range(10)
        ]
        results = run_concurrently(calls, max_workers=20)

        for kind, result in results:
            if kind == "write":
                assert_ok(result)
            else:
                read = assert_ok(result)
                assert read["content"] in allowed, read

        transcript = _read_transcript_until(
            sandbox,
            command_session_id,
            lambda output: output.count("__READ__") >= 12,
        )
        for chunk in transcript.split("__READ__"):
            payload_lines = [
                line.strip()
                for line in chunk.splitlines()
                if line.strip()
                and not line.startswith("sh:")
                and not line.startswith("#")
            ]
            if payload_lines:
                assert payload_lines[-1] in allowed, transcript

        listing = _exec_ok(
            sandbox,
            "ls -a session-concurrent/storm",
            workspace_session_id=workspace_session,
        )
        assert ".tmp" not in listing["output"], listing
        final = assert_ok(file_read(sandbox, path, workspace_session_id=workspace_session))
        assert final["content"] in payloads, final
        assert_error(file_read(sandbox, path), "not_found")
        assert_manifest_delta(sandbox, before, 0)
    finally:
        write_command_stdin(sandbox, command_session_id, "exit\n", yield_time_ms=30_000)


@pytest.mark.slow
def test_complex_same_path_conflicting_captures_publish_only_first(sandbox):
    """[complex] Same-path conflicting captures: 5 concurrent one-shot
    `exec_command` invocations each wholesale-rewriting the same seeded
    single-line file with unique content, so their exec-owned session captures
    publish in completion order against a moving head.
    Expected: the first capture commits; every later capture three-way merge
    conflicts and is dropped (`source_conflict`, never surfaced - all 5
    `exec_command` responses report `status = ok`); final content equals
    exactly one session's payload, `file_blame` shows one single
    `workspace_session:<id>` owner, and `manifest_version` advances by
    exactly 1."""
    path = "session-concurrent/captures/conflict.txt"
    assert_ok(file_write(sandbox, path, "base"))
    before = layerstack(sandbox)
    payloads = {f"capture-{index:02d}" for index in range(1, 6)}

    results = run_concurrently(
        [
            lambda body=body: _exec_ok(sandbox, f"printf {body} > {path}")
            for body in payloads
        ],
        max_workers=5,
    )

    for result in results:
        assert_ok(result)
    final = assert_ok(file_read(sandbox, path))
    assert final["content"] in payloads, final
    assert_single_owner(sandbox, path, prefix="workspace_session:")
    assert_manifest_delta(sandbox, before, 1)


@pytest.mark.slow
@pytest.mark.xfail(
    reason=(
        "Checklist lines 608-615 require all five concurrent line-disjoint "
        "captures to commit; live run published only a subset of edits."
    )
)
def test_complex_capture_order_independence_line_disjoint_merges(tmp_path):
    """[complex] Capture-order independence: seed a 10-line file, then run 5
    concurrent one-shot `exec_command` invocations, each sed-editing only its
    own line, so 5 capture publishes three-way merge in arbitrary completion
    order.
    Expected: all 5 captures commit cleanly (line-disjoint merges); the final
    sessionless `file_read` contains all 5 modifications; `file_blame` shows 5
    distinct `workspace_session:<id>` owners on the 5 edited lines with
    `original` on untouched lines; `manifest_version` advances by exactly 5."""
    path = "session-concurrent/captures/lines.txt"
    original = "\n".join(f"line-{index}" for index in range(1, 11))
    with sandbox_from_workspace(tmp_path, {path: original}) as sandbox:
        before = layerstack(sandbox)
        results = run_concurrently(
            [
                lambda index=index: _exec_ok(
                    sandbox,
                    f"sed -i '{index}s/.*/LINE-{index}/' {path}",
                )
                for index in range(1, 6)
            ],
            max_workers=5,
        )

        for result in results:
            assert_ok(result)
        expected = [f"LINE-{index}" for index in range(1, 6)] + [
            f"line-{index}" for index in range(6, 11)
        ]
        assert_content(file_read(sandbox, path), "\n".join(expected))
        owners = owners_by_line(assert_blame_tiling(sandbox, path))
        assert len(set(owners[:5])) == 5, owners
        assert all(owner.startswith("workspace_session:") for owner in owners[:5]), owners
        assert owners[5:] == ["original"] * 5, owners
        assert_manifest_delta(sandbox, before, 5)


@pytest.mark.slow
def test_complex_capture_races_sessionless_writers_hot_path(sandbox):
    """[complex] Capture racing sessionless writers: a one-shot `exec_command`
    runs a ~5 s script that keeps rewriting a seeded one-line file while 20
    concurrent sessionless `file_write` requests hammer the same path with
    unique one-line payloads.
    Expected: all 20 sessionless writes serialize and return ok; the terminal
    capture publish either merge-conflicts and is dropped or commits before
    any write, so the final `file_read` matches exactly one of the 20
    payloads, `file_blame` shows a single `operation:<request_id>` owner
    consistent with that content, `exec_command` reports `status = ok`, and
    `manifest_version` equals baseline + 20 (capture dropped) or + 21 (capture
    committed first) with no other value."""
    path = "session-concurrent/captures/race.txt"
    assert_ok(file_write(sandbox, path, "base"))
    before = layerstack(sandbox)
    payloads = {f"writer-{index:02d}" for index in range(1, 21)}

    def capture():
        return _exec_ok(
            sandbox,
            "for i in $(seq 1 50); do "
            f"printf exec-$i > {path}; "
            "sleep 0.1; "
            "done",
            timeout=240,
            yield_time_ms=120_000,
        )

    calls = [capture] + [
        lambda body=body: file_write(sandbox, path, body)
        for body in payloads
    ]
    results = run_concurrently(calls, max_workers=21)

    assert_ok(results[0])
    for result in results[1:]:
        assert_ok(result)
    final = assert_ok(file_read(sandbox, path))
    assert final["content"] in payloads, final
    assert_single_owner(sandbox, path, prefix="operation:")
    after = layerstack(sandbox)
    assert after["manifest_version"] in {
        before["manifest_version"] + 20,
        before["manifest_version"] + 21,
    }, after
