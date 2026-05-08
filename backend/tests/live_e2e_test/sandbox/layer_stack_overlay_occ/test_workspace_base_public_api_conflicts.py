"""Public API conflict probes over an imported `/testbed` workspace base."""

from __future__ import annotations

import asyncio
from pathlib import PurePosixPath

import pytest

import sandbox.host.daemon_client as daemon_client_mod
from sandbox.contract import GuardedResultBase

from .._harness.concurrency import gather_with_barrier
from .._harness.integrated_cases import (
    assert_committed,
    assert_read,
    assert_rejected,
    emit_metric,
    paths_visible_summary,
    q,
    remove_tmp,
    summarize_calls,
    timed_call,
    tmp_path,
    token,
    touch_tmp,
    wait_for_tmp,
)
from .._harness.sandbox_fixture import SandboxHandle, WORKSPACE_ROOT


pytestmark = pytest.mark.asyncio


async def test_workspace_base_concurrent_public_writes_conflict_on_existing_base_file(
    workspace_base_sandbox: SandboxHandle,
) -> None:
    handle = workspace_base_sandbox
    path = "tracked/base-write-conflict.txt"
    await _seed_imported_base(handle, {path: "base\n"})

    async def write_one(index: int):
        return await timed_call(
            f"base_write_conflict_{index}",
            handle.tool.write_file(
                path,
                f"writer-{index}\n",
                description=f"phase01 base write conflict writer {index}",
            ),
        )

    rows = await gather_with_barrier([lambda: write_one(0), lambda: write_one(1)])
    accepted = [(result, metric) for result, metric in rows if result.success]
    rejected = [(result, metric) for result, metric in rows if not result.success]

    assert len(accepted) == 1, rows
    assert len(rejected) == 1, rows
    assert_committed(accepted[0][0], path=path)
    _assert_conflicted(rejected[0][0], path=path)

    final = await handle.tool.read_file(path)
    assert final.success
    assert final.exists
    assert final.content in {"writer-0\n", "writer-1\n"}
    emit_metric(
        "workspace_base.public_api_write_conflict",
        {
            **summarize_calls([metric for _, metric in rows]),
            "final_content": final.content,
        },
    )


async def test_workspace_base_public_edits_handle_disjoint_and_overlapping_hunks(
    workspace_base_sandbox: SandboxHandle,
) -> None:
    handle = workspace_base_sandbox
    disjoint_path = "tracked/base-edit-disjoint.txt"
    overlap_path = "tracked/base-edit-overlap.txt"
    await _seed_imported_base(
        handle,
        {
            disjoint_path: "alpha=old\nbeta=stable\ngamma=old\n",
            overlap_path: "shared=old\n",
        },
    )

    async def edit_disjoint(label: str, old: str, new: str):
        return await timed_call(
            label,
            handle.tool.edit_file(
                disjoint_path,
                [(old, new)],
                description=f"phase01 base disjoint edit {label}",
            ),
        )

    disjoint_rows = await gather_with_barrier(
        [
            lambda: edit_disjoint("base_edit_disjoint_alpha", "alpha=old", "alpha=new"),
            lambda: edit_disjoint("base_edit_disjoint_gamma", "gamma=old", "gamma=new"),
        ]
    )
    for result, _ in disjoint_rows:
        assert_committed(result, path=disjoint_path)
    await assert_read(
        handle,
        disjoint_path,
        "alpha=new\nbeta=stable\ngamma=new\n",
    )

    async def edit_overlap(index: int):
        return await timed_call(
            f"base_edit_overlap_{index}",
            handle.tool.edit_file(
                overlap_path,
                [("shared=old", f"shared=writer-{index}")],
                description=f"phase01 base overlap edit {index}",
            ),
        )

    overlap_rows = await gather_with_barrier(
        [lambda: edit_overlap(0), lambda: edit_overlap(1)]
    )
    overlap_accepted = [
        (result, metric) for result, metric in overlap_rows if result.success
    ]
    overlap_rejected = [
        (result, metric) for result, metric in overlap_rows if not result.success
    ]
    assert len(overlap_accepted) == 1, overlap_rows
    assert len(overlap_rejected) == 1, overlap_rows
    assert_committed(overlap_accepted[0][0], path=overlap_path)
    _assert_conflicted(overlap_rejected[0][0], path=overlap_path)

    final = await handle.tool.read_file(overlap_path)
    assert final.success
    assert final.exists
    assert final.content in {"shared=writer-0\n", "shared=writer-1\n"}
    emit_metric(
        "workspace_base.public_api_edit_conflicts",
        {
            **summarize_calls([metric for _, metric in disjoint_rows + overlap_rows]),
            "overlap_final_content": final.content,
        },
    )


async def test_workspace_base_shell_tracked_conflict_rejects_gitignored_output(
    workspace_base_sandbox: SandboxHandle,
) -> None:
    handle = workspace_base_sandbox
    tracked_path = "tracked/base-shell-conflict.txt"
    ignored_path = "dist/base-shell-output.txt"
    await _seed_imported_base(
        handle,
        {
            ".gitignore": "dist/\n",
            tracked_path: "base\n",
        },
    )

    run = token("workspace-base-shell-conflict")
    started = tmp_path(f"{run}-started")
    proceed = tmp_path(f"{run}-go")
    await remove_tmp(handle, started, proceed)
    command = (
        "set -e; "
        f"touch {q(started)}; "
        f"while [ ! -f {q(proceed)} ]; do sleep 0.01; done; "
        "mkdir -p dist; "
        f"printf 'stale-shell\\n' > {q(tracked_path)}; "
        f"printf 'ignored-output\\n' > {q(ignored_path)}"
    )

    shell_task = asyncio.create_task(
        timed_call(
            "base_shell_conflict_with_ignored_output",
            handle.tool.shell(
                command,
                timeout=30,
                description="phase01 base shell tracked conflict with ignored output",
            ),
        )
    )
    await wait_for_tmp(handle, started)
    winner, winner_metric = await timed_call(
        "base_shell_conflict_winning_write",
        handle.tool.write_file(
            tracked_path,
            "winner\n",
            description="phase01 base winning write during shell lease",
        ),
    )
    assert_committed(winner, path=tracked_path)
    await touch_tmp(handle, proceed)
    rejected, rejected_metric = await shell_task
    assert_rejected(rejected, path=tracked_path)
    assert rejected.changed_paths == ()

    tracked = await assert_read(handle, tracked_path, "winner\n")
    ignored = await handle.tool.read_file(ignored_path)
    assert ignored.success
    assert not ignored.exists
    emit_metric(
        "workspace_base.public_shell_conflict_ignored_output",
        {
            **summarize_calls([winner_metric, rejected_metric]),
            **paths_visible_summary([tracked, ignored]),
            "ignored_output_committed": ignored.exists,
        },
    )


async def test_workspace_base_shell_delete_conflicts_with_public_write(
    workspace_base_sandbox: SandboxHandle,
) -> None:
    handle = workspace_base_sandbox
    path = "tracked/base-delete-write.txt"
    await _seed_imported_base(handle, {path: "base\n"})

    run = token("workspace-base-delete-write")
    started = tmp_path(f"{run}-started")
    proceed = tmp_path(f"{run}-go")
    await remove_tmp(handle, started, proceed)
    command = (
        "set -e; "
        f"touch {q(started)}; "
        f"while [ ! -f {q(proceed)} ]; do sleep 0.01; done; "
        f"rm -f -- {q(path)}"
    )

    shell_task = asyncio.create_task(
        timed_call(
            "base_shell_delete_stale",
            handle.tool.shell(
                command,
                timeout=30,
                description="phase01 base stale shell delete",
            ),
        )
    )
    await wait_for_tmp(handle, started)
    writer, writer_metric = await timed_call(
        "base_delete_write_winner",
        handle.tool.write_file(
            path,
            "writer\n",
            description="phase01 base public write wins over stale delete",
        ),
    )
    assert_committed(writer, path=path)
    await touch_tmp(handle, proceed)
    rejected, rejected_metric = await shell_task
    assert_rejected(rejected, path=path)
    await assert_read(handle, path, "writer\n")
    emit_metric(
        "workspace_base.public_shell_delete_conflict",
        summarize_calls([writer_metric, rejected_metric]),
    )


async def test_workspace_base_raw_workspace_mutation_does_not_move_occ_base(
    workspace_base_sandbox: SandboxHandle,
) -> None:
    handle = workspace_base_sandbox
    path = "tracked/base-raw-mutation.txt"
    await _seed_imported_base(handle, {path: "base\n"})

    before = await assert_read(handle, path, "base\n")
    raw = await handle.raw_exec(
        handle.sandbox_id,
        f"printf 'raw-mutated\\n' > {q(f'{WORKSPACE_ROOT}/{path}')}",
        timeout=30,
    )
    assert raw.success, raw.stderr or raw.stdout
    after_raw = await assert_read(handle, path, "base\n")

    shell = await handle.tool.shell(
        f"set -e; printf 'from-shell\\n' > {q(path)}",
        timeout=30,
        description="phase01 base shell after raw workspace mutation",
    )
    assert_committed(shell, path=path)
    final = await assert_read(handle, path, "from-shell\n")
    emit_metric(
        "workspace_base.raw_mutation_occ_base_isolation",
        {
            **paths_visible_summary([before, after_raw, final]),
            "raw_workspace_mutation_influenced_occ_base": False,
        },
    )


async def _seed_imported_base(
    handle: SandboxHandle,
    files: dict[str, str],
) -> dict[str, object]:
    commands = ["set -e"]
    for path, content in files.items():
        _validate_relative_path(path)
        full_path = f"{WORKSPACE_ROOT}/{path}"
        parent = str(PurePosixPath(full_path).parent)
        commands.append(f"mkdir -p -- {q(parent)}")
        commands.append(f"printf %s {q(content)} > {q(full_path)}")
    result = await handle.raw_exec(handle.sandbox_id, "; ".join(commands), timeout=60)
    assert result.success, result.stderr or result.stdout

    built = await daemon_client_mod.call_daemon_api(
        handle.sandbox_id,
        "api.build_workspace_base",
        {"workspace_root": WORKSPACE_ROOT},
        timeout=180,
    )
    assert built.get("success") is True, built
    binding = built.get("binding")
    assert isinstance(binding, dict)
    assert binding.get("workspace_root") == WORKSPACE_ROOT
    assert binding.get("base_manifest_version") == 1
    return binding


def _validate_relative_path(path: str) -> None:
    posix = PurePosixPath(path)
    if posix.is_absolute() or ".." in posix.parts:
        raise ValueError(f"test fixture path must be workspace-relative: {path!r}")


def _assert_conflicted(result: GuardedResultBase, *, path: str) -> None:
    assert not result.success, result
    assert result.conflict_reason
    assert result.status.startswith("aborted_"), result
    assert path in result.changed_paths
    if result.conflict is not None:
        assert result.conflict.conflict_file in {path, None}
