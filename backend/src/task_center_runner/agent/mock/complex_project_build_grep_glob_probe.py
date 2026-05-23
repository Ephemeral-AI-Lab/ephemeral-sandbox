"""Probe for ``sandbox.complex_project_build_grep_glob``.

This sibling probe keeps the baseline scheduler_demo project-build shape but
turns search into the primary workload: every edit is bracketed by structured
``glob`` / ``grep`` calls, then the full variant amplifies the same
glob-grep-edit loop until the heavy tool-call floor is reached.
"""

from __future__ import annotations

import json
import re
import time
from collections.abc import Sequence
from dataclasses import dataclass, replace
from typing import Any

from tools._framework.core.results import ToolResult
from tools._framework.core.runtime import ExecutionMetadata
from tools.sandbox.glob import glob as glob_tool
from tools.sandbox.grep import grep as grep_tool

from task_center_runner.agent.mock.complex_project_build_probe import (
    CallTool,
    EmitStreamEvent,
    METRICS_PATH,
    ProbeContext,
    ProbeStats,
    PublishEvent,
    PublishMockRecord,
    RecordToolCheck,
    WORKSPACE_ROOT,
    _capture_metadata,
    _edit_file,
    _phase0_bootstrap,
    _phase_a_skeleton,
    _phase_d_refactor,
    _phase_f_intentional_conflicts,
    _phase_f_per_module_imports,
    _phase_f_pytest,
    _phase_f_tri_source_consistency,
    _projection_consistency_check,
    _read_file,
    _select_files,
    _select_refactor_passes,
    _shell,
    _write_file,
)
from task_center_runner.agent.mock.sandbox_probe import SandboxCheck
from task_center_runner.audit.events import EventType
from task_center_runner.scenarios.sandbox._fixtures.refactor_passes import RefactorPass
from task_center_runner.scenarios.sandbox._fixtures.scheduler_demo_data import (
    FixtureFile,
)
from task_center_runner.scenarios.sandbox._metrics import aggregate_perf_metrics


SUMMARY_PATH = f"{WORKSPACE_ROOT}/.metrics/summary.json"
_FULL_TOOL_CALL_FLOOR = 2000
_SMOKE_TOOL_CALL_FLOOR = 250
_AMP_ANCHOR = "from __future__ import annotations\n"
_GREP_MODES = ("files_with_matches", "count", "content")


@dataclass
class GrepGlobStats(ProbeStats):
    grep_count: int = 0
    glob_count: int = 0
    grep_matches: int = 0
    glob_matches: int = 0
    search_checks: int = 0
    search_failures: int = 0
    negative_grep_checks: int = 0
    grep_mode_counts: dict[str, int] | None = None

    def mode_counts(self) -> dict[str, int]:
        if self.grep_mode_counts is None:
            self.grep_mode_counts = {}
        return self.grep_mode_counts


async def run_complex_project_build_grep_glob_probe(
    *,
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
    publish: PublishEvent,
    publish_mock_record: PublishMockRecord,
    record_tool_check: RecordToolCheck,
    caller,
    sandbox_id: str,
    smoke: bool,
) -> str:
    """Run the heavy grep/glob/edit probe and return summary.json path."""
    started_at = time.monotonic()
    ctx = ProbeContext(
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        publish=publish,
        publish_mock_record=publish_mock_record,
        record_tool_check=record_tool_check,
        caller=caller,
        sandbox_id=sandbox_id,
        smoke=smoke,
    )
    stats = GrepGlobStats()
    selected_files = _with_searchable_init_stubs(_select_files(smoke))
    refactor_passes = _select_refactor_passes(smoke)

    await _phase0_bootstrap(ctx, stats)
    await _phase_a_skeleton(ctx, stats, selected_files)
    await _phase_b_grep_glob_patches(ctx, stats, selected_files)
    selected_paths = frozenset(f.relative_path for f in selected_files)
    await _phase_d_refactor(ctx, stats, refactor_passes, selected_paths)
    amp_pairs = await _phase_e_grep_glob_amplification(ctx, stats, selected_files)
    pytest_exit_code, pytest_stdout = await _phase_f_pytest(ctx, stats)
    await _phase_f_per_module_imports(ctx, stats, selected_files)
    await _phase_f_search_sweep(ctx, stats, selected_files)
    await _phase_f_tri_source_consistency(ctx, stats, selected_files)
    await _phase_f_intentional_conflicts(ctx, stats, selected_files)

    wall_seconds = time.monotonic() - started_at
    return await _phase_f_emit_metrics(
        ctx,
        stats,
        wall_seconds=wall_seconds,
        selected_files=selected_files,
        refactor_passes=refactor_passes,
        pytest_exit_code=pytest_exit_code,
        pytest_stdout=pytest_stdout,
        amp_pairs=amp_pairs,
    )


async def _phase_b_grep_glob_patches(
    ctx: ProbeContext,
    stats: GrepGlobStats,
    selected: Sequence[FixtureFile],
) -> None:
    phase_started = time.monotonic()
    for fixture in selected:
        path = f"{WORKSPACE_ROOT}/{fixture.relative_path}"
        await _assert_glob_contains(
            ctx,
            stats,
            pattern=fixture.relative_path,
            expected=fixture.relative_path,
            label=f"phase_b.inventory.{fixture.relative_path}",
        )
        for patch_index, patch in enumerate(fixture.patches):
            await _assert_grep_contains(
                ctx,
                stats,
                pattern=_literal_pattern(patch.old_text),
                path=path,
                expected=fixture.relative_path,
                label=f"phase_b.before.{fixture.relative_path}.{patch_index}",
                mode=_mode_for_text(patch.old_text, patch_index),
            )
            await _edit_file(
                ctx,
                stats,
                path=path,
                old_text=patch.old_text,
                new_text=patch.new_text,
                description=f"grep/glob patch {patch.description}",
            )
            await _assert_grep_contains(
                ctx,
                stats,
                pattern=_literal_pattern(patch.new_text),
                path=path,
                expected=fixture.relative_path,
                label=f"phase_b.after.{fixture.relative_path}.{patch_index}",
                mode=_mode_for_text(patch.new_text, patch_index + 1),
            )
            if patch_index % 2 == 0:
                await _assert_glob_contains(
                    ctx,
                    stats,
                    pattern=_module_family_pattern(fixture.relative_path),
                    expected=fixture.relative_path,
                    label=f"phase_b.family.{fixture.relative_path}.{patch_index}",
                )
        await _projection_consistency_check(ctx, stats, path)

    await _shell_phase_checkpoint(ctx, stats, "grep_glob_patches")
    stats.phases.append(
        {
            "name": "B_grep_glob_patches",
            "duration_s": time.monotonic() - phase_started,
            "tool_calls_at_end": _total_probe_calls(stats),
        }
    )


async def _shell_phase_checkpoint(
    ctx: ProbeContext,
    stats: GrepGlobStats,
    phase: str,
) -> None:
    await _shell(
        ctx,
        stats,
        command=f"printf 'phase=%s\\n' {phase!r}",
        timeout=180,
    )


async def _phase_e_grep_glob_amplification(
    ctx: ProbeContext,
    stats: GrepGlobStats,
    selected: Sequence[FixtureFile],
) -> int:
    phase_started = time.monotonic()
    targets = [
        fixture
        for fixture in selected
        if fixture.relative_path.endswith(".py") and _AMP_ANCHOR in fixture.final
    ]
    pairs = _compute_amp_pairs(stats, targets, smoke=ctx.smoke)
    for fixture in targets:
        path = f"{WORKSPACE_ROOT}/{fixture.relative_path}"
        for index in range(pairs):
            sentinel = f"# grep_glob_amp_{fixture.relative_path.replace('/', '_')}_{index}\n"
            inserted = sentinel + _AMP_ANCHOR
            await _assert_glob_contains(
                ctx,
                stats,
                pattern=fixture.relative_path,
                expected=fixture.relative_path,
                label=f"amp.glob.{fixture.relative_path}.{index}",
            )
            await _assert_grep_contains(
                ctx,
                stats,
                pattern=_literal_pattern(_AMP_ANCHOR),
                path=path,
                expected=fixture.relative_path,
                label=f"amp.before.{fixture.relative_path}.{index}",
            )
            await _edit_file(
                ctx,
                stats,
                path=path,
                old_text=_AMP_ANCHOR,
                new_text=inserted,
                description=f"grep/glob amp insert {fixture.relative_path}#{index}",
            )
            await _assert_grep_contains(
                ctx,
                stats,
                pattern=_literal_pattern(sentinel.strip()),
                path=path,
                expected=fixture.relative_path,
                label=f"amp.inserted.{fixture.relative_path}.{index}",
            )
            await _edit_file(
                ctx,
                stats,
                path=path,
                old_text=inserted,
                new_text=_AMP_ANCHOR,
                description=f"grep/glob amp revert {fixture.relative_path}#{index}",
            )
            await _assert_grep_absent(
                ctx,
                stats,
                pattern=_literal_pattern(sentinel.strip()),
                path=path,
                label=f"amp.reverted.{fixture.relative_path}.{index}",
            )

    stats.phases.append(
        {
            "name": "E_grep_glob_amp",
            "duration_s": time.monotonic() - phase_started,
            "tool_calls_at_end": _total_probe_calls(stats),
            "amp_pairs": pairs,
        }
    )
    return pairs


async def _phase_f_search_sweep(
    ctx: ProbeContext,
    stats: GrepGlobStats,
    selected: Sequence[FixtureFile],
) -> None:
    phase_started = time.monotonic()
    await _assert_glob_contains(
        ctx,
        stats,
        pattern="*.py",
        expected=next(f.relative_path for f in selected if f.relative_path.endswith(".py")),
        label="phase_f.all_python",
    )
    for index, fixture in enumerate(selected):
        if not fixture.relative_path.endswith(".py"):
            continue
        path = f"{WORKSPACE_ROOT}/{fixture.relative_path}"
        line_pattern = _fixture_line_pattern(fixture)
        if line_pattern is not None:
            await _assert_grep_contains(
                ctx,
                stats,
                pattern=line_pattern,
                path=path,
                expected=fixture.relative_path,
                label=f"phase_f.package_name.{fixture.relative_path}",
                mode=_mode_for_index(index),
            )
        await _assert_glob_contains(
            ctx,
            stats,
            pattern=fixture.relative_path,
            expected=fixture.relative_path,
            label=f"phase_f.exact.{fixture.relative_path}",
        )
    stats.phases.append(
        {
            "name": "F_search_sweep",
            "duration_s": time.monotonic() - phase_started,
            "tool_calls_at_end": _total_probe_calls(stats),
        }
    )


async def _phase_f_emit_metrics(
    ctx: ProbeContext,
    stats: GrepGlobStats,
    *,
    wall_seconds: float,
    selected_files: Sequence[FixtureFile],
    refactor_passes: Sequence[RefactorPass],
    pytest_exit_code: int,
    pytest_stdout: str,
    amp_pairs: int,
) -> str:
    scenario = (
        "sandbox.complex_project_build_grep_glob_smoke"
        if ctx.smoke
        else "sandbox.complex_project_build_grep_glob"
    )
    perf_payload = aggregate_perf_metrics(
        run_id=str(ctx.metadata.get("task_center_task_id") or ""),
        scenario=scenario,
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
    perf_payload["tool_use"]["total_calls"] = _toolkit_calls(stats)
    perf_payload["grep_glob"] = _grep_glob_metrics(stats)
    await _write_file(
        ctx,
        stats,
        path=METRICS_PATH,
        content=json.dumps(perf_payload, indent=2, sort_keys=True) + "\n",
    )
    await _read_file(ctx, stats, path=METRICS_PATH)

    summary = {
        "probe": "complex_project_build_grep_glob" + ("_smoke" if ctx.smoke else ""),
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
            "grep": stats.grep_count,
            "glob": stats.glob_count,
            "lsp": dict(stats.lsp_counts),
            "toolkit_total": _toolkit_calls(stats),
        },
        "api_calls": {
            "read": stats.api_read_count,
            "edit": stats.api_edit_count,
            "shell": stats.api_shell_count,
        },
        "grep_glob": _grep_glob_metrics(stats),
        "intentional_conflicts": stats.intentional_conflicts,
    }
    await _write_file(
        ctx,
        stats,
        path=SUMMARY_PATH,
        content=json.dumps(summary, indent=2, sort_keys=True) + "\n",
    )
    return SUMMARY_PATH


async def _grep(
    ctx: ProbeContext,
    stats: GrepGlobStats,
    *,
    pattern: str,
    path: str | None = None,
    glob_filter: str | None = None,
    output_mode: str = "files_with_matches",
    head_limit: int = 25,
    line_numbers: bool = False,
    multiline: bool = True,
) -> ToolResult:
    result = await ctx.call_tool(
        grep_tool,
        {
            "pattern": pattern,
            "path": path,
            "glob_filter": glob_filter,
            "output_mode": output_mode,
            "head_limit": head_limit,
            "line_numbers": line_numbers,
            "multiline": multiline,
        },
        ctx.metadata,
        ctx.emit,
    )
    stats.grep_count += 1
    stats.mode_counts()[output_mode] = stats.mode_counts().get(output_mode, 0) + 1
    stats.tool_call_metadata.append(_capture_metadata("grep", result))
    ctx.record_tool_check(f"tool.grep.{_safe_label(pattern)}", result)
    payload = _tool_json(result)
    stats.grep_matches += int(payload.get("num_matches") or 0)
    return result


async def _glob(
    ctx: ProbeContext,
    stats: GrepGlobStats,
    *,
    pattern: str,
    path: str | None = None,
) -> ToolResult:
    result = await ctx.call_tool(
        glob_tool,
        {"pattern": pattern, "path": path},
        ctx.metadata,
        ctx.emit,
    )
    stats.glob_count += 1
    stats.tool_call_metadata.append(_capture_metadata("glob", result))
    ctx.record_tool_check(f"tool.glob.{_safe_label(pattern)}", result)
    payload = _tool_json(result)
    stats.glob_matches += int(payload.get("num_files") or 0)
    return result


async def _assert_grep_contains(
    ctx: ProbeContext,
    stats: GrepGlobStats,
    *,
    pattern: str,
    path: str,
    expected: str,
    label: str,
    mode: str = "files_with_matches",
) -> None:
    result = await _grep(
        ctx,
        stats,
        pattern=pattern,
        path=path,
        output_mode=mode,
        line_numbers=mode == "content",
    )
    payload = _tool_json(result)
    filenames = {str(item) for item in payload.get("filenames") or ()}
    content = str(payload.get("content") or "")
    passed = bool(
        (not result.is_error)
        and (
            expected in filenames
            or any(name.endswith("/" + expected) for name in filenames)
            or expected in content
        )
    )
    _record_search_check(
        ctx,
        stats,
        name=f"grep.contains.{label}",
        passed=passed,
        detail=f"expected={expected} mode={mode} files={sorted(filenames)[:5]}",
    )


async def _assert_grep_absent(
    ctx: ProbeContext,
    stats: GrepGlobStats,
    *,
    pattern: str,
    path: str,
    label: str,
) -> None:
    result = await _grep(ctx, stats, pattern=pattern, path=path)
    payload = _tool_json(result)
    filenames = tuple(str(item) for item in payload.get("filenames") or ())
    passed = bool((not result.is_error) and not filenames)
    if passed:
        stats.negative_grep_checks += 1
    _record_search_check(
        ctx,
        stats,
        name=f"grep.absent.{label}",
        passed=passed,
        detail=f"files={filenames[:5]}",
    )


async def _assert_glob_contains(
    ctx: ProbeContext,
    stats: GrepGlobStats,
    *,
    pattern: str,
    expected: str,
    label: str,
) -> None:
    result = await _glob(ctx, stats, pattern=pattern)
    payload = _tool_json(result)
    filenames = {str(item) for item in payload.get("filenames") or ()}
    passed = bool(
        (not result.is_error)
        and (
            expected in filenames
            or any(name.endswith("/" + expected) for name in filenames)
        )
    )
    _record_search_check(
        ctx,
        stats,
        name=f"glob.contains.{label}",
        passed=passed,
        detail=f"expected={expected} files={sorted(filenames)[:8]}",
    )


def _record_search_check(
    ctx: ProbeContext,
    stats: GrepGlobStats,
    *,
    name: str,
    passed: bool,
    detail: str,
) -> None:
    stats.search_checks += 1
    if not passed:
        stats.search_failures += 1
    check = SandboxCheck(name=name, passed=passed, detail=detail)
    ctx.publish_mock_record(EventType.MOCK_SANDBOX_CHECK_RECORDED, check)
    if not passed:
        raise RuntimeError(f"search check failed: {name}: {detail}")


def _compute_amp_pairs(
    stats: GrepGlobStats,
    targets: Sequence[FixtureFile],
    *,
    smoke: bool,
) -> int:
    if not targets:
        return 0
    floor = _SMOKE_TOOL_CALL_FLOOR if smoke else _FULL_TOOL_CALL_FLOOR
    calls_per_pair = 6
    deficit = max(0, floor - _toolkit_calls(stats))
    pairs = (deficit + calls_per_pair * len(targets) - 1) // (
        calls_per_pair * len(targets)
    )
    return max(pairs, 2 if smoke else 1)


def _grep_glob_metrics(stats: GrepGlobStats) -> dict[str, Any]:
    return {
        "grep_count": stats.grep_count,
        "glob_count": stats.glob_count,
        "grep_matches": stats.grep_matches,
        "glob_matches": stats.glob_matches,
        "search_checks": stats.search_checks,
        "search_failures": stats.search_failures,
        "negative_grep_checks": stats.negative_grep_checks,
        "grep_modes": dict(stats.mode_counts()),
    }


def _toolkit_calls(stats: GrepGlobStats) -> int:
    return (
        stats.write_count
        + stats.edit_count
        + stats.read_count
        + stats.shell_count
        + sum(stats.lsp_counts.values())
        + stats.grep_count
        + stats.glob_count
    )


def _total_probe_calls(stats: GrepGlobStats) -> int:
    return _toolkit_calls(stats) + stats.api_read_count + stats.api_edit_count + stats.api_shell_count


def _mode_for_index(index: int) -> str:
    return _GREP_MODES[index % len(_GREP_MODES)]


def _mode_for_text(text: str, index: int) -> str:
    mode = _mode_for_index(index)
    if mode == "content" and "\n" in text:
        return "files_with_matches"
    return mode


def _literal_pattern(text: str) -> str:
    return re.escape(text)


def _fixture_line_pattern(fixture: FixtureFile) -> str | None:
    for line in fixture.final.splitlines():
        if line.strip():
            return _literal_pattern(line)
    return None


def _with_searchable_init_stubs(
    selected: Sequence[FixtureFile],
) -> tuple[FixtureFile, ...]:
    normalized: list[FixtureFile] = []
    for fixture in selected:
        if fixture.is_init and not fixture.final.strip():
            content = f'"""Package marker for {fixture.relative_path}."""\n'
            normalized.append(replace(fixture, final=content, skeleton=content))
        else:
            normalized.append(fixture)
    return tuple(normalized)


def _module_family_pattern(relative_path: str) -> str:
    if "/" not in relative_path:
        return relative_path
    directory = relative_path.rsplit("/", 1)[0]
    return f"{directory}/*"


def _tool_json(result: ToolResult) -> dict[str, Any]:
    try:
        payload = json.loads(result.output)
    except (TypeError, json.JSONDecodeError):
        return {}
    return payload if isinstance(payload, dict) else {}


def _safe_label(text: str) -> str:
    compact = re.sub(r"[^A-Za-z0-9_.-]+", "_", text.strip())[:60]
    return compact or "empty"


__all__ = [
    "METRICS_PATH",
    "SUMMARY_PATH",
    "WORKSPACE_ROOT",
    "run_complex_project_build_grep_glob_probe",
]
