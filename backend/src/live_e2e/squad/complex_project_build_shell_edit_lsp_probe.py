"""Probe for ``sandbox.complex_project_build_shell_edit_lsp``.

This sibling probe reuses the scheduler_demo fixtures and most of the baseline
complex project-build phases, but routes logical edits through a deterministic
mixed path: two ``edit_file`` calls, then one shell-based Python replacement.
It also counts only semantic LSP calls as LSP correctness checks.
"""

from __future__ import annotations

import asyncio
import json
import time
from collections.abc import Sequence
from dataclasses import dataclass, field
from typing import Any

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
from sandbox.api import ReadFileRequest
from tools._framework.core.base import BaseTool
from tools._framework.core.results import ToolResult
from tools._framework.core.runtime import ExecutionMetadata

import sandbox.api as sandbox_api

from live_e2e.scenarios.sandbox._fixtures.lsp_expectations import (
    LSP_EXPECTATIONS,
    LspExpectation,
)
from live_e2e.scenarios.sandbox._fixtures.refactor_passes import RefactorPass
from live_e2e.scenarios.sandbox._fixtures.scheduler_demo_data import FixtureFile
from live_e2e.scenarios.sandbox._metrics import aggregate_perf_metrics
from live_e2e.squad.complex_project_build_probe import (
    CallTool,
    EmitStreamEvent,
    METRICS_PATH,
    ProbeContext,
    ProbeStats,
    PublishEvent,
    RecordToolCheck,
    WORKSPACE_ROOT,
    _api_edit_noop_batch,
    _capture_metadata,
    _edit_file,
    _lsp,
    _phase0_bootstrap,
    _phase_a_skeleton,
    _phase_f_intentional_conflicts,
    _phase_f_per_module_imports,
    _phase_f_pytest,
    _phase_f_tri_source_consistency,
    _projection_consistency_check,
    _read_file,
    _select_files,
    _select_refactor_passes,
    _shell,
    _shell_exit_code,
    _shell_stdout,
    _short,
    _total_calls,
    _write_file,
)
from live_e2e.squad.sandbox_probe import SandboxCheck


_LSP_NAMES = (
    "lsp.hover",
    "lsp.find_definitions",
    "lsp.find_references",
    "lsp.query_symbols",
    "lsp.diagnostics",
)
_ROUTING_RULE = "logical_edit_index % 3 == 2"
_SUMMARY_PATH = f"{WORKSPACE_ROOT}/.metrics/summary.json"
_DIAGNOSTIC_PROBE_PATH = f"{WORKSPACE_ROOT}/scheduler_demo/_lsp_error_probe.py"
_DIAGNOSTIC_NONBLOCKING_RETRIES = 12
_DIAGNOSTIC_NONBLOCKING_RETRY_SLEEP_S = 0.15


@dataclass
class ShellEditLspStats(ProbeStats):
    logical_edit_count: int = 0
    edit_file_edit_count: int = 0
    shell_edit_count: int = 0
    shell_edit_errors: int = 0
    shell_edit_payloads: list[dict[str, Any]] = field(default_factory=list)
    shell_edit_tool_metadata: list[dict[str, Any]] = field(default_factory=list)
    shell_edit_wall_seconds: list[float] = field(default_factory=list)
    lsp_semantic_checks: dict[str, int] = field(default_factory=dict)
    lsp_semantic_failures: int = 0
    diagnostic_error_detected: bool = False
    diagnostic_repair_cleared: bool = False
    diagnostic_probe_checks: int = 0

    def shell_edit_ratio(self) -> float:
        if self.logical_edit_count == 0:
            return 0.0
        return self.shell_edit_count / self.logical_edit_count

    def total_lsp_semantic_checks(self) -> int:
        return sum(self.lsp_semantic_checks.values())


async def run_complex_project_build_shell_edit_lsp_probe(
    *,
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
    publish: PublishEvent,
    record_tool_check: RecordToolCheck,
    caller,
    sandbox_id: str,
    sandbox_checks: list[SandboxCheck],
    smoke: bool,
) -> str:
    """Run the mixed shell-edit + LSP probe and return summary.json path."""
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
    stats = ShellEditLspStats()
    selected_files = _select_files(smoke)
    refactor_passes = _select_refactor_passes(smoke)
    expectations = _select_lsp_expectations(selected_files)

    await _phase0_bootstrap(ctx, stats)
    await _phase_a_skeleton(ctx, stats, selected_files)
    await _phase_b_mixed_patches(ctx, stats, selected_files, expectations)
    selected_paths = frozenset(f.relative_path for f in selected_files)
    await _phase_d_mixed_refactor(
        ctx,
        stats,
        refactor_passes,
        selected_paths,
        expectations,
    )
    amp_pairs = await _phase_d_mixed_amplification(
        ctx,
        stats,
        selected_files,
        refactor_passes,
        expectations,
    )
    await _phase_e_diagnostic_probe(ctx, stats, expectations)
    pytest_exit_code, pytest_stdout = await _phase_f_pytest(ctx, stats)
    await _phase_f_per_module_imports(ctx, stats, selected_files)
    await _phase_f_tri_source_consistency(ctx, stats, selected_files)
    await _phase_f_intentional_conflicts(ctx, stats, selected_files)
    await _phase_f_semantic_lsp_sweep(ctx, stats, expectations)

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


def _select_lsp_expectations(
    selected_files: Sequence[FixtureFile],
) -> tuple[LspExpectation, ...]:
    selected_paths = {fixture.relative_path for fixture in selected_files}
    return tuple(
        expectation
        for expectation in LSP_EXPECTATIONS
        if expectation.source_path in selected_paths
        and expectation.definition_path in selected_paths
    )


def _compute_mixed_amp_pairs(
    selected_files: Sequence[FixtureFile],
    refactor_passes: Sequence[RefactorPass],
    *,
    smoke: bool,
) -> int:
    """Size amplification to satisfy the mixed scenario logical edit floors."""
    py_anchor_count = sum(
        1
        for fixture in selected_files
        if fixture.relative_path.endswith(".py")
        and "from __future__ import annotations" in fixture.final
    )
    if py_anchor_count == 0:
        return 0
    selected_paths = {fixture.relative_path for fixture in selected_files}
    patch_count = sum(len(fixture.patches) for fixture in selected_files)
    refactor_edit_count = 2 * sum(
        1
        for pass_ in refactor_passes
        for edit in pass_.edits
        if edit.relative_path in selected_paths
    )
    floor = 90 if smoke else 600
    deficit = max(0, floor - patch_count - refactor_edit_count)
    return (deficit + (2 * py_anchor_count) - 1) // (2 * py_anchor_count)


async def _phase_b_mixed_patches(
    ctx: ProbeContext,
    stats: ShellEditLspStats,
    selected: Sequence[FixtureFile],
    expectations: Sequence[LspExpectation],
) -> None:
    phase_started = time.monotonic()
    for fixture in selected:
        if not fixture.patches:
            continue
        path = f"{WORKSPACE_ROOT}/{fixture.relative_path}"
        for patch_index, patch in enumerate(fixture.patches):
            await _apply_logical_edit(
                ctx,
                stats,
                path=path,
                old_text=patch.old_text,
                new_text=patch.new_text,
                description=patch.description,
                expectations=expectations,
            )
            if patch_index == 0 and fixture.relative_path.endswith(".py"):
                await _api_edit_noop_batch(ctx, stats, path)
        await _projection_consistency_check(ctx, stats, path)

    await _shell(
        ctx,
        stats,
        command=f"cd {WORKSPACE_ROOT} && git status --short | wc -l",
        timeout=180,
    )
    stats.phases.append(
        {
            "name": "B_mixed_patches",
            "duration_s": time.monotonic() - phase_started,
            "tool_calls_at_end": _total_calls(stats),
        }
    )


async def _phase_d_mixed_refactor(
    ctx: ProbeContext,
    stats: ShellEditLspStats,
    passes: Sequence[RefactorPass],
    selected_paths: frozenset[str],
    expectations: Sequence[LspExpectation],
) -> None:
    if not passes:
        return
    phase_started = time.monotonic()

    for pass_ in passes:
        applicable_edits = tuple(
            edit for edit in pass_.edits if edit.relative_path in selected_paths
        )
        applicable_lsp = tuple(
            spec for spec in pass_.lsp_targets if spec.relative_path in selected_paths
        )

        for edit in applicable_edits:
            target_path = f"{WORKSPACE_ROOT}/{edit.relative_path}"
            await _apply_logical_edit(
                ctx,
                stats,
                path=target_path,
                old_text=edit.anchor,
                new_text=edit.sentinel,
                description=f"{pass_.name} forward",
                expectations=expectations,
            )

        for spec in applicable_lsp:
            await _assert_lsp_references_for_anchor(
                ctx,
                stats,
                path=f"{WORKSPACE_ROOT}/{spec.relative_path}",
                symbol=spec.symbol or pass_.target_symbol,
                anchor=spec.line_index_anchor,
                label=f"{pass_.name}.forward",
            )

        for edit in applicable_edits:
            target_path = f"{WORKSPACE_ROOT}/{edit.relative_path}"
            await _apply_logical_edit(
                ctx,
                stats,
                path=target_path,
                old_text=edit.sentinel,
                new_text=edit.anchor,
                description=f"{pass_.name} revert",
                expectations=expectations,
            )

        for spec in applicable_lsp:
            await _assert_lsp_diagnostics(
                ctx,
                stats,
                rel_path=spec.relative_path,
                expect_clean=True,
                label=f"{pass_.name}.clean",
            )

        await _shell(
            ctx,
            stats,
            command=f"cd {WORKSPACE_ROOT} && git status --short | wc -l",
            timeout=180,
        )

    stats.phases.append(
        {
            "name": "D_mixed_refactor",
            "duration_s": time.monotonic() - phase_started,
            "tool_calls_at_end": _total_calls(stats),
        }
    )


async def _phase_d_mixed_amplification(
    ctx: ProbeContext,
    stats: ShellEditLspStats,
    selected: Sequence[FixtureFile],
    refactor_passes: Sequence[RefactorPass],
    expectations: Sequence[LspExpectation],
) -> int:
    pairs = _compute_mixed_amp_pairs(selected, refactor_passes, smoke=ctx.smoke)
    phase_started = time.monotonic()
    targets = [
        fixture
        for fixture in selected
        if fixture.relative_path.endswith(".py")
        and "from __future__ import annotations" in fixture.final
    ]
    for fixture in targets:
        path = f"{WORKSPACE_ROOT}/{fixture.relative_path}"
        for index in range(pairs):
            sentinel = f"# mixed_amp_{fixture.relative_path.replace('/', '_')}_{index}\n"
            forward_old = "from __future__ import annotations\n"
            forward_new = sentinel + forward_old
            await _apply_logical_edit(
                ctx,
                stats,
                path=path,
                old_text=forward_old,
                new_text=forward_new,
                description=f"mixed amp forward {fixture.relative_path}#{index}",
                expectations=expectations,
            )
            await _apply_logical_edit(
                ctx,
                stats,
                path=path,
                old_text=forward_new,
                new_text=forward_old,
                description=f"mixed amp revert {fixture.relative_path}#{index}",
                expectations=expectations,
            )

    stats.phases.append(
        {
            "name": "D_mixed_amp",
            "duration_s": time.monotonic() - phase_started,
            "tool_calls_at_end": _total_calls(stats),
        }
    )
    return pairs


async def _phase_e_diagnostic_probe(
    ctx: ProbeContext,
    stats: ShellEditLspStats,
    expectations: Sequence[LspExpectation],
) -> None:
    phase_started = time.monotonic()
    clean, broken = _diagnostic_probe_sources()
    await _write_file(ctx, stats, path=_DIAGNOSTIC_PROBE_PATH, content=clean)
    await _apply_logical_edit(
        ctx,
        stats,
        path=_DIAGNOSTIC_PROBE_PATH,
        old_text="    return 1\n",
        new_text="    return (\n",
        description="diagnostic probe inject syntax error",
        expectations=expectations,
        forced_route="shell",
    )
    broken_checks = 5 if not ctx.smoke else 2
    for index in range(broken_checks):
        await _assert_lsp_diagnostics(
            ctx,
            stats,
            rel_path="scheduler_demo/_lsp_error_probe.py",
            expect_clean=False,
            label=f"diagnostic_probe.broken.{index}",
        )
    stats.diagnostic_error_detected = True

    await _apply_logical_edit(
        ctx,
        stats,
        path=_DIAGNOSTIC_PROBE_PATH,
        old_text=broken,
        new_text=clean,
        description="diagnostic probe repair syntax error",
        expectations=expectations,
        forced_route="edit_file",
    )
    clean_checks = 5 if not ctx.smoke else 2
    for index in range(clean_checks):
        await _assert_lsp_diagnostics(
            ctx,
            stats,
            rel_path="scheduler_demo/_lsp_error_probe.py",
            expect_clean=True,
            label=f"diagnostic_probe.repaired.{index}",
        )
    stats.diagnostic_repair_cleared = True
    import_check = await _shell(
        ctx,
        stats,
        command=(
            f"cd {WORKSPACE_ROOT} && "
            "python3 -c 'import scheduler_demo._lsp_error_probe'"
        ),
        timeout=30,
    )
    ctx.sandbox_checks.append(
        SandboxCheck(
            name="diagnostic_probe.import_after_repair",
            passed=_shell_exit_code(import_check) == 0,
            detail=f"exit_code={_shell_exit_code(import_check)}",
        )
    )
    stats.phases.append(
        {
            "name": "E_diagnostic_probe",
            "duration_s": time.monotonic() - phase_started,
            "tool_calls_at_end": _total_calls(stats),
        }
    )


def _diagnostic_probe_sources() -> tuple[str, str]:
    clean = (
        '"""Temporary LSP diagnostic probe."""\n'
        "from __future__ import annotations\n\n"
        "def probe_value() -> int:\n"
        "    return 1\n"
    )
    broken = clean.replace("    return 1\n", "    return (\n")
    return clean, broken


async def _apply_logical_edit(
    ctx: ProbeContext,
    stats: ShellEditLspStats,
    *,
    path: str,
    old_text: str,
    new_text: str,
    description: str,
    expectations: Sequence[LspExpectation],
    forced_route: str | None = None,
) -> ToolResult:
    logical_index = stats.logical_edit_count
    stats.logical_edit_count += 1
    use_shell = (
        forced_route == "shell"
        if forced_route is not None
        else logical_index % 3 == 2
    )
    if forced_route == "edit_file":
        use_shell = False

    if use_shell:
        result = await _apply_shell_edit(
            ctx,
            stats,
            path=path,
            old_text=old_text,
            new_text=new_text,
            description=description,
        )
    else:
        result = await _edit_file(
            ctx,
            stats,
            path=path,
            old_text=old_text,
            new_text=new_text,
            description=description,
        )
        stats.edit_file_edit_count += 1

    if stats.logical_edit_count % 10 == 0:
        await _semantic_lsp_mini_suite(
            ctx,
            stats,
            expectations,
            label=f"logical_edit_{stats.logical_edit_count}",
        )
    return result


async def _apply_shell_edit(
    ctx: ProbeContext,
    stats: ShellEditLspStats,
    *,
    path: str,
    old_text: str,
    new_text: str,
    description: str,
) -> ToolResult:
    started = time.monotonic()
    result = await _shell(
        ctx,
        stats,
        command=_shell_edit_command(path=path, old_text=old_text, new_text=new_text),
        timeout=180,
    )
    wall_seconds = time.monotonic() - started
    stats.shell_edit_count += 1
    stats.shell_edit_wall_seconds.append(wall_seconds)
    stats.shell_edit_tool_metadata.append(_capture_metadata("shell", result))

    exit_code = _shell_exit_code(result)
    payload = _parse_shell_edit_payload(result)
    if result.is_error or exit_code != 0 or payload is None:
        stats.shell_edit_errors += 1
        detail = f"exit_code={exit_code} stdout={_shell_stdout(result)[-300:]!r}"
        ctx.sandbox_checks.append(
            SandboxCheck(
                name=f"shell_edit.{_short(path)}",
                passed=False,
                detail=detail,
            )
        )
        raise RuntimeError(f"shell edit failed: {description}: {detail}")

    stats.shell_edit_payloads.append(payload)
    api_read = await sandbox_api.read_file(
        ctx.sandbox_id,
        ReadFileRequest(path=path, caller=ctx.caller),
    )
    stats.api_read_count += 1
    content = api_read.content if api_read.success else ""
    old_absent_or_insert = old_text not in content or old_text in new_text
    changed = payload.get("before_sha256") != payload.get("after_sha256")
    passed = bool(
        api_read.success
        and new_text in content
        and old_absent_or_insert
        and changed
    )
    changed_paths = tuple(
        str(p) for p in (result.metadata or {}).get("changed_paths", ())
    )
    ctx.sandbox_checks.append(
        SandboxCheck(
            name=f"shell_edit.{_short(path)}",
            passed=passed,
            detail=(
                f"description={description!r} changed={changed} "
                f"changed_paths={changed_paths}"
            ),
            changed_paths=changed_paths,
        )
    )
    if not passed:
        raise RuntimeError(f"shell edit verification failed: {description}")
    return result


def _shell_edit_command(*, path: str, old_text: str, new_text: str) -> str:
    return "\n".join(
        (
            "python3 - <<'PY'",
            "import hashlib",
            "import json",
            "from pathlib import Path",
            "",
            f"path = Path({json.dumps(path)})",
            f"old = {json.dumps(old_text)}",
            f"new = {json.dumps(new_text)}",
            "data = path.read_text(encoding='utf-8')",
            "count = data.count(old)",
            "if count != 1:",
            "    raise SystemExit(f'expected exactly one match, found {count}')",
            "before_hash = hashlib.sha256(data.encode('utf-8')).hexdigest()",
            "updated = data.replace(old, new, 1)",
            "path.write_text(updated, encoding='utf-8')",
            "after_hash = hashlib.sha256(updated.encode('utf-8')).hexdigest()",
            "print(json.dumps({",
            "    'path': str(path),",
            "    'before_bytes': len(data.encode('utf-8')),",
            "    'after_bytes': len(updated.encode('utf-8')),",
            "    'before_sha256': before_hash,",
            "    'after_sha256': after_hash,",
            "}, sort_keys=True))",
            "PY",
        )
    )


def _parse_shell_edit_payload(result: ToolResult) -> dict[str, Any] | None:
    stdout = _shell_stdout(result)
    for line in reversed(stdout.splitlines()):
        try:
            payload = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(payload, dict) and "after_sha256" in payload:
            return payload
    return None


async def _semantic_lsp_mini_suite(
    ctx: ProbeContext,
    stats: ShellEditLspStats,
    expectations: Sequence[LspExpectation],
    *,
    label: str,
) -> None:
    available = await _available_lsp_expectations(ctx, stats, expectations)
    if not available:
        return
    use_site_expectations = _use_site_expectations(available)
    if use_site_expectations:
        await _assert_lsp_hover(
            ctx,
            stats,
            _next_expectation(stats, "lsp.hover", use_site_expectations),
            label,
        )
        await _assert_lsp_definition(
            ctx,
            stats,
            _next_expectation(stats, "lsp.find_definitions", use_site_expectations),
            label,
        )
        await _assert_lsp_references(
            ctx,
            stats,
            _next_expectation(stats, "lsp.find_references", use_site_expectations),
            label,
        )
    await _assert_lsp_query_symbols(
        ctx,
        stats,
        _next_expectation(stats, "lsp.query_symbols", available),
        label,
    )
    diag_expectation = _next_expectation(stats, "lsp.diagnostics", available)
    await _assert_lsp_diagnostics(
        ctx,
        stats,
        rel_path=diag_expectation.definition_path,
        expect_clean=True,
        label=label,
    )


def _next_expectation(
    stats: ShellEditLspStats,
    tool_name: str,
    expectations: Sequence[LspExpectation],
) -> LspExpectation:
    index = stats.lsp_semantic_checks.get(tool_name, 0) % len(expectations)
    return expectations[index]


def _use_site_expectations(
    expectations: Sequence[LspExpectation],
) -> tuple[LspExpectation, ...]:
    return tuple(
        expectation
        for expectation in expectations
        if (
            expectation.source_path != expectation.definition_path
            or expectation.source_anchor != expectation.definition_anchor
        )
    )


_hover_expectations = _use_site_expectations


async def _available_lsp_expectations(
    ctx: ProbeContext,
    stats: ShellEditLspStats,
    expectations: Sequence[LspExpectation],
) -> tuple[LspExpectation, ...]:
    available: list[LspExpectation] = []
    for expectation in expectations:
        source_ready = await _anchor_exists(
            ctx,
            stats,
            expectation.source_path,
            expectation.source_anchor,
        )
        if not source_ready:
            continue
        definition_ready = await _anchor_exists(
            ctx,
            stats,
            expectation.definition_path,
            expectation.definition_anchor,
        )
        if definition_ready:
            available.append(expectation)
    return tuple(available)


async def _anchor_exists(
    ctx: ProbeContext,
    stats: ShellEditLspStats,
    rel_path: str,
    anchor: str,
) -> bool:
    read = await sandbox_api.read_file(
        ctx.sandbox_id,
        ReadFileRequest(path=f"{WORKSPACE_ROOT}/{rel_path}", caller=ctx.caller),
    )
    stats.api_read_count += 1
    return bool(read.success and anchor in read.content)


async def _assert_lsp_hover(
    ctx: ProbeContext,
    stats: ShellEditLspStats,
    expectation: LspExpectation,
    label: str,
) -> None:
    line, character = await _position_for_expectation(ctx, stats, expectation)
    result = await _lsp_semantic_call(
        ctx,
        stats,
        tool_obj=lsp_hover_tool,
        tool_name="lsp.hover",
        args={
            "file_path": f"{WORKSPACE_ROOT}/{expectation.source_path}",
            "line": line,
            "character": character,
        },
    )
    payload = _tool_json(result)
    text = json.dumps(payload, sort_keys=True)
    symbol_ok = expectation.symbol.lower() in text.lower()
    _record_lsp_semantic_check(
        ctx,
        stats,
        "lsp.hover",
        label=f"{label}.{expectation.symbol}",
        passed=bool(
            (not result.is_error)
            and isinstance(payload, dict)
            and "hover" in payload
        ),
        detail=f"symbol_ok={symbol_ok}",
    )


async def _assert_lsp_definition(
    ctx: ProbeContext,
    stats: ShellEditLspStats,
    expectation: LspExpectation,
    label: str,
) -> None:
    line, character = await _position_for_expectation(ctx, stats, expectation)
    expected_line, _ = await _anchor_position(
        ctx,
        stats,
        expectation.definition_path,
        expectation.definition_anchor,
        expectation.symbol,
    )
    result = await _lsp_semantic_call(
        ctx,
        stats,
        tool_obj=lsp_find_definitions_tool,
        tool_name="lsp.find_definitions",
        args={
            "file_path": f"{WORKSPACE_ROOT}/{expectation.source_path}",
            "line": line,
            "character": character,
        },
    )
    payload = _tool_json(result)
    definitions = payload.get("definitions") if isinstance(payload, dict) else None
    locations = definitions if isinstance(definitions, list) else []
    definitions_ok = isinstance(definitions, list)
    matched = any(
        _location_matches(
            location,
            expected_path=expectation.definition_path,
            expected_line=expected_line,
        )
        for location in locations
    )
    _record_lsp_semantic_check(
        ctx,
        stats,
        "lsp.find_definitions",
        label=f"{label}.{expectation.symbol}",
        passed=bool(
            (not result.is_error)
            and definitions_ok
            and (len(locations) == 0 or matched)
        ),
        detail=f"locations={locations[:3]!r} expected_line={expected_line}",
    )


async def _assert_lsp_references(
    ctx: ProbeContext,
    stats: ShellEditLspStats,
    expectation: LspExpectation,
    label: str,
) -> None:
    line, character = await _position_for_expectation(ctx, stats, expectation)
    result = await _lsp_semantic_call(
        ctx,
        stats,
        tool_obj=lsp_find_references_tool,
        tool_name="lsp.find_references",
        args={
            "file_path": f"{WORKSPACE_ROOT}/{expectation.source_path}",
            "line": line,
            "character": character,
            "include_declaration": True,
        },
    )
    payload = _tool_json(result)
    references = payload.get("references") if isinstance(payload, dict) else None
    locations = references if isinstance(references, list) else []
    references_ok = isinstance(references, list)
    paths = [_location_path(location) for location in locations]
    path_scope_ok = all(
        path and (not path.startswith("/") or path.startswith(WORKSPACE_ROOT))
        for path in paths
    )
    has_source_ref = any(path and not _is_test_path(path) for path in paths)
    needs_test_ref = expectation.source_path.startswith("tests/")
    has_test_ref = any(path and _is_test_path(path) for path in paths)
    passed = bool(
        (not result.is_error)
        and references_ok
        and (
            len(locations) == 0
            or (
                len(locations) >= expectation.min_references
                and path_scope_ok
                and has_source_ref
                and (has_test_ref if needs_test_ref else True)
            )
        )
    )
    _record_lsp_semantic_check(
        ctx,
        stats,
        "lsp.find_references",
        label=f"{label}.{expectation.symbol}",
        passed=passed,
        detail=(
            f"count={len(locations)} min={expectation.min_references} "
            f"source={has_source_ref} test={has_test_ref}"
        ),
    )


async def _assert_lsp_references_for_anchor(
    ctx: ProbeContext,
    stats: ShellEditLspStats,
    *,
    path: str,
    symbol: str,
    anchor: str,
    label: str,
) -> None:
    rel_path = path.removeprefix(WORKSPACE_ROOT + "/")
    line, character = await _anchor_position(ctx, stats, rel_path, anchor, symbol)
    result = await _lsp_semantic_call(
        ctx,
        stats,
        tool_obj=lsp_find_references_tool,
        tool_name="lsp.find_references",
        args={
            "file_path": path,
            "line": line,
            "character": character,
            "include_declaration": True,
        },
    )
    payload = _tool_json(result)
    references = payload.get("references") if isinstance(payload, dict) else None
    locations = references if isinstance(references, list) else []
    _record_lsp_semantic_check(
        ctx,
        stats,
        "lsp.find_references",
        label=f"{label}.{symbol}",
        passed=bool((not result.is_error) and isinstance(references, list)),
        detail=f"count={len(locations)}",
    )


async def _assert_lsp_query_symbols(
    ctx: ProbeContext,
    stats: ShellEditLspStats,
    expectation: LspExpectation,
    label: str,
) -> None:
    result = await _lsp_semantic_call(
        ctx,
        stats,
        tool_obj=lsp_query_symbols_tool,
        tool_name="lsp.query_symbols",
        args={"query": expectation.symbol},
    )
    payload = _tool_json(result)
    raw_symbols = payload.get("symbols") if isinstance(payload, dict) else None
    symbols = list(_flatten_symbols(raw_symbols if isinstance(raw_symbols, list) else []))
    matched = any(
        str(symbol.get("name") or "") == expectation.symbol
        and expectation.definition_path in json.dumps(symbol, sort_keys=True)
        for symbol in symbols
    )
    _record_lsp_semantic_check(
        ctx,
        stats,
        "lsp.query_symbols",
        label=f"{label}.{expectation.symbol}",
        passed=bool((not result.is_error) and matched),
        detail=f"symbols={[s.get('name') for s in symbols[:8]]}",
    )


async def _assert_lsp_diagnostics(
    ctx: ProbeContext,
    stats: ShellEditLspStats,
    *,
    rel_path: str,
    expect_clean: bool,
    label: str,
    expected_message: str | None = None,
) -> None:
    attempts = 1 if expect_clean else _DIAGNOSTIC_NONBLOCKING_RETRIES
    result: ToolResult | None = None
    diagnostics: object = None
    entries: list[object] = []

    for attempt in range(attempts):
        result = await _lsp_semantic_call(
            ctx,
            stats,
            tool_obj=lsp_diagnostics_tool,
            tool_name="lsp.diagnostics",
            args={
                "file_path": f"{WORKSPACE_ROOT}/{rel_path}",
                "wait_for_diagnostics": False,
            },
        )
        payload = _tool_json(result)
        diagnostics = payload.get("diagnostics") if isinstance(payload, dict) else None
        entries = diagnostics if isinstance(diagnostics, list) else []
        if expect_clean or result.is_error or entries:
            break
        if attempt + 1 < attempts:
            await asyncio.sleep(_DIAGNOSTIC_NONBLOCKING_RETRY_SLEEP_S)

    assert result is not None
    if expect_clean:
        passed = (not result.is_error) and not entries
    else:
        message_blob = json.dumps(entries, sort_keys=True)
        passed = bool(
            (not result.is_error)
            and isinstance(diagnostics, list)
            and entries
            and (
                expected_message is None
                or expected_message in message_blob
            )
        )
    stats.diagnostic_probe_checks += int("diagnostic_probe" in label)
    _record_lsp_semantic_check(
        ctx,
        stats,
        "lsp.diagnostics",
        label=f"{label}.{rel_path}",
        passed=passed,
        detail=f"diagnostic_count={len(entries)}",
    )


async def _lsp_semantic_call(
    ctx: ProbeContext,
    stats: ShellEditLspStats,
    *,
    tool_obj: BaseTool,
    tool_name: str,
    args: dict[str, Any],
) -> ToolResult:
    return await _lsp(
        ctx,
        stats,
        tool_obj=tool_obj,
        tool_name=tool_name,
        args=args,
    )


def _record_lsp_semantic_check(
    ctx: ProbeContext,
    stats: ShellEditLspStats,
    tool_name: str,
    *,
    label: str,
    passed: bool,
    detail: str,
) -> None:
    stats.lsp_semantic_checks[tool_name] = stats.lsp_semantic_checks.get(tool_name, 0) + 1
    if not passed:
        stats.lsp_semantic_failures += 1
    ctx.sandbox_checks.append(
        SandboxCheck(
            name=f"semantic.{tool_name}.{label}",
            passed=passed,
            detail=detail,
        )
    )
    if not passed:
        raise RuntimeError(f"semantic {tool_name} check failed: {label}: {detail}")


async def _position_for_expectation(
    ctx: ProbeContext,
    stats: ShellEditLspStats,
    expectation: LspExpectation,
) -> tuple[int, int]:
    return await _anchor_position(
        ctx,
        stats,
        expectation.source_path,
        expectation.source_anchor,
        expectation.symbol,
    )


async def _anchor_position(
    ctx: ProbeContext,
    stats: ShellEditLspStats,
    rel_path: str,
    anchor: str,
    symbol: str,
) -> tuple[int, int]:
    read = await sandbox_api.read_file(
        ctx.sandbox_id,
        ReadFileRequest(path=f"{WORKSPACE_ROOT}/{rel_path}", caller=ctx.caller),
    )
    stats.api_read_count += 1
    if not read.success:
        raise RuntimeError(f"failed to read {rel_path} while resolving {anchor!r}")
    for line_index, line in enumerate(read.content.splitlines()):
        if anchor in line:
            return line_index, _symbol_cursor_offset(line, anchor, symbol)
    raise RuntimeError(f"missing LSP anchor {anchor!r} in {rel_path}")


def _symbol_cursor_offset(line: str, anchor: str, symbol: str) -> int:
    """Return a cursor offset inside the target token for LSP lookups."""
    character = line.find(symbol)
    if character >= 0 and symbol:
        return character + min(max(len(symbol) // 2, 1), len(symbol) - 1)
    return max(line.find(anchor), 0)


def _location_matches(
    location: Any,
    *,
    expected_path: str,
    expected_line: int,
) -> bool:
    path = _location_path(location)
    if path is None:
        return False
    if not (path == expected_path or path.endswith("/" + expected_path)):
        return False
    line = _location_line(location)
    return line is not None and abs(line - expected_line) <= 2


def _location_path(location: Any) -> str | None:
    if not isinstance(location, dict):
        return None
    raw = location.get("file_path")
    if raw is not None:
        return str(raw)
    uri = (
        (location.get("location") or {}).get("uri")
        if isinstance(location.get("location"), dict)
        else location.get("uri")
    )
    if uri is None:
        return None
    text = str(uri)
    marker = f"{WORKSPACE_ROOT}/"
    if marker in text:
        return text.split(marker, 1)[1]
    return text


def _location_line(location: Any) -> int | None:
    if not isinstance(location, dict):
        return None
    range_obj = location.get("range")
    if not isinstance(range_obj, dict) and isinstance(location.get("location"), dict):
        range_obj = location["location"].get("range")
    if not isinstance(range_obj, dict):
        return None
    start = range_obj.get("start")
    if not isinstance(start, dict):
        return None
    raw = start.get("line")
    return int(raw) if isinstance(raw, int) else None


def _flatten_symbols(symbols: Sequence[Any]) -> Sequence[dict[str, Any]]:
    flattened: list[dict[str, Any]] = []
    stack = list(symbols)
    while stack:
        item = stack.pop(0)
        if not isinstance(item, dict):
            continue
        flattened.append(item)
        children = item.get("children")
        if isinstance(children, list):
            stack.extend(children)
    return flattened


def _is_test_path(path: str) -> bool:
    return path.startswith("tests/") or "/tests/" in path


def _tool_json(result: ToolResult) -> dict[str, Any]:
    try:
        payload = json.loads(result.output)
    except (TypeError, json.JSONDecodeError):
        return {}
    return payload if isinstance(payload, dict) else {}


async def _phase_f_semantic_lsp_sweep(
    ctx: ProbeContext,
    stats: ShellEditLspStats,
    expectations: Sequence[LspExpectation],
) -> None:
    floor = 5 if ctx.smoke else 40
    total_floor = 25 if ctx.smoke else 200
    phase_started = time.monotonic()
    iteration = 0
    while (
        min(stats.lsp_semantic_checks.get(name, 0) for name in _LSP_NAMES) < floor
        or stats.total_lsp_semantic_checks() < total_floor
    ):
        await _semantic_lsp_mini_suite(
            ctx,
            stats,
            expectations,
            label=f"final_sweep_{iteration}",
        )
        iteration += 1
        if iteration > 96:
            raise RuntimeError(
                "semantic LSP final sweep exceeded safety bound: "
                f"{stats.lsp_semantic_checks}"
            )
    stats.phases.append(
        {
            "name": "F_semantic_lsp_sweep",
            "duration_s": time.monotonic() - phase_started,
            "tool_calls_at_end": _total_calls(stats),
        }
    )


async def _phase_f_emit_metrics(
    ctx: ProbeContext,
    stats: ShellEditLspStats,
    *,
    wall_seconds: float,
    selected_files: Sequence[FixtureFile],
    refactor_passes: Sequence[RefactorPass],
    pytest_exit_code: int,
    pytest_stdout: str,
    amp_pairs: int,
) -> str:
    scenario = (
        "sandbox.complex_project_build_shell_edit_lsp_smoke"
        if ctx.smoke
        else "sandbox.complex_project_build_shell_edit_lsp"
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
    perf_payload["shell_edit"] = _shell_edit_metrics(stats)
    perf_payload["lsp_correctness"] = _lsp_correctness_summary(stats)
    await _write_file(
        ctx,
        stats,
        path=METRICS_PATH,
        content=json.dumps(perf_payload, indent=2, sort_keys=True) + "\n",
    )
    await _read_file(ctx, stats, path=METRICS_PATH)

    summary = {
        "probe": "complex_project_build_shell_edit_lsp"
        + ("_smoke" if ctx.smoke else ""),
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
        "edit_routing": {
            "logical_edit_count": stats.logical_edit_count,
            "edit_file_edit_count": stats.edit_file_edit_count,
            "shell_edit_count": stats.shell_edit_count,
            "shell_edit_ratio": stats.shell_edit_ratio(),
            "routing_rule": _ROUTING_RULE,
        },
        "lsp_correctness": _lsp_correctness_summary(stats),
        "shell_edit": _shell_edit_metrics(stats),
        "diagnostic_probe": {
            "error_detected": stats.diagnostic_error_detected,
            "repair_cleared": stats.diagnostic_repair_cleared,
            "diagnostic_checks": stats.diagnostic_probe_checks,
        },
        "intentional_conflicts": stats.intentional_conflicts,
    }
    await _write_file(
        ctx,
        stats,
        path=_SUMMARY_PATH,
        content=json.dumps(summary, indent=2, sort_keys=True) + "\n",
    )
    return _SUMMARY_PATH


def _lsp_correctness_summary(stats: ShellEditLspStats) -> dict[str, Any]:
    passed = stats.total_lsp_semantic_checks() - stats.lsp_semantic_failures
    return {
        "total_checks": stats.total_lsp_semantic_checks(),
        "passed_checks": passed,
        "failed_checks": stats.lsp_semantic_failures,
        "by_tool": {name: stats.lsp_semantic_checks.get(name, 0) for name in _LSP_NAMES},
    }


def _shell_edit_metrics(stats: ShellEditLspStats) -> dict[str, Any]:
    changed_paths_total = 0
    overlay_capture_count = 0
    for entry in stats.shell_edit_tool_metadata:
        metadata = entry.get("metadata") or {}
        changed_paths = metadata.get("changed_paths") or ()
        if isinstance(changed_paths, (list, tuple)):
            changed_paths_total += len(changed_paths)
        timings = metadata.get("timings") or {}
        if isinstance(timings, dict) and "command_exec.capture_upperdir_s" in timings:
            overlay_capture_count += 1
    return {
        "count": stats.shell_edit_count,
        "errors": stats.shell_edit_errors,
        "overlay_capture_count": overlay_capture_count,
        "changed_paths_total": changed_paths_total,
        "wall_seconds_p50": _percentile(stats.shell_edit_wall_seconds, 0.5),
        "wall_seconds_p95": _percentile(stats.shell_edit_wall_seconds, 0.95),
    }


def _percentile(values: Sequence[float], p: float) -> float:
    sorted_values = sorted(float(value) for value in values)
    if not sorted_values:
        return 0.0
    if len(sorted_values) == 1:
        return sorted_values[0]
    rank = max(0, min(len(sorted_values) - 1, int(round(p * (len(sorted_values) - 1)))))
    return float(sorted_values[rank])


__all__ = [
    "METRICS_PATH",
    "WORKSPACE_ROOT",
    "_compute_mixed_amp_pairs",
    "run_complex_project_build_shell_edit_lsp_probe",
]
