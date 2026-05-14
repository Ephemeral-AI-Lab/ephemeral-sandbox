"""Probe handler for the ``sandbox.complex_project_build`` scenario.

Drives the full plan §6 phase sequence inside a freshly-initialized
``/ephemeral-os`` git repository, with the layer-stack/OCC/overlay binding
rebound to that workspace root. The probe is split into a smoke variant (≥250
calls, 6 source files, 1 refactor pass) and a full variant (≥2,000 calls, 21
source files, 3 refactor passes).

Public entry point: :func:`run_complex_project_build_probe`. The runner
delegates here from ``MockSquadRunner._run_executor`` so this module owns the
phase-by-phase orchestration without bloating ``runner.py``.

Path discipline: every tool call uses an **absolute** ``/ephemeral-os/...``
file path so that the toolkit's ``resolve_sandbox_path`` does not rewrite the
target against the SWE-EVO ``repo_root`` (``/testbed``). The OCC/layer-stack
binding is rebound to ``/ephemeral-os`` via
``api.build_workspace_base(workspace_root='/ephemeral-os', reset=True)`` before
the first mutation.
"""

from __future__ import annotations

import asyncio
import json
import re
import time
from collections.abc import Awaitable, Callable, Sequence
from dataclasses import dataclass, field
from typing import Any

from message.stream_events import StreamEvent
from plugins.catalog.lsp.tools.diagnostics import diagnostics as lsp_diagnostics_tool
from plugins.catalog.lsp.tools.find_definitions import (
    find_definitions as lsp_find_definitions_tool,
)
from plugins.catalog.lsp.tools.find_references import (
    find_references as lsp_find_references_tool,
)
from plugins.catalog.lsp.tools.hover import hover as lsp_hover_tool
from plugins.catalog.lsp.tools.query_symbols import (
    query_symbols as lsp_query_symbols_tool,
)
from sandbox.api import (
    EditFileRequest,
    ReadFileRequest,
    SandboxCaller,
    SearchReplaceEdit,
    ShellRequest,
)
from sandbox.host.daemon_client import call_daemon_api
from tools._framework.core.base import BaseTool
from tools._framework.core.results import ToolResult
from tools._framework.core.runtime import ExecutionMetadata
from tools.sandbox.edit_file import edit_file as edit_file_tool
from tools.sandbox.read_file import read_file as read_file_tool
from tools.sandbox.shell import shell as shell_tool
from tools.sandbox.write_file import write_file as write_file_tool

import sandbox.api as sandbox_api

from live_e2e.audit.events import EventType
from live_e2e.scenarios.sandbox._fixtures.refactor_passes import (
    REFACTOR_PASSES,
    RefactorPass,
)
from live_e2e.scenarios.sandbox._fixtures.scheduler_demo_data import (
    SCHEDULER_DEMO_FILES,
    SMOKE_FILE_PATHS,
    FixtureFile,
)
from live_e2e.scenarios.sandbox._metrics import (
    aggregate_perf_metrics,
)
from live_e2e.squad.sandbox_probe import SandboxCheck


EmitStreamEvent = Callable[[StreamEvent], Awaitable[None]]
CallTool = Callable[..., Awaitable[ToolResult]]
PublishEvent = Callable[..., None]
RecordToolCheck = Callable[[str, ToolResult], None]


WORKSPACE_ROOT = "/ephemeral-os"
METRICS_PATH = f"{WORKSPACE_ROOT}/.metrics/perf.json"


@dataclass
class ProbeStats:
    """Counters tracked during a probe run for §7 assertions and metrics."""

    write_count: int = 0
    edit_count: int = 0
    read_count: int = 0
    shell_count: int = 0
    lsp_counts: dict[str, int] = field(default_factory=dict)
    api_read_count: int = 0
    api_edit_count: int = 0
    api_shell_count: int = 0
    intentional_conflicts: int = 0
    tool_call_metadata: list[dict[str, Any]] = field(default_factory=list)
    phases: list[dict[str, Any]] = field(default_factory=list)

    def edit_to_write_ratio(self) -> float:
        if self.write_count == 0:
            return float("inf") if self.edit_count else 0.0
        return self.edit_count / max(self.write_count, 1)


@dataclass
class ProbeContext:
    """Bundles the runner helpers the probe needs.

    The runner stays the source of truth for ``_call_tool`` semantics,
    ``_publish``, ``_record_tool_check``, and the sandbox caller — they're
    passed in here so this module can be pure orchestration without owning a
    runner reference.
    """

    metadata: ExecutionMetadata
    emit: EmitStreamEvent
    call_tool: CallTool
    publish: PublishEvent
    record_tool_check: RecordToolCheck
    caller: SandboxCaller
    sandbox_id: str
    sandbox_checks: list[SandboxCheck]
    smoke: bool


# ---------------------------------------------------------------------------
# Public entry point
# ---------------------------------------------------------------------------


async def run_complex_project_build_probe(
    *,
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
    publish: PublishEvent,
    record_tool_check: RecordToolCheck,
    caller: SandboxCaller,
    sandbox_id: str,
    sandbox_checks: list[SandboxCheck],
    smoke: bool,
) -> str:
    """Run the complex_project_build probe and return its summary path."""
    started_at = time.monotonic()
    ctx = ProbeContext(
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        publish=publish,
        record_tool_check=record_tool_check,
        caller=caller,
        sandbox_id=sandbox_id,
        sandbox_checks=sandbox_checks,
        smoke=smoke,
    )
    stats = ProbeStats()

    selected_files = _select_files(smoke)
    refactor_passes = _select_refactor_passes(smoke)

    await _phase0_bootstrap(ctx, stats)
    await _phase_a_skeleton(ctx, stats, selected_files)
    await _phase_b_apply_patches(
        ctx,
        stats,
        selected_files,
        section_name="phase_b_core",
    )
    selected_paths = frozenset(f.relative_path for f in selected_files)
    await _phase_d_refactor(ctx, stats, refactor_passes, selected_paths)
    amp_pairs = await _phase_d_edit_amplification(ctx, stats, selected_files)
    pytest_exit_code, pytest_stdout = await _phase_f_pytest(ctx, stats)
    await _phase_f_per_module_imports(ctx, stats, selected_files)
    await _phase_f_lsp_saturation(ctx, stats, selected_files)
    await _phase_f_tri_source_consistency(ctx, stats, selected_files)
    await _phase_f_intentional_conflicts(ctx, stats, selected_files)

    wall_seconds = time.monotonic() - started_at
    summary_path = await _phase_f_emit_metrics(
        ctx,
        stats,
        wall_seconds=wall_seconds,
        selected_files=selected_files,
        refactor_passes=refactor_passes,
        pytest_exit_code=pytest_exit_code,
        pytest_stdout=pytest_stdout,
        amp_pairs=amp_pairs,
    )

    return summary_path


# ---------------------------------------------------------------------------
# File selection
# ---------------------------------------------------------------------------


def _select_files(smoke: bool) -> tuple[FixtureFile, ...]:
    if not smoke:
        return SCHEDULER_DEMO_FILES
    selected = tuple(
        f for f in SCHEDULER_DEMO_FILES if f.relative_path in SMOKE_FILE_PATHS
    )
    return selected


def _select_refactor_passes(smoke: bool) -> tuple[RefactorPass, ...]:
    if smoke:
        return REFACTOR_PASSES[:1]
    return REFACTOR_PASSES


# ---------------------------------------------------------------------------
# Phase 0 — sandbox bootstrap (workspace rebind + git init)
# ---------------------------------------------------------------------------


async def _phase0_bootstrap(ctx: ProbeContext, stats: ProbeStats) -> None:
    phase_started = time.monotonic()

    # Ensure /ephemeral-os exists on disk before rebinding the workspace_root.
    # The daemon's workspace mount rejects any cwd that escapes the bound
    # workspace_root, and the binding may already be /ephemeral-os from a
    # previous attempt in the same session — so we try the SWE-EVO default
    # (/testbed) first, fall through to /ephemeral-os if that escapes.
    mkdir_result = None
    last_err = ""
    for candidate_cwd in ("/testbed", WORKSPACE_ROOT):
        try:
            candidate_result = await sandbox_api.shell(
                ctx.sandbox_id,
                ShellRequest(
                    command=f"mkdir -p {WORKSPACE_ROOT}",
                    cwd=candidate_cwd,
                    timeout=60,
                    caller=ctx.caller,
                    description=f"bootstrap api.shell mkdir cwd={candidate_cwd}",
                ),
            )
        except Exception as exc:
            last_err = f"cwd={candidate_cwd} error={type(exc).__name__}: {exc}"
            continue
        if candidate_result.success and candidate_result.exit_code == 0:
            stats.api_shell_count += 1
            mkdir_result = candidate_result
            break
        last_err = (
            f"cwd={candidate_cwd} exit={candidate_result.exit_code} "
            f"stderr={candidate_result.stderr!r}"
        )
    if mkdir_result is None:
        ctx.sandbox_checks.append(
            SandboxCheck(
                name="api.shell.bootstrap.mkdir_workspace",
                passed=False,
                detail=last_err,
            )
        )
        raise RuntimeError(f"mkdir {WORKSPACE_ROOT} failed: {last_err}")
    ctx.sandbox_checks.append(
        SandboxCheck(
            name="api.shell.bootstrap.mkdir_workspace",
            passed=True,
            detail=f"exit_code={mkdir_result.exit_code}",
        )
    )

    rebind = await call_daemon_api(
        ctx.sandbox_id,
        "api.build_workspace_base",
        {"workspace_root": WORKSPACE_ROOT, "reset": True},
        timeout=240,
    )
    ctx.sandbox_checks.append(
        SandboxCheck(
            name="api.build_workspace_base.ephemeral_os",
            passed=bool(rebind.get("success")),
            detail=f"workspace_root={WORKSPACE_ROOT}",
        )
    )
    if not rebind.get("success"):
        raise RuntimeError(
            f"workspace rebind to {WORKSPACE_ROOT} failed: {rebind!r}"
        )

    # Mutate metadata so subsequent toolkit calls (write_file, edit_file,
    # read_file, shell) default their cwd / repo_root to /ephemeral-os.
    ctx.metadata.repo_root = WORKSPACE_ROOT
    ctx.metadata.cwd = WORKSPACE_ROOT
    ctx.metadata.exec_cwd = WORKSPACE_ROOT

    install = await _shell(
        ctx,
        stats,
        command=(
            "set -e; if ! command -v git >/dev/null 2>&1; then "
            "(apt-get update -y && apt-get install -y --no-install-recommends git) "
            "|| (apk add --no-cache git) "
            "|| (yum install -y git) "
            "; fi; "
            "command -v git || { echo 'git unavailable'; exit 1; }"
        ),
        timeout=600,
    )
    if install.is_error:
        raise RuntimeError(f"git install failed: {install.output}")

    init = await _shell(
        ctx,
        stats,
        command=(
            f"set -e && cd {WORKSPACE_ROOT} && "
            f"git init -b main && "
            f"git config user.email 'mock@ephemeral-os.test' && "
            f"git config user.name 'Mock Agent'"
        ),
        timeout=120,
    )
    if init.is_error:
        raise RuntimeError(f"git init failed: {init.output}")

    await _write_file(
        ctx,
        stats,
        path=f"{WORKSPACE_ROOT}/.gitignore",
        content="__pycache__/\n*.pyc\n.pytest_cache/\n.metrics/\n",
    )
    await _read_file(
        ctx,
        stats,
        path=f"{WORKSPACE_ROOT}/.gitignore",
    )
    await _shell(
        ctx,
        stats,
        command=f"cd {WORKSPACE_ROOT} && git add .gitignore && git commit -m 'init'",
        timeout=120,
    )

    # Direct sandbox.api round-trip — counts toward saturation (§7.15–17).
    api_read = await sandbox_api.read_file(
        ctx.sandbox_id,
        ReadFileRequest(path=f"{WORKSPACE_ROOT}/.gitignore", caller=ctx.caller),
    )
    stats.api_read_count += 1
    ctx.sandbox_checks.append(
        SandboxCheck(
            name="api.read_file.bootstrap.gitignore",
            passed=bool(api_read.success and api_read.exists),
            detail=f"bytes={len(api_read.content) if api_read.success else 0}",
        )
    )

    api_shell = await sandbox_api.shell(
        ctx.sandbox_id,
        ShellRequest(
            command="test -d . && printf 'workspace-exists\\n'",
            cwd=WORKSPACE_ROOT,
            timeout=60,
            caller=ctx.caller,
            description="bootstrap api.shell workspace exists",
        ),
    )
    if api_shell.success and api_shell.exit_code == 0:
        stats.api_shell_count += 1
    ctx.sandbox_checks.append(
        SandboxCheck(
            name="api.shell.bootstrap.workspace_exists",
            passed=bool(api_shell.success and api_shell.exit_code == 0),
            detail=(
                f"exit_code={api_shell.exit_code} "
                f"stdout={api_shell.stdout!r} stderr={api_shell.stderr!r}"
            ),
        )
    )

    api_shell_status = await sandbox_api.shell(
        ctx.sandbox_id,
        ShellRequest(
            command="pwd",
            cwd=WORKSPACE_ROOT,
            timeout=60,
            caller=ctx.caller,
            description="bootstrap api.shell workspace cwd",
        ),
    )
    if api_shell_status.success and api_shell_status.exit_code == 0:
        stats.api_shell_count += 1
    ctx.sandbox_checks.append(
        SandboxCheck(
            name="api.shell.bootstrap.workspace_cwd",
            passed=bool(
                api_shell_status.success and api_shell_status.exit_code == 0
            ),
            detail=(
                f"exit_code={api_shell_status.exit_code} "
                f"stdout={api_shell_status.stdout!r} "
                f"stderr={api_shell_status.stderr!r}"
            ),
        )
    )

    stats.phases.append(
        {
            "name": "0_bootstrap",
            "duration_s": time.monotonic() - phase_started,
            "tool_calls_at_end": _total_calls(stats),
        }
    )


# ---------------------------------------------------------------------------
# Phase A — skeleton writes (write_file every fixture's skeleton form)
# ---------------------------------------------------------------------------


async def _phase_a_skeleton(
    ctx: ProbeContext,
    stats: ProbeStats,
    selected: Sequence[FixtureFile],
) -> None:
    phase_started = time.monotonic()

    # Build the directory tree first so write_file (which requires the parent
    # dir to exist) succeeds for every fixture.
    dirs = sorted({_dirname(f.relative_path) for f in selected if "/" in f.relative_path})
    if dirs:
        mkdirs = " ".join(f"{WORKSPACE_ROOT}/{d}" for d in dirs)
        await _shell(
            ctx,
            stats,
            command=f"mkdir -p {mkdirs}",
            timeout=60,
        )

    for fixture in selected:
        await _write_file(
            ctx,
            stats,
            path=f"{WORKSPACE_ROOT}/{fixture.relative_path}",
            content=fixture.skeleton,
        )
        # Read back every newly-written file to exercise the projection layer.
        await _read_file(
            ctx,
            stats,
            path=f"{WORKSPACE_ROOT}/{fixture.relative_path}",
        )

    # LSP diagnostics on a couple of __init__ files to warm the workspace
    # symbol index (§6.2). Only do this for .py files.
    py_files = [f for f in selected if f.relative_path.endswith(".py")]
    for fixture in py_files[: 4 if ctx.smoke else 8]:
        await _lsp(
            ctx,
            stats,
            tool_obj=lsp_diagnostics_tool,
            tool_name="lsp.diagnostics",
            args={"file_path": f"{WORKSPACE_ROOT}/{fixture.relative_path}"},
        )

    await _shell(
        ctx,
        stats,
        command=f"cd {WORKSPACE_ROOT} && git add -A && git commit -m 'skeleton'",
        timeout=180,
    )

    stats.phases.append(
        {
            "name": "A_skeleton",
            "duration_s": time.monotonic() - phase_started,
            "tool_calls_at_end": _total_calls(stats),
        }
    )


# ---------------------------------------------------------------------------
# Phases B/C/E — apply patches incrementally to build each file (edit_file
# heavy). We collapse §6.3, §6.4, and §6.6 into a single helper that walks
# every fixture's patch list, since the per-phase distinction is just file
# scope.
# ---------------------------------------------------------------------------


async def _phase_b_apply_patches(
    ctx: ProbeContext,
    stats: ProbeStats,
    selected: Sequence[FixtureFile],
    *,
    section_name: str,
) -> None:
    phase_started = time.monotonic()

    lsp_every_n = 6 if ctx.smoke else 3
    edit_index = 0

    for fixture in selected:
        if not fixture.patches:
            continue
        path = f"{WORKSPACE_ROOT}/{fixture.relative_path}"
        for patch_index, patch in enumerate(fixture.patches):
            await _edit_file(
                ctx,
                stats,
                path=path,
                old_text=patch.old_text,
                new_text=patch.new_text,
                description=patch.description,
            )
            edit_index += 1

            # Mix in LSP requests + read_file to drive the saturation targets.
            if edit_index % lsp_every_n == 0 and fixture.relative_path.endswith(".py"):
                await _lsp_saturation_round(ctx, stats, path)
                await _read_file(ctx, stats, path=path)

            # Once per file, send a batch sandbox.api.edit_file (a no-op
            # search/replace pair) to exercise the direct-API code path.
            if patch_index == 0 and fixture.relative_path.endswith(".py"):
                await _api_edit_noop_batch(ctx, stats, path)

        # Cross-check: tool.read_file vs sandbox.api.read_file content equality.
        await _projection_consistency_check(ctx, stats, path)

    # One git commit at the end of the phase block.
    await _shell(
        ctx,
        stats,
        command=f"cd {WORKSPACE_ROOT} && git add -A && git commit -m 'patches' || true",
        timeout=180,
    )

    stats.phases.append(
        {
            "name": section_name,
            "duration_s": time.monotonic() - phase_started,
            "tool_calls_at_end": _total_calls(stats),
        }
    )


async def _lsp_saturation_round(
    ctx: ProbeContext,
    stats: ProbeStats,
    path: str,
) -> None:
    """Issue one of each LSP tool, rotating through tools so all 5 hit the
    ≥30 floor (or ≥3 in smoke)."""
    rotation = (
        ("lsp.hover", lsp_hover_tool, {"file_path": path, "line": 1, "character": 0}),
        (
            "lsp.find_definitions",
            lsp_find_definitions_tool,
            {"file_path": path, "line": 1, "character": 0},
        ),
        (
            "lsp.find_references",
            lsp_find_references_tool,
            {
                "file_path": path,
                "line": 1,
                "character": 0,
                "include_declaration": False,
            },
        ),
        (
            "lsp.query_symbols",
            lsp_query_symbols_tool,
            {"query": "Task", "file_path": path},
        ),
        ("lsp.diagnostics", lsp_diagnostics_tool, {"file_path": path}),
    )
    for tool_name, tool_obj, args in rotation:
        await _lsp(ctx, stats, tool_obj=tool_obj, tool_name=tool_name, args=args)


async def _projection_consistency_check(
    ctx: ProbeContext,
    stats: ProbeStats,
    path: str,
) -> None:
    """For one path, ensure tool.read_file and sandbox.api.read_file see the
    same logical file. The toolkit ``read_file`` annotates each line with a
    1-based line number prefix (``{N:4d}: ``) — it is *not* byte-equal to the
    raw file content. We strip the prefix and compare line counts + the
    stripped text against the canonical sandbox.api.read_file content."""
    tool_read = await _read_file(ctx, stats, path=path)
    api_read = await sandbox_api.read_file(
        ctx.sandbox_id,
        ReadFileRequest(path=path, caller=ctx.caller),
    )
    stats.api_read_count += 1
    api_content = api_read.content if api_read.success else ""
    tool_stripped = _strip_line_number_prefix(tool_read)
    api_stripped = api_content.rstrip("\n")
    matches = bool(tool_stripped) and tool_stripped == api_stripped
    ctx.sandbox_checks.append(
        SandboxCheck(
            name=f"api.read_file.equal_to_tool.{_short(path)}",
            passed=matches,
            detail=(
                f"tool_lines={tool_stripped.count(chr(10)) + 1} "
                f"api_lines={api_stripped.count(chr(10)) + 1}"
            ),
        )
    )


async def _api_edit_noop_batch(
    ctx: ProbeContext,
    stats: ProbeStats,
    path: str,
) -> None:
    """Apply a no-op batch edit (write content unchanged) via sandbox.api."""
    api_read = await sandbox_api.read_file(
        ctx.sandbox_id,
        ReadFileRequest(path=path, caller=ctx.caller),
    )
    stats.api_read_count += 1
    if not api_read.success or not api_read.content:
        return
    snippet = api_read.content[:32]
    if len(snippet) < 4:
        return
    result = await sandbox_api.edit_file(
        ctx.sandbox_id,
        EditFileRequest(
            path=path,
            edits=(
                SearchReplaceEdit(old_text=snippet, new_text=snippet),
                SearchReplaceEdit(old_text=snippet[:8], new_text=snippet[:8]),
            ),
            caller=ctx.caller,
            description=f"complex_project_build api batch noop {_short(path)}",
        ),
    )
    stats.api_edit_count += 1
    ctx.sandbox_checks.append(
        SandboxCheck(
            name=f"api.edit_file.batch_noop.{_short(path)}",
            passed=bool(result.success and result.applied_edits >= 0),
            detail=f"applied_edits={result.applied_edits} status={result.status}",
        )
    )


# ---------------------------------------------------------------------------
# Phase D — sentinel-comment refactor passes (§6.5)
# ---------------------------------------------------------------------------


async def _phase_d_refactor(
    ctx: ProbeContext,
    stats: ProbeStats,
    passes: Sequence[RefactorPass],
    selected_paths: frozenset[str],
) -> None:
    if not passes:
        return
    phase_started = time.monotonic()

    for pass_ in passes:
        # Filter edits to files actually in the selected (smoke/full) set.
        applicable_edits = tuple(
            e for e in pass_.edits if e.relative_path in selected_paths
        )
        applicable_lsp = tuple(
            s for s in pass_.lsp_targets if s.relative_path in selected_paths
        )

        # Forward edits — insert sentinel comments.
        for edit in applicable_edits:
            target_path = f"{WORKSPACE_ROOT}/{edit.relative_path}"
            await _edit_file(
                ctx,
                stats,
                path=target_path,
                old_text=edit.anchor,
                new_text=edit.sentinel,
                description=f"{pass_.name} forward",
            )

        # LSP find_references on the renamed symbol.
        for spec in applicable_lsp:
            target_path = f"{WORKSPACE_ROOT}/{spec.relative_path}"
            line, character = await _resolve_anchor_position(
                ctx,
                stats,
                target_path,
                anchor=spec.line_index_anchor,
            )
            await _lsp(
                ctx,
                stats,
                tool_obj=lsp_find_references_tool,
                tool_name="lsp.find_references",
                args={
                    "file_path": target_path,
                    "line": line,
                    "character": character,
                    "include_declaration": False,
                },
            )

        # Revert edits — remove sentinel comments to restore parseable source.
        for edit in applicable_edits:
            target_path = f"{WORKSPACE_ROOT}/{edit.relative_path}"
            await _edit_file(
                ctx,
                stats,
                path=target_path,
                old_text=edit.sentinel,
                new_text=edit.anchor,
                description=f"{pass_.name} revert",
            )

        # Commit so each pass is a discrete log entry.
        await _shell(
            ctx,
            stats,
            command=(
                f"cd {WORKSPACE_ROOT} && git add -A && "
                f"git commit -m 'refactor: {pass_.name}' || true"
            ),
            timeout=180,
        )

    stats.phases.append(
        {
            "name": "D_refactor",
            "duration_s": time.monotonic() - phase_started,
            "tool_calls_at_end": _total_calls(stats),
        }
    )


def _compute_amp_pairs(
    selected_files: Sequence[FixtureFile],
    *,
    smoke: bool,
    edit_write_ratio_floor: float = 4.0,
    headroom: float = 1.25,
    phase1_full_floor: int = 30,
    phase1_smoke_floor: int = 6,
) -> int:
    """Size the Phase D' amplification so §13.6 (edit:write ≥4×) + §7.2
    (≥2000 toolkit calls for full) both hold without a magic constant.

    Returns the per-anchor ``pairs`` value (each pair = 2 ``edit_file`` calls
    — forward + revert) computed from the §13.6 ratio target plus a §7.2
    tool-call floor pad.

    ``phase1_full_floor`` / ``phase1_smoke_floor`` are regression anchors,
    not principled floors: they keep this from returning a *smaller* value
    than the Phase 1 magic constants (30 / 6) if the formula auto-scales
    down as the fixture set grows. ``floor_pairs`` from §7.2 already
    provides the principled lower bound; these anchors only kick in if the
    formula would otherwise regress call volume below Phase 1's known-good
    behaviour.
    """
    py_anchor_count = sum(
        1 for f in selected_files
        if f.relative_path.endswith(".py")
        and "from __future__ import annotations" in f.final
    )
    if py_anchor_count == 0:
        return 0
    write_count_est = len(selected_files) + 5
    existing_edits_est = sum(len(f.patches) for f in selected_files)
    target_edits = int(edit_write_ratio_floor * headroom * write_count_est)
    deficit = max(0, target_edits - existing_edits_est)
    pairs_from_ratio = (deficit + 2 * py_anchor_count - 1) // (2 * py_anchor_count)
    if not smoke:
        # baseline = observed non-amp toolkit call count from Phase 1 full
        # run (bootstrap + patches + refactor + F-phases ≈600); §7.2 floor 2000.
        baseline = 600
        floor_pairs = (2000 - baseline + 2 * py_anchor_count - 1) // (2 * py_anchor_count)
        return max(pairs_from_ratio, floor_pairs, phase1_full_floor)
    return max(pairs_from_ratio, phase1_smoke_floor)


async def _phase_d_edit_amplification(
    ctx: ProbeContext,
    stats: ProbeStats,
    selected: Sequence[FixtureFile],
) -> int:
    """Drive the edit:write ratio above the §13.6 ≥4× floor.

    Walks every fixture with at least one patch and applies ``pairs`` sentinel
    insert+revert pairs against ``from __future__ import annotations``. Each
    pair is two edit_file calls — one forward (insert ``# amp_N``), one
    revert (remove it) — so the file ends up byte-identical to its
    post-Phase-B form.

    Returns the computed ``pairs`` value so the caller can record it in the
    probe summary for visibility.
    """
    pairs = _compute_amp_pairs(selected, smoke=ctx.smoke)
    phase_started = time.monotonic()

    targets = [
        f for f in selected
        if f.relative_path.endswith(".py") and "from __future__ import annotations" in f.final
    ]
    if not targets:
        return pairs

    for fixture in targets:
        path = f"{WORKSPACE_ROOT}/{fixture.relative_path}"
        for index in range(pairs):
            sentinel = f"# amp_{fixture.relative_path.replace('/', '_')}_{index}\n"
            forward_old = "from __future__ import annotations\n"
            forward_new = sentinel + forward_old
            await _edit_file(
                ctx,
                stats,
                path=path,
                old_text=forward_old,
                new_text=forward_new,
                description=f"amp forward {fixture.relative_path}#{index}",
            )
            await _edit_file(
                ctx,
                stats,
                path=path,
                old_text=forward_new,
                new_text=forward_old,
                description=f"amp revert {fixture.relative_path}#{index}",
            )

    stats.phases.append(
        {
            "name": "D_amp",
            "duration_s": time.monotonic() - phase_started,
            "tool_calls_at_end": _total_calls(stats),
        }
    )
    return pairs


async def _resolve_anchor_position(
    ctx: ProbeContext,
    stats: ProbeStats,
    path: str,
    *,
    anchor: str,
) -> tuple[int, int]:
    api_read = await sandbox_api.read_file(
        ctx.sandbox_id,
        ReadFileRequest(path=path, caller=ctx.caller),
    )
    stats.api_read_count += 1
    if not api_read.success:
        return (1, 0)
    for line_index, line in enumerate(api_read.content.splitlines()):
        if anchor in line:
            character = max(line.find(anchor), 0)
            return (line_index, character)
    return (1, 0)


# ---------------------------------------------------------------------------
# Phase F — pytest, LSP saturation, tri-source consistency, conflicts, metrics
# ---------------------------------------------------------------------------


async def _phase_f_pytest(
    ctx: ProbeContext, stats: ProbeStats
) -> tuple[int, str]:
    phase_started = time.monotonic()

    pytest_path = f"{WORKSPACE_ROOT}/.metrics/pytest.xml"
    await _shell(
        ctx,
        stats,
        command=f"mkdir -p {WORKSPACE_ROOT}/.metrics",
        timeout=60,
    )
    # Prefer `python3` since some sandbox images don't symlink `python`.
    result = await _shell(
        ctx,
        stats,
        command=(
            f"cd {WORKSPACE_ROOT} && "
            f"PY=$(command -v python3 || command -v python) && "
            f"$PY -m pytest tests/ -v --tb=short --junit-xml={pytest_path}"
        ),
        timeout=600,
    )
    stdout = _shell_stdout(result)
    exit_code = _shell_exit_code(result)
    ctx.sandbox_checks.append(
        SandboxCheck(
            name="shell.pytest.full_run",
            passed=exit_code == 0,
            detail=f"exit_code={exit_code} bytes={len(stdout)}",
        )
    )

    stats.phases.append(
        {
            "name": "F_pytest",
            "duration_s": time.monotonic() - phase_started,
            "tool_calls_at_end": _total_calls(stats),
        }
    )
    return exit_code, stdout


def _importable_dotted_names(selected: Sequence[FixtureFile]) -> list[str]:
    """Return the dotted module names from ``selected`` that should be
    importable via ``python3 -c 'import <name>'``.

    Excludes package ``__init__`` files (importing them re-exercises the
    package init but doesn't add unique coverage), ``conftest.py``, anything
    under ``tests/`` (those are pytest-collected, not import-as-module), and
    project-root non-Python files like ``.gitignore`` / ``pyproject.toml``.
    """
    names: list[str] = []
    for fixture in selected:
        path = fixture.relative_path
        if not path.endswith(".py"):
            continue
        if path.endswith("/__init__.py") or path == "__init__.py":
            continue
        if path == "conftest.py" or path.startswith("tests/"):
            continue
        dotted = path[:-3].replace("/", ".")
        names.append(dotted)
    return names


async def _phase_f_per_module_imports(
    ctx: ProbeContext,
    stats: ProbeStats,
    selected: Sequence[FixtureFile],
) -> None:
    """Plan §7.7 — every source module must be importable on its own.

    pytest collection in :func:`_phase_f_pytest` covers this transitively,
    but the §7.7 contract is stated as an independent per-module probe so
    we run it explicitly. Each module produces a ``module.import.<dotted>``
    SandboxCheck which rolls up via ``passed_sandbox_checks``.
    """
    dotted_names = _importable_dotted_names(selected)
    if not dotted_names:
        return
    phase_started = time.monotonic()
    for dotted in dotted_names:
        result = await _shell(
            ctx,
            stats,
            command=f"cd {WORKSPACE_ROOT} && python3 -c 'import {dotted}'",
            timeout=30,
        )
        exit_code = _shell_exit_code(result)
        ctx.sandbox_checks.append(
            SandboxCheck(
                name=f"module.import.{dotted}",
                passed=exit_code == 0,
                detail=f"exit_code={exit_code}",
            )
        )
    stats.phases.append(
        {
            "name": "F_per_module_imports",
            "duration_s": time.monotonic() - phase_started,
            "tool_calls_at_end": _total_calls(stats),
        }
    )


async def _phase_f_lsp_saturation(
    ctx: ProbeContext,
    stats: ProbeStats,
    selected: Sequence[FixtureFile],
) -> None:
    """Top up LSP counts so each tool hits the ≥30 (full) / ≥3 (smoke) floor."""
    floor = 3 if ctx.smoke else 30
    py_files = [f for f in selected if f.relative_path.endswith(".py")]
    if not py_files:
        return
    rotation_index = 0
    while min(stats.lsp_counts.get(name, 0) for name in _LSP_NAMES) < floor:
        fixture = py_files[rotation_index % len(py_files)]
        path = f"{WORKSPACE_ROOT}/{fixture.relative_path}"
        await _lsp_saturation_round(ctx, stats, path)
        rotation_index += 1
        if rotation_index > 64:
            break  # safety net so we don't loop forever


async def _phase_f_tri_source_consistency(
    ctx: ProbeContext,
    stats: ProbeStats,
    selected: Sequence[FixtureFile],
) -> None:
    """For up to N files, assert tool.read_file == shell cat == sandbox.api."""
    targets = list(selected[: 5 if ctx.smoke else 12])
    for fixture in targets:
        path = f"{WORKSPACE_ROOT}/{fixture.relative_path}"
        tool_read = await _read_file(ctx, stats, path=path)
        tool_stripped = _strip_line_number_prefix(tool_read)

        cat = await _shell_cat_with_retry(ctx, stats, path=path)
        cat_stdout = _shell_stdout(cat).rstrip("\n")
        api_read = await sandbox_api.read_file(
            ctx.sandbox_id,
            ReadFileRequest(path=path, caller=ctx.caller),
        )
        stats.api_read_count += 1
        api_content = (api_read.content if api_read.success else "").rstrip("\n")

        # Byte-equality between cat (raw) and sandbox.api.read_file (raw); the
        # toolkit read_file is stripped of its line-number annotation and
        # compared against the same canonical content.
        passed = (
            bool(api_content)
            and api_content == cat_stdout
            and api_content == tool_stripped
        )
        ctx.sandbox_checks.append(
            SandboxCheck(
                name=f"projection.tri_source.{_short(path)}",
                passed=passed,
                detail=(
                    f"tool={len(tool_stripped)} cat={len(cat_stdout)} "
                    f"api={len(api_content)} cat_exit={_shell_exit_code(cat)}"
                ),
            )
        )


async def _shell_cat_with_retry(
    ctx: ProbeContext,
    stats: ProbeStats,
    *,
    path: str,
) -> ToolResult:
    result: ToolResult | None = None
    for attempt in range(_TRI_SOURCE_SHELL_RETRIES):
        result = await _shell(
            ctx,
            stats,
            command=f"cat {path}",
            timeout=60,
        )
        if (not result.is_error) and _shell_exit_code(result) == 0:
            return result
        if attempt + 1 < _TRI_SOURCE_SHELL_RETRIES:
            await asyncio.sleep(_TRI_SOURCE_SHELL_RETRY_SLEEP_S)
    assert result is not None
    return result


async def _phase_f_intentional_conflicts(
    ctx: ProbeContext,
    stats: ProbeStats,
    selected: Sequence[FixtureFile],
) -> None:
    """Trigger intentional missing-anchor conflicts via tool + sandbox.api."""
    target = next(
        (
            f
            for f in selected
            if f.relative_path.endswith(".py") and f.patches
        ),
        None,
    )
    if target is None:
        return
    path = f"{WORKSPACE_ROOT}/{target.relative_path}"

    tool_conflict = await _edit_file(
        ctx,
        stats,
        path=path,
        old_text="missing-anchor-text-XYZ\n",
        new_text="should-not-apply\n",
        description="intentional missing-anchor conflict (tool)",
        allow_error=True,
    )
    tool_meta = dict(tool_conflict.metadata or {})
    tool_reason = str(tool_meta.get("conflict_reason") or "")
    if not tool_reason:
        tool_reason = str(tool_conflict.output or "")
    tool_passed = bool(
        tool_conflict.is_error
        and tool_reason
        and "anchor not found" in tool_reason
    )
    ctx.sandbox_checks.append(
        SandboxCheck(
            name="tool.edit_file.intentional_conflict",
            passed=tool_passed,
            detail=f"reason={tool_reason!r}",
        )
    )
    if tool_passed:
        ctx.publish(
            EventType.SANDBOX_CONFLICT_DETECTED,
            metadata=ctx.metadata,
            payload={"conflict_reason": tool_reason},
        )
        stats.intentional_conflicts += 1

    stats.api_edit_count += 1
    api_conflict_reason = ""
    try:
        api_conflict = await sandbox_api.edit_file(
            ctx.sandbox_id,
            EditFileRequest(
                path=path,
                edits=(
                    SearchReplaceEdit(
                        old_text="missing-anchor-text-XYZ\n",
                        new_text="should-not-apply\n",
                    ),
                ),
                caller=ctx.caller,
                description="intentional missing-anchor conflict (api)",
            ),
        )
        api_passed = (not api_conflict.success) and bool(api_conflict.conflict_reason)
        api_conflict_reason = api_conflict.conflict_reason or ""
    except Exception as exc:
        api_conflict_reason = str(exc)
        api_passed = "anchor not found" in api_conflict_reason
    ctx.sandbox_checks.append(
        SandboxCheck(
            name="api.edit_file.intentional_conflict",
            passed=api_passed,
            detail=f"reason={api_conflict_reason!r}",
        )
    )
    if api_passed:
        ctx.publish(
            EventType.SANDBOX_CONFLICT_DETECTED,
            metadata=ctx.metadata,
            payload={"conflict_reason": api_conflict_reason or "api conflict"},
        )
        stats.intentional_conflicts += 1


async def _phase_f_emit_metrics(
    ctx: ProbeContext,
    stats: ProbeStats,
    *,
    wall_seconds: float,
    selected_files: Sequence[FixtureFile],
    refactor_passes: Sequence[RefactorPass],
    pytest_exit_code: int,
    pytest_stdout: str,
    amp_pairs: int,
) -> str:
    perf_payload = aggregate_perf_metrics(
        run_id=str(ctx.metadata.get("task_center_task_id") or ""),
        scenario=(
            "sandbox.complex_project_build_smoke"
            if ctx.smoke
            else "sandbox.complex_project_build"
        ),
        wall_seconds_total=wall_seconds,
        tool_call_metadata=stats.tool_call_metadata,
        phases=stats.phases,
        write_count=stats.write_count,
        edit_count=stats.edit_count,
        read_count=stats.read_count,
        shell_count=stats.shell_count,
        lsp_counts=stats.lsp_counts,
        api_read_count=stats.api_read_count,
        api_edit_count=stats.api_edit_count,
        api_shell_count=stats.api_shell_count,
        intentional_conflicts=stats.intentional_conflicts,
    )
    perf_text = json.dumps(perf_payload, indent=2, sort_keys=True) + "\n"
    await _write_file(
        ctx,
        stats,
        path=METRICS_PATH,
        content=perf_text,
    )
    await _read_file(ctx, stats, path=METRICS_PATH)

    summary = {
        "probe": "complex_project_build" + ("_smoke" if ctx.smoke else ""),
        "wall_seconds_total": wall_seconds,
        "selected_file_count": len(selected_files),
        "refactor_pass_count": len(refactor_passes),
        "amp_pairs": amp_pairs,
        "pytest_exit_code": pytest_exit_code,
        "pytest_stdout_tail": pytest_stdout[-2048:],
        "metrics_path": METRICS_PATH,
        "tool_use": {
            "write": stats.write_count,
            "edit": stats.edit_count,
            "read": stats.read_count,
            "shell": stats.shell_count,
            "lsp": dict(stats.lsp_counts),
            "edit_to_write_ratio": stats.edit_to_write_ratio(),
        },
        "api_calls": {
            "read": stats.api_read_count,
            "edit": stats.api_edit_count,
            "shell": stats.api_shell_count,
        },
        "intentional_conflicts": stats.intentional_conflicts,
    }
    summary_path = f"{WORKSPACE_ROOT}/.metrics/summary.json"
    await _write_file(
        ctx,
        stats,
        path=summary_path,
        content=json.dumps(summary, indent=2, sort_keys=True) + "\n",
    )
    return summary_path


# ---------------------------------------------------------------------------
# Tool wrappers (count + record_tool_check + capture metadata)
# ---------------------------------------------------------------------------


_LSP_NAMES = (
    "lsp.hover",
    "lsp.find_definitions",
    "lsp.find_references",
    "lsp.query_symbols",
    "lsp.diagnostics",
)
_TRI_SOURCE_SHELL_RETRIES = 3
_TRI_SOURCE_SHELL_RETRY_SLEEP_S = 0.5


async def _write_file(
    ctx: ProbeContext,
    stats: ProbeStats,
    *,
    path: str,
    content: str,
) -> ToolResult:
    result = await ctx.call_tool(
        write_file_tool,
        {"file_path": path, "content": content},
        ctx.metadata,
        ctx.emit,
    )
    stats.write_count += 1
    stats.tool_call_metadata.append(_capture_metadata("write_file", result))
    ctx.record_tool_check(f"tool.write_file.{_short(path)}", result)
    return result


async def _edit_file(
    ctx: ProbeContext,
    stats: ProbeStats,
    *,
    path: str,
    old_text: str,
    new_text: str,
    description: str,
    allow_error: bool = False,
) -> ToolResult:
    result = await ctx.call_tool(
        edit_file_tool,
        {
            "file_path": path,
            "old_text": old_text,
            "new_text": new_text,
            "description": description,
        },
        ctx.metadata,
        ctx.emit,
        allow_error=allow_error,
    )
    stats.edit_count += 1
    stats.tool_call_metadata.append(_capture_metadata("edit_file", result))
    if not allow_error:
        ctx.record_tool_check(f"tool.edit_file.{_short(path)}", result)
    return result


async def _read_file(
    ctx: ProbeContext,
    stats: ProbeStats,
    *,
    path: str,
) -> ToolResult:
    result = await ctx.call_tool(
        read_file_tool,
        {"file_path": path, "start_line": 1, "end_line": 200},
        ctx.metadata,
        ctx.emit,
    )
    stats.read_count += 1
    stats.tool_call_metadata.append(_capture_metadata("read_file", result))
    return result


async def _shell(
    ctx: ProbeContext,
    stats: ProbeStats,
    *,
    command: str,
    timeout: int = 60,
) -> ToolResult:
    """Run a toolkit shell command. cwd is controlled by ctx.metadata.repo_root
    (the toolkit shell tool uses ``get_repo_root(context)`` as cwd) — the
    probe rebinds workspace_root in Phase 0 and updates metadata.repo_root so
    subsequent shells run inside ``/ephemeral-os``."""
    args: dict[str, Any] = {"command": command, "timeout": timeout}
    result = await ctx.call_tool(
        shell_tool,
        args,
        ctx.metadata,
        ctx.emit,
        allow_error=True,
    )
    stats.shell_count += 1
    stats.tool_call_metadata.append(_capture_metadata("shell", result))
    return result


async def _lsp(
    ctx: ProbeContext,
    stats: ProbeStats,
    *,
    tool_obj: BaseTool,
    tool_name: str,
    args: dict[str, Any],
) -> ToolResult:
    result = await ctx.call_tool(
        tool_obj,
        args,
        ctx.metadata,
        ctx.emit,
        allow_error=True,
    )
    stats.lsp_counts[tool_name] = stats.lsp_counts.get(tool_name, 0) + 1
    stats.tool_call_metadata.append(_capture_metadata(tool_name, result))
    return result


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


_LINE_NUMBER_PREFIX_RE = re.compile(r"^ *\d+: ?")


def _strip_line_number_prefix(tool_read: ToolResult) -> str:
    """Remove the toolkit ``read_file`` line-number prefix from each line.

    The toolkit returns content of the form ``"   1: line\n   2: line\n..."``
    which is *not* byte-equal to the raw file. We parse the JSON, split on
    newlines, strip the prefix, and re-join.
    """
    try:
        payload = json.loads(tool_read.output)
    except (json.JSONDecodeError, TypeError):
        return ""
    raw = str(payload.get("content") or "")
    if not raw:
        return ""
    return "\n".join(
        _LINE_NUMBER_PREFIX_RE.sub("", line) for line in raw.split("\n")
    )


def _capture_metadata(tool_name: str, result: ToolResult) -> dict[str, Any]:
    return {
        "tool_name": tool_name,
        "is_error": bool(result.is_error),
        "metadata": dict(result.metadata or {}),
    }


def _shell_stdout(result: ToolResult) -> str:
    try:
        payload = json.loads(result.output)
    except (json.JSONDecodeError, TypeError):
        return str(result.output or "")
    return str(payload.get("stdout") or "")


def _shell_exit_code(result: ToolResult) -> int:
    try:
        payload = json.loads(result.output)
    except (json.JSONDecodeError, TypeError):
        return 0 if not result.is_error else 1
    raw = payload.get("exit_code")
    try:
        return int(raw)
    except (TypeError, ValueError):
        return 0 if not result.is_error else 1


def _dirname(rel_path: str) -> str:
    return rel_path.rsplit("/", 1)[0]


def _short(path: str) -> str:
    return path.replace(WORKSPACE_ROOT + "/", "").replace("/", "_")


def _total_calls(stats: ProbeStats) -> int:
    return (
        stats.write_count
        + stats.edit_count
        + stats.read_count
        + stats.shell_count
        + sum(stats.lsp_counts.values())
        + stats.api_read_count
        + stats.api_edit_count
        + stats.api_shell_count
    )


__all__ = [
    "METRICS_PATH",
    "WORKSPACE_ROOT",
    "run_complex_project_build_probe",
]
