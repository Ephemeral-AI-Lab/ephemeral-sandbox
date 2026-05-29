"""Probe coroutines for the event-source mock path.

Re-homed from the imperative ``MockSquadRunner._run_*_probe`` methods. Each probe
is an async generator that ``yield``s one :class:`ToolCall` per loop turn (driven
by ``scenario_adapter._executor_script``, which is what turns each yield into a
``Turn`` for the real loop) and uses a :class:`ProbeContext` for the out-of-band
sandbox verification the loop does not do for it (direct ``sandbox_api`` edits +
sandbox-check audit records). The ``ToolResult`` sent back into each ``yield`` is
the normalized result of the loop having executed that tool.
"""

from __future__ import annotations

import json
from collections.abc import AsyncGenerator
from typing import Any

import sandbox.api as sandbox_api
from sandbox.api import EditFileRequest, SandboxCaller, SearchReplaceEdit
from tools import ToolResult

from task_center_runner.audit.events import Event, EventType
from task_center_runner.audit.legacy import LegacySandboxAuditSink
from task_center_runner.audit.node_id import NodeId
from task_center_runner.agent.mock.event_source import ToolCall
from task_center_runner.agent.mock.sandbox_probe import SandboxCheck

# A probe yields one ToolCall and is resumed with the normalized ToolResult.
ProbeStep = AsyncGenerator[ToolCall, ToolResult]

_PROBE_DIR = ".ephemeralos/sweevo-mock"
_PROBE_PATH = f"{_PROBE_DIR}/probe.txt"


class ProbeContext:
    """Out-of-band sandbox helpers a probe coroutine needs.

    Carries the live ``tool_metadata`` (sandbox id, caller identity), the repo
    root (for absolute paths), and the audit bus (for sandbox-check records +
    re-homed ``SANDBOX_*`` events). Tool *calls* go through the loop via the
    probe's ``yield ToolCall``; only the direct sandbox verification lives here.
    """

    def __init__(self, *, metadata: Any, repo_dir: str, bus: Any | None) -> None:
        self._metadata = metadata
        self._repo_dir = repo_dir
        self._bus = bus
        self._sink = LegacySandboxAuditSink(bus) if bus is not None else None

    @property
    def metadata(self) -> Any:
        """The live loop ``tool_metadata`` (heavy probes pass it to call_tool)."""
        return self._metadata

    def probe_path(self) -> str:
        return _PROBE_PATH

    def _absolute_probe_path(self, path: str) -> str:
        if path.startswith("/"):
            return path
        return f"{self._repo_dir.rstrip('/')}/{path}"

    def _require_sandbox_id(self) -> str:
        sandbox_id = str(self._metadata.get("sandbox_id") or "").strip()
        if not sandbox_id:
            raise RuntimeError("Sandbox id is required for SWE-EVO sandbox checks.")
        return sandbox_id

    def _caller(self) -> SandboxCaller:
        md = self._metadata
        return SandboxCaller(
            agent_id=str(md.agent_name or "sweevo-mock"),
            run_id=str(md.get("run_id") or ""),
            agent_run_id=str(md.agent_run_id or ""),
            task_id=str(md.get("task_center_task_id") or ""),
            task_center_run_id=str(md.get("task_center_run_id") or ""),
            task_center_task_id=str(md.get("task_center_task_id") or ""),
            task_center_attempt_id=str(md.get("task_center_attempt_id") or ""),
            task_center_workflow_id=str(md.get("task_center_workflow_id") or ""),
            task_center_request_id=str(md.get("task_center_request_id") or ""),
            tool_id=str(md.get("tool_use_id") or ""),
        )

    def _publish(self, event_type: EventType, payload: dict[str, Any]) -> None:
        if self._bus is None:
            return
        self._bus.publish(
            Event(type=event_type, node=NodeId(task_center_run_id=""), payload=payload)
        )

    def publish(
        self,
        event_type: EventType,
        *,
        metadata: Any = None,  # noqa: ARG002 — node identity is re-homed off the bus
        payload: dict[str, Any] | None = None,
    ) -> None:
        """``publish`` callback the heavy probes expect (re-homed SANDBOX_* events)."""
        self._publish(event_type, payload or {})

    def publish_mock_record(self, event_type: EventType, record: Any) -> None:
        """``publish_mock_record`` callback: publish a dataclass record to the bus."""
        if self._bus is None:
            return
        import dataclasses

        payload = (
            dataclasses.asdict(record)
            if dataclasses.is_dataclass(record) and not isinstance(record, type)
            else dict(record)
        )
        self._bus.publish(
            Event(type=event_type, node=NodeId(task_center_run_id=""), payload=payload)
        )

    def _publish_check(self, check: SandboxCheck) -> None:
        if self._bus is None:
            return
        import dataclasses

        self._bus.publish(
            Event(
                type=EventType.MOCK_SANDBOX_CHECK_RECORDED,
                node=NodeId(task_center_run_id=""),
                payload=dataclasses.asdict(check),
            )
        )

    def record_check(self, name: str, result: ToolResult) -> None:
        changed_paths = tuple(
            str(path) for path in (result.metadata or {}).get("changed_paths", ())
        )
        status = str((result.metadata or {}).get("status") or "ok")
        self._publish_check(
            SandboxCheck(
                name=name,
                passed=not result.is_error,
                detail=status,
                changed_paths=changed_paths,
            )
        )

    def assert_read_contains(
        self, result: ToolResult, needle: str, check_name: str
    ) -> None:
        try:
            payload = json.loads(result.output)
        except json.JSONDecodeError:
            payload = {"content": result.output}
        content = str(payload.get("content") or "")
        passed = needle in content
        self._publish_check(
            SandboxCheck(name=check_name, passed=passed, detail=f"needle={needle!r}")
        )
        if not passed:
            raise RuntimeError(f"{check_name} did not find {needle!r}.")

    async def run_batch_edit(self, probe_path: str) -> None:
        result = await sandbox_api.edit_file(
            self._require_sandbox_id(),
            EditFileRequest(
                path=self._absolute_probe_path(probe_path),
                edits=(
                    SearchReplaceEdit(old_text="alpha\n", new_text="alpha-batch\n"),
                    SearchReplaceEdit(old_text="beta-edited\n", new_text="beta-batch\n"),
                ),
                caller=self._caller(),
                description="batch edit for mock SWE-EVO probe",
            ),
            audit_sink=self._sink,
        )
        passed = result.success and result.applied_edits == 2
        self._publish_check(
            SandboxCheck(
                name="api.edit_file.batch",
                passed=passed,
                detail=f"applied_edits={result.applied_edits} status={result.status}",
                changed_paths=tuple(result.changed_paths),
            )
        )
        if passed:
            self._publish(
                EventType.SANDBOX_BATCH_EDIT_APPLIED,
                {"applied_edits": result.applied_edits},
            )
        else:
            raise RuntimeError("Batch edit did not apply both replacements.")

    async def run_expected_conflict(self, probe_path: str) -> None:
        result = await sandbox_api.edit_file(
            self._require_sandbox_id(),
            EditFileRequest(
                path=self._absolute_probe_path(probe_path),
                edits=(
                    SearchReplaceEdit(
                        old_text="missing-old-text\n", new_text="should-not-apply\n"
                    ),
                ),
                caller=self._caller(),
                description="expected conflict for mock SWE-EVO probe",
            ),
            audit_sink=self._sink,
        )
        passed = not result.success
        detail = result.conflict_reason or result.status or "conflict reported"
        self._publish_check(
            SandboxCheck(
                name="api.edit_file.conflict_detection",
                passed=passed,
                detail=detail,
                changed_paths=tuple(result.changed_paths),
            )
        )
        if passed:
            self._publish(
                EventType.SANDBOX_CONFLICT_DETECTED, {"conflict_reason": detail}
            )
        else:
            raise RuntimeError("Expected conflict edit unexpectedly succeeded.")


async def preflight_probe(ctx: ProbeContext) -> ProbeStep:
    result = yield ToolCall(
        "shell",
        {"command": "pwd && git rev-parse --is-inside-work-tree", "timeout": 60},
    )
    ctx.record_check("tool.shell.preflight", result)


async def sandbox_integrity_probe(ctx: ProbeContext) -> ProbeStep:
    probe_path = ctx.probe_path()

    mkdir = yield ToolCall(
        "shell",
        {
            "command": (
                f"mkdir -p {_PROBE_DIR} && "
                f"printf 'shell-created\\n' > {_PROBE_DIR}/shell.txt"
            ),
            "timeout": 60,
        },
    )
    ctx.record_check("tool.shell.gated_merge", mkdir)

    written = yield ToolCall(
        "write_file", {"file_path": probe_path, "content": "alpha\nbeta\n"}
    )
    ctx.record_check("tool.write_file.direct_merge", written)

    first_read = yield ToolCall(
        "read_file", {"file_path": probe_path, "start_line": 1, "end_line": 20}
    )
    ctx.assert_read_contains(first_read, "alpha", "tool.read_file.after_write")

    edited = yield ToolCall(
        "edit_file",
        {
            "file_path": probe_path,
            "old_text": "beta\n",
            "new_text": "beta-edited\n",
            "description": "single edit for mock SWE-EVO probe",
        },
    )
    ctx.record_check("tool.edit_file.direct_merge", edited)

    await ctx.run_batch_edit(probe_path)
    await ctx.run_expected_conflict(probe_path)

    squash = yield ToolCall(
        "shell",
        {"command": f"printf 'squash-check\\n' >> {probe_path}", "timeout": 60},
    )
    ctx.record_check("tool.shell.squash_append", squash)

    final_read = yield ToolCall(
        "read_file", {"file_path": probe_path, "start_line": 1, "end_line": 20}
    )
    ctx.assert_read_contains(final_read, "squash-check", "tool.read_file.after_squash")


async def final_probe(ctx: ProbeContext) -> ProbeStep:
    final_read = yield ToolCall(
        "read_file", {"file_path": ctx.probe_path(), "start_line": 1, "end_line": 20}
    )
    ctx.assert_read_contains(final_read, "squash-check", "tool.read_file.final_probe")
    verify = yield ToolCall(
        "shell", {"command": f"grep -q 'squash-check' {ctx.probe_path()}", "timeout": 60}
    )
    ctx.record_check("tool.shell.final_probe", verify)


PROBE_BUILDERS = {
    "preflight": preflight_probe,
    "sandbox_integrity": sandbox_integrity_probe,
    "final_probe": final_probe,
}

PROBE_SUMMARY = {
    "preflight": "Workspace preflight completed.",
    "sandbox_integrity": "Sandbox integrity probe passed.",
    "final_probe": "Continuation final probe passed.",
}


__all__ = [
    "PROBE_BUILDERS",
    "PROBE_SUMMARY",
    "ProbeContext",
    "ProbeStep",
    "final_probe",
    "preflight_probe",
    "sandbox_integrity_probe",
]
