"""Phase 07 — Complex-case performance metrics for shell large captures.

Three matrices, each parametrised over the gated (`tracked/`) and the
gitignored (`dist/`) routes the OCC commit groups separately:

1. **Size × K** — varies file size to expose the byte-traffic ceiling on
   capture / stager throughput, holding the path count low so the
   per-path filesystem work that Phase 2.3 (Lane D) optimised stays
   small. Answers: at what file size does the stager copy dominate?

2. **Kind × K** — varies the OverlayPathChange kind (new / modify /
   delete / mixed) so we measure the validate path against existing
   layer-stack entries, not just empty negatives. The K-scaling
   benchmark (Phase 06) only ever exercised 100 % NEW paths.

3. **Mixed routing** — populates *both* ``gated_path_count`` and
   ``direct_path_count`` from a single shell call so the routing-decision
   codepath in ``OccCommitTransaction`` is measured under load. Phase 06
   ran each prefix in isolation.

Each cell emits a JSONL row to ``.omc/results/phase07-complex-capture-
metrics-<run_id>.jsonl`` with every timing key plus the new dimensional
parameters (``file_size_bytes`` / ``kind`` / ``k_gated`` / ``k_dist``).
After every cell the test runs a count-files probe and asserts the
post-commit filesystem state matches the workload's intent — the
correctness check the K-scaling benchmark never did.
"""

from __future__ import annotations

from collections.abc import Mapping
from pathlib import Path

import pytest

from .._harness.integrated_cases import emit_metric, timed_call
from .._harness.large_capture_workload import (
    build_count_files_command,
    build_delete_capture,
    build_mixed_kinds_capture,
    build_mixed_routing_capture,
    build_modify_capture,
    build_seed_capture,
    build_sized_capture,
)
from .._harness.phase05_public_file_ops import seed_phase05_imported_base
from .._harness.sandbox_fixture import SandboxHandle
from .._harness.streaming_artifact import (
    load_prior_data_rows as _load_prior_data_rows,
    resolve_run_id as _resolve_run_id,
    rewrite_artifact as _rewrite_artifact,
    stream_row as _stream_row,
)


pytestmark = pytest.mark.asyncio


_GATED_ROOT = "tracked/load/phase07"
_DIST_ROOT = "dist/phase07"


def _artifact_path(label: str = "phase07-complex-capture-metrics") -> Path:
    target = Path.cwd() / ".omc" / "results" / f"{label}-{_resolve_run_id()}.jsonl"
    target.parent.mkdir(parents=True, exist_ok=True)
    return target


def _slug(value: str) -> str:
    return value.replace("/", "_").replace(".", "")


def _route_root(prefix: str) -> str:
    if prefix == "tracked":
        return _GATED_ROOT
    if prefix == "dist":
        return _DIST_ROOT
    raise ValueError(f"unknown route prefix {prefix!r}")


def _row_for_cell(
    *,
    matrix: str,
    cell: str,
    timings: Mapping[str, float],
    wall_ms: float,
    extras: Mapping[str, object],
) -> dict[str, object]:
    capture_s = float(timings.get("command_exec.capture_upperdir_s", 0.0))
    occ_apply_s = float(timings.get("command_exec.occ_apply_s", 0.0))
    commit_s = float(timings.get("occ.commit.total_s", 0.0))
    validate_groups_s = float(timings.get("occ.commit.validate_groups_s", 0.0))
    publish_layer_s = float(timings.get("occ.commit.publish_layer_s", 0.0))
    stager_write_total_s = float(
        timings.get("occ.commit.stager_write_total_s", 0.0)
    )
    stager_write_count = float(timings.get("occ.commit.stager_write_count", 0.0))
    prepare_groups_s = float(timings.get("occ.prepare.prepare_groups_s", 0.0))
    group_by_route_s = float(timings.get("occ.prepare.group_by_route_s", 0.0))
    gated_read_s = float(timings.get("occ.commit.gated_read_current_total_s", 0.0))
    gated_apply_s = float(
        timings.get("occ.commit.gated_apply_changes_total_s", 0.0)
    )
    gated_stage_s = float(timings.get("occ.commit.gated_stage_delta_total_s", 0.0))
    gated_count = float(timings.get("occ.commit.gated_path_count", 0.0))
    direct_read_s = float(timings.get("occ.commit.direct_read_current_total_s", 0.0))
    direct_apply_s = float(
        timings.get("occ.commit.direct_apply_changes_total_s", 0.0)
    )
    direct_stage_s = float(timings.get("occ.commit.direct_stage_delta_total_s", 0.0))
    direct_count = float(timings.get("occ.commit.direct_path_count", 0.0))
    total_paths = float(extras.get("total_paths", gated_count + direct_count) or 1)
    row: dict[str, object] = {
        "schema": "phase07.complex_capture_metrics.v1",
        "matrix": matrix,
        "cell": cell,
        "wall_ms": round(wall_ms, 3),
        "capture_upperdir_s": round(capture_s, 6),
        "occ_apply_s": round(occ_apply_s, 6),
        "commit_s": round(commit_s, 6),
        "validate_groups_s": round(validate_groups_s, 6),
        "publish_layer_s": round(publish_layer_s, 6),
        "stager_write_total_s": round(stager_write_total_s, 6),
        "stager_write_count": stager_write_count,
        "occ_prepare_groups_s": round(prepare_groups_s, 6),
        "occ_group_by_route_s": round(group_by_route_s, 6),
        "gated_read_current_total_s": round(gated_read_s, 6),
        "gated_apply_changes_total_s": round(gated_apply_s, 6),
        "gated_stage_delta_total_s": round(gated_stage_s, 6),
        "gated_path_count": gated_count,
        "direct_read_current_total_s": round(direct_read_s, 6),
        "direct_apply_changes_total_s": round(direct_apply_s, 6),
        "direct_stage_delta_total_s": round(direct_stage_s, 6),
        "direct_path_count": direct_count,
        "commit_per_file_us": round(commit_s * 1_000_000.0 / total_paths, 3),
        "capture_per_file_us": round(capture_s * 1_000_000.0 / total_paths, 3),
        "stager_per_file_us": round(
            stager_write_total_s * 1_000_000.0 / total_paths, 3
        ),
    }
    row.update(extras)
    return row


async def _shell_ok(
    handle: SandboxHandle, command: str, *, description: str, timeout: int = 600
) -> None:
    """Run a shell command (untimed setup); fail loudly on non-zero exit."""
    result = await handle.tool.shell(
        command, timeout=timeout, description=description
    )
    assert result.success, (
        f"setup shell failed ({description}): "
        f"exit={result.exit_code} stderr={result.stderr!r} stdout={result.stdout[-400:]!r}"
    )


async def _count_files(handle: SandboxHandle, prefix: str) -> int:
    """Count regular files under ``prefix`` via an in-sandbox python probe."""
    cmd = build_count_files_command(prefix)
    result = await handle.tool.shell(cmd, timeout=60, description=f"count {prefix}")
    assert result.success, f"count probe failed for {prefix}: {result.stderr!r}"
    return int(result.stdout.strip().splitlines()[-1])


async def _assert_content_prefix(
    handle: SandboxHandle,
    *,
    cell_id: str,
    path: str,
    expected_prefix: bytes,
) -> None:
    """Phase 3 verification A — read one path via tool.read_file and check.

    Asserts the daemon-API exposes the bytes the workload wrote — the
    correctness check the K-scaling benchmark never did. Uses
    ``tool.read_file`` (not a python3 probe) so the path travels through
    the layer-stack merged-view, exercising the same code paths that
    serve real agent reads.
    """
    expected_text = expected_prefix.decode("utf-8")
    result = await handle.tool.read_file(path)
    assert result.exists, (
        f"{cell_id}: tool.read_file({path!r}) returned exists=False; "
        f"status={getattr(result, 'status', '?')!r}"
    )
    assert result.content.startswith(expected_text), (
        f"{cell_id}: {path!r} content prefix mismatch — "
        f"expected {expected_text!r}, got {result.content[: len(expected_text)]!r}"
    )


async def _assert_path_absent(
    handle: SandboxHandle,
    *,
    cell_id: str,
    path: str,
) -> None:
    """Verify a deleted path is absent via the daemon-API merged view."""
    result = await handle.tool.read_file(path)
    assert not result.exists, (
        f"{cell_id}: expected {path!r} to be absent after delete, "
        f"tool.read_file returned exists=True content={result.content[:60]!r}"
    )


async def _run_and_record(
    handle: SandboxHandle,
    *,
    label: str,
    command: str,
    matrix: str,
    cell: str,
    extras: Mapping[str, object],
    timeout: int = 600,
) -> dict[str, object]:
    result, metric = await timed_call(
        label,
        handle.tool.shell(command, timeout=timeout, description=label),
    )
    assert result.success, (
        f"shell failed ({label}): exit={result.exit_code} "
        f"stderr={result.stderr!r} stdout={result.stdout[-400:]!r}"
    )
    row = _row_for_cell(
        matrix=matrix,
        cell=cell,
        timings=metric.timings,
        wall_ms=metric.elapsed_ms,
        extras=extras,
    )
    emit_metric(label, row)
    return row


async def _reset_workload_dirs(handle: SandboxHandle) -> None:
    """Wipe the phase07 prefixes so each test starts from a clean slate."""
    cmd = (
        f"rm -rf {_GATED_ROOT} {_DIST_ROOT}; "
        f"mkdir -p {_GATED_ROOT} {_DIST_ROOT}"
    )
    await _shell_ok(handle, cmd, description="phase07 reset")


# Note: per-cell cleanup is intentionally NOT done. Each shell call on
# this sandbox runs in copy-backed mode (the daytona image has no unshare
# privileges) so the daemon copies the entire workspace lower into
# /dev/shm (64 MiB tmpfs) on every invocation. The matrices below are
# sized so the cumulative committed bytes fit inside that budget; an
# extra rm-shell cell would also have to copy the same lower (and then
# write 500+ whiteout entries) which on this daytona instance hits a
# transient EIO inside the rm command itself. Skipping the cleanup
# trades workload accumulation for stability.


# ---------------------------------------------------------------------------
# Size × K matrix
# ---------------------------------------------------------------------------

# Size grid spans 64 B → 1 MiB (a 16384× range) but K shrinks for larger
# sizes so the cumulative committed bytes fit inside /dev/shm's 64 MiB
# budget without any inter-cell cleanup. Per-prefix accumulation ≈ 12 MiB,
# both prefixes ≈ 24 MiB, leaving headroom for daemon overhead.
_SIZE_BYTES_AND_K: tuple[tuple[int, tuple[int, ...]], ...] = (
    (64, (16, 256)),
    (4_096, (16, 64)),
    (65_536, (8, 32)),
    (1_048_576, (1, 8)),
)
_SIZE_PREFIXES = ("tracked", "dist")


async def test_phase07_size_matrix(
    workspace_base_sandbox: SandboxHandle,
) -> None:
    handle = workspace_base_sandbox
    await seed_phase05_imported_base(handle)
    await _reset_workload_dirs(handle)

    artifact = _artifact_path("phase07-size-matrix")
    prior_rows = _load_prior_data_rows(artifact)
    completed: set[str] = {
        str(row["cell"]) for row in prior_rows if row.get("cell")
    }
    rows: list[dict[str, object]] = list(prior_rows)

    for prefix in _SIZE_PREFIXES:
        for size, k_values in _SIZE_BYTES_AND_K:
            for k in k_values:
                cell_id = f"{prefix}_size{size}_k{k}"
                if cell_id in completed:
                    continue
                cell_dir = f"{_route_root(prefix)}/size_{size}_k{k}"
                command = build_sized_capture(cell_dir, k, size)
                row = await _run_and_record(
                    handle,
                    label=f"phase07.size.{cell_id}",
                    command=command,
                    matrix="size_x_k",
                    cell=cell_id,
                    extras={
                        "prefix": prefix,
                        "file_size_bytes": size,
                        "k": k,
                        "total_paths": k,
                        "expected_files": k,
                    },
                )
                actual = await _count_files(handle, cell_dir)
                assert actual == k, (
                    f"{cell_id}: expected {k} files, found {actual}"
                )
                row["actual_files"] = actual

                # Verification A — content prefix on the first path of
                # the cell. build_sized_capture writes filler `b'x' *
                # (size-16)` followed by `f'i={i:013d}\n'`; for size=64
                # the prefix is 48 'x' bytes. We assert the first 16
                # bytes are 'x' so the check works at every size on the
                # grid.
                await _assert_content_prefix(
                    handle,
                    cell_id=cell_id,
                    path=f"{cell_dir}/file_000001.bin",
                    expected_prefix=b"xxxxxxxxxxxxxxxx",
                )
                row["content_prefix_check"] = True
                _stream_row(artifact, row)
                rows.append(row)

    expected_cells = sum(len(k_values) for _, k_values in _SIZE_BYTES_AND_K) * len(
        _SIZE_PREFIXES
    )
    summary_row: dict[str, object] = {
        "schema": "phase07.size_matrix.summary.v1",
        "matrix": "size_x_k",
        "artifact": str(artifact),
        "total_cells": len(rows),
        "expected_cells": expected_cells,
        "passed_cells": len(rows),
        "failed_cells": 0,
        "run_id": _resolve_run_id(),
    }
    _rewrite_artifact(artifact, rows, summary_row)

    print(f"\n[phase07:size_matrix] artifact={artifact}")
    emit_metric("phase07.size_matrix.summary", summary_row)
    assert len(rows) == expected_cells


# ---------------------------------------------------------------------------
# Kind × K matrix
# ---------------------------------------------------------------------------

_KINDS = ("new_files", "modify_files", "delete_files", "mixed_kinds")
_KIND_K = (100, 1000)
_KIND_PREFIXES = ("tracked", "dist")


async def _run_kind_cell(
    handle: SandboxHandle,
    *,
    prefix: str,
    kind: str,
    k: int,
) -> dict[str, object]:
    cell_id = f"{prefix}_{kind}_k{k}"
    cell_dir = f"{_route_root(prefix)}/{kind}_k{k}"
    label = f"phase07.kind.{cell_id}"

    if kind == "new_files":
        # First-time create. No setup needed; expect K files post-commit.
        command = build_sized_capture(cell_dir, k, file_size_bytes=64)
        expected_files = k
        expected_paths = k

    elif kind == "modify_files":
        await _shell_ok(
            handle,
            build_seed_capture(cell_dir, k, file_size_bytes=64),
            description=f"seed {cell_dir} k={k}",
        )
        command = build_modify_capture(cell_dir, k, file_size_bytes=64)
        expected_files = k
        expected_paths = k

    elif kind == "delete_files":
        await _shell_ok(
            handle,
            build_seed_capture(cell_dir, k, file_size_bytes=64),
            description=f"seed {cell_dir} k={k}",
        )
        command = build_delete_capture(cell_dir, k)
        expected_files = 0
        expected_paths = k

    elif kind == "mixed_kinds":
        # Split K into thirds: ~K/3 modify, ~K/3 delete, ~K/3 new.
        k_modify = k // 3
        k_delete = k // 3
        k_new = k - k_modify - k_delete
        seed_count = k_modify + k_delete
        await _shell_ok(
            handle,
            build_seed_capture(cell_dir, seed_count, file_size_bytes=64),
            description=f"seed {cell_dir} mixed setup",
        )
        command = build_mixed_kinds_capture(
            cell_dir,
            k_new=k_new,
            k_modify=k_modify,
            k_delete=k_delete,
            file_size_bytes=64,
        )
        expected_files = k_modify + k_new  # deleted ones gone
        expected_paths = k_modify + k_delete + k_new

    else:
        raise ValueError(f"unknown kind {kind!r}")

    row = await _run_and_record(
        handle,
        label=label,
        command=command,
        matrix="kind_x_k",
        cell=cell_id,
        extras={
            "prefix": prefix,
            "kind": kind,
            "k": k,
            "total_paths": expected_paths,
            "expected_files": expected_files,
        },
    )
    actual = await _count_files(handle, cell_dir)
    assert actual == expected_files, (
        f"{cell_id}: expected {expected_files} files, found {actual}"
    )
    row["actual_files"] = actual

    # Verification A — content prefix per kind. All three live kinds
    # use file_size_bytes=64 with deterministic head bytes from
    # build_*_capture helpers.
    if kind == "new_files":
        # build_sized_capture: filler `b'x' * (size-16)` + `i=...\n`.
        await _assert_content_prefix(
            handle,
            cell_id=cell_id,
            path=f"{cell_dir}/file_000001.bin",
            expected_prefix=b"xxxxxxxxxxxxxxxx",
        )
    elif kind == "modify_files":
        # build_modify_capture: head `b'modified i=...\n'` + pad.
        await _assert_content_prefix(
            handle,
            cell_id=cell_id,
            path=f"{cell_dir}/file_000001.bin",
            expected_prefix=b"modified i=",
        )
    elif kind == "delete_files":
        # build_delete_capture: file is gone; merged view returns absent.
        await _assert_path_absent(
            handle,
            cell_id=cell_id,
            path=f"{cell_dir}/file_000001.bin",
        )
    elif kind == "mixed_kinds":
        # mixed_kinds packs modified + deleted + new. Verify one path
        # from EACH of the live ranges (modified + new) per plan §4.1.
        # build_mixed_kinds_capture indices: 1..k_modify modify,
        # k_modify+1..k_modify+k_delete delete, then k_modify+k_delete+1
        # onward = new.
        k_modify = k // 3
        k_delete = k // 3
        modify_idx = 1
        new_idx = k_modify + k_delete + 1
        await _assert_content_prefix(
            handle,
            cell_id=cell_id,
            path=f"{cell_dir}/file_{modify_idx:06d}.bin",
            expected_prefix=b"modified i=",
        )
        await _assert_content_prefix(
            handle,
            cell_id=cell_id,
            path=f"{cell_dir}/file_{new_idx:06d}.bin",
            expected_prefix=b"new i=",
        )
    row["content_prefix_check"] = True
    return row


async def test_phase07_kind_matrix(
    workspace_base_sandbox: SandboxHandle,
) -> None:
    handle = workspace_base_sandbox
    await seed_phase05_imported_base(handle)
    await _reset_workload_dirs(handle)

    artifact = _artifact_path("phase07-kind-matrix")
    prior_rows = _load_prior_data_rows(artifact)
    completed: set[str] = {
        str(row["cell"]) for row in prior_rows if row.get("cell")
    }
    rows: list[dict[str, object]] = list(prior_rows)

    for prefix in _KIND_PREFIXES:
        for kind in _KINDS:
            for k in _KIND_K:
                cell_id = f"{prefix}_{kind}_k{k}"
                if cell_id in completed:
                    continue
                row = await _run_kind_cell(handle, prefix=prefix, kind=kind, k=k)
                _stream_row(artifact, row)
                rows.append(row)

    expected_cells = len(_KINDS) * len(_KIND_K) * len(_KIND_PREFIXES)
    summary_row: dict[str, object] = {
        "schema": "phase07.kind_matrix.summary.v1",
        "matrix": "kind_x_k",
        "artifact": str(artifact),
        "total_cells": len(rows),
        "expected_cells": expected_cells,
        "passed_cells": len(rows),
        "failed_cells": 0,
        "run_id": _resolve_run_id(),
    }
    _rewrite_artifact(artifact, rows, summary_row)

    print(f"\n[phase07:kind_matrix] artifact={artifact}")
    emit_metric("phase07.kind_matrix.summary", summary_row)
    assert len(rows) == expected_cells


# ---------------------------------------------------------------------------
# Mixed-routing matrix
# ---------------------------------------------------------------------------

_ROUTING_SPLITS: tuple[tuple[int, int], ...] = (
    (500, 500),
    (1000, 100),
    (100, 1000),
)


async def test_phase07_mixed_routing_matrix(
    workspace_base_sandbox: SandboxHandle,
) -> None:
    handle = workspace_base_sandbox
    await seed_phase05_imported_base(handle)
    await _reset_workload_dirs(handle)

    artifact = _artifact_path("phase07-mixed-routing")
    prior_rows = _load_prior_data_rows(artifact)
    completed: set[str] = {
        str(row["cell"]) for row in prior_rows if row.get("cell")
    }
    rows: list[dict[str, object]] = list(prior_rows)

    for k_gated, k_dist in _ROUTING_SPLITS:
        cell_id = f"gated{k_gated}_dist{k_dist}"
        if cell_id in completed:
            continue
        gated_dir = f"{_GATED_ROOT}/routing_g{k_gated}_d{k_dist}"
        dist_dir = f"{_DIST_ROOT}/routing_g{k_gated}_d{k_dist}"
        command = build_mixed_routing_capture(
            gated_prefix=gated_dir,
            dist_prefix=dist_dir,
            k_gated=k_gated,
            k_dist=k_dist,
            file_size_bytes=64,
        )
        total = k_gated + k_dist
        row = await _run_and_record(
            handle,
            label=f"phase07.mixed_routing.{cell_id}",
            command=command,
            matrix="mixed_routing",
            cell=cell_id,
            extras={
                "k_gated": k_gated,
                "k_dist": k_dist,
                "total_paths": total,
                "expected_gated_files": k_gated,
                "expected_dist_files": k_dist,
            },
        )

        # Routing-decision correctness — exact OCC counts must match
        # the workload split. This is the codepath the K-scaling
        # benchmark never exercised.
        observed_gated = int(row["gated_path_count"])
        observed_dist = int(row["direct_path_count"])
        assert observed_gated == k_gated, (
            f"{cell_id}: gated_path_count={observed_gated}, expected {k_gated}"
        )
        assert observed_dist == k_dist, (
            f"{cell_id}: direct_path_count={observed_dist}, expected {k_dist}"
        )

        # Filesystem-state correctness: both prefixes carry their share.
        actual_gated = await _count_files(handle, gated_dir)
        actual_dist = await _count_files(handle, dist_dir)
        assert actual_gated == k_gated, (
            f"{cell_id}: gated dir has {actual_gated} files, expected {k_gated}"
        )
        assert actual_dist == k_dist, (
            f"{cell_id}: dist dir has {actual_dist} files, expected {k_dist}"
        )
        row["actual_gated_files"] = actual_gated
        row["actual_dist_files"] = actual_dist

        # Verification A — content prefix on one gated path AND one
        # dist path per cell. build_mixed_routing_capture writes
        # `b'gated i=...\n'` under gated_dir and `b'dist  i=...\n'`
        # (note: TWO spaces to align with `gated `) under dist_dir.
        await _assert_content_prefix(
            handle,
            cell_id=cell_id,
            path=f"{gated_dir}/file_000001.bin",
            expected_prefix=b"gated i=",
        )
        await _assert_content_prefix(
            handle,
            cell_id=cell_id,
            path=f"{dist_dir}/file_000001.bin",
            expected_prefix=b"dist  i=",
        )
        row["content_prefix_check"] = True
        _stream_row(artifact, row)
        rows.append(row)

    expected_cells = len(_ROUTING_SPLITS)
    summary_row: dict[str, object] = {
        "schema": "phase07.mixed_routing.summary.v1",
        "matrix": "mixed_routing",
        "artifact": str(artifact),
        "total_cells": len(rows),
        "expected_cells": expected_cells,
        "passed_cells": len(rows),
        "failed_cells": 0,
        "run_id": _resolve_run_id(),
    }
    _rewrite_artifact(artifact, rows, summary_row)

    print(f"\n[phase07:mixed_routing] artifact={artifact}")
    emit_metric("phase07.mixed_routing.summary", summary_row)
    assert len(rows) == expected_cells
