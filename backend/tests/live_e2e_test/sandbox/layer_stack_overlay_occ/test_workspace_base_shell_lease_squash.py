"""Public in-flight shell lease coverage over an imported workspace base."""

from __future__ import annotations

import asyncio

import pytest

from sandbox.host.daemon_client import DEFAULT_LAYER_STACK_ROOT

from .._harness.concurrency import gather_with_barrier
from .._harness.integrated_cases import (
    assert_committed,
    assert_read,
    assert_rejected,
    emit_metric,
    q,
    remove_tmp,
    summarize_calls,
    timed_call,
    tmp_path,
    token,
    touch_tmp,
    wait_for_tmp,
)
from .._harness.sandbox_fixture import SandboxHandle
from .._harness.workspace_base_public import seed_imported_base


pytestmark = pytest.mark.asyncio

AUTO_SQUASH_MAX_DEPTH = 32
AUTO_SQUASH_TRIGGER_WRITES = AUTO_SQUASH_MAX_DEPTH + 4
_TRANSIENT_LOWERDIR_DIR = "transient-lowerdirs"


async def test_concurrent_public_shell_leases_survive_mutation_burst(
    workspace_base_sandbox: SandboxHandle,
) -> None:
    handle = workspace_base_sandbox
    watched_path = "tracked/phase08/burst/watched.txt"
    edit_path = "tracked/phase08/burst/edit.txt"
    extra_path = "tracked/phase08/burst/extra.txt"
    await seed_imported_base(
        handle,
        {
            ".gitignore": "dist/\n",
            watched_path: "base\n",
            edit_path: "alpha=old\n",
        },
    )

    for index in range(10):
        result = await handle.tool.write_file(
            f"tracked/phase08/burst/depth-{index:02d}.txt",
            f"depth-{index:02d}\n",
            description=f"phase08 concurrent lease depth seed {index:02d}",
        )
        assert_committed(result)

    run = token("phase08-concurrent-shell-leases")
    proceed = tmp_path(f"{run}-go")
    started_paths = [tmp_path(f"{run}-started-{index}") for index in range(2)]
    await remove_tmp(handle, proceed, *started_paths)

    async def run_shell(index: int):
        output = f"dist/phase08/burst/lease-{index:02d}.txt"
        command = (
            "set -e; "
            f"first=$(cat {q(watched_path)}); "
            f"edit_first=$(cat {q(edit_path)}); "
            f"touch {q(started_paths[index])}; "
            f"while [ ! -f {q(proceed)} ]; do sleep 0.01; done; "
            f"second=$(cat {q(watched_path)}); "
            f"edit_second=$(cat {q(edit_path)}); "
            "mkdir -p dist/phase08/burst; "
            f"printf '%s|%s|%s|%s\\n' "
            f"\"$first\" \"$second\" \"$edit_first\" \"$edit_second\" > {q(output)}"
        )
        result, metric = await timed_call(
            f"phase08_concurrent_shell_lease_{index}",
            handle.tool.shell(
                command,
                timeout=45,
                description=f"phase08 concurrent shell lease {index}",
            ),
        )
        return result, metric, output

    shell_tasks = [asyncio.create_task(run_shell(index)) for index in range(2)]
    await asyncio.gather(*(wait_for_tmp(handle, path) for path in started_paths))

    mid_metrics = await handle.tool.layer_metrics()
    assert int(mid_metrics["active_leases"]) >= 2, mid_metrics
    assert int(mid_metrics["leased_layers"]) >= 1, mid_metrics

    async def write_watched():
        return await timed_call(
            "phase08_burst_write_watched",
            handle.tool.write_file(
                watched_path,
                "active-after\n",
                description="phase08 write while two shell leases are held",
            ),
        )

    async def edit_existing():
        return await timed_call(
            "phase08_burst_edit_existing",
            handle.tool.edit_file(
                edit_path,
                [("alpha=old", "alpha=new")],
                description="phase08 edit while two shell leases are held",
            ),
        )

    async def write_extra():
        return await timed_call(
            "phase08_burst_write_extra",
            handle.tool.write_file(
                extra_path,
                "extra\n",
                description="phase08 extra write while two shell leases are held",
            ),
        )

    mutation_rows = await gather_with_barrier(
        [write_watched, edit_existing, write_extra]
    )
    for result, _ in mutation_rows:
        assert_committed(result)

    await touch_tmp(handle, proceed)
    shell_rows = await asyncio.gather(*shell_tasks)
    for shell, _, output in shell_rows:
        assert_committed(shell, path=output)
        assert shell.exit_code == 0, shell.stderr
        _assert_no_cache_shell_timings(shell.timings)
        await assert_read(handle, output, "base|base|alpha=old|alpha=old\n")

    await assert_read(handle, watched_path, "active-after\n")
    await assert_read(handle, edit_path, "alpha=new\n")
    await assert_read(handle, extra_path, "extra\n")
    after_metrics = await handle.tool.layer_metrics()
    assert int(after_metrics["active_leases"]) == 0, after_metrics
    emit_metric(
        "phase08.concurrent_public_shell_leases",
        {
            **summarize_calls(
                [metric for _, metric in mutation_rows]
                + [metric for _, metric, _ in shell_rows]
            ),
            "mid_active_leases": mid_metrics["active_leases"],
            "mid_leased_layers": mid_metrics["leased_layers"],
        },
    )


async def test_failed_public_shell_releases_lease_and_transient_lowerdir(
    workspace_base_sandbox: SandboxHandle,
) -> None:
    handle = workspace_base_sandbox
    await seed_imported_base(
        handle,
        {
            ".gitignore": "dist/\n",
            "tracked/phase08/failure/base.txt": "base\n",
        },
    )

    assert await _transient_lowerdir_parent_count(handle) == 0
    shell, metric = await timed_call(
        "phase08_failed_shell_cleanup",
        handle.tool.shell(
            "set -e; cat tracked/phase08/failure/base.txt >/dev/null; exit 7",
            timeout=30,
            description="phase08 failed shell cleanup",
        ),
    )

    assert shell.success is False
    assert shell.exit_code == 7
    assert shell.changed_paths == ()
    _assert_no_cache_shell_timings(shell.timings, require_occ=False)
    metrics = await handle.tool.layer_metrics()
    assert int(metrics["active_leases"]) == 0, metrics
    assert await _transient_lowerdir_parent_count(handle) == 0
    emit_metric(
        "phase08.failed_shell_cleanup",
        {
            **summarize_calls([metric]),
            "active_leases_after": metrics["active_leases"],
        },
    )


async def test_public_shell_multi_path_conflict_drops_entire_capture(
    workspace_base_sandbox: SandboxHandle,
) -> None:
    handle = workspace_base_sandbox
    conflict_path = "tracked/phase08/conflict/shared.txt"
    disjoint_path = "tracked/phase08/conflict/disjoint.txt"
    ignored_path = "dist/phase08/conflict/ignored.txt"
    await seed_imported_base(
        handle,
        {
            ".gitignore": "dist/\n",
            conflict_path: "base\n",
        },
    )

    run = token("phase08-shell-multipath-conflict")
    started = tmp_path(f"{run}-started")
    proceed = tmp_path(f"{run}-go")
    await remove_tmp(handle, started, proceed)
    command = (
        "set -e; "
        f"touch {q(started)}; "
        f"while [ ! -f {q(proceed)} ]; do sleep 0.01; done; "
        "mkdir -p tracked/phase08/conflict dist/phase08/conflict; "
        f"printf 'stale-shell\\n' > {q(conflict_path)}; "
        f"printf 'should-not-commit\\n' > {q(disjoint_path)}; "
        f"printf 'ignored-should-not-commit\\n' > {q(ignored_path)}"
    )

    shell_task = asyncio.create_task(
        timed_call(
            "phase08_multipath_conflict_shell",
            handle.tool.shell(
                command,
                timeout=45,
                description="phase08 multi-path shell capture conflict",
            ),
        )
    )
    await wait_for_tmp(handle, started)
    winner, winner_metric = await timed_call(
        "phase08_multipath_conflict_winner",
        handle.tool.write_file(
            conflict_path,
            "winner\n",
            description="phase08 public write wins over stale multi-path shell",
        ),
    )
    assert_committed(winner, path=conflict_path)

    await touch_tmp(handle, proceed)
    rejected, rejected_metric = await shell_task
    assert_rejected(rejected, path=conflict_path)
    _assert_no_cache_shell_timings(rejected.timings)

    await assert_read(handle, conflict_path, "winner\n")
    disjoint = await handle.tool.read_file(disjoint_path)
    ignored = await handle.tool.read_file(ignored_path)
    assert disjoint.success and not disjoint.exists
    assert ignored.success and not ignored.exists
    metrics = await handle.tool.layer_metrics()
    assert int(metrics["active_leases"]) == 0, metrics
    emit_metric(
        "phase08.public_shell_multipath_conflict",
        summarize_calls([winner_metric, rejected_metric]),
    )


async def test_in_flight_public_shell_lease_survives_active_edit(
    workspace_base_sandbox: SandboxHandle,
) -> None:
    handle = workspace_base_sandbox
    path = "tracked/lease-squash/value.txt"
    shell_output = "dist/lease-squash/frozen-view.txt"
    await seed_imported_base(
        handle,
        {
            ".gitignore": "dist/\n",
            path: "base-view\n",
        },
    )

    for index in range(8):
        result = await handle.tool.write_file(
            f"tracked/lease-squash/depth-{index:02d}.txt",
            f"depth-{index:02d}\n",
            description=f"phase01 lease squash depth seed {index:02d}",
        )
        assert_committed(result)

    run = token("workspace-base-lease-squash")
    started = tmp_path(f"{run}-started")
    proceed = tmp_path(f"{run}-go")
    await remove_tmp(handle, started, proceed)
    command = (
        "set -e; "
        f"first=$(cat {q(path)}); "
        f"touch {q(started)}; "
        f"while [ ! -f {q(proceed)} ]; do sleep 0.01; done; "
        f"second=$(cat {q(path)}); "
        "mkdir -p dist/lease-squash; "
        f"printf '%s|%s\\n' \"$first\" \"$second\" > {q(shell_output)}"
    )

    shell_task = asyncio.create_task(
        timed_call(
            "base_shell_lease_frozen_view",
            handle.tool.shell(
                command,
                timeout=30,
                description="phase01 shell lease frozen view across squash",
            ),
        )
    )
    await wait_for_tmp(handle, started)

    mid_metrics = await handle.tool.layer_metrics()
    assert int(mid_metrics["active_leases"]) >= 1, mid_metrics

    update, update_metric = await timed_call(
        "base_shell_lease_active_update",
        handle.tool.write_file(
            path,
            "active-after\n",
            description="phase01 active update while shell lease is held",
        ),
    )
    assert_committed(update, path=path)

    await touch_tmp(handle, proceed)
    shell, shell_metric = await shell_task
    assert_committed(shell, path=shell_output)
    assert shell.exit_code == 0, shell.stderr

    await assert_read(handle, path, "active-after\n")
    await assert_read(handle, shell_output, "base-view|base-view\n")
    _assert_no_cache_shell_timings(shell.timings)

    after_metrics = await handle.tool.layer_metrics()
    assert int(after_metrics["active_leases"]) == 0, after_metrics
    emit_metric(
        "phase08.in_flight_public_shell_lease",
        {
            **summarize_calls([update_metric, shell_metric]),
            "mid_active_leases": mid_metrics["active_leases"],
            "active_leases_after": after_metrics["active_leases"],
        },
    )


async def test_public_mutations_naturally_trigger_squash_and_keep_workspace_view(
    workspace_base_sandbox: SandboxHandle,
) -> None:
    handle = workspace_base_sandbox
    watched_path = "tracked/phase08/auto-squash/watched.txt"
    edit_path = "tracked/phase08/auto-squash/edit.txt"
    shell_output = "tracked/phase08/auto-squash/shell-view.txt"
    await seed_imported_base(
        handle,
        {
            ".gitignore": "dist/\n",
            watched_path: "base-view\n",
            edit_path: "alpha=old\n",
        },
    )

    before_metrics = await handle.tool.layer_metrics()
    assert before_metrics["workspace_bound"] is True, before_metrics
    assert int(before_metrics["manifest_depth"]) == 1, before_metrics

    run = token("phase08-natural-squash")
    started = tmp_path(f"{run}-started")
    proceed = tmp_path(f"{run}-go")
    await remove_tmp(handle, started, proceed)
    command = (
        "set -e; "
        f"first=$(cat {q(watched_path)}); "
        f"touch {q(started)}; "
        f"while [ ! -f {q(proceed)} ]; do sleep 0.01; done; "
        f"second=$(cat {q(watched_path)}); "
        "mkdir -p tracked/phase08/auto-squash; "
        f"printf '%s|%s\\n' \"$first\" \"$second\" > {q(shell_output)}"
    )
    shell_task = asyncio.create_task(
        timed_call(
            "phase08_natural_squash_shell_lease",
            handle.tool.shell(
                command,
                timeout=60,
                description="phase08 shell lease across natural auto squash",
            ),
        )
    )
    await wait_for_tmp(handle, started)

    metrics = []
    for index in range(AUTO_SQUASH_TRIGGER_WRITES):
        path = f"tracked/phase08/auto-squash/write-{index:02d}.txt"
        write, metric = await timed_call(
            f"phase08_natural_squash_write_{index:02d}",
            handle.tool.write_file(
                path,
                f"write-{index:02d}\n",
                description=f"phase08 natural squash write {index:02d}",
            ),
        )
        metrics.append(metric)
        assert_committed(write, path=path)

    squashed_metrics = await handle.tool.layer_metrics()
    assert squashed_metrics["workspace_bound"] is True, squashed_metrics
    assert int(squashed_metrics["active_leases"]) >= 1, squashed_metrics
    assert int(squashed_metrics["manifest_depth"]) <= AUTO_SQUASH_MAX_DEPTH, (
        squashed_metrics
    )
    assert int(squashed_metrics["manifest_version"]) > (
        int(before_metrics["manifest_version"]) + AUTO_SQUASH_TRIGGER_WRITES
    ), squashed_metrics

    await assert_read(handle, watched_path, "base-view\n")
    await assert_read(handle, "tracked/phase08/auto-squash/write-00.txt", "write-00\n")
    await assert_read(
        handle,
        f"tracked/phase08/auto-squash/write-{AUTO_SQUASH_TRIGGER_WRITES - 1:02d}.txt",
        f"write-{AUTO_SQUASH_TRIGGER_WRITES - 1:02d}\n",
    )

    edit, edit_metric = await timed_call(
        "phase08_natural_squash_edit_after_trigger",
        handle.tool.edit_file(
            edit_path,
            [("alpha=old", "alpha=new")],
            description="phase08 edit after natural auto squash",
        ),
    )
    metrics.append(edit_metric)
    assert_committed(edit, path=edit_path)

    await touch_tmp(handle, proceed)
    shell, shell_metric = await shell_task
    metrics.append(shell_metric)
    assert_committed(shell, path=shell_output)
    assert shell.exit_code == 0, shell.stderr
    _assert_no_cache_shell_timings(shell.timings)

    await assert_read(handle, shell_output, "base-view|base-view\n")
    await assert_read(handle, edit_path, "alpha=new\n")
    final_metrics = await handle.tool.layer_metrics()
    assert int(final_metrics["active_leases"]) == 0, final_metrics
    assert int(final_metrics["manifest_depth"]) <= AUTO_SQUASH_MAX_DEPTH, final_metrics
    emit_metric(
        "phase08.natural_auto_squash_workspace_view",
        {
            **summarize_calls(metrics),
            "trigger_writes": AUTO_SQUASH_TRIGGER_WRITES,
            "manifest_depth_after_trigger": squashed_metrics["manifest_depth"],
            "manifest_version_after_trigger": squashed_metrics["manifest_version"],
            "final_manifest_depth": final_metrics["manifest_depth"],
        },
    )


def _assert_no_cache_shell_timings(
    timings: dict[str, float],
    *,
    require_occ: bool = True,
) -> None:
    required = {
        "layer_stack.materialize_s",
        "layer_stack.prepare_workspace_snapshot.total_s",
        "command_exec.prepare_snapshot_s",
        "command_exec.mount_workspace_s",
        "command_exec.run_command_s",
        "command_exec.capture_upperdir_s",
        "command_exec.occ_apply_s",
        "command_exec.release_snapshot_s",
        "command_exec.total_s",
        "api.shell.overlay_s",
        "api.shell.occ_apply_s",
        "api.shell.total_s",
    }
    if require_occ:
        required |= {
            "occ.prepare.total_s",
            "occ.commit.total_s",
            "occ.apply.total_s",
        }
    assert required <= timings.keys()
    assert timings.keys().isdisjoint(
        {
            "cache_hit",
            "cache_policy",
            "lowerdir_cache_hit",
            "lowerdir_cache_hits",
            "lowerdir_cache_misses",
            "materialized_byte_count",
        }
    )


async def _transient_lowerdir_parent_count(handle: SandboxHandle) -> int:
    root = f"{DEFAULT_LAYER_STACK_ROOT}/runtime/{_TRANSIENT_LOWERDIR_DIR}"
    result = await handle.raw_exec(
        handle.sandbox_id,
        (
            f"if [ -d {q(root)} ]; then "
            f"find {q(root)} -mindepth 1 -maxdepth 1 -type d | wc -l; "
            "else echo 0; fi"
        ),
        timeout=30,
    )
    assert result.exit_code == 0, result.stderr or result.stdout
    return int(result.stdout.strip() or "0")
