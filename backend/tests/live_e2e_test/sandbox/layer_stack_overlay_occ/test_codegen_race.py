"""E13 integrated codegen race coverage.

Tracked generated files are OCC-gated and stale shell captures must reject.
Gitignored build artifacts are routed through the pathspec oracle and use direct
last-writer-wins semantics.
"""

from __future__ import annotations

import asyncio

import pytest

from sandbox.api import ExecCommandResult

from .._harness.integrated_cases import (
    RuntimeCallMetric,
    assert_committed,
    assert_read,
    emit_metric,
    q,
    remove_tmp,
    summarize_calls,
    timed_call,
    tmp_path,
    token,
    touch_tmp,
    wait_for_tmps,
)
from .._harness.sandbox_fixture import SandboxHandle


pytestmark = pytest.mark.asyncio


async def _paired_shell_writes(
    handle: SandboxHandle,
    *,
    path: str,
    left_content: str,
    right_content: str,
    label: str,
) -> tuple[list[RuntimeCallMetric], list[ExecCommandResult]]:
    run = token(label)
    proceed = tmp_path(f"{run}-go")
    start_left = tmp_path(f"{run}-left-started")
    start_right = tmp_path(f"{run}-right-started")
    await remove_tmp(handle, proceed, start_left, start_right)

    def command(content: str, started: str) -> str:
        return (
            "set -e; "
            f"touch {q(started)}; "
            f"while [ ! -f {q(proceed)} ]; do sleep 0.01; done; "
            f"mkdir -p {q(path.rsplit('/', 1)[0])}; "
            f"cat > {q(path)} <<'EOF'\n{content}EOF\n"
        )

    left_task = asyncio.create_task(
        timed_call(
            f"{label}_left",
            handle.tool.shell(
                command(left_content, start_left),
                timeout=30,
                description=f"phase3 {label} left writer",
            ),
        )
    )
    right_task = asyncio.create_task(
        timed_call(
            f"{label}_right",
            handle.tool.shell(
                command(right_content, start_right),
                timeout=30,
                description=f"phase3 {label} right writer",
            ),
        )
    )
    await wait_for_tmps(handle, [start_left, start_right])
    await touch_tmp(handle, proceed)
    rows = await asyncio.gather(left_task, right_task)
    return [metric for _, metric in rows], [result for result, _ in rows]


async def test_two_agents_writing_same_tracked_generated_file_second_rejects_with_path_conflict(
    integrated_sandbox: SandboxHandle,
) -> None:
    path = "generated/schema.py"
    seed = await integrated_sandbox.tool.write_file(path, "VERSION = 'base'\n")
    assert_committed(seed, path=path)

    metrics, results = await _paired_shell_writes(
        integrated_sandbox,
        path=path,
        left_content="VERSION = 'left'\n",
        right_content="VERSION = 'right'\n",
        label="tracked_codegen_race",
    )
    accepted = [result for result in results if result.success]
    rejected = [result for result in results if not result.success]
    assert len(accepted) == 1
    assert len(rejected) == 1
    assert path in accepted[0].changed_paths
    assert rejected[0].changed_paths == ()
    final = await integrated_sandbox.tool.read_file(path)
    assert final.success and final.exists
    assert final.content in {"VERSION = 'left'\n", "VERSION = 'right'\n"}
    emit_metric(
        "codegen_race.tracked_conflict",
        {
            **summarize_calls(metrics),
            "accepted": len(accepted),
            "rejected": len(rejected),
            "final": final.content.strip(),
        },
    )


async def test_dist_artifact_concurrent_writes_both_accept_lww(
    integrated_sandbox: SandboxHandle,
) -> None:
    ignore = await integrated_sandbox.tool.write_file(
        ".gitignore",
        "dist/\n",
        description="phase3 codegen seed gitignore",
    )
    assert_committed(ignore, path=".gitignore")

    path = "dist/bundle.js"
    metrics, results = await _paired_shell_writes(
        integrated_sandbox,
        path=path,
        left_content="console.log('left');\n",
        right_content="console.log('right');\n",
        label="gitignored_codegen_race",
    )
    assert all(result.success for result in results), results
    assert all(path in result.changed_paths for result in results)
    final = await integrated_sandbox.tool.read_file(path)
    assert final.success and final.exists
    assert final.content in {"console.log('left');\n", "console.log('right');\n"}
    emit_metric(
        "codegen_race.gitignored_lww",
        {
            **summarize_calls(metrics),
            "accepted": 2,
            "final": final.content.strip(),
        },
    )


async def test_mixed_tracked_conflict_drops_gitignored_shell_capture(
    integrated_sandbox: SandboxHandle,
) -> None:
    ignore = await integrated_sandbox.tool.write_file(
        ".gitignore",
        "dist/\n",
        description="phase3 mixed capture seed gitignore",
    )
    assert_committed(ignore, path=".gitignore")
    tracked_path = "generated/mixed.py"
    ignored_path = "dist/mixed.js"
    seed = await integrated_sandbox.tool.write_file(tracked_path, "state = 'base'\n")
    assert_committed(seed, path=tracked_path)

    run = token("mixed-codegen")
    started = tmp_path(f"{run}-started")
    proceed = tmp_path(f"{run}-go")
    await remove_tmp(integrated_sandbox, started, proceed)
    command = (
        "set -e; "
        f"touch {q(started)}; "
        f"while [ ! -f {q(proceed)} ]; do sleep 0.01; done; "
        "mkdir -p generated dist; "
        f"printf \"state = 'stale'\\n\" > {q(tracked_path)}; "
        f"printf \"console.log('stale');\\n\" > {q(ignored_path)}"
    )
    shell_task = asyncio.create_task(
        timed_call(
            "mixed_codegen_stale_shell",
            integrated_sandbox.tool.shell(
                command,
                timeout=30,
                description="phase3 mixed tracked/gitignored stale shell",
            ),
        )
    )
    await wait_for_tmps(integrated_sandbox, [started])
    winner, winner_metric = await timed_call(
        "mixed_codegen_winning_api_write",
        integrated_sandbox.tool.write_file(
            tracked_path,
            "state = 'winner'\n",
            description="phase3 mixed capture winning tracked write",
        ),
    )
    assert_committed(winner, path=tracked_path)
    await touch_tmp(integrated_sandbox, proceed)
    shell, shell_metric = await shell_task
    assert not shell.success
    assert shell.changed_paths == ()
    await assert_read(integrated_sandbox, tracked_path, "state = 'winner'\n")
    ignored = await integrated_sandbox.tool.read_file(ignored_path)
    assert ignored.success
    assert not ignored.exists
    emit_metric(
        "codegen_race.mixed_strict_reject",
        {
            **summarize_calls([winner_metric, shell_metric]),
            "tracked_visible": "winner",
            "gitignored_dropped": True,
        },
    )
