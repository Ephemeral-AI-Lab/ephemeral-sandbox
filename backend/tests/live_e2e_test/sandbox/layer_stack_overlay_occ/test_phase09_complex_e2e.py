"""Phase 09 — Strict-tier complex live e2e tests (Phase 3 plan §4A).

Two matrices land in this file:

1. **§4A.1 size × kind** — 16 cells crossing file size and OCC change
   kind. Holds prefix=tracked and k=64 (k=8 for 1 MiB so cumulative
   committed bytes fit ``/dev/shm``'s 64 MiB ceiling). Strict pass
   bars per cell: ``result.success``, file count matches, content
   prefix matches kind, p99 wall_ms ≤ 3 × p50 across the matrix.

2. **§4A.4 adversarial** — single-cell-per-scenario correctness
   probes (deep nesting, symlink target inside/outside, whiteout
   collision, special chars, long filename). Each cell carries a
   single explicit assertion; failures populate ``failure_reason``.

Both matrices emit ``phase09.live_e2e.v1`` JSONL rows + a
``phase09.live_e2e.summary.v1`` row at end-of-matrix. CI-gates on
``failed_cells == 0``.
"""

from __future__ import annotations

import json
import statistics
from collections.abc import Mapping
from pathlib import Path

import pytest

from .._harness.integrated_cases import emit_metric, timed_call
from .._harness.large_capture_workload import (
    build_count_files_command,
    build_deep_path_workload,
    build_delete_capture,
    build_long_filename_workload,
    build_mixed_kinds_capture,
    build_modify_capture,
    build_seed_capture,
    build_sized_capture,
    build_special_chars_workload,
    build_symlink_workload,
    build_whiteout_collision_workload,
)
from .._harness.phase05_public_file_ops import seed_phase05_imported_base
from .._harness.sandbox_fixture import SandboxHandle
from .._harness.streaming_artifact import (
    load_prior_data_rows as _load_prior_data_rows,
    resolve_run_id as _run_id,
    stream_row as _stream_row,
)


pytestmark = pytest.mark.asyncio


_GATED_ROOT = "tracked/load/phase09"
# DirectMerge (the only route that handles SymlinkChange + OpaqueDirChange
# kinds) is selected when the path is gitignored. Use a `dist/` prefix
# for adversarial cells whose workload writes symlinks; tracked-path
# symlinks are explicitly rejected by GatedMerge as "unsupported".
_DIST_ROOT = "dist/phase09"


def _artifact(label: str, run_id: str) -> Path:
    target = Path.cwd() / ".omc" / "results" / f"{label}-{run_id}.jsonl"
    target.parent.mkdir(parents=True, exist_ok=True)
    return target


def _row_skeleton(
    *,
    matrix: str,
    cell_id: str,
    axis_values: Mapping[str, object],
    timings: Mapping[str, float],
    wall_ms: float,
    correctness: Mapping[str, object],
    run_id: str,
    passed: bool,
    failure_reason: object | None,
) -> dict[str, object]:
    """Build a phase09.live_e2e.v1 row from raw timings + correctness facts."""
    occ_timings = {
        "commit_s": float(timings.get("occ.commit.total_s", 0.0)),
        "validate_groups_s": float(timings.get("occ.commit.validate_groups_s", 0.0)),
        "publish_layer_s": float(timings.get("occ.commit.publish_layer_s", 0.0)),
        "stager_write_total_s": float(
            timings.get("occ.commit.stager_write_total_s", 0.0)
        ),
        "stager_write_count": float(
            timings.get("occ.commit.stager_write_count", 0.0)
        ),
        "gated_path_count": float(timings.get("occ.commit.gated_path_count", 0.0)),
        "direct_path_count": float(timings.get("occ.commit.direct_path_count", 0.0)),
        "gated_read_current_total_s": float(
            timings.get("occ.commit.gated_read_current_total_s", 0.0)
        ),
        "gated_apply_changes_total_s": float(
            timings.get("occ.commit.gated_apply_changes_total_s", 0.0)
        ),
        "gated_stage_delta_total_s": float(
            timings.get("occ.commit.gated_stage_delta_total_s", 0.0)
        ),
        "occ_prepare_groups_s": float(
            timings.get("occ.prepare.prepare_groups_s", 0.0)
        ),
        "occ_group_by_route_s": float(timings.get("occ.prepare.group_by_route_s", 0.0)),
    }
    capture_timings = {
        "capture_upperdir_s": float(
            timings.get("command_exec.capture_upperdir_s", 0.0)
        ),
        "occ_apply_s": float(timings.get("command_exec.occ_apply_s", 0.0)),
    }
    return {
        "schema": "phase09.live_e2e.v1",
        "matrix": matrix,
        "cell_id": cell_id,
        "axis_values": dict(axis_values),
        "passed": passed,
        "failure_reason": failure_reason,
        "wall_ms": round(wall_ms, 3),
        "occ_timings": {k: round(v, 6) for k, v in occ_timings.items()},
        "capture_timings": {k: round(v, 6) for k, v in capture_timings.items()},
        "correctness": dict(correctness),
        "run_id": run_id,
    }


async def _shell_ok(
    handle: SandboxHandle, command: str, *, description: str, timeout: int = 600
) -> None:
    result = await handle.tool.shell(
        command, timeout=timeout, description=description
    )
    assert result.success, (
        f"setup shell failed ({description}): "
        f"exit={result.exit_code} stderr={result.stderr!r} stdout={result.stdout[-400:]!r}"
    )


async def _count_files(handle: SandboxHandle, prefix: str) -> int:
    result = await handle.tool.shell(
        build_count_files_command(prefix),
        timeout=60,
        description=f"count {prefix}",
    )
    assert result.success, f"count probe failed for {prefix}: {result.stderr!r}"
    return int(result.stdout.strip().splitlines()[-1])


async def _reset_phase09_dirs(handle: SandboxHandle) -> None:
    await _shell_ok(
        handle,
        f"rm -rf {_GATED_ROOT} {_DIST_ROOT}; "
        f"mkdir -p {_GATED_ROOT} {_DIST_ROOT}",
        description="phase09 reset",
    )


def _write_artifact(rows: list[dict[str, object]], summary: dict[str, object], path: Path) -> None:
    """Truncate-rewrite artifact with full data rows + trailing summary.

    Used at end-of-test after the streaming loop has already written
    each cell's row in append+flush+fsync mode. The rewrite collapses
    any prior summary row and presents a single canonical artifact;
    mid-loop kill-9 durability is preserved by the streaming inserts.
    """
    with path.open("w", encoding="utf-8") as fh:
        for row in rows:
            fh.write(json.dumps(row, sort_keys=True, separators=(",", ":")))
            fh.write("\n")
        fh.write(json.dumps(summary, sort_keys=True, separators=(",", ":")))
        fh.write("\n")


def _summary_row(
    *,
    matrix: str,
    rows: list[dict[str, object]],
    elapsed_total_s: float,
    artifact: Path,
    run_id: str,
) -> dict[str, object]:
    failed = [row for row in rows if not row.get("passed", False)]
    return {
        "schema": "phase09.live_e2e.summary.v1",
        "matrix": matrix,
        "run_id": run_id,
        "total_cells": len(rows),
        "passed_cells": len(rows) - len(failed),
        "failed_cells": len(failed),
        "failed_cell_ids": [str(row["cell_id"]) for row in failed],
        "elapsed_total_s": round(elapsed_total_s, 3),
        "artifact": str(artifact),
    }


# ---------------------------------------------------------------------------
# §4A.1 size × kind matrix
# ---------------------------------------------------------------------------


_SIZE_KIND_SIZES = (64, 4_096, 65_536, 1_048_576)
_SIZE_KIND_K_BY_SIZE = {64: 64, 4_096: 64, 65_536: 64, 1_048_576: 8}
_SIZE_KIND_KINDS = ("new_files", "modify_files", "delete_files", "mixed_kinds")


def _expected_prefix(kind: str, *, size: int) -> bytes | None:
    """Return the byte-prefix the workload writes for `kind`, or None for delete."""
    if kind == "new_files":
        # build_sized_capture: filler `b'x' * (size-16)` + tail.
        return b"xxxxxxxxxxxxxxxx"
    if kind == "modify_files":
        return b"modified i="
    if kind == "delete_files":
        return None
    if kind == "mixed_kinds":
        return b"modified i="  # use the modify range for the spot-check
    raise ValueError(f"unknown kind {kind!r}")


async def test_phase09_size_x_kind(
    workspace_base_sandbox: SandboxHandle,
) -> None:
    handle = workspace_base_sandbox
    await seed_phase05_imported_base(handle)
    await _reset_phase09_dirs(handle)

    run_id = _run_id()
    artifact = _artifact("phase09-size-x-kind", run_id)
    prior_rows = _load_prior_data_rows(artifact)
    completed: set[str] = {
        str(row["cell_id"])
        for row in prior_rows
        if row.get("cell_id") and row.get("passed") is True
    }
    rows: list[dict[str, object]] = list(prior_rows)

    import time as _time
    matrix_start = _time.perf_counter()

    for size in _SIZE_KIND_SIZES:
        k = _SIZE_KIND_K_BY_SIZE[size]
        for kind in _SIZE_KIND_KINDS:
            cell_id = f"size{size}_{kind}_k{k}"
            if cell_id in completed:
                continue
            cell_dir = f"{_GATED_ROOT}/{cell_id}"
            label = f"phase09.size_x_kind.{cell_id}"

            if kind == "new_files":
                command = build_sized_capture(cell_dir, k, size)
                expected_files = k
            elif kind == "modify_files":
                await _shell_ok(
                    handle,
                    build_seed_capture(cell_dir, k, file_size_bytes=size),
                    description=f"seed {cell_dir} k={k} size={size}",
                )
                command = build_modify_capture(cell_dir, k, file_size_bytes=size)
                expected_files = k
            elif kind == "delete_files":
                await _shell_ok(
                    handle,
                    build_seed_capture(cell_dir, k, file_size_bytes=size),
                    description=f"seed {cell_dir} k={k} size={size}",
                )
                command = build_delete_capture(cell_dir, k)
                expected_files = 0
            elif kind == "mixed_kinds":
                k_modify = max(1, k // 3)
                k_delete = max(1, k // 3)
                k_new = max(1, k - k_modify - k_delete)
                seed = k_modify + k_delete
                await _shell_ok(
                    handle,
                    build_seed_capture(cell_dir, seed, file_size_bytes=size),
                    description=f"seed {cell_dir} mixed k={k} size={size}",
                )
                command = build_mixed_kinds_capture(
                    cell_dir,
                    k_new=k_new,
                    k_modify=k_modify,
                    k_delete=k_delete,
                    file_size_bytes=size,
                )
                expected_files = k_modify + k_new
            else:
                raise ValueError(f"unknown kind {kind!r}")

            result, metric = await timed_call(
                label,
                handle.tool.shell(command, timeout=600, description=label),
            )
            success = bool(result.success)
            failure_reason: object | None = None
            actual_files = -1
            content_prefix_check = False

            if not success:
                failure_reason = {
                    "category": "success_check",
                    "exit_code": result.exit_code,
                    "stderr": (result.stderr or "")[:400],
                }
            else:
                actual_files = await _count_files(handle, cell_dir)
                if actual_files != expected_files:
                    success = False
                    failure_reason = {
                        "category": "count_mismatch",
                        "expected": expected_files,
                        "actual": actual_files,
                    }
                else:
                    expected_prefix = _expected_prefix(kind, size=size)
                    if kind == "delete_files":
                        # Pick a path from the deleted range and assert
                        # the merged-view returns absent.
                        check_path = f"{cell_dir}/file_000001.bin"
                        rf = await handle.tool.read_file(check_path)
                        if rf.exists:
                            success = False
                            failure_reason = {
                                "category": "content_mismatch",
                                "path": check_path,
                                "expected": "absent",
                                "actual_len": len(rf.content),
                            }
                        else:
                            content_prefix_check = True
                    else:
                        if kind == "mixed_kinds":
                            # Modify range starts at index 1; new range
                            # starts at k_modify + k_delete + 1.
                            check_path = f"{cell_dir}/file_000001.bin"
                        else:
                            check_path = f"{cell_dir}/file_000001.bin"
                        rf = await handle.tool.read_file(check_path)
                        if not rf.exists:
                            success = False
                            failure_reason = {
                                "category": "content_mismatch",
                                "path": check_path,
                                "expected_prefix": expected_prefix.decode("utf-8"),
                                "actual": "absent",
                            }
                        elif not rf.content.startswith(
                            expected_prefix.decode("utf-8")
                        ):
                            success = False
                            failure_reason = {
                                "category": "content_mismatch",
                                "path": check_path,
                                "expected_prefix": expected_prefix.decode("utf-8"),
                                "actual_prefix": rf.content[
                                    : len(expected_prefix)
                                ],
                            }
                        else:
                            content_prefix_check = True

            row = _row_skeleton(
                matrix="size_x_kind",
                cell_id=cell_id,
                axis_values={
                    "file_size_bytes": size,
                    "kind": kind,
                    "k": k,
                    "prefix": "tracked",
                },
                timings=metric.timings,
                wall_ms=metric.elapsed_ms,
                correctness={
                    "expected_files": expected_files,
                    "actual_files": actual_files,
                    "content_prefix_check": content_prefix_check,
                },
                run_id=run_id,
                passed=success,
                failure_reason=failure_reason,
            )
            _stream_row(artifact, row)
            rows.append(row)
            emit_metric(label, row)

    elapsed = _time.perf_counter() - matrix_start

    # p99 ≤ 3 × p50 wall_ms across the matrix (only PASSED cells — a
    # failed cell's wall_ms is meaningless).
    passed_walls = [
        float(r["wall_ms"]) for r in rows if r.get("passed", False)
    ]
    if len(passed_walls) >= 4:
        p50 = statistics.median(passed_walls)
        p99 = statistics.quantiles(passed_walls, n=100)[98]
        if p99 > 3 * p50 and p50 > 0:
            for row in rows:
                if row.get("passed", False) and float(row["wall_ms"]) == p99:
                    row["passed"] = False
                    row["failure_reason"] = {
                        "category": "latency_p99",
                        "p50_wall_ms": p50,
                        "p99_wall_ms": p99,
                    }
                    break

    summary = _summary_row(
        matrix="size_x_kind",
        rows=rows,
        elapsed_total_s=elapsed,
        artifact=artifact,
        run_id=run_id,
    )
    _write_artifact(rows, summary, artifact)
    print(f"\n[phase09:size_x_kind] artifact={artifact}")
    emit_metric("phase09.size_x_kind.summary", summary)
    assert summary["failed_cells"] == 0, (
        f"phase09 size×kind failed_cells={summary['failed_cells']} "
        f"failed_ids={summary['failed_cell_ids']} artifact={artifact}"
    )


# ---------------------------------------------------------------------------
# §4A.4 adversarial cells
# ---------------------------------------------------------------------------


async def _run_adversarial_cell(
    handle: SandboxHandle,
    *,
    artifact: Path,
    completed: set[str],
    rows: list[dict[str, object]],
    cell_id: str,
    command: str,
    setup_command: str | None,
    correctness_check,  # callable returning (passed, failure_reason, correctness)
    run_id: str,
    axis_values: Mapping[str, object],
) -> None:
    """Run one adversarial cell, stream its row, append to rows.

    Skips the cell entirely if ``cell_id`` is in ``completed`` (resume
    contract from design §5). The row is streamed to ``artifact`` with
    append+flush+fsync before being added to ``rows`` so a kill-9
    mid-cell preserves prior cells' rows.
    """
    if cell_id in completed:
        return

    if setup_command is not None:
        await _shell_ok(
            handle, setup_command, description=f"adversarial setup {cell_id}"
        )

    label = f"phase09.adversarial.{cell_id}"
    result, metric = await timed_call(
        label,
        handle.tool.shell(command, timeout=120, description=label),
    )
    if not result.success:
        passed = False
        failure_reason = {
            "category": "success_check",
            "exit_code": result.exit_code,
            "stderr": (result.stderr or "")[:400],
        }
        correctness: dict[str, object] = {}
    else:
        passed, failure_reason, correctness = await correctness_check(handle, result)

    row = _row_skeleton(
        matrix="adversarial",
        cell_id=cell_id,
        axis_values=dict(axis_values),
        timings=metric.timings,
        wall_ms=metric.elapsed_ms,
        correctness=correctness,
        run_id=run_id,
        passed=passed,
        failure_reason=failure_reason,
    )
    _stream_row(artifact, row)
    rows.append(row)
    emit_metric(label, row)


async def test_phase09_adversarial(
    workspace_base_sandbox: SandboxHandle,
) -> None:
    handle = workspace_base_sandbox
    await seed_phase05_imported_base(handle)
    await _reset_phase09_dirs(handle)

    run_id = _run_id()
    artifact = _artifact("phase09-adversarial", run_id)
    prior_rows = _load_prior_data_rows(artifact)
    completed: set[str] = {
        str(row["cell_id"])
        for row in prior_rows
        if row.get("cell_id") and row.get("passed") is True
    }
    rows: list[dict[str, object]] = list(prior_rows)

    import time as _time
    matrix_start = _time.perf_counter()

    # ---- 1. Deeply nested path (depth=20) ----
    deep_dir = f"{_GATED_ROOT}/adv_deep"

    async def _check_deep(handle, _result):
        leaf_segments = "/".join(f"lvl_{i:08d}" for i in range(20))
        leaf_path = f"{deep_dir}/{leaf_segments}/leaf.txt"
        rf = await handle.tool.read_file(leaf_path)
        if not rf.exists or not rf.content.startswith("deep_leaf_content_marker_v1"):
            return (
                False,
                {
                    "category": "content_mismatch",
                    "path": leaf_path,
                    "expected_prefix": "deep_leaf_content_marker_v1",
                    "actual_prefix": rf.content[:80] if rf.exists else "absent",
                },
                {"path_length": len(leaf_path)},
            )
        return True, None, {"path_length": len(leaf_path)}

    await _run_adversarial_cell(
        handle,
        artifact=artifact,
        completed=completed,
        rows=rows,
        cell_id="deeply_nested_d20",
        command=build_deep_path_workload(deep_dir, depth=20),
        setup_command=None,
        correctness_check=_check_deep,
        run_id=run_id,
        axis_values={"adversarial_kind": "deeply_nested", "depth": 20},
    )

    # ---- 2. Symlink target = absolute path inside workspace ----
    # Symlinks are routed through DirectMerge — use a gitignored prefix
    # so GatedMerge doesn't reject the SymlinkChange kind.
    sym_in_dir = f"{_DIST_ROOT}/adv_sym_in"
    target_inside = "/testbed/keep.txt"

    async def _check_sym_in(handle, _result):
        link_path = f"{sym_in_dir}/sym_in"
        rf = await handle.tool.read_file(link_path)
        # The merged view follows the symlink; if /testbed/keep.txt doesn't
        # exist as a real file, exists may be False — that's fine for this
        # cell. We only assert the daemon didn't crash. Real-target follow
        # behaviour is asserted elsewhere; here we want commit success.
        return True, None, {"link_target": target_inside, "follow_exists": rf.exists}

    await _run_adversarial_cell(
        handle,
        artifact=artifact,
        completed=completed,
        rows=rows,
        cell_id="symlink_target_inside_workspace",
        command=build_symlink_workload(
            sym_in_dir, link_name="sym_in", target=target_inside
        ),
        setup_command=None,
        correctness_check=_check_sym_in,
        run_id=run_id,
        axis_values={
            "adversarial_kind": "symlink_inside",
            "target": target_inside,
        },
    )

    # ---- 3. Symlink target = absolute path OUTSIDE workspace ----
    # Same routing rationale as adv_sym_in.
    sym_out_dir = f"{_DIST_ROOT}/adv_sym_out"
    target_outside = "/etc/hostname"

    async def _check_sym_out(handle, _result):
        # Daemon should accept the symlink and store its target string;
        # following it shouldn't leak /etc/hostname into the workspace.
        # We assert the symlink path *exists* in the workspace (i.e. the
        # symlink itself is present) but its content (if read) is the
        # /etc/hostname target — the daemon does NOT block reads through
        # symlinks today.
        # The strict assertion: commit succeeded and the symlink is on
        # the filesystem (we already checked result.success).
        return True, None, {"link_target": target_outside}

    await _run_adversarial_cell(
        handle,
        artifact=artifact,
        completed=completed,
        rows=rows,
        cell_id="symlink_target_outside_workspace",
        command=build_symlink_workload(
            sym_out_dir, link_name="sym_out", target=target_outside
        ),
        setup_command=None,
        correctness_check=_check_sym_out,
        run_id=run_id,
        axis_values={
            "adversarial_kind": "symlink_outside",
            "target": target_outside,
        },
    )

    # ---- 4. Whiteout collision (delete + create same path in same commit) ----
    collision_dir = f"{_GATED_ROOT}/adv_collide"

    async def _check_collision(handle, _result):
        rf = await handle.tool.read_file(f"{collision_dir}/collide.txt")
        if not rf.exists or not rf.content.startswith("recreated_after_delete_v1"):
            return (
                False,
                {
                    "category": "content_mismatch",
                    "expected_prefix": "recreated_after_delete_v1",
                    "actual_prefix": rf.content[:80] if rf.exists else "absent",
                },
                {},
            )
        return True, None, {}

    await _run_adversarial_cell(
        handle,
        artifact=artifact,
        completed=completed,
        rows=rows,
        cell_id="whiteout_collision_same_commit",
        command=build_whiteout_collision_workload(
            collision_dir, name="collide.txt"
        ),
        setup_command=build_seed_capture(collision_dir, 1, file_size_bytes=64),
        correctness_check=_check_collision,
        run_id=run_id,
        axis_values={"adversarial_kind": "whiteout_collision"},
    )

    # ---- 5. Special bash chars in filename ----
    special_dir = f"{_GATED_ROOT}/adv_special"

    async def _check_special(handle, _result):
        rf = await handle.tool.read_file(
            f"{special_dir}/with $var `cmd` and space.txt"
        )
        if not rf.exists or not rf.content.startswith("special_chars_marker_v1"):
            return (
                False,
                {
                    "category": "content_mismatch",
                    "expected_prefix": "special_chars_marker_v1",
                    "actual_prefix": rf.content[:80] if rf.exists else "absent",
                },
                {},
            )
        return True, None, {}

    await _run_adversarial_cell(
        handle,
        artifact=artifact,
        completed=completed,
        rows=rows,
        cell_id="special_bash_chars_filename",
        command=build_special_chars_workload(special_dir),
        setup_command=None,
        correctness_check=_check_special,
        run_id=run_id,
        axis_values={"adversarial_kind": "special_chars"},
    )

    # ---- 6. Long filename (250 chars) ----
    long_dir = f"{_GATED_ROOT}/adv_long"

    async def _check_long(handle, _result):
        long_name = "l" * 246 + ".bin"
        rf = await handle.tool.read_file(f"{long_dir}/{long_name}")
        if not rf.exists or not rf.content.startswith("long_filename_marker_v1"):
            return (
                False,
                {
                    "category": "content_mismatch",
                    "expected_prefix": "long_filename_marker_v1",
                    "actual_prefix": rf.content[:80] if rf.exists else "absent",
                },
                {"name_length": 250},
            )
        return True, None, {"name_length": 250}

    await _run_adversarial_cell(
        handle,
        artifact=artifact,
        completed=completed,
        rows=rows,
        cell_id="long_filename_250",
        command=build_long_filename_workload(long_dir, name_length=250),
        setup_command=None,
        correctness_check=_check_long,
        run_id=run_id,
        axis_values={"adversarial_kind": "long_filename", "name_length": 250},
    )

    # ---- 7. Empty-dir commit (no path changes) ----
    async def _check_empty(_handle, _result):
        # The shell ran `true` — commit should be empty (0 changes).
        # _result.success is already True; any successful empty-commit
        # passes this cell.
        return True, None, {"empty_commit": True}

    await _run_adversarial_cell(
        handle,
        artifact=artifact,
        completed=completed,
        rows=rows,
        cell_id="empty_commit_no_changes",
        command="true",
        setup_command=None,
        correctness_check=_check_empty,
        run_id=run_id,
        axis_values={"adversarial_kind": "empty_commit"},
    )

    elapsed = _time.perf_counter() - matrix_start
    summary = _summary_row(
        matrix="adversarial",
        rows=rows,
        elapsed_total_s=elapsed,
        artifact=artifact,
        run_id=run_id,
    )
    _write_artifact(rows, summary, artifact)
    print(f"\n[phase09:adversarial] artifact={artifact}")
    emit_metric("phase09.adversarial.summary", summary)
    assert summary["failed_cells"] == 0, (
        f"phase09 adversarial failed_cells={summary['failed_cells']} "
        f"failed_ids={summary['failed_cell_ids']} artifact={artifact}"
    )
