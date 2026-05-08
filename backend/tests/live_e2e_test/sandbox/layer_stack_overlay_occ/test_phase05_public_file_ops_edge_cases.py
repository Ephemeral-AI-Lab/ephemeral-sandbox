"""Phase 05 complex correctness and conflict probes."""

from __future__ import annotations

import asyncio

import pytest

from sandbox.contract import GuardedResultBase

from .._harness.concurrency import gather_with_barrier
from .._harness.integrated_cases import (
    RuntimeCallMetric,
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
from .._harness.phase05_public_file_ops import (
    LARGE_NEW_TAIL,
    LARGE_OLD_TAIL,
    LARGE_PATH,
    edited_large_text_content,
    phase05_call_row,
    phase05_summary_row,
    public_reconcile,
    seed_phase05_imported_base,
    write_phase05_jsonl_artifact,
)
from .._harness.sandbox_fixture import SandboxHandle


pytestmark = pytest.mark.asyncio


async def test_phase05_public_conflicts_and_disjoint_concurrency(
    workspace_base_sandbox: SandboxHandle,
) -> None:
    handle = workspace_base_sandbox
    binding = await seed_phase05_imported_base(handle)
    metrics: list[RuntimeCallMetric] = []

    same_path = "tracked/edge/same-write.txt"

    async def same_write(index: int):
        return await timed_call(
            f"phase05_edge_same_write_{index}",
            handle.tool.write_file(
                same_path,
                f"writer-{index}\n",
                description=f"phase05 same-path writer {index}",
            ),
        )

    same_rows = await gather_with_barrier(
        [lambda: same_write(0), lambda: same_write(1)]
    )
    metrics.extend(metric for _, metric in same_rows)
    same_successes = [result for result, _ in same_rows if result.success]
    same_rejects = [result for result, _ in same_rows if not result.success]
    assert len(same_successes) == 1, same_rows
    assert len(same_rejects) == 1, same_rows
    assert_committed(same_successes[0], path=same_path)
    _assert_conflicted(same_rejects[0], path=same_path)
    same_final = await handle.tool.read_file(same_path)
    assert same_final.success and same_final.exists
    assert same_final.content in {"writer-0\n", "writer-1\n"}

    async def disjoint_write(index: int):
        path = f"tracked/edge/disjoint-write-{index:02d}.txt"
        return await timed_call(
            f"phase05_edge_disjoint_write_{index:02d}",
            handle.tool.write_file(
                path,
                f"disjoint-{index:02d}\n",
                description=f"phase05 disjoint writer {index:02d}",
            ),
        )

    disjoint_rows = await gather_with_barrier(
        [lambda index=index: disjoint_write(index) for index in range(20)]
    )
    metrics.extend(metric for _, metric in disjoint_rows)
    for result, _ in disjoint_rows:
        assert_committed(result)
    await public_reconcile(
        handle,
        {
            f"tracked/edge/disjoint-write-{index:02d}.txt": (
                f"disjoint-{index:02d}\n"
            )
            for index in range(20)
        },
    )

    disjoint_specs = [
        ("alpha", "alpha=old", "alpha=new"),
        ("gamma", "gamma=old", "gamma=new"),
    ]

    async def disjoint_edit(label: str, old: str, new: str):
        return await timed_call(
            f"phase05_edge_disjoint_edit_{label}",
            handle.tool.edit_file(
                "tracked/edge/disjoint-edit.txt",
                [(old, new)],
                description=f"phase05 disjoint edit {label}",
            ),
        )

    disjoint_edit_rows = await gather_with_barrier(
        [
            lambda label=label, old=old, new=new: disjoint_edit(label, old, new)
            for label, old, new in disjoint_specs
        ]
    )
    metrics.extend(metric for _, metric in disjoint_edit_rows)
    for (label, old, new), (result, _) in zip(
        disjoint_specs,
        disjoint_edit_rows,
        strict=True,
    ):
        if result.success:
            assert_committed(result, path="tracked/edge/disjoint-edit.txt")
            continue
        retry, retry_metric = await timed_call(
            f"phase05_edge_disjoint_edit_{label}_retry",
            handle.tool.edit_file(
                "tracked/edge/disjoint-edit.txt",
                [(old, new)],
                description=f"phase05 deterministic retry for disjoint edit {label}",
            ),
        )
        metrics.append(retry_metric)
        assert_committed(retry, path="tracked/edge/disjoint-edit.txt")
    await assert_read(
        handle,
        "tracked/edge/disjoint-edit.txt",
        "alpha=new\nbeta=stable\ngamma=new\n",
    )

    async def overlap_edit(index: int):
        return await timed_call(
            f"phase05_edge_overlap_edit_{index}",
            handle.tool.edit_file(
                "tracked/edge/overlap-edit.txt",
                [("shared=old", f"shared=writer-{index}")],
                description=f"phase05 overlap edit {index}",
            ),
        )

    overlap_rows = await gather_with_barrier(
        [lambda: overlap_edit(0), lambda: overlap_edit(1)]
    )
    metrics.extend(metric for _, metric in overlap_rows)
    overlap_successes = [result for result, _ in overlap_rows if result.success]
    overlap_rejects = [result for result, _ in overlap_rows if not result.success]
    assert len(overlap_successes) == 1, overlap_rows
    assert len(overlap_rejects) == 1, overlap_rows
    assert_committed(overlap_successes[0], path="tracked/edge/overlap-edit.txt")
    _assert_conflicted(overlap_rejects[0], path="tracked/edge/overlap-edit.txt")

    summary = phase05_summary_row(
        case="edge_conflicts",
        binding=binding,
        concurrency=20,
        metrics=metrics,
        batch_wall_ms=sum(metric.elapsed_ms for metric in metrics),
        correctness={
            "same_path_one_commit_one_conflict": True,
            "disjoint_writes_all_visible": True,
            "disjoint_edits_both_visible": True,
            "overlapping_edits_one_conflict": True,
        },
    )
    artifact = write_phase05_jsonl_artifact(
        case="edge_conflicts",
        rows=[
            summary,
            *(
                phase05_call_row(
                    case="edge_conflicts",
                    metric=metric,
                    concurrency=20,
                )
                for metric in metrics
            ),
        ],
    )
    emit_metric(
        "phase05.public_file_ops.edge_conflicts",
        {
            **summarize_calls(metrics),
            "artifact": str(artifact),
        },
    )


async def test_phase05_shell_stale_conflicts_and_nonzero_policy(
    workspace_base_sandbox: SandboxHandle,
) -> None:
    handle = workspace_base_sandbox
    binding = await seed_phase05_imported_base(handle)
    metrics: list[RuntimeCallMetric] = []

    stale_path = "tracked/edge/shell-stale.txt"
    ignored_path = "dist/edge-shell-ignored.txt"
    run = token("phase05-shell-stale")
    started = tmp_path(f"{run}-started")
    proceed = tmp_path(f"{run}-go")
    await remove_tmp(handle, started, proceed)
    command = (
        "set -e; "
        f"touch {q(started)}; "
        f"while [ ! -f {q(proceed)} ]; do sleep 0.01; done; "
        "mkdir -p dist; "
        f"printf 'stale-shell\\n' > {q(stale_path)}; "
        f"printf 'ignored-output\\n' > {q(ignored_path)}"
    )
    shell_task = asyncio.create_task(
        timed_call(
            "phase05_edge_shell_stale_conflict",
            handle.tool.shell(
                command,
                timeout=30,
                description="phase05 stale shell tracked conflict",
            ),
        )
    )
    await wait_for_tmp(handle, started)
    winner, winner_metric = await timed_call(
        "phase05_edge_shell_stale_winner",
        handle.tool.write_file(
            stale_path,
            "winner\n",
            description="phase05 public write wins stale shell race",
        ),
    )
    metrics.append(winner_metric)
    assert_committed(winner, path=stale_path)
    await touch_tmp(handle, proceed)
    rejected, rejected_metric = await shell_task
    metrics.append(rejected_metric)
    assert_rejected(rejected, path=stale_path)
    await assert_read(handle, stale_path, "winner\n")
    ignored = await handle.tool.read_file(ignored_path)
    assert ignored.success and not ignored.exists

    delete_path = "tracked/edge/delete-vs-write.txt"
    run = token("phase05-delete-write")
    started = tmp_path(f"{run}-started")
    proceed = tmp_path(f"{run}-go")
    await remove_tmp(handle, started, proceed)
    delete_command = (
        "set -e; "
        f"touch {q(started)}; "
        f"while [ ! -f {q(proceed)} ]; do sleep 0.01; done; "
        f"rm -f -- {q(delete_path)}"
    )
    delete_task = asyncio.create_task(
        timed_call(
            "phase05_edge_shell_delete_stale",
            handle.tool.shell(
                delete_command,
                timeout=30,
                description="phase05 stale shell delete",
            ),
        )
    )
    await wait_for_tmp(handle, started)
    delete_winner, delete_winner_metric = await timed_call(
        "phase05_edge_shell_delete_winner",
        handle.tool.write_file(
            delete_path,
            "writer\n",
            description="phase05 public write wins stale shell delete",
        ),
    )
    metrics.append(delete_winner_metric)
    assert_committed(delete_winner, path=delete_path)
    await touch_tmp(handle, proceed)
    delete_rejected, delete_rejected_metric = await delete_task
    metrics.append(delete_rejected_metric)
    assert_rejected(delete_rejected, path=delete_path)
    await assert_read(handle, delete_path, "writer\n")

    nonzero, nonzero_metric = await timed_call(
        "phase05_edge_shell_nonzero_publishes_side_effects",
        handle.tool.shell(
            "printf 'nonzero side effect\\n' > tracked/edge/nonzero.txt; exit 7",
            timeout=30,
            description="phase05 nonzero shell side effects policy",
        ),
    )
    metrics.append(nonzero_metric)
    assert not nonzero.success
    assert nonzero.exit_code == 7
    assert "tracked/edge/nonzero.txt" in nonzero.changed_paths
    await assert_read(handle, "tracked/edge/nonzero.txt", "nonzero side effect\n")

    summary = phase05_summary_row(
        case="edge_shell_conflicts",
        binding=binding,
        concurrency=2,
        metrics=metrics,
        batch_wall_ms=sum(metric.elapsed_ms for metric in metrics),
        correctness={
            "stale_shell_conflict_rejected": True,
            "stale_shell_delete_rejected": True,
            "nonzero_shell_side_effects_publish": True,
        },
        pass_bars={
            "nonzero_shell_policy": (
                "workspace side effects publish even when exit_code is nonzero"
            ),
        },
    )
    artifact = write_phase05_jsonl_artifact(
        case="edge_shell_conflicts",
        rows=[
            summary,
            *(
                phase05_call_row(
                    case="edge_shell_conflicts",
                    metric=metric,
                    concurrency=2,
                )
                for metric in metrics
            ),
        ],
    )
    emit_metric(
        "phase05.public_file_ops.edge_shell_conflicts",
        {
            **summarize_calls(metrics),
            "artifact": str(artifact),
        },
    )


async def test_phase05_fail_closed_and_large_file_cases(
    workspace_base_sandbox: SandboxHandle,
) -> None:
    handle = workspace_base_sandbox
    await seed_phase05_imported_base(handle)

    create_only = await handle.tool.write_file(
        "tracked/edge/create-only-existing.txt",
        "should not overwrite\n",
        overwrite=False,
        description="phase05 create-only existing path",
    )
    assert not create_only.success
    assert create_only.status == "rejected"
    assert create_only.conflict_reason == "create_only_existing"
    await assert_read(handle, "tracked/edge/create-only-existing.txt", "existing\n")

    large_edit = await handle.tool.edit_file(
        LARGE_PATH,
        [(LARGE_OLD_TAIL, LARGE_NEW_TAIL)],
        description="phase05 large text edit near tail",
    )
    assert_committed(large_edit, path=LARGE_PATH)
    large_after = await handle.tool.read_file(LARGE_PATH)
    assert large_after.success and large_after.exists
    assert large_after.content == edited_large_text_content()

    before_binary = await _manifest_version(handle)
    with pytest.raises(RuntimeError, match="not valid UTF-8 text"):
        await handle.tool.edit_file(
            "tracked/binary.bin",
            [("phase05", "updated")],
            description="phase05 binary edit should fail closed",
        )
    assert await _manifest_version(handle) == before_binary

    missing_workspace = await handle.tool.read_file("tracked/edge/missing.txt")
    assert missing_workspace.success
    assert not missing_workspace.exists
    assert missing_workspace.content == ""

    missing_outside = await handle.tool.read_file("/tmp/phase05-edge-missing.txt")
    assert missing_outside.success
    assert not missing_outside.exists
    assert missing_outside.content == ""

    before_timeout = await _manifest_version(handle)
    timeout_result = await handle.tool.shell(
        "sleep 2; printf 'late\\n' > tracked/edge/timeout-late.txt",
        timeout=1,
        description="phase05 timeout before workspace write",
    )
    assert not timeout_result.success
    assert timeout_result.changed_paths == ()
    assert await _manifest_version(handle) == before_timeout
    timeout_read = await handle.tool.read_file("tracked/edge/timeout-late.txt")
    assert timeout_read.success and not timeout_read.exists
    after_metrics = await handle.tool.layer_metrics()
    assert int(after_metrics["active_leases"]) == 0, after_metrics
    assert int(after_metrics["staging_dirs"]) == 0, after_metrics


def _assert_conflicted(result: GuardedResultBase, *, path: str) -> None:
    assert not result.success, result
    assert result.conflict_reason
    assert result.status.startswith("aborted_"), result
    assert path in result.changed_paths
    if result.conflict is not None:
        assert result.conflict.conflict_file in {path, None}


async def _manifest_version(handle: SandboxHandle) -> int:
    metrics = await handle.tool.layer_metrics()
    assert metrics["success"] is True
    return int(metrics["manifest_version"])
