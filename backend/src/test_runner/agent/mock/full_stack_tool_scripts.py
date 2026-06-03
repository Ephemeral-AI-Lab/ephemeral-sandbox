"""Prepared tool scripts for the full-stack adversarial live scenario."""

from __future__ import annotations

import json
from collections.abc import Sequence
from typing import Any

from test_runner.scenarios._scenario_helpers import instruction_field
from test_runner.scenarios.base import ScenarioContext
from test_runner.scenarios.sandbox._constants import AUTO_SQUASH_MAX_DEPTH
from test_runner.agent.mock.tool_scripts import (
    PreparedToolScript,
    ToolScriptStep,
)
from plugins.catalog.lsp.tools.apply_workspace_edit import (
    apply_workspace_edit as lsp_apply_workspace_edit_tool,
)
from plugins.catalog.lsp.tools.diagnostics import diagnostics as lsp_diagnostics_tool
from plugins.catalog.lsp.tools.find_definitions import (
    find_definitions as lsp_find_definitions_tool,
)
from plugins.catalog.lsp.tools.find_references import (
    find_references as lsp_find_references_tool,
)
from plugins.catalog.lsp.tools.hover import hover as lsp_hover_tool
from plugins.catalog.lsp.tools.query_symbols import query_symbols as lsp_query_symbols_tool
from tools.sandbox.edit_file import edit_file as edit_file_tool
from tools.sandbox.read_file import read_file as read_file_tool
from tools.sandbox.exec_command import exec_command as exec_command_tool
from tools.sandbox.write_file import write_file as write_file_tool


_ROOT = ".ephemeralos/sweevo-mock/full_stack"
_RECURSIVE_ROOT = ".ephemeralos/sweevo-mock/recursive"
_LEDGER_PATH = f"{_ROOT}/requirement-ledger.json"
_PACKAGE_PLAN_PATH = f"{_ROOT}/package-plan.json"
_WORKSPACE_PROOF_PATH = f"{_ROOT}/workspace-proof.txt"
_CONFLICT_PROBE_PATH = f"{_ROOT}/conflict-probe.txt"
_OCC_PATH = f"{_ROOT}/occ-matrix.json"
_OVERLAY_PATH = f"{_ROOT}/overlay-matrix.json"
_LAYERSTACK_PATH = f"{_ROOT}/layerstack-matrix.json"
_LSP_PATH = f"{_ROOT}/lsp-matrix.json"
_FINAL_PATH = f"{_ROOT}/final-reconciliation.json"
_RECURSIVE_CLOSE_PATH = f"{_RECURSIVE_ROOT}/full-stack-close-report.json"


def full_stack_metrics_path(ctx: ScenarioContext) -> str:
    """Return the scenario metrics JSONL path inside the sandbox workspace."""
    run_id = str(
        ctx.metadata.get("request_id")
        or ctx.metadata.get("run_id")
        or "unknown-run"
    )
    return f".omc/results/full-stack-adversarial-{_safe_slug(run_id)}.jsonl"


def _full_stack_metric_fragment_root(ctx: ScenarioContext) -> str:
    run_id = _safe_slug(_run_id(ctx))
    return f".omc/results/full-stack-adversarial-{run_id}/fragments"


def inspect_full_user_input_script(ctx: ScenarioContext) -> PreparedToolScript:
    """Persist the rendered prompt ledger and package DAG through tools."""
    requirements = _dict_list(ctx.requirement_ledger)
    packages = _dict_list(ctx.package_plan)
    # The default SWE-EVO instance (dask) renders ~39 requirements; this floor
    # guards against a degenerate/empty plan, not the historical >100 target.
    if len(requirements) <= 30:
        raise RuntimeError(
            "full_stack_adversarial requires the default rendered SWE-EVO "
            f"prompt with >30 requirements; saw {len(requirements)}."
        )
    if len(packages) < 4:
        raise RuntimeError(
            "full_stack_adversarial requires at least four generated packages; "
            f"saw {len(packages)}."
        )

    ledger = {
        "scenario": "full_stack_adversarial",
        "task_id": ctx.task_id,
        "requirement_count": len(requirements),
        "requirements": requirements,
    }
    package_plan = {
        "scenario": "full_stack_adversarial",
        "task_id": ctx.task_id,
        "package_count": len(packages),
        "packages": packages,
        "matrix_cells": _dict_list(ctx.matrix_plan),
    }
    return PreparedToolScript(
        name="inspect_full_user_input",
        summary="Rendered prompt ledger, package DAG, and workspace proof captured.",
        artifact=_LEDGER_PATH,
        steps=(
            ToolScriptStep(
                "mkdir-evidence-root",
                exec_command_tool,
                {
                    "cmd": (
                        f"mkdir -p {_ROOT} .omc/results && "
                        "test -d /testbed/.git && "
                        "printf 'declared_workspace=/testbed\\nrepo_git=yes\\n' "
                        f"> {_WORKSPACE_PROOF_PATH}"
                    ),
                    "timeout": 60,
                },
            ),
            ToolScriptStep(
                "write-requirement-ledger",
                write_file_tool,
                {"file_path": _LEDGER_PATH, "content": _json(ledger)},
            ),
            ToolScriptStep(
                "write-package-plan",
                write_file_tool,
                {"file_path": _PACKAGE_PLAN_PATH, "content": _json(package_plan)},
            ),
            ToolScriptStep(
                "read-ledger",
                read_file_tool,
                {"file_path": _LEDGER_PATH, "start_line": 1, "end_line": 40},
            ),
            ToolScriptStep(
                "read-package-plan",
                read_file_tool,
                {"file_path": _PACKAGE_PLAN_PATH, "start_line": 1, "end_line": 40},
            ),
            ToolScriptStep(
                "write-conflict-probe",
                write_file_tool,
                {"file_path": _CONFLICT_PROBE_PATH, "content": "stable-anchor\n"},
            ),
            ToolScriptStep(
                "missing-anchor-conflict",
                edit_file_tool,
                {
                    "file_path": _CONFLICT_PROBE_PATH,
                    "old_text": "missing-anchor\n",
                    "new_text": "should-not-apply\n",
                    "description": "expected full-stack missing-anchor conflict",
                },
                expect_error=True,
            ),
        ),
    )


def occ_conflict_matrix_script(ctx: ScenarioContext) -> PreparedToolScript:
    """Exercise OCC conflict and retry-adjacent filesystem behavior."""
    root = f"{_ROOT}/occ"
    artifact = _artifact(
        ctx,
        subsystem="occ",
        conflicts_detected=2,
        expected_tool_errors=1,
    )
    steps: list[ToolScriptStep] = [
        ToolScriptStep(
            "mkdir-occ-root",
            exec_command_tool,
            {"cmd": f"mkdir -p {root}", "timeout": 60},
        ),
        ToolScriptStep(
            "same-path-write-one",
            write_file_tool,
            {"file_path": f"{root}/same-path.txt", "content": "first\n"},
        ),
        ToolScriptStep(
            "same-path-write-two",
            write_file_tool,
            {"file_path": f"{root}/same-path.txt", "content": "second\n"},
        ),
        ToolScriptStep(
            "same-path-read",
            read_file_tool,
            {"file_path": f"{root}/same-path.txt", "start_line": 1, "end_line": 20},
        ),
        ToolScriptStep(
            "disjoint-write-a",
            write_file_tool,
            {"file_path": f"{root}/disjoint-a.txt", "content": "a\n"},
        ),
        ToolScriptStep(
            "disjoint-write-b",
            write_file_tool,
            {"file_path": f"{root}/disjoint-b.txt", "content": "b\n"},
        ),
        ToolScriptStep(
            "disjoint-read-a",
            read_file_tool,
            {"file_path": f"{root}/disjoint-a.txt", "start_line": 1, "end_line": 20},
        ),
        ToolScriptStep(
            "write-disjoint-edit-file",
            write_file_tool,
            {
                "file_path": f"{root}/disjoint-edits.txt",
                "content": "alpha\nmiddle\nomega\n",
            },
        ),
        ToolScriptStep(
            "edit-disjoint-head",
            edit_file_tool,
            {
                "file_path": f"{root}/disjoint-edits.txt",
                "old_text": "alpha\n",
                "new_text": "alpha-edited\n",
                "description": "disjoint head edit",
            },
        ),
        ToolScriptStep(
            "edit-disjoint-tail",
            edit_file_tool,
            {
                "file_path": f"{root}/disjoint-edits.txt",
                "old_text": "omega\n",
                "new_text": "omega-edited\n",
                "description": "disjoint tail edit",
            },
        ),
        ToolScriptStep(
            "write-overlap-file",
            write_file_tool,
            {"file_path": f"{root}/overlap.txt", "content": "winner\n"},
        ),
        ToolScriptStep(
            "same-file-overlap-conflict",
            edit_file_tool,
            {
                "file_path": f"{root}/overlap.txt",
                "old_text": "missing-overlap-anchor\n",
                "new_text": "loser\n",
                "description": "expected overlap conflict",
            },
            expect_error=True,
        ),
        ToolScriptStep(
            "shell-stale-seed",
            exec_command_tool,
            {
                "cmd": f"printf 'shell-stale\\n' > {root}/stale.txt",
                "timeout": 60,
            },
        ),
        ToolScriptStep(
            "public-write-wins-stale",
            write_file_tool,
            {"file_path": f"{root}/stale.txt", "content": "public-write\n"},
        ),
        ToolScriptStep(
            "read-public-write",
            read_file_tool,
            {"file_path": f"{root}/stale.txt", "start_line": 1, "end_line": 20},
        ),
        ToolScriptStep(
            "nonzero-shell-commits-side-effect",
            exec_command_tool,
            {
                "cmd": (
                    f"sh -c 'printf partial > {root}/nonzero.txt; exit 7'"
                ),
                "timeout": 60,
            },
            expect_error=True,
        ),
        ToolScriptStep(
            "read-nonzero-side-effect",
            read_file_tool,
            {"file_path": f"{root}/nonzero.txt", "start_line": 1, "end_line": 20},
        ),
        ToolScriptStep(
            "tracked-and-ignored-mixed",
            exec_command_tool,
            {
                "cmd": (
                    f"mkdir -p {root} .pytest_cache && "
                    f"printf tracked > {root}/tracked.txt && "
                    "printf ignored > .pytest_cache/full-stack-ignored.txt"
                ),
                "timeout": 60,
            },
        ),
        ToolScriptStep(
            "read-tracked-mixed",
            read_file_tool,
            {"file_path": f"{root}/tracked.txt", "start_line": 1, "end_line": 20},
        ),
        ToolScriptStep(
            "delete-write-seed",
            write_file_tool,
            {"file_path": f"{root}/delete-vs-write.txt", "content": "seed\n"},
        ),
        ToolScriptStep(
            "delete-path",
            exec_command_tool,
            {"cmd": f"rm -f {root}/delete-vs-write.txt", "timeout": 60},
        ),
        ToolScriptStep(
            "write-after-delete",
            write_file_tool,
            {
                "file_path": f"{root}/delete-vs-write.txt",
                "content": "replacement\n",
            },
        ),
        ToolScriptStep(
            "write-occ-artifact",
            write_file_tool,
            {"file_path": _OCC_PATH, "content": _json(artifact)},
        ),
        ToolScriptStep(
            "read-occ-artifact",
            read_file_tool,
            {"file_path": _OCC_PATH, "start_line": 1, "end_line": 80},
        ),
    ]
    steps.extend(
        _metric_steps(
            ctx,
            "occ",
            expected_errors={
                "same_file_overlap_edits",
                "nonzero_shell_commits_side_effect",
            },
        )
    )
    return PreparedToolScript(
        name="occ_conflict_matrix",
        summary="OCC conflict matrix completed with expected conflict evidence.",
        artifact=_OCC_PATH,
        steps=tuple(steps),
    )


def overlay_edge_matrix_script(ctx: ScenarioContext) -> PreparedToolScript:
    """Exercise overlay capture edge cases through the exec_command tool."""
    root = f"{_ROOT}/overlay"
    artifact = _artifact(
        ctx,
        subsystem="overlay",
        conflicts_detected=0,
        expected_tool_errors=2,
    )
    steps: list[ToolScriptStep] = [
        ToolScriptStep(
            "overlay-mixed-mutations",
            exec_command_tool,
            {"cmd": _overlay_mutation_command(root), "timeout": 120},
        ),
        ToolScriptStep(
            "read-overlay-new",
            read_file_tool,
            {"file_path": f"{root}/new.txt", "start_line": 1, "end_line": 20},
        ),
        ToolScriptStep(
            "read-overlay-modified",
            read_file_tool,
            {"file_path": f"{root}/modify.txt", "start_line": 1, "end_line": 20},
        ),
        ToolScriptStep(
            "read-overlay-deleted",
            read_file_tool,
            {"file_path": f"{root}/delete.txt", "start_line": 1, "end_line": 20},
            expect_error=True,
        ),
        ToolScriptStep(
            "read-overlay-deep",
            read_file_tool,
            {
                "file_path": f"{root}/deep/a/b/c/deep.txt",
                "start_line": 1,
                "end_line": 20,
            },
        ),
        ToolScriptStep(
            "read-overlay-special-chars",
            read_file_tool,
            {
                "file_path": f"{root}/special [case] quote-safe.txt",
                "start_line": 1,
                "end_line": 20,
            },
        ),
        ToolScriptStep(
            "read-overlay-whiteout",
            read_file_tool,
            {"file_path": f"{root}/whiteout/file.txt", "start_line": 1, "end_line": 20},
        ),
        ToolScriptStep(
            "symlink-inside-captured",
            exec_command_tool,
            {"cmd": f"ln -s new.txt {root}/symlink_inside", "timeout": 60},
        ),
        ToolScriptStep(
            "write-symlink-inside-status",
            write_file_tool,
            {
                "file_path": f"{root}/symlink_inside.status",
                "content": "symlink-capture-accepted\n",
            },
        ),
        ToolScriptStep(
            "read-symlink-inside-status",
            read_file_tool,
            {
                "file_path": f"{root}/symlink_inside.status",
                "start_line": 1,
                "end_line": 20,
            },
        ),
        ToolScriptStep(
            "symlink-escape-classified-outside",
            exec_command_tool,
            {"cmd": f"ln -s /tmp/full-stack-symlink-escape {root}/symlink_escape", "timeout": 60},
        ),
        ToolScriptStep(
            "write-symlink-escape-status",
            write_file_tool,
            {
                "file_path": f"{root}/symlink_escape.status",
                "content": "symlink-escape-not-committed\n",
            },
        ),
        ToolScriptStep(
            "read-symlink-escape-status",
            read_file_tool,
            {
                "file_path": f"{root}/symlink_escape.status",
                "start_line": 1,
                "end_line": 20,
            },
        ),
        ToolScriptStep(
            "outside-workspace-write",
            exec_command_tool,
            {
                "cmd": (
                    "printf outside > /tmp/full-stack-outside-workspace.txt && "
                    "rm -f /tmp/full-stack-outside-workspace.txt && "
                    f"printf not-committed > {root}/outside-workspace.status"
                ),
                "timeout": 60,
            },
        ),
        ToolScriptStep(
            "noop-shell",
            exec_command_tool,
            {"cmd": "true", "timeout": 60},
        ),
        ToolScriptStep(
            "write-overlay-artifact",
            write_file_tool,
            {"file_path": _OVERLAY_PATH, "content": _json(artifact)},
        ),
        ToolScriptStep(
            "read-overlay-artifact",
            read_file_tool,
            {"file_path": _OVERLAY_PATH, "start_line": 1, "end_line": 80},
        ),
    ]
    steps.extend(
        _metric_steps(
            ctx,
            "overlay",
            expected_errors={"delete_files"},
        )
    )
    return PreparedToolScript(
        name="overlay_edge_matrix",
        summary="Overlay edge matrix completed with public readback evidence.",
        artifact=_OVERLAY_PATH,
        steps=tuple(steps),
    )


def layerstack_squash_lease_script(ctx: ScenarioContext) -> PreparedToolScript:
    """Exercise layer-stack depth, readback, and squash observability."""
    root = f"{_ROOT}/layerstack"
    depth_write_count = AUTO_SQUASH_MAX_DEPTH + 4
    artifact = _artifact(
        ctx,
        subsystem="layerstack",
        manifest_start=1,
        manifest_end=depth_write_count + 5,
        expected_tool_errors=0,
    )
    steps: list[ToolScriptStep] = [
        ToolScriptStep(
            "read-workspace-binding",
            exec_command_tool,
            {
                "cmd": (
                    f"mkdir -p {root}/depth && test -d /testbed/.git && "
                    f"printf 'workspace=/testbed\\n' > {root}/binding.txt"
                ),
                "timeout": 60,
            },
        ),
        ToolScriptStep(
            "write-manifest-seed",
            write_file_tool,
            {"file_path": f"{root}/manifest-depth.txt", "content": "version=1\n"},
        ),
        ToolScriptStep(
            "edit-manifest-seed",
            edit_file_tool,
            {
                "file_path": f"{root}/manifest-depth.txt",
                "old_text": "version=1\n",
                "new_text": "version=2\n",
                "description": "manifest monotonicity marker",
            },
        ),
        ToolScriptStep(
            "write-old-snapshot-evidence",
            write_file_tool,
            {"file_path": f"{root}/old-snapshot.txt", "content": "old-readable\n"},
        ),
    ]
    for index in range(depth_write_count):
        steps.append(
            ToolScriptStep(
                f"layer-depth-write-{index:03d}",
                write_file_tool,
                {
                    "file_path": f"{root}/depth/layer-{index:03d}.txt",
                    "content": f"layer={index}\n",
                },
            )
        )
    steps.extend(
        [
            ToolScriptStep(
                "read-layer-current",
                read_file_tool,
                {
                    "file_path": f"{root}/manifest-depth.txt",
                    "start_line": 1,
                    "end_line": 20,
                },
            ),
            ToolScriptStep(
                "shell-cat-layer-current",
                exec_command_tool,
                {"cmd": f"cat {root}/manifest-depth.txt", "timeout": 60},
            ),
            ToolScriptStep(
                "write-layerstack-artifact",
                write_file_tool,
                {"file_path": _LAYERSTACK_PATH, "content": _json(artifact)},
            ),
            ToolScriptStep(
                "read-layerstack-artifact",
                read_file_tool,
                {"file_path": _LAYERSTACK_PATH, "start_line": 1, "end_line": 80},
            ),
        ]
    )
    steps.extend(
        _metric_steps(
            ctx,
            "layerstack",
            manifest_before=1,
            manifest_after=depth_write_count + 5,
        )
    )
    return PreparedToolScript(
        name="layerstack_squash_lease",
        summary="Layer-stack lease/squash matrix completed with merged readback.",
        artifact=_LAYERSTACK_PATH,
        steps=tuple(steps),
    )


def lsp_refresh_semantics_script(ctx: ScenarioContext) -> PreparedToolScript:
    """Run all Pyright LSP tools after public write/edit mutations."""
    root = f"{_ROOT}/lsp_pkg"
    init_path = f"{root}/__init__.py"
    model_path = f"{root}/model.py"
    service_path = f"{root}/service.py"
    consumer_path = f"{root}/consumer.py"
    config_path = "pyrightconfig.json"
    artifact = _artifact(ctx, subsystem="lsp", lsp_warm_p95_ms=0.0)
    steps: list[ToolScriptStep] = [
        ToolScriptStep(
            "mkdir-lsp-package",
            exec_command_tool,
            {"cmd": f"mkdir -p {root}", "timeout": 60},
        ),
        ToolScriptStep(
            "write-lsp-init",
            write_file_tool,
            {"file_path": init_path, "content": ""},
        ),
        ToolScriptStep(
            "write-lsp-model",
            write_file_tool,
            {"file_path": model_path, "content": _LSP_MODEL_V1},
        ),
        ToolScriptStep(
            "write-lsp-service",
            write_file_tool,
            {"file_path": service_path, "content": _LSP_SERVICE_V1},
        ),
        ToolScriptStep(
            "write-lsp-consumer",
            write_file_tool,
            {"file_path": consumer_path, "content": _LSP_CONSUMER_BAD},
        ),
        ToolScriptStep(
            "lsp-hot-server-warmup",
            lsp_diagnostics_tool,
            {"file_path": model_path, "wait_for_diagnostics": False},
        ),
        ToolScriptStep(
            "lsp-hover-initial",
            lsp_hover_tool,
            {"file_path": model_path, "line": 3, "character": 4},
        ),
        ToolScriptStep(
            "lsp-find-definitions",
            lsp_find_definitions_tool,
            {"file_path": service_path, "line": 3, "character": 12},
        ),
        ToolScriptStep(
            "lsp-find-references",
            lsp_find_references_tool,
            {
                "file_path": model_path,
                "line": 3,
                "character": 4,
                "include_declaration": False,
            },
        ),
        ToolScriptStep(
            "lsp-query-symbols",
            lsp_query_symbols_tool,
            {"query": "display_name", "file_path": model_path},
        ),
        ToolScriptStep(
            "lsp-diagnostics-present",
            lsp_diagnostics_tool,
            {"file_path": consumer_path},
        ),
        ToolScriptStep(
            "fix-lsp-diagnostic",
            edit_file_tool,
            {
                "file_path": consumer_path,
                "old_text": "final: str = missing_value\n",
                "new_text": "final: str = name\n",
                "description": "fix undefined diagnostic",
            },
        ),
        ToolScriptStep(
            "lsp-diagnostics-fixed",
            lsp_diagnostics_tool,
            {"file_path": consumer_path},
        ),
        ToolScriptStep(
            "edit-lsp-signature",
            edit_file_tool,
            {
                "file_path": model_path,
                "old_text": "def display_name(profile: UserProfile) -> str:\n",
                "new_text": "def display_name(profile: UserProfile) -> int:\n",
                "description": "change hover signature",
            },
        ),
        ToolScriptStep(
            "edit-lsp-return",
            edit_file_tool,
            {
                "file_path": model_path,
                "old_text": "    return profile.name\n",
                "new_text": "    return len(profile.name)\n",
                "description": "make changed signature internally consistent",
            },
        ),
        ToolScriptStep(
            "lsp-hover-updated",
            lsp_hover_tool,
            {"file_path": model_path, "line": 3, "character": 4},
        ),
        ToolScriptStep(
            "lsp-references-after-edit",
            lsp_find_references_tool,
            {
                "file_path": model_path,
                "line": 3,
                "character": 4,
                "include_declaration": False,
            },
        ),
        ToolScriptStep(
            "lsp-apply-workspace-edit",
            lsp_apply_workspace_edit_tool,
            {
                "edit": {
                    "changes": {
                        f"file:///testbed/{service_path}": [
                            {
                                "range": {
                                    "start": {"line": 3, "character": 6},
                                    "end": {"line": 3, "character": 9},
                                },
                                "newText": "int",
                            }
                        ]
                    }
                },
            },
        ),
        ToolScriptStep(
            "read-lsp-service-after-plugin-edit",
            read_file_tool,
            {"file_path": service_path, "start_line": 1, "end_line": 20},
        ),
        ToolScriptStep(
            "write-pyright-config",
            write_file_tool,
            {
                "file_path": config_path,
                "content": _json({"include": [".ephemeralos/sweevo-mock/full_stack"]}),
            },
        ),
        ToolScriptStep(
            "lsp-config-diagnostics",
            lsp_diagnostics_tool,
            {"file_path": service_path},
        ),
        ToolScriptStep(
            "opened-file-renamed",
            exec_command_tool,
            {"cmd": f"mv {consumer_path} {root}/consumer_renamed.py", "timeout": 60},
        ),
        ToolScriptStep(
            "lsp-diagnostics-renamed",
            lsp_diagnostics_tool,
            {"file_path": f"{root}/consumer_renamed.py"},
        ),
        ToolScriptStep(
            "write-lsp-artifact",
            write_file_tool,
            {"file_path": _LSP_PATH, "content": _json(artifact)},
        ),
        ToolScriptStep(
            "read-lsp-artifact",
            read_file_tool,
            {"file_path": _LSP_PATH, "start_line": 1, "end_line": 80},
        ),
    ]
    steps.extend(_metric_steps(ctx, "lsp"))
    return PreparedToolScript(
        name="lsp_refresh_semantics",
        summary="LSP refresh matrix completed with all Pyright tools exercised.",
        artifact=_LSP_PATH,
        steps=tuple(steps),
    )


def recursive_oversized_matrix_script(ctx: ScenarioContext) -> PreparedToolScript:
    """Persist recursive workflow evidence and close report through tools."""
    instruction = ctx.instruction or ""
    slice_id = instruction_field(instruction, "slice") or "slice"
    is_close = (
        instruction_field(instruction, "close") == "true"
        or slice_id == "close"
    )
    evidence_path = f"{_RECURSIVE_ROOT}/oversized-{_safe_slug(slice_id)}.json"
    payload = {
        "scenario": "full_stack_adversarial",
        "task_id": ctx.task_id,
        "slice": slice_id,
        "close_report": is_close,
        "matrix_cells": _subsystem_cells(ctx, "recursive"),
    }
    steps: list[ToolScriptStep] = [
        ToolScriptStep(
            "mkdir-recursive-root",
            exec_command_tool,
            {"cmd": f"mkdir -p {_RECURSIVE_ROOT}", "timeout": 60},
        ),
        ToolScriptStep(
            "write-recursive-evidence",
            write_file_tool,
            {"file_path": evidence_path, "content": _json(payload)},
        ),
        ToolScriptStep(
            "read-recursive-evidence",
            read_file_tool,
            {"file_path": evidence_path, "start_line": 1, "end_line": 80},
        ),
    ]
    if is_close:
        steps.append(
            ToolScriptStep(
                "write-recursive-close-report",
                write_file_tool,
                {
                    "file_path": _RECURSIVE_CLOSE_PATH,
                    "content": _json(
                        {
                            "scenario": "full_stack_adversarial",
                            "task_id": ctx.task_id,
                            "status": "closed",
                            "recursive_cells": _subsystem_cells(ctx, "recursive"),
                        }
                    ),
                },
            )
        )
        steps.append(
            ToolScriptStep(
                "read-recursive-close-report",
                read_file_tool,
                {
                    "file_path": _RECURSIVE_CLOSE_PATH,
                    "start_line": 1,
                    "end_line": 80,
                },
            )
        )
        steps.extend(_metric_steps(ctx, "recursive"))
    return PreparedToolScript(
        name="recursive_oversized_matrix",
        summary="Recursive oversized matrix evidence completed.",
        artifact=_RECURSIVE_CLOSE_PATH if is_close else evidence_path,
        steps=tuple(steps),
    )


def final_reconciliation_script(ctx: ScenarioContext) -> PreparedToolScript:
    """Read subsystem artifacts and append the final summary metrics row."""
    matrix_cells = _dict_list(ctx.matrix_plan)
    total_cells = len(matrix_cells)
    summary = {
        "scenario": "full_stack_adversarial",
        "passed_cells": total_cells,
        "failed_cells": 0,
        "conflicts_detected": 3,
        "expected_tool_errors": 4,
        "unexpected_tool_errors": 0,
        "recursive_workflows": 1,
        "lsp_warm_p95_ms": 0,
        "manifest_start": 1,
        "manifest_end": 17,
    }
    summary_row = {
        "schema": "full_stack_adversarial.summary.v1",
        "run_id": _run_id(ctx),
        "scenario": "full_stack_adversarial",
        "total_cells": total_cells,
        "passed_cells": total_cells,
        "failed_cells": 0,
        "failed_cell_ids": [],
        "expected_tool_errors": 4,
        "unexpected_tool_errors": 0,
        "conflicts_detected": 3,
        "recursive_workflows": 1,
        "request_status": "done",
        "artifact": full_stack_metrics_path(ctx),
    }
    payload = {
        **summary,
        "task_id": ctx.task_id,
        "stage": instruction_field(ctx.instruction or "", "stage") or "final",
        "metrics_artifact": full_stack_metrics_path(ctx),
        "matrix_cells": matrix_cells,
    }
    steps = [
        ToolScriptStep(
            "read-occ-artifact",
            read_file_tool,
            {"file_path": _OCC_PATH, "start_line": 1, "end_line": 80},
        ),
        ToolScriptStep(
            "read-overlay-artifact",
            read_file_tool,
            {"file_path": _OVERLAY_PATH, "start_line": 1, "end_line": 80},
        ),
        ToolScriptStep(
            "read-layerstack-artifact",
            read_file_tool,
            {"file_path": _LAYERSTACK_PATH, "start_line": 1, "end_line": 80},
        ),
        ToolScriptStep(
            "read-lsp-artifact",
            read_file_tool,
            {"file_path": _LSP_PATH, "start_line": 1, "end_line": 80},
        ),
        ToolScriptStep(
            "read-recursive-close-report",
            read_file_tool,
            {"file_path": _RECURSIVE_CLOSE_PATH, "start_line": 1, "end_line": 80},
        ),
        ToolScriptStep(
            "write-final-reconciliation",
            write_file_tool,
            {"file_path": _FINAL_PATH, "content": _json(payload)},
        ),
        ToolScriptStep(
            "write-canonical-metrics-artifact",
            exec_command_tool,
            {
                "cmd": _write_metrics_artifact_command(ctx, summary_row),
                "timeout": 60,
            },
        ),
        ToolScriptStep(
            "read-final-reconciliation",
            read_file_tool,
            {"file_path": _FINAL_PATH, "start_line": 1, "end_line": 80},
        ),
        ToolScriptStep(
            "read-metrics-artifact",
            read_file_tool,
            {
                "file_path": full_stack_metrics_path(ctx),
                "start_line": 1,
                "end_line": 200,
            },
        ),
    ]
    return PreparedToolScript(
        name="final_reconciliation",
        summary="Final full-stack reconciliation and metrics summary completed.",
        artifact=_FINAL_PATH,
        steps=tuple(steps),
    )


def _metric_steps(
    ctx: ScenarioContext,
    subsystem: str,
    *,
    expected_errors: set[str] | None = None,
    manifest_before: int = 0,
    manifest_after: int = 0,
) -> list[ToolScriptStep]:
    expected = expected_errors or set()
    steps: list[ToolScriptStep] = []
    for cell in _subsystem_cells(ctx, subsystem):
        cell_id = str(cell.get("id") or "")
        steps.append(
            _metric_row_step(
                ctx,
                f"metric-{subsystem}-{cell_id}",
                _cell_row(
                    ctx,
                    cell,
                    expected_error=cell_id in expected,
                    manifest_before=manifest_before,
                    manifest_after=manifest_after,
                ),
            )
        )
    return steps


def _metric_row_step(
    ctx: ScenarioContext,
    label: str,
    row: dict[str, Any],
) -> ToolScriptStep:
    fragment_path = _metric_fragment_path(ctx, label)
    return ToolScriptStep(
        label,
        write_file_tool,
        {"file_path": fragment_path, "content": _json(row) + "\n"},
    )


def _metric_fragment_path(ctx: ScenarioContext, label: str) -> str:
    task_id = _safe_slug(ctx.task_id or "unknown-task")
    safe_label = _safe_slug(label)
    return f"{_full_stack_metric_fragment_root(ctx)}/{task_id}/{safe_label}.jsonl"


def _write_metrics_artifact_command(
    ctx: ScenarioContext,
    summary_row: dict[str, Any],
) -> str:
    path = full_stack_metrics_path(ctx)
    fragment_root = _full_stack_metric_fragment_root(ctx)
    summary_line = _json(summary_row)
    code = (
        "from pathlib import Path\n"
        f"path = {path!r}\n"
        f"fragment_root = Path({fragment_root!r})\n"
        f"summary_line = {summary_line!r}\n"
        "target = Path(path)\n"
        "target.parent.mkdir(parents=True, exist_ok=True)\n"
        "rows = []\n"
        "if fragment_root.exists():\n"
        "    for fragment in sorted(fragment_root.rglob('*.jsonl')):\n"
        "        rows.extend(\n"
        "            line for line in fragment.read_text(encoding='utf-8').splitlines()\n"
        "            if line.strip()\n"
        "        )\n"
        "rows.append(summary_line)\n"
        "target.write_text('\\n'.join(rows) + '\\n', encoding='utf-8')\n"
        "print(path)\n"
    )
    return f"python3 - <<'PY'\n{code}PY"


def _cell_row(
    ctx: ScenarioContext,
    cell: dict[str, Any],
    *,
    expected_error: bool = False,
    manifest_before: int = 0,
    manifest_after: int = 0,
) -> dict[str, Any]:
    return {
        "schema": "full_stack_adversarial.cell.v1",
        "run_id": _run_id(ctx),
        "scenario": "full_stack_adversarial",
        "cell": cell.get("id"),
        "subsystem": cell.get("subsystem"),
        "tool_names": list(cell.get("tool_names") or ()),
        "agent_task_id": ctx.task_id,
        "agent_run_id": str(ctx.metadata.agent_run_id or ""),
        "passed": True,
        "expected_error": expected_error,
        "failure_reason": None,
        "wall_ms": 0.0,
        "manifest_before": manifest_before,
        "manifest_after": manifest_after,
        "route": cell.get("route") or "gated",
        "correctness": {"read_matches_expected": True},
        "timings": {
            "occ.commit.total_s": 0.0,
            "occ.commit.publish_layer_s": 0.0,
            "command_exec.capture_upperdir_s": 0.0,
        },
    }


def _artifact(
    ctx: ScenarioContext,
    *,
    subsystem: str,
    conflicts_detected: int = 0,
    expected_tool_errors: int = 0,
    manifest_start: int = 0,
    manifest_end: int = 0,
    lsp_warm_p95_ms: float = 0.0,
) -> dict[str, Any]:
    cells = _subsystem_cells(ctx, subsystem)
    return {
        "scenario": "full_stack_adversarial",
        "subsystem": subsystem,
        "task_id": ctx.task_id,
        "cell_count": len(cells),
        "passed_cells": len(cells),
        "failed_cells": 0,
        "conflicts_detected": conflicts_detected,
        "expected_tool_errors": expected_tool_errors,
        "unexpected_tool_errors": 0,
        "manifest_start": manifest_start,
        "manifest_end": manifest_end,
        "lsp_warm_p95_ms": lsp_warm_p95_ms,
        "metrics_artifact": full_stack_metrics_path(ctx),
        "cells": cells,
    }


def _subsystem_cells(ctx: ScenarioContext, subsystem: str) -> list[dict[str, Any]]:
    return [
        cell
        for cell in _dict_list(ctx.matrix_plan)
        if str(cell.get("subsystem") or "") == subsystem
    ]


def _overlay_mutation_command(root: str) -> str:
    code = (
        "from pathlib import Path\n"
        "import os\n"
        f"root = Path({root!r})\n"
        "root.mkdir(parents=True, exist_ok=True)\n"
        "(root / 'new.txt').write_text('new-file\\n', encoding='utf-8')\n"
        "(root / 'modify.txt').write_text('old\\n', encoding='utf-8')\n"
        "(root / 'modify.txt').write_text('modified\\n', encoding='utf-8')\n"
        "(root / 'delete.txt').write_text('delete-me\\n', encoding='utf-8')\n"
        "(root / 'delete.txt').unlink()\n"
        "deep = root / 'deep' / 'a' / 'b' / 'c'\n"
        "deep.mkdir(parents=True, exist_ok=True)\n"
        "(deep / 'deep.txt').write_text('deep\\n', encoding='utf-8')\n"
        "(root / 'special [case] quote-safe.txt').write_text('special\\n', encoding='utf-8')\n"
        "long_name = 'l' * 120 + '.txt'\n"
        "(root / long_name).write_text('long\\n', encoding='utf-8')\n"
        "whiteout = root / 'whiteout'\n"
        "whiteout.mkdir(exist_ok=True)\n"
        "(whiteout / 'file.txt').write_text('before\\n', encoding='utf-8')\n"
        "(whiteout / 'file.txt').unlink()\n"
        "(whiteout / 'file.txt').write_text('after\\n', encoding='utf-8')\n"
        "print(root)\n"
    )
    return f"python3 - <<'PY'\n{code}PY"


def _run_id(ctx: ScenarioContext) -> str:
    return str(
        ctx.metadata.get("request_id")
        or ctx.metadata.get("run_id")
        or "unknown-run"
    )


def _dict_list(value: Any) -> list[dict[str, Any]]:
    if value is None:
        return []
    items: Sequence[Any] = value if isinstance(value, Sequence) else ()
    return [dict(item) for item in items if isinstance(item, dict)]


def _safe_slug(value: str) -> str:
    return "".join(ch if ch.isalnum() or ch in "-_" else "_" for ch in value)


def _json(payload: dict[str, Any]) -> str:
    return json.dumps(payload, ensure_ascii=True, sort_keys=True, separators=(",", ":"))


_LSP_MODEL_V1 = (
    "class UserProfile:\n"
    "    name: str\n"
    "\n"
    "def display_name(profile: UserProfile) -> str:\n"
    "    return profile.name\n"
)

_LSP_SERVICE_V1 = (
    "from .model import UserProfile, display_name\n"
    "\n"
    "profile = UserProfile()\n"
    "name: str = display_name(profile)\n"
)

_LSP_CONSUMER_BAD = "from .service import name\n\nfinal: str = missing_value\n"


__all__ = [
    "final_reconciliation_script",
    "full_stack_metrics_path",
    "inspect_full_user_input_script",
    "layerstack_squash_lease_script",
    "lsp_refresh_semantics_script",
    "occ_conflict_matrix_script",
    "overlay_edge_matrix_script",
    "recursive_oversized_matrix_script",
]
