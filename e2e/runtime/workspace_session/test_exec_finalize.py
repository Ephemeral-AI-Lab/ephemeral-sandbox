"""exec_command finalize-policy live coverage."""

from __future__ import annotations

import os
import threading
import time

import pytest

from runtime.workspace_session.helpers import (
    assert_error,
    assert_exec_workspace_not_found,
    assert_file_workspace_not_found,
    assert_ok,
    assert_output,
    assert_teardown_clean,
    squash_layerstacks,
    destroy_session,
    exec_bare,
    exec_in,
    file_read,
    interrupt,
    is_error,
    manifest_version,
    read_command_lines,
    record_case,
    wait_command,
    write_command_stdin,
    workspace_entry,
    workspace_tracker,
    snapshot,
)


@pytest.mark.smoke
def test_EX_01_implicit_exec_response_contract(sandbox, workspace_tracker):
    with record_case("EX-01") as rec:
        result = assert_output(exec_bare(sandbox, "echo hi"), "hi")
        session = workspace_tracker.track_workspace(result["workspace_session_id"])
        assert result["status"] == "ok", result
        assert "publish_rejected" not in result, result
        assert_exec_workspace_not_found(exec_in(sandbox, session, "true"), session)
        workspace_tracker.untrack_workspace(session)

        rec.axis("correctness", True, "implicit exec returned session id and finalized it")
        assert_teardown_clean(rec, sandbox, workspace_tracker)


@pytest.mark.smoke
def test_EX_02_implicit_exec_publishes_then_destroys(sandbox, workspace_tracker):
    with record_case("EX-02") as rec:
        first = assert_ok(exec_bare(sandbox, "echo v1 > /workspace/e2e-implicit.txt"))
        first_session = workspace_tracker.track_workspace(first["workspace_session_id"])
        workspace_tracker.wait_finalized(first_session)

        second = assert_output(exec_bare(sandbox, "cat /workspace/e2e-implicit.txt"), "v1")
        second_session = workspace_tracker.track_workspace(second["workspace_session_id"])
        workspace_tracker.wait_finalized(second_session)

        snap = snapshot(sandbox)
        rec.add_artifact("post-publish-snapshot.json", snap)
        assert workspace_entry(snap, first_session) is None, snap
        rec.axis("correctness", True, "implicit exec published the file and destroyed its session")
        assert_teardown_clean(rec, sandbox, workspace_tracker)


@pytest.mark.smoke
def test_EX_03_session_exec_carries_the_session_id(sandbox, workspace_tracker):
    with record_case("EX-03") as rec:
        session = workspace_tracker.create_session()["workspace_session_id"]
        running = assert_ok(exec_in(sandbox, session, "sleep 0.3; echo ex03", yield_time_ms=0))
        command_id = workspace_tracker.track_command(running["command_session_id"])
        assert running["status"] == "running", running
        assert running["workspace_session_id"] == session, running

        terminal = wait_command(sandbox, command_id, timeout_s=10)
        workspace_tracker.untrack_command(command_id)
        assert terminal["status"] == "ok", terminal
        assert terminal["workspace_session_id"] == session, terminal
        assert terminal["output"] == "ex03", terminal

        assert_ok(workspace_tracker.destroy(session))
        rec.axis("correctness", True, "running and terminal responses carried the no_op session id")
        assert_teardown_clean(rec, sandbox, workspace_tracker)


@pytest.mark.medium
def test_EX_04_rider_defers_finalization(sandbox, workspace_tracker):
    with record_case("EX-04") as rec:
        long = assert_ok(
            exec_bare(
                sandbox,
                "sleep 8; echo done > /workspace/rider.txt",
                yield_time_ms=0,
                timeout=45,
            )
        )
        session = workspace_tracker.track_workspace(long["workspace_session_id"])
        command_id = workspace_tracker.track_command(long["command_session_id"])
        assert long["status"] == "running", long

        rider = assert_ok(exec_in(sandbox, session, "ls /workspace >/dev/null"))
        assert rider["workspace_session_id"] == session, rider
        second_rider = assert_ok(exec_in(sandbox, session, "true"))
        assert second_rider["workspace_session_id"] == session, second_rider

        terminal = wait_command(sandbox, command_id, timeout_s=20)
        workspace_tracker.untrack_command(command_id)
        assert terminal["status"] == "ok", terminal
        terminal_at = time.monotonic()
        finalized = workspace_tracker.wait_finalized(session)
        elapsed_ms = finalized["elapsed_ms"]
        observed_ms = round((time.monotonic() - terminal_at) * 1000.0, 3)
        rec.add_timer("T_finalize_after_terminal", observed_ms)

        published = assert_output(exec_bare(sandbox, "cat /workspace/rider.txt"), "done")
        published_session = workspace_tracker.track_workspace(published["workspace_session_id"])
        workspace_tracker.wait_finalized(published_session)

        rec.axis("correctness", True, "riders resolved before final command completion")
        rec.axis(
            "timing",
            observed_ms <= 30_000,
            "implicit session finalized within 30 s after terminal status",
            metrics={"wait_finalized_ms": elapsed_ms, "observed_ms": observed_ms},
        )
        assert_teardown_clean(rec, sandbox, workspace_tracker)


@pytest.mark.medium
def test_EX_05_publish_rejection_surfaces_on_terminal_response(sandbox, workspace_tracker):
    with record_case("EX-05") as rec:
        result = assert_ok(
            exec_bare(sandbox, "mkdir -p /workspace/layers && echo x > /workspace/layers/evil.txt")
        )
        session = workspace_tracker.track_workspace(result["workspace_session_id"])
        assert result["status"] == "ok", result
        assert result.get("publish_rejected") is True, result
        assert result.get("publish_reject_class") == "protected_path", result
        assert_exec_workspace_not_found(exec_in(sandbox, session, "true"), session)
        workspace_tracker.untrack_workspace(session)

        discarded = assert_output(
            exec_bare(sandbox, "test ! -e /workspace/layers/evil.txt && echo discarded"),
            "discarded",
        )
        discarded_session = workspace_tracker.track_workspace(discarded["workspace_session_id"])
        workspace_tracker.wait_finalized(discarded_session)

        rec.axis("correctness", True, "protected-path publish was rejected, destroyed, and discarded")
        assert_teardown_clean(rec, sandbox, workspace_tracker)


@pytest.mark.medium
def test_EX_06_file_op_racing_last_completion_gets_not_found(sandbox, workspace_tracker):
    with record_case("EX-06") as rec:
        running = assert_ok(exec_bare(sandbox, "sleep 2", yield_time_ms=0))
        session = workspace_tracker.track_workspace(running["workspace_session_id"])
        command_id = workspace_tracker.track_command(running["command_session_id"])
        terminal_at = None
        reads = {"ok": 0, "dead": 0}
        deadline = time.monotonic() + 35
        last_read = None
        terminal = None
        while time.monotonic() < deadline:
            status = read_command_lines(sandbox, command_id, start_offset=0, limit=20)
            if not is_error(status) and status.get("status") != "running" and terminal_at is None:
                terminal = status
                terminal_at = time.monotonic()
                workspace_tracker.untrack_command(command_id)

            last_read = file_read(sandbox, ".gitkeep", workspace_session_id=session, timeout=30)
            if is_error(last_read):
                assert_file_workspace_not_found(last_read, session)
                reads["dead"] += 1
                if terminal_at is None:
                    terminal = wait_command(sandbox, command_id, timeout_s=5)
                    terminal_at = time.monotonic()
                    workspace_tracker.untrack_command(command_id)
                break
            assert_ok(last_read)
            assert last_read["content"] == "", last_read
            reads["ok"] += 1
            time.sleep(0.05)
        else:
            raise AssertionError(f"session did not disappear: {last_read}")

        assert terminal is None or terminal["status"] == "ok", terminal
        observed_ms = round((time.monotonic() - terminal_at) * 1000.0, 3)
        workspace_tracker.untrack_workspace(session)
        rec.add_timer("T_finalize_after_terminal", observed_ms)
        rec.axis("correctness", True, "file_read saw only complete content before not-found")
        rec.axis(
            "timing",
            observed_ms <= 30_000,
            "file_read flipped to workspace-session not-found within 30 s",
            metrics=reads | {"observed_ms": observed_ms},
        )
        assert_teardown_clean(rec, sandbox, workspace_tracker)


@pytest.mark.medium
def test_EX_07_interrupt_and_timeout_paths_still_finalize(sandbox, workspace_tracker):
    with record_case("EX-07") as rec:
        running = assert_ok(exec_bare(sandbox, "sleep 60", yield_time_ms=0))
        interrupted_session = workspace_tracker.track_workspace(running["workspace_session_id"])
        interrupted_command = workspace_tracker.track_command(running["command_session_id"])
        cancelled = assert_ok(interrupt(sandbox, interrupted_command))
        workspace_tracker.untrack_command(interrupted_command)
        assert cancelled["status"] == "cancelled", cancelled
        assert cancelled["exit_code"] == 130, cancelled
        assert cancelled["workspace_session_id"] == interrupted_session, cancelled
        workspace_tracker.wait_finalized(interrupted_session)

        timed = assert_ok(exec_bare(sandbox, "sleep 5", timeout_ms=1000, yield_time_ms=0))
        timed_session = workspace_tracker.track_workspace(timed["workspace_session_id"])
        timed_command = workspace_tracker.track_command(timed["command_session_id"])
        terminal = wait_command(sandbox, timed_command, timeout_s=10)
        workspace_tracker.untrack_command(timed_command)
        assert terminal["status"] == "timed_out", terminal
        assert terminal["exit_code"] == 124, terminal
        assert terminal["workspace_session_id"] == timed_session, terminal
        workspace_tracker.wait_finalized(timed_session)

        rec.axis("correctness", True, "cancelled and timed-out implicit sessions finalized")
        assert_teardown_clean(rec, sandbox, workspace_tracker)


@pytest.mark.hard
@pytest.mark.skipif(os.environ.get("E2E_RETENTION") != "1", reason="set E2E_RETENTION=1")
def test_EX_08_drain_retention_cap(sandbox, workspace_tracker):
    with record_case("EX-08") as rec:
        session = workspace_tracker.create_session()["workspace_session_id"]
        command_ids = []
        for index in range(520):
            started = assert_ok(
                exec_in(
                    sandbox,
                    session,
                    f"printf 'retention-{index}\\n'",
                    yield_time_ms=0,
                    timeout=30,
                )
            )
            command_id = workspace_tracker.track_command(started["command_session_id"])
            command_ids.append(command_id)
            terminal = wait_command(sandbox, command_id, timeout_s=10)
            workspace_tracker.untrack_command(command_id)
            assert terminal["status"] == "ok", terminal

        first = write_command_stdin(sandbox, command_ids[0], "late\n", yield_time_ms=0, timeout=30)
        assert_error(first, message_contains="command not found")
        newest = assert_ok(read_command_lines(sandbox, command_ids[-1], start_offset=0, limit=10))
        assert newest["status"] == "ok", newest
        assert "retention-519" in newest["output"], newest

        assert_ok(workspace_tracker.destroy(session))
        rec.axis("correctness", True, "terminal drain retention evicted oldest but ledger stayed empty")
        assert_teardown_clean(rec, sandbox, workspace_tracker)


@pytest.mark.medium
def test_FP_01_remount_sweep_cannot_finalize_idle_implicit_session(sandbox, workspace_tracker):
    with record_case("FP-01") as rec:
        implicit = assert_ok(
            exec_bare(
                sandbox,
                "sleep 6; echo fp01 > /workspace/fp01.txt",
                yield_time_ms=0,
                timeout=45,
            )
        )
        implicit_session = workspace_tracker.track_workspace(implicit["workspace_session_id"])
        implicit_command = workspace_tracker.track_command(implicit["command_session_id"])
        noop_session = workspace_tracker.create_session()["workspace_session_id"]

        squash = assert_ok(squash_layerstacks(sandbox))
        rec.add_artifact("squash-layerstacks.json", squash)
        drain = assert_ok(read_command_lines(sandbox, implicit_command, start_offset=0, limit=10))
        assert drain["status"] == "running", drain
        assert_ok(exec_in(sandbox, noop_session, "true"))
        rider = assert_ok(exec_in(sandbox, implicit_session, "true"))
        assert rider["workspace_session_id"] == implicit_session, rider

        terminal = wait_command(sandbox, implicit_command, timeout_s=15)
        workspace_tracker.untrack_command(implicit_command)
        assert terminal["status"] == "ok", terminal
        terminal_at = time.monotonic()
        finalized = workspace_tracker.wait_finalized(implicit_session)
        observed_ms = round((time.monotonic() - terminal_at) * 1000.0, 3)
        rec.add_timer("T_finalize_after_terminal", observed_ms)
        assert finalized["elapsed_ms"] <= 30_000, finalized

        published = assert_output(exec_bare(sandbox, "cat /workspace/fp01.txt"), "fp01")
        published_session = workspace_tracker.track_workspace(published["workspace_session_id"])
        workspace_tracker.wait_finalized(published_session)
        assert_ok(workspace_tracker.destroy(noop_session))

        rec.axis("correctness", True, "sweep preserved running implicit and idle no_op sessions")
        rec.axis(
            "timing",
            observed_ms <= 30_000,
            "implicit session finalized within 30 s after terminal status",
            metrics={"wait_finalized_ms": finalized["elapsed_ms"], "observed_ms": observed_ms},
        )
        assert_teardown_clean(rec, sandbox, workspace_tracker)


@pytest.mark.medium
def test_FP_02_empty_capture_skips_publish(sandbox, workspace_tracker):
    with record_case("FP-02") as rec:
        before = manifest_version(sandbox)
        result = assert_ok(exec_bare(sandbox, "true"))
        session = workspace_tracker.track_workspace(result["workspace_session_id"])
        assert result["status"] == "ok", result
        workspace_tracker.wait_finalized(session)
        after = manifest_version(sandbox)
        assert after == before, {"before": before, "after": after, "result": result}

        rec.axis("correctness", True, "empty implicit exec did not advance manifest version")
        assert_teardown_clean(rec, sandbox, workspace_tracker)


@pytest.mark.medium
def test_FP_03_back_to_back_implicit_execs_are_independent(sandbox, workspace_tracker):
    with record_case("FP-03") as rec:
        first = assert_ok(exec_bare(sandbox, "echo first > /workspace/fp03.txt"))
        first_session = workspace_tracker.track_workspace(first["workspace_session_id"])
        workspace_tracker.wait_finalized(first_session)

        second = assert_output(exec_bare(sandbox, "cat /workspace/fp03.txt"), "first")
        second_session = workspace_tracker.track_workspace(second["workspace_session_id"])
        workspace_tracker.wait_finalized(second_session)
        assert first_session != second_session, {"first": first, "second": second}

        rec.axis("correctness", True, "sequential implicit execs used different sessions and shared layers")
        assert_teardown_clean(rec, sandbox, workspace_tracker)


@pytest.mark.hard
@pytest.mark.skipif(os.environ.get("E2E_STORM") != "1", reason="set E2E_STORM=1")
def test_FP_04_finalize_vs_destroy_interleave_storm(sandbox, workspace_tracker):
    with record_case("FP-04") as rec:
        deadline = time.monotonic() + float(os.environ.get("E2E_STORM_SECONDS", "60"))
        faults = []
        finalize_times = []
        lock = threading.Lock()

        def add_fault(payload):
            with lock:
                faults.append(payload)

        def add_finalize_ms(value):
            with lock:
                finalize_times.append(value)

        def worker(worker_id):
            iteration = 0
            while time.monotonic() < deadline:
                try:
                    if iteration % 2 == 0:
                        result = exec_bare(
                            sandbox,
                            f"echo storm-{worker_id}-{iteration} > /workspace/fp04-{worker_id}-{iteration}.txt",
                            timeout=60,
                        )
                        if is_error(result):
                            add_fault({"worker": worker_id, "result": result})
                            continue
                        session = workspace_tracker.track_workspace(result["workspace_session_id"])
                        terminal_at = time.monotonic()
                        finalized = workspace_tracker.wait_finalized(session)
                        add_finalize_ms(round((time.monotonic() - terminal_at) * 1000.0, 3))
                        if finalized["elapsed_ms"] > 30_000:
                            add_fault({"worker": worker_id, "slow_finalize": finalized})
                    else:
                        created = workspace_tracker.create_session()
                        session = created["workspace_session_id"]
                        command = exec_in(sandbox, session, "sleep 0.2", yield_time_ms=0, timeout=30)
                        if is_error(command):
                            add_fault({"worker": worker_id, "result": command})
                            continue
                        command_id = workspace_tracker.track_command(command["command_session_id"])
                        destroyed = destroy_session(sandbox, session, grace_s=1, timeout=30)
                        if is_error(destroyed):
                            active = (
                                destroyed.get("error", {})
                                .get("details", {})
                                .get("active_command_session_ids", [])
                            )
                            if command_id not in active:
                                add_fault({"worker": worker_id, "destroy": destroyed})
                            interrupt(sandbox, command_id)
                            workspace_tracker.untrack_command(command_id)
                            destroyed = workspace_tracker.destroy(session)
                            if is_error(destroyed):
                                add_fault({"worker": worker_id, "destroy_retry": destroyed})
                        else:
                            terminal = wait_command(sandbox, command_id, timeout_s=5)
                            workspace_tracker.untrack_command(command_id)
                            if terminal.get("status") != "ok":
                                add_fault({"worker": worker_id, "terminal": terminal})
                            workspace_tracker.untrack_workspace(session)
                except Exception as exc:
                    add_fault({"worker": worker_id, "exception": repr(exc)})
                iteration += 1

        threads = [threading.Thread(target=worker, args=(index,)) for index in range(8)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join()

        assert not faults, faults[:5]
        snap = snapshot(sandbox)
        rec.add_artifact("storm-final-snapshot.json", snap)
        leaked = [ws for ws in snap.get("workspaces", []) if ws.get("workspace_id") in workspace_tracker.seen_workspace_ids]
        assert not leaked, leaked
        max_finalize = max(finalize_times or [0.0])
        rec.add_timer("T_finalize_after_terminal_max", max_finalize)
        rec.axis(
            "correctness",
            True,
            "storm completed with no unexpected operation errors",
            metrics={"iterations_with_finalize": len(finalize_times)},
        )
        rec.axis(
            "timing",
            max_finalize <= 30_000,
            "all observed implicit finalizations completed within 30 s",
            metrics={"max_finalize_ms": max_finalize},
        )
        assert_teardown_clean(rec, sandbox, workspace_tracker)
