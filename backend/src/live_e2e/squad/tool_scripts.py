"""Prepared mock-agent tool scripts for SWE-EVO live scenarios.

The mock squad is deterministic, but it should still behave like an agent:
pick a prepared sequence, announce the step, call real sandbox tools, inspect
their outputs, and then submit through the real terminal tool.
"""

from __future__ import annotations

import json
from collections.abc import Awaitable, Callable, Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Protocol

from live_e2e.scenarios.base import ScenarioContext
from message.stream_events import AssistantTextDelta, StreamEvent
from tools._framework.core.base import BaseTool
from tools._framework.core.results import ToolResult
from tools._framework.core.runtime import ExecutionMetadata
from tools.sandbox.edit_file import edit_file as edit_file_tool
from tools.sandbox.read_file import read_file as read_file_tool
from tools.sandbox.shell import shell as shell_tool
from tools.sandbox.write_file import write_file as write_file_tool


EmitStreamEvent = Callable[[StreamEvent], Awaitable[None]]


class CallTool(Protocol):
    async def __call__(
        self,
        tool_obj: BaseTool,
        raw_input: dict[str, Any],
        metadata: ExecutionMetadata,
        emit: EmitStreamEvent,
        *,
        allow_error: bool = False,
    ) -> ToolResult: ...

_ROOT = ".ephemeralos/sweevo-mock/full_case"
_PACKAGE_DIR = f"{_ROOT}/packages"
_RECURSIVE_DIR = ".ephemeralos/sweevo-mock/recursive"
_LEDGER_PATH = f"{_ROOT}/requirement-ledger.json"
_FINAL_PATH = f"{_ROOT}/final-reconciliation.json"
_RECURSIVE_CLOSE_PATH = f"{_RECURSIVE_DIR}/close-report.json"
_WORKSPACE_PROOF_PATH = f"{_ROOT}/workspace-proof.txt"
_CONFLICT_PROBE_PATH = f"{_ROOT}/conflict-probe.txt"


@dataclass(frozen=True, slots=True)
class ToolScriptStep:
    """One concrete tool call inside a prepared script."""

    label: str
    tool: BaseTool
    args: dict[str, Any]
    expect_error: bool = False


@dataclass(frozen=True, slots=True)
class PreparedToolScript:
    """A deterministic sequence of real tool calls."""

    name: str
    summary: str
    artifact: str
    steps: tuple[ToolScriptStep, ...]


@dataclass(frozen=True, slots=True)
class ToolScriptResult:
    """Result summary returned after a prepared script runs."""

    script_name: str
    summary: str
    artifact: str
    results: tuple[ToolResult, ...]


class MockToolScriptEngine:
    """Execute prepared scripts through the same tool path real agents use."""

    def __init__(self, call_tool: CallTool) -> None:
        self._call_tool = call_tool

    async def run(
        self,
        script: PreparedToolScript,
        *,
        metadata: ExecutionMetadata,
        emit: EmitStreamEvent,
    ) -> ToolScriptResult:
        await _emit_text(
            emit,
            metadata,
            f"Prepared script {script.name}: {script.summary}\n",
        )
        results: list[ToolResult] = []
        for step in script.steps:
            await _emit_text(
                emit,
                metadata,
                f"Running script step {step.label} with {step.tool.name}.\n",
            )
            result = await self._call_tool(
                step.tool,
                dict(step.args),
                metadata,
                emit,
                allow_error=step.expect_error,
            )
            if step.expect_error and not result.is_error:
                raise RuntimeError(
                    f"Expected script step {step.label} to fail, but it succeeded."
                )
            results.append(result)
        await _emit_text(
            emit,
            metadata,
            f"Completed prepared script {script.name}.\n",
        )
        return ToolScriptResult(
            script_name=script.name,
            summary=script.summary,
            artifact=script.artifact,
            results=tuple(results),
        )


PreparedToolScriptEngine = MockToolScriptEngine


def inspect_user_input_script(ctx: ScenarioContext) -> PreparedToolScript:
    """Persist and verify the requirement ledger parsed from user input."""
    requirements = _dict_list(ctx.requirement_ledger)
    packages = _dict_list(ctx.package_plan)
    content = _json(
        {
            "task_id": ctx.task_id,
            "requirement_count": len(requirements),
            "package_count": len(packages),
            "requirements_sample": requirements[:20],
            "package_index": [
                {
                    "id": package.get("id"),
                    "subsystem": package.get("subsystem"),
                    "risk": package.get("risk"),
                    "item_count": len(package.get("item_ids") or ()),
                }
                for package in packages
            ],
        }
    )
    return PreparedToolScript(
        name="inspect_user_input",
        summary="Requirement ledger and package DAG were written and read back.",
        artifact=_LEDGER_PATH,
        steps=(
            ToolScriptStep(
                "create-ledger-dir",
                shell_tool,
                {"command": f"mkdir -p {_PACKAGE_DIR}", "timeout": 60},
            ),
            ToolScriptStep(
                "assert-daytona-workspace",
                shell_tool,
                {
                    "command": (
                        "test -d /testbed/.git && "
                        f"mkdir -p {Path(_WORKSPACE_PROOF_PATH).parent.as_posix()} && "
                        f"printf 'declared_workspace=/testbed\\n' > {_WORKSPACE_PROOF_PATH} && "
                        f"test -s /testbed/{_WORKSPACE_PROOF_PATH}"
                    ),
                    "timeout": 60,
                },
            ),
            ToolScriptStep(
                "write-ledger",
                write_file_tool,
                {"file_path": _LEDGER_PATH, "content": content},
            ),
            ToolScriptStep(
                "write-conflict-probe",
                write_file_tool,
                {
                    "file_path": _CONFLICT_PROBE_PATH,
                    "content": "stable-anchor\n",
                },
            ),
            ToolScriptStep(
                "fabricate-conflict-detection",
                edit_file_tool,
                {
                    "file_path": _CONFLICT_PROBE_PATH,
                    "old_text": "missing-anchor\n",
                    "new_text": "should-not-apply\n",
                    "description": "expected SWE-EVO conflict probe",
                },
                expect_error=True,
            ),
            ToolScriptStep(
                "read-ledger",
                read_file_tool,
                {"file_path": _LEDGER_PATH, "start_line": 1, "end_line": 20},
            ),
            ToolScriptStep(
                "check-ledger",
                shell_tool,
                {
                    "command": (
                        f"test -s {_LEDGER_PATH} && "
                        f"printf 'requirements={len(requirements)} packages={len(packages)}\\n'"
                    ),
                    "timeout": 60,
                },
            ),
        ),
    )


def execute_package_script(
    ctx: ScenarioContext,
    *,
    package_id: str,
) -> PreparedToolScript:
    """Execute one package by writing, reading, and shell-checking evidence."""
    package = _find_package(ctx, package_id)
    evidence_path = f"{_PACKAGE_DIR}/{_safe_slug(package_id)}.json"
    payload = {
        "task_id": ctx.task_id,
        "package_id": package_id,
        "wave": _field(ctx.rendered_prompt or "", "wave"),
        "subsystem": _field(ctx.rendered_prompt or "", "subsystem")
        or package.get("subsystem"),
        "risk": _field(ctx.rendered_prompt or "", "risk") or package.get("risk"),
        "item_count": _field(ctx.rendered_prompt or "", "item_count")
        or len(package.get("item_ids") or ()),
        "item_ids": list(package.get("item_ids") or ()),
        "edited": False,
    }
    return PreparedToolScript(
        name=f"execute_package:{package_id}",
        summary=f"Executed package {package_id} with sandbox evidence.",
        artifact=evidence_path,
        steps=(
            ToolScriptStep(
                "ensure-package-dir",
                shell_tool,
                {"command": f"mkdir -p {_PACKAGE_DIR}", "timeout": 60},
            ),
            ToolScriptStep(
                "write-package-evidence",
                write_file_tool,
                {"file_path": evidence_path, "content": _json(payload)},
            ),
            ToolScriptStep(
                "edit-package-evidence",
                edit_file_tool,
                {
                    "file_path": evidence_path,
                    "old_text": '"edited":false',
                    "new_text": '"edited":true',
                    "description": "mark package evidence as edited",
                },
            ),
            ToolScriptStep(
                "read-package-evidence",
                read_file_tool,
                {"file_path": evidence_path, "start_line": 1, "end_line": 20},
            ),
            ToolScriptStep(
                "check-package-evidence",
                shell_tool,
                {
                    "command": (
                        f"test -s {evidence_path} && "
                        f"printf 'package={package_id}\\n'"
                    ),
                    "timeout": 60,
                },
            ),
        ),
    )


def recursive_step_script(ctx: ScenarioContext) -> PreparedToolScript:
    """Persist evidence for a delegated recursive mission step."""
    task_id = _safe_slug(ctx.task_id or "recursive")
    rendered_prompt = ctx.rendered_prompt or ""
    evidence_path = f"{_RECURSIVE_DIR}/{task_id}.json"
    is_close = "recursive_reconcile" in rendered_prompt
    payload = {
        "task_id": ctx.task_id,
        "action": rendered_prompt,
        "checkpoint": _field(rendered_prompt, "checkpoint"),
        "slice": _field(rendered_prompt, "slice"),
        "close_report": is_close,
    }
    steps: list[ToolScriptStep] = [
        ToolScriptStep(
            "ensure-recursive-dir",
            shell_tool,
            {"command": f"mkdir -p {_RECURSIVE_DIR}", "timeout": 60},
        ),
        ToolScriptStep(
            "write-recursive-evidence",
            write_file_tool,
            {"file_path": evidence_path, "content": _json(payload)},
        ),
        ToolScriptStep(
            "read-recursive-evidence",
            read_file_tool,
            {"file_path": evidence_path, "start_line": 1, "end_line": 20},
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
                            "task_id": ctx.task_id,
                            "status": "recursive-close-ready",
                            "evidence_path": evidence_path,
                        }
                    ),
                },
            )
        )
    steps.append(
        ToolScriptStep(
            "check-recursive-evidence",
            shell_tool,
            {
                "command": f"test -s {evidence_path} && ls -1 {_RECURSIVE_DIR}",
                "timeout": 60,
            },
        )
    )
    return PreparedToolScript(
        name="recursive_step",
        summary="Recursive mission step completed with sandbox evidence.",
        artifact=_RECURSIVE_CLOSE_PATH if is_close else evidence_path,
        steps=tuple(steps),
    )


def final_reconciliation_script(ctx: ScenarioContext) -> PreparedToolScript:
    """Write and verify final release-bundle reconciliation evidence."""
    stage = _field(ctx.rendered_prompt or "", "stage") or "final"
    high_risk_count = _field(ctx.rendered_prompt or "", "high_risk_count")
    payload = {
        "task_id": ctx.task_id,
        "stage": stage,
        "high_risk_count": high_risk_count,
        "package_count": len(_dict_list(ctx.package_plan)),
        "requirement_count": len(_dict_list(ctx.requirement_ledger)),
    }
    stage_path = f"{_ROOT}/final-{_safe_slug(stage)}.json"
    return PreparedToolScript(
        name=f"final_reconciliation:{stage}",
        summary="Final coverage ledger and readback probe passed.",
        artifact=_FINAL_PATH,
        steps=(
            ToolScriptStep(
                "ensure-final-dir",
                shell_tool,
                {"command": f"mkdir -p {_ROOT}", "timeout": 60},
            ),
            ToolScriptStep(
                "write-final-stage",
                write_file_tool,
                {"file_path": stage_path, "content": _json(payload)},
            ),
            ToolScriptStep(
                "read-final-stage",
                read_file_tool,
                {"file_path": stage_path, "start_line": 1, "end_line": 20},
            ),
            ToolScriptStep(
                "write-final-summary",
                write_file_tool,
                {
                    "file_path": _FINAL_PATH,
                    "content": _json(
                        {
                            "task_id": ctx.task_id,
                            "status": "final-reconciliation-ready",
                            "stage": stage,
                            "stage_path": stage_path,
                        }
                    ),
                },
            ),
            ToolScriptStep(
                "check-final-summary",
                shell_tool,
                {
                    "command": (
                        f"test -s {_FINAL_PATH} && "
                        f"find {_ROOT} -maxdepth 2 -type f | sort | head -40"
                    ),
                    "timeout": 60,
                },
            ),
        ),
    )


def verifier_checkpoint_script(ctx: ScenarioContext) -> PreparedToolScript:
    """Run verifier readback checks for the requested checkpoint."""
    checkpoint = _field(ctx.rendered_prompt or "", "checkpoint") or "checkpoint"
    if checkpoint == "inventory":
        read_path = _LEDGER_PATH
    elif checkpoint == "recursive_return":
        read_path = _RECURSIVE_CLOSE_PATH
    elif checkpoint == "final_release":
        read_path = _FINAL_PATH
    else:
        read_path = _LEDGER_PATH
    return PreparedToolScript(
        name=f"verify:{checkpoint}",
        summary=f"Verifier checked checkpoint {checkpoint} with sandbox readback.",
        artifact=read_path,
        steps=(
            ToolScriptStep(
                "read-checkpoint-evidence",
                read_file_tool,
                {"file_path": read_path, "start_line": 1, "end_line": 20},
            ),
            ToolScriptStep(
                "shell-check-checkpoint",
                shell_tool,
                {
                    "command": (
                        f"test -s {read_path} && "
                        f"printf 'checkpoint={checkpoint}\\n' && "
                        f"find .ephemeralos/sweevo-mock -maxdepth 3 -type f | sort | head -40"
                    ),
                    "timeout": 60,
                },
            ),
        ),
    )


async def _emit_text(
    emit: EmitStreamEvent,
    metadata: ExecutionMetadata,
    text: str,
) -> None:
    await emit(
        AssistantTextDelta(
            text=text,
            agent_name=str(metadata.agent_name or ""),
            run_id=_stream_run_id(metadata),
        )
    )


def _stream_run_id(metadata: ExecutionMetadata) -> str:
    return str(
        metadata.get("task_center_task_id")
        or metadata.agent_run_id
        or metadata.get("run_id")
        or ""
    )


def _dict_list(value: Any) -> list[dict[str, Any]]:
    if value is None:
        return []
    items: Sequence[Any] = value if isinstance(value, Sequence) else ()
    return [dict(item) for item in items if isinstance(item, dict)]


def _find_package(ctx: ScenarioContext, package_id: str) -> dict[str, Any]:
    for package in _dict_list(ctx.package_plan):
        if str(package.get("id") or "") == package_id:
            return package
    return {"id": package_id, "item_ids": (), "risk": "unknown"}


def _field(text: str, name: str) -> str | None:
    prefix = f"{name}="
    for part in text.split():
        if part.startswith(prefix):
            return part[len(prefix) :].strip()
    return None


def _safe_slug(value: str) -> str:
    return "".join(ch if ch.isalnum() or ch in "-_" else "_" for ch in value)


def _json(payload: dict[str, Any]) -> str:
    return json.dumps(payload, ensure_ascii=True, sort_keys=True, separators=(",", ":"))


__all__ = [
    "MockToolScriptEngine",
    "PreparedToolScriptEngine",
    "PreparedToolScript",
    "ToolScriptResult",
    "ToolScriptStep",
    "execute_package_script",
    "final_reconciliation_script",
    "inspect_user_input_script",
    "recursive_step_script",
    "verifier_checkpoint_script",
]
