"""Scenario 0: replace_all / multi_edit complex correctness + perf (live-e2e).

Unit tests already cover ``apply_search_replace`` semantics
(``test_occ/test_edit_replace_all.py``); this exercises the SAME semantics end
to end through the real sandbox OCC publish path and records ``api.edit.*``
timing JSONL (``timed_call``), per plan D9 (correctness + recorded metrics with
thresholds).

Covered:
  * ``replace_all`` over many real occurrences in a real file → all replaced.
  * default mode on >1 occurrence → ``aborted_overlap`` / "anchor occurrence
    count mismatch", file unchanged.
  * ``multi_edit`` sequential-evolving (edit 2 operates on edit 1's output).
  * ``multi_edit`` all-or-nothing (one failing op ⇒ file unchanged).
  * single ``old_text``-not-found aborts, file unchanged.
  * a large-file (≥64 KiB, many anchors) ``replace_all`` perf cell with a
    regression threshold vs a single-edit baseline.
"""

from __future__ import annotations

import pytest

import sandbox.api as sandbox_api
from sandbox.api import EditFileRequest, EditFileResult, SearchReplaceEdit

from .._harness.integrated_cases import RuntimeCallMetric, assert_committed, timed_call
from .._harness.sandbox_fixture import SandboxHandle


pytestmark = pytest.mark.asyncio

_LARGE_FILE_MIN_BYTES = 64 * 1024
# Generous band: replace_all rewrites the whole file in one OCC commit, so it
# should stay within a small multiple of a single-edit baseline. A floor keeps
# the assertion stable when the baseline is sub-millisecond on fast hosts.
_PERF_RATIO = 4.0
_PERF_FLOOR_MS = 250.0


async def _edit(
    handle: SandboxHandle,
    label: str,
    path: str,
    edits: tuple[tuple[str, str, bool], ...],
) -> tuple[EditFileResult, RuntimeCallMetric]:
    request = EditFileRequest(
        path=path,
        edits=tuple(
            SearchReplaceEdit(old_text=old, new_text=new, replace_all=replace_all)
            for old, new, replace_all in edits
        ),
        caller=handle.caller,
        description=label,
    )
    return await timed_call(label, sandbox_api.edit_file(handle.sandbox_id, request))


def _is_anchor_abort(result: EditFileResult) -> bool:
    reason = (result.conflict_reason or "").lower()
    return (not result.success) and (
        result.status == "aborted_overlap"
        or "anchor not found" in reason
        or "occurrence count mismatch" in reason
    )


async def test_edit_replace_all_replaces_every_occurrence(
    integrated_sandbox: SandboxHandle,
) -> None:
    handle = integrated_sandbox
    path = "edit_scenarios/replace_all.txt"
    body = "".join(f"line {i}: OLD_TOKEN tail\n" for i in range(30))
    seed = await handle.tool.write_file(path, body)
    assert_committed(seed, path=path)

    result, _ = await _edit(
        handle,
        "edit_replace_all_many",
        path,
        (("OLD_TOKEN", "NEW_TOKEN", True),),
    )
    assert_committed(result, path=path)

    read = await handle.tool.read_file(path)
    assert read.success and read.exists, read
    assert "OLD_TOKEN" not in read.content, read.content[:200]
    assert read.content.count("NEW_TOKEN") == 30, read.content[:200]


async def test_edit_default_mode_multiple_occurrences_aborts(
    integrated_sandbox: SandboxHandle,
) -> None:
    handle = integrated_sandbox
    path = "edit_scenarios/default_multi.txt"
    body = "DUP\nmiddle\nDUP\ntail\nDUP\n"
    seed = await handle.tool.write_file(path, body)
    assert_committed(seed, path=path)

    result, _ = await _edit(
        handle,
        "edit_default_mode_overlap",
        path,
        (("DUP", "ONE", False),),
    )
    assert _is_anchor_abort(result), result
    assert "occurrence count mismatch" in (result.conflict_reason or "").lower(), result

    read = await handle.tool.read_file(path)
    assert read.success and read.content == body, read  # unchanged


async def test_edit_multi_edit_sequential_evolving(
    integrated_sandbox: SandboxHandle,
) -> None:
    handle = integrated_sandbox
    path = "edit_scenarios/sequential.txt"
    body = "AAA keep BBB\n"
    seed = await handle.tool.write_file(path, body)
    assert_committed(seed, path=path)

    # Edit 2's anchor ("CCC") only exists because edit 1 produced it — proving
    # edits apply sequentially to the evolving buffer, not the original.
    result, _ = await _edit(
        handle,
        "edit_multi_sequential",
        path,
        (("AAA", "CCC", False), ("CCC", "DDD", False)),
    )
    assert_committed(result, path=path)

    read = await handle.tool.read_file(path)
    assert read.success, read
    assert read.content == "DDD keep BBB\n", read.content


async def test_edit_multi_edit_all_or_nothing(
    integrated_sandbox: SandboxHandle,
) -> None:
    handle = integrated_sandbox
    path = "edit_scenarios/atomic.txt"
    body = "XXX keep YYY\n"
    seed = await handle.tool.write_file(path, body)
    assert_committed(seed, path=path)

    # First op would succeed; second op's anchor is absent ⇒ the whole group
    # aborts and the file is byte-for-byte unchanged.
    result, _ = await _edit(
        handle,
        "edit_multi_atomic_abort",
        path,
        (("XXX", "ZZZ", False), ("NOPE", "QQQ", False)),
    )
    assert _is_anchor_abort(result), result

    read = await handle.tool.read_file(path)
    assert read.success and read.content == body, read  # nothing partially applied
    assert "ZZZ" not in read.content, read.content


async def test_edit_old_text_not_found_aborts(
    integrated_sandbox: SandboxHandle,
) -> None:
    handle = integrated_sandbox
    path = "edit_scenarios/missing_anchor.txt"
    body = "alpha beta gamma\n"
    seed = await handle.tool.write_file(path, body)
    assert_committed(seed, path=path)

    result, _ = await _edit(
        handle,
        "edit_missing_anchor",
        path,
        (("DOES_NOT_EXIST", "x", False),),
    )
    assert _is_anchor_abort(result), result

    read = await handle.tool.read_file(path)
    assert read.success and read.content == body, read  # unchanged


async def test_edit_large_file_replace_all_perf(
    integrated_sandbox: SandboxHandle,
) -> None:
    handle = integrated_sandbox
    path = "edit_scenarios/large.txt"
    # ≥64 KiB with many TOKEN anchors and one unique baseline anchor.
    lines = [f"row {i:05d}: value=TOKEN end\n" for i in range(2600)]
    lines.append("UNIQUE_BASELINE_ANCHOR marker\n")
    body = "".join(lines)
    assert len(body.encode()) >= _LARGE_FILE_MIN_BYTES, len(body.encode())
    seed = await handle.tool.write_file(path, body)
    assert_committed(seed, path=path)
    token_count = body.count("TOKEN")

    # Single-edit baseline: one unique anchor, default mode.
    baseline, baseline_metric = await _edit(
        handle,
        "edit_large_single_baseline",
        path,
        (("UNIQUE_BASELINE_ANCHOR", "UNIQUE_BASELINE_DONE", False),),
    )
    assert_committed(baseline, path=path)

    # replace_all over every TOKEN anchor in the large file.
    bulk, bulk_metric = await _edit(
        handle,
        "edit_large_replace_all",
        path,
        (("TOKEN", "REPLACED", True),),
    )
    assert_committed(bulk, path=path)

    read = await handle.tool.read_file(path)
    assert read.success, read
    assert "TOKEN" not in read.content, read.content[:200]
    assert read.content.count("REPLACED") == token_count, (
        token_count, read.content.count("REPLACED"),
    )

    ceiling_ms = max(_PERF_RATIO * baseline_metric.elapsed_ms, _PERF_FLOOR_MS)
    assert bulk_metric.elapsed_ms <= ceiling_ms, (
        f"replace_all over {token_count} anchors took "
        f"{bulk_metric.elapsed_ms:.1f}ms; baseline single-edit "
        f"{baseline_metric.elapsed_ms:.1f}ms; ceiling {ceiling_ms:.1f}ms "
        "(O(N²) edit-apply regression suspected)"
    )
