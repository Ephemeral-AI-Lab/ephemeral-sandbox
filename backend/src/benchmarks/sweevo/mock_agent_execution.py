"""Mock-agent TaskCenter integration runner for SWE-EVO benchmarks.

The runner uses real TaskCenter orchestration, real context composition, real
sandbox tool wrappers, and real terminal submission tools. Only the model loop
is replaced by deterministic role handlers so benchmark setup can validate the
runtime and sandbox boundaries without spending model tokens.
"""

from __future__ import annotations

import contextlib
import json
import time
from collections.abc import Iterator
from dataclasses import asdict, dataclass, field
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import sandbox.api as sandbox_api
from agents import (
    AgentDefinition,
    list_definitions,
    register_definition,
    unregister_definition,
)
from db.base import Base
import db.models  # noqa: F401 - populate SQLAlchemy metadata
from db.stores.attempt_store import AttemptStore
from db.stores.context_packet_store import ContextPacketStore
from db.stores.episode_store import EpisodeStore
from db.stores.mission_store import MissionStore
from db.stores.task_center_store import TaskCenterStore
from engine.api import EphemeralRunResult
from sandbox.api import (
    EditFileRequest,
    SandboxCaller,
    SearchReplaceEdit,
)
from sqlalchemy import create_engine
from sqlalchemy.engine import Engine
from sqlalchemy.orm import sessionmaker
from task_center.api import TaskCenterSandboxBridge, start_task_center_entry_run
from task_center.attempt import Attempt
from task_center.episode.episode import Episode
from tools.core.base import BaseTool
from tools.core.context import ToolExecutionContextService
from tools.core.results import ToolResult
from tools.core.runtime import ExecutionMetadata
from tools.execution.tool_call import execute_tool_once
from tools.sandbox_toolkit.edit_file import edit_file as edit_file_tool
from tools.sandbox_toolkit.read_file import read_file as read_file_tool
from tools.sandbox_toolkit.shell import shell as shell_tool
from tools.sandbox_toolkit.write_file import write_file as write_file_tool
from tools.submission.main_agent.evaluator import (
    submit_evaluation_failure,
    submit_evaluation_success,
)
from tools.submission.main_agent.generator.executor import (
    submit_execution_success,
)
from tools.submission.main_agent.generator.request_mission_solution import (
    request_mission_solution,
)
from tools.submission.main_agent.planner import (
    submit_full_plan,
    submit_partial_plan,
)

from benchmarks.sweevo.dataset import summarize_sweevo_instance
from benchmarks.sweevo.models import SWEEvoInstance, _REPO_DIR


@dataclass(frozen=True, slots=True)
class PromptInspection:
    task_id: str
    agent_name: str
    role: str
    checks: dict[str, bool]
    justification: str

    @property
    def passed(self) -> bool:
        return all(self.checks.values())

    def as_dict(self) -> dict[str, Any]:
        payload = asdict(self)
        payload["passed"] = self.passed
        return payload


@dataclass(frozen=True, slots=True)
class SandboxCheck:
    name: str
    passed: bool
    detail: str
    changed_paths: tuple[str, ...] = ()

    def as_dict(self) -> dict[str, Any]:
        return {
            "name": self.name,
            "passed": self.passed,
            "detail": self.detail,
            "changed_paths": list(self.changed_paths),
        }


@dataclass(frozen=True, slots=True)
class LaunchRecord:
    task_id: str
    attempt_id: str | None
    agent_name: str
    role: str
    prompt_preview: str

    def as_dict(self) -> dict[str, Any]:
        return asdict(self)


@dataclass(frozen=True, slots=True)
class ToolCallRecord:
    task_id: str
    tool_name: str
    is_error: bool
    metadata: dict[str, Any] = field(default_factory=dict)

    def as_dict(self) -> dict[str, Any]:
        return asdict(self)


@dataclass(slots=True)
class TaskCenterStoreBundle:
    engine: Engine
    task_store: TaskCenterStore
    mission_store: MissionStore
    episode_store: EpisodeStore
    attempt_store: AttemptStore
    context_packet_store: ContextPacketStore

    def close(self) -> None:
        self.engine.dispose()


def create_in_memory_task_center_stores() -> TaskCenterStoreBundle:
    """Create isolated real TaskCenter stores for a benchmark run."""
    engine = create_engine("sqlite:///:memory:")
    Base.metadata.create_all(engine)
    session_factory = sessionmaker(
        bind=engine,
        autoflush=False,
        expire_on_commit=False,
    )

    task_store = TaskCenterStore()
    mission_store = MissionStore()
    episode_store = EpisodeStore()
    attempt_store = AttemptStore()
    context_packet_store = ContextPacketStore()
    for store in (
        task_store,
        mission_store,
        episode_store,
        attempt_store,
        context_packet_store,
    ):
        store.initialize(session_factory)

    return TaskCenterStoreBundle(
        engine=engine,
        task_store=task_store,
        mission_store=mission_store,
        episode_store=episode_store,
        attempt_store=attempt_store,
        context_packet_store=context_packet_store,
    )


@contextlib.contextmanager
def registered_mock_sweevo_agents() -> Iterator[None]:
    """Temporarily install the minimal TaskCenter squad definitions."""
    previous = list_definitions()
    for definition in previous:
        unregister_definition(definition.name)

    for definition in _mock_agent_definitions():
        register_definition(definition)

    try:
        yield
    finally:
        for definition in list_definitions():
            unregister_definition(definition.name)
        for definition in previous:
            register_definition(definition)


def _mock_agent_definitions() -> tuple[AgentDefinition, ...]:
    return (
        AgentDefinition(
            name="entry_executor",
            description="SWE-EVO mock entry executor",
            role="executor",
            context_recipe="entry_executor_v1",
            terminals=[
                "request_mission_solution",
                "submit_execution_success",
                "submit_execution_failure",
            ],
        ),
        AgentDefinition(
            name="planner",
            description="SWE-EVO mock planner",
            role="planner",
            context_recipe="planner_v1",
            terminals=["submit_full_plan", "submit_partial_plan"],
        ),
        AgentDefinition(
            name="executor",
            description="SWE-EVO mock executor",
            role="executor",
            context_recipe="generator_v1",
            allowed_tools=["read_file", "write_file", "edit_file", "shell"],
            terminals=[
                "request_mission_solution",
                "submit_execution_success",
                "submit_execution_failure",
            ],
        ),
        AgentDefinition(
            name="evaluator",
            description="SWE-EVO mock evaluator",
            role="evaluator",
            context_recipe="evaluator_v1",
            terminals=["submit_evaluation_success", "submit_evaluation_failure"],
        ),
    )


async def _noop_emit(_event: Any) -> None:
    return None


class MockSWEvoAgentExecution:
    """Deterministic agent execution handlers that call real tools."""

    def __init__(self, *, instance: SWEEvoInstance, repo_dir: str = _REPO_DIR) -> None:
        self._instance = instance
        self._repo_dir = repo_dir
        self.launches: list[LaunchRecord] = []
        self.tool_calls: list[ToolCallRecord] = []
        self.prompt_inspections: list[PromptInspection] = []
        self.sandbox_checks: list[SandboxCheck] = []

    async def __call__(
        self,
        config: Any,
        prompt: str,
        *,
        agent_def: AgentDefinition | None = None,
        sandbox_id: str | None = None,
        extra_tool_metadata: ExecutionMetadata | dict[str, Any] | None = None,
        **_kwargs: Any,
    ) -> EphemeralRunResult:
        if agent_def is None:
            raise RuntimeError("MockSWEvoAgentExecution requires agent_def.")

        metadata = self._metadata_for(
            config=config,
            agent_def=agent_def,
            sandbox_id=sandbox_id,
            extra_tool_metadata=extra_tool_metadata,
        )
        task_id = str(metadata.get("task_center_task_id") or "")
        attempt_id = str(metadata.get("task_center_attempt_id") or "") or None
        self.launches.append(
            LaunchRecord(
                task_id=task_id,
                attempt_id=attempt_id,
                agent_name=agent_def.name,
                role=str(agent_def.role or ""),
                prompt_preview=prompt[:500],
            )
        )
        self.prompt_inspections.append(
            self._inspect_prompt(
                prompt=prompt,
                agent_def=agent_def,
                metadata=metadata,
            )
        )

        if agent_def.name == "entry_executor":
            terminal = await self._run_entry_executor(prompt, metadata)
        elif agent_def.role == "planner":
            terminal = await self._run_planner(metadata)
        elif agent_def.role == "executor":
            terminal = await self._run_executor(prompt, metadata)
        elif agent_def.role == "evaluator":
            terminal = await self._run_evaluator(metadata)
        else:
            raise RuntimeError(f"Unsupported SWE-EVO mock agent role: {agent_def.role!r}")

        return EphemeralRunResult(
            status="completed",
            error=None,
            terminal_result=terminal,
            agent_name=agent_def.name,
            event_count=1,
        )

    def _metadata_for(
        self,
        *,
        config: Any,
        agent_def: AgentDefinition,
        sandbox_id: str | None,
        extra_tool_metadata: ExecutionMetadata | dict[str, Any] | None,
    ) -> ExecutionMetadata:
        if isinstance(extra_tool_metadata, ExecutionMetadata):
            metadata = extra_tool_metadata.copy()
        else:
            metadata = ExecutionMetadata()
            metadata.update(extra_tool_metadata or {})

        metadata.sandbox_id = str(sandbox_id or metadata.sandbox_id or "")
        metadata.agent_name = agent_def.name
        metadata.repo_root = self._repo_dir
        metadata.cwd = str(getattr(config, "cwd", self._repo_dir) or self._repo_dir)
        metadata.exec_cwd = self._repo_dir
        metadata["role"] = str(agent_def.role or "")
        metadata["agent_type"] = agent_def.agent_type
        metadata["run_id"] = str(metadata.task_center_run_id or "")
        metadata["task_id"] = str(metadata.task_center_task_id or "")
        return metadata

    async def _run_entry_executor(
        self,
        prompt: str,
        metadata: ExecutionMetadata,
    ) -> ToolResult:
        return await self._call_tool(
            request_mission_solution,
            {"goal": prompt},
            metadata,
        )

    async def _run_planner(self, metadata: ExecutionMetadata) -> ToolResult:
        attempt, episode = self._current_attempt_and_episode(metadata)
        if episode.sequence_no == 1 and attempt.attempt_sequence_no == 1:
            return await self._call_tool(
                submit_full_plan,
                {
                    "task_specification": (
                        "Preflight the SWE-EVO workspace and expose an evaluator "
                        "retry signal without making benchmark source edits."
                    ),
                    "evaluation_criteria": [
                        "Workspace preflight completed.",
                        "Retry path was exercised by evaluator feedback.",
                    ],
                    "tasks": [{"id": "preflight", "agent_name": "executor", "deps": []}],
                    "task_specs": {
                        "preflight": (
                            "Run a lightweight workspace preflight and report the "
                            "observed sandbox root."
                        )
                    },
                },
                metadata,
            )

        if episode.sequence_no == 1:
            return await self._call_tool(
                submit_partial_plan,
                {
                    "task_specification": (
                        "Validate sandbox read/write/edit/shell consistency, direct "
                        "OCC file mutation, gated shell mutation, batch edit handling, "
                        "and conflict reporting for the SWE-EVO workspace."
                    ),
                    "evaluation_criteria": [
                        "Dedicated sandbox tools can read, write, edit, and run shell.",
                        "Final file content survives the shell/OCC squash boundary.",
                        "Batch edit succeeds and a stale edit reports conflict.",
                    ],
                    "tasks": [
                        {
                            "id": "sandbox_integrity",
                            "agent_name": "executor",
                            "deps": [],
                        }
                    ],
                    "task_specs": {
                        "sandbox_integrity": (
                            "Exercise the sandbox filesystem with write_file, "
                            "read_file, edit_file, shell, a batch public edit, and "
                            "an expected conflict."
                        )
                    },
                    "continuation_goal": (
                        "Run the final SWE-EVO mock grading episode after sandbox "
                        "integrity evidence has been persisted."
                    ),
                },
                metadata,
            )

        return await self._call_tool(
            submit_full_plan,
            {
                "task_specification": (
                    "Confirm the sandbox integrity artifacts remain readable in the "
                    "continuation episode and close the benchmark mission."
                ),
                "evaluation_criteria": [
                    "Continuation episode received previous episode context.",
                    "Persisted sandbox evidence is readable from the workspace.",
                ],
                "tasks": [{"id": "final_probe", "agent_name": "executor", "deps": []}],
                "task_specs": {
                    "final_probe": (
                        "Read the sandbox integrity artifact and verify the final "
                        "squash marker is still present."
                    )
                },
            },
            metadata,
        )

    async def _run_executor(
        self,
        prompt: str,
        metadata: ExecutionMetadata,
    ) -> ToolResult:
        if "sandbox filesystem" in prompt or "sandbox read/write/edit" in prompt:
            await self._run_sandbox_integrity_probe(metadata)
            summary = "Sandbox integrity probe passed."
            artifacts = [self._probe_path()]
        elif "squash marker" in prompt:
            await self._run_final_probe(metadata)
            summary = "Continuation final probe passed."
            artifacts = [self._probe_path()]
        else:
            await self._run_preflight_probe(metadata)
            summary = "Workspace preflight completed."
            artifacts = []

        return await self._call_tool(
            submit_execution_success,
            {"summary": summary, "artifacts": artifacts},
            metadata,
        )

    async def _run_evaluator(self, metadata: ExecutionMetadata) -> ToolResult:
        attempt, episode = self._current_attempt_and_episode(metadata)
        if episode.sequence_no == 1 and attempt.attempt_sequence_no == 1:
            return await self._call_tool(
                submit_evaluation_failure,
                {
                    "summary": (
                        "Intentional mock failure to verify episode retry and "
                        "failed-attempt context."
                    ),
                    "failed_criteria": ["Retry path was exercised by evaluator feedback."],
                },
                metadata,
            )

        return await self._call_tool(
            submit_evaluation_success,
            {
                "summary": "Mock evaluator accepted the current attempt evidence.",
                "passed_criteria": list(attempt.evaluation_criteria),
            },
            metadata,
        )

    async def _run_preflight_probe(self, metadata: ExecutionMetadata) -> None:
        result = await self._call_tool(
            shell_tool,
            {"command": "pwd && git rev-parse --is-inside-work-tree", "timeout": 60},
            metadata,
        )
        self._record_tool_check("tool.shell.preflight", result)

    async def _run_sandbox_integrity_probe(self, metadata: ExecutionMetadata) -> None:
        probe_dir = ".ephemeralos/sweevo-mock"
        probe_path = self._probe_path()

        mkdir = await self._call_tool(
            shell_tool,
            {
                "command": (
                    f"mkdir -p {probe_dir} && "
                    f"printf 'shell-created\\n' > {probe_dir}/shell.txt"
                ),
                "timeout": 60,
            },
            metadata,
        )
        self._record_tool_check("tool.shell.gated_merge", mkdir)

        written = await self._call_tool(
            write_file_tool,
            {
                "file_path": probe_path,
                "content": "alpha\nbeta\n",
            },
            metadata,
        )
        self._record_tool_check("tool.write_file.direct_merge", written)

        first_read = await self._call_tool(
            read_file_tool,
            {"file_path": probe_path, "start_line": 1, "end_line": 20},
            metadata,
        )
        self._assert_read_contains(first_read, "alpha", "tool.read_file.after_write")

        edited = await self._call_tool(
            edit_file_tool,
            {
                "file_path": probe_path,
                "old_text": "beta\n",
                "new_text": "beta-edited\n",
                "description": "single edit for mock SWE-EVO probe",
            },
            metadata,
        )
        self._record_tool_check("tool.edit_file.direct_merge", edited)

        await self._run_batch_edit(metadata, probe_path)
        await self._run_expected_conflict(metadata, probe_path)

        squash = await self._call_tool(
            shell_tool,
            {
                "command": f"printf 'squash-check\\n' >> {probe_path}",
                "timeout": 60,
            },
            metadata,
        )
        self._record_tool_check("tool.shell.squash_append", squash)

        final_read = await self._call_tool(
            read_file_tool,
            {"file_path": probe_path, "start_line": 1, "end_line": 20},
            metadata,
        )
        self._assert_read_contains(final_read, "squash-check", "tool.read_file.after_squash")

    async def _run_batch_edit(
        self,
        metadata: ExecutionMetadata,
        probe_path: str,
    ) -> None:
        sandbox_id = self._require_sandbox_id(metadata)
        result = await sandbox_api.edit_file(
            sandbox_id,
            EditFileRequest(
                path=self._absolute_probe_path(probe_path),
                edits=(
                    SearchReplaceEdit(old_text="alpha\n", new_text="alpha-batch\n"),
                    SearchReplaceEdit(
                        old_text="beta-edited\n",
                        new_text="beta-batch\n",
                    ),
                ),
                caller=self._caller(metadata),
                description="batch edit for mock SWE-EVO probe",
            ),
        )
        passed = result.success and result.applied_edits == 2
        self.sandbox_checks.append(
            SandboxCheck(
                name="api.edit_file.batch",
                passed=passed,
                detail=f"applied_edits={result.applied_edits} status={result.status}",
                changed_paths=tuple(result.changed_paths),
            )
        )
        if not passed:
            raise RuntimeError("Batch edit did not apply both replacements.")

    async def _run_expected_conflict(
        self,
        metadata: ExecutionMetadata,
        probe_path: str,
    ) -> None:
        sandbox_id = self._require_sandbox_id(metadata)
        result = await sandbox_api.edit_file(
            sandbox_id,
            EditFileRequest(
                path=self._absolute_probe_path(probe_path),
                edits=(
                    SearchReplaceEdit(
                        old_text="missing-old-text\n",
                        new_text="should-not-apply\n",
                    ),
                ),
                caller=self._caller(metadata),
                description="expected conflict for mock SWE-EVO probe",
            ),
        )
        passed = not result.success
        detail = result.conflict_reason or result.status or "conflict reported"
        self.sandbox_checks.append(
            SandboxCheck(
                name="api.edit_file.conflict_detection",
                passed=passed,
                detail=detail,
                changed_paths=tuple(result.changed_paths),
            )
        )
        if not passed:
            raise RuntimeError("Expected conflict edit unexpectedly succeeded.")

    async def _run_final_probe(self, metadata: ExecutionMetadata) -> None:
        final_read = await self._call_tool(
            read_file_tool,
            {"file_path": self._probe_path(), "start_line": 1, "end_line": 20},
            metadata,
        )
        self._assert_read_contains(final_read, "squash-check", "tool.read_file.final_probe")
        verify = await self._call_tool(
            shell_tool,
            {
                "command": f"grep -q 'squash-check' {self._probe_path()}",
                "timeout": 60,
            },
            metadata,
        )
        self._record_tool_check("tool.shell.final_probe", verify)

    async def _call_tool(
        self,
        tool_obj: BaseTool,
        raw_input: dict[str, Any],
        metadata: ExecutionMetadata,
    ) -> ToolResult:
        result = await execute_tool_once(
            tool_obj,
            raw_input,
            ToolExecutionContextService(cwd=Path(self._repo_dir), services=metadata),
            emit=_noop_emit,
            emit_started=False,
        )
        self.tool_calls.append(
            ToolCallRecord(
                task_id=str(metadata.get("task_center_task_id") or ""),
                tool_name=tool_obj.name,
                is_error=result.is_error,
                metadata=dict(result.metadata or {}),
            )
        )
        if result.is_error:
            raise RuntimeError(f"{tool_obj.name} failed: {result.output}")
        return result

    def _record_tool_check(self, name: str, result: ToolResult) -> None:
        changed_paths = tuple(str(path) for path in result.metadata.get("changed_paths", ()))
        status = str(result.metadata.get("status") or "ok")
        self.sandbox_checks.append(
            SandboxCheck(
                name=name,
                passed=not result.is_error,
                detail=status,
                changed_paths=changed_paths,
            )
        )

    def _assert_read_contains(
        self,
        result: ToolResult,
        needle: str,
        check_name: str,
    ) -> None:
        try:
            payload = json.loads(result.output)
        except json.JSONDecodeError:
            payload = {"content": result.output}
        content = str(payload.get("content") or "")
        passed = needle in content
        self.sandbox_checks.append(
            SandboxCheck(
                name=check_name,
                passed=passed,
                detail=f"needle={needle!r}",
            )
        )
        if not passed:
            raise RuntimeError(f"{check_name} did not find {needle!r}.")

    def _inspect_prompt(
        self,
        *,
        prompt: str,
        agent_def: AgentDefinition,
        metadata: ExecutionMetadata,
    ) -> PromptInspection:
        role = str(agent_def.role or "")
        checks: dict[str, bool]
        reason: str
        if agent_def.name == "entry_executor":
            checks = {
                "entry_request_heading": "# Entry request" in prompt,
                "workspace_root": self._repo_dir in prompt,
                "pr_description": "<pr_description>" in prompt,
            }
            reason = (
                "Entry executor receives the exact SWE-EVO user request as a "
                "required entry_request block before it delegates the mission."
            )
        elif role == "planner":
            attempt, episode = self._current_attempt_and_episode(metadata)
            checks = {
                "mission": "# Mission" in prompt,
                "current_episode": (
                    "# Current Episode" in prompt
                    or "# Mission / Current Episode" in prompt
                ),
            }
            if attempt.attempt_sequence_no > 1:
                checks["failed_attempts"] = "# Failed Attempts" in prompt
            if episode.sequence_no > 1:
                checks["previous_episode_results"] = "# Previous Episode Results" in prompt
            reason = (
                "Planner context is mission and episode scoped; retry planners "
                "also receive failed-attempt evidence, and continuation planners "
                "receive previous episode results."
            )
        elif role == "executor":
            checks = {
                "attempt_plan": "# Attempt Plan" in prompt,
                "assigned_task": "# Assigned Task" in prompt,
            }
            reason = (
                "Executor context is local to the current planned task with the "
                "attempt contract as framing."
            )
        elif role == "evaluator":
            checks = {
                "attempt_plan": "# Attempt Plan" in prompt,
                "dependency_results": "# Dependency Results" in prompt,
                "evaluation_criteria": "# Evaluation Criteria" in prompt,
            }
            reason = (
                "Evaluator context is graph-local: attempt contract, completed "
                "generator evidence, and the criteria it must judge."
            )
        else:
            checks = {"known_role": False}
            reason = f"Unknown role {role!r}."

        return PromptInspection(
            task_id=str(metadata.get("task_center_task_id") or ""),
            agent_name=agent_def.name,
            role=role,
            checks=checks,
            justification=reason,
        )

    def _current_attempt_and_episode(
        self,
        metadata: ExecutionMetadata,
    ) -> tuple[Attempt, Episode]:
        runtime = metadata.get("attempt_runtime")
        if runtime is None:
            raise RuntimeError("Missing AttemptRuntime in mocked agent metadata.")
        attempt_id = str(metadata.get("task_center_attempt_id") or "")
        attempt = runtime.attempt_store.get(attempt_id)
        if attempt is None:
            raise RuntimeError(f"Attempt {attempt_id!r} not found.")
        episode = runtime.episode_store.get(attempt.episode_id)
        if episode is None:
            raise RuntimeError(f"Episode {attempt.episode_id!r} not found.")
        return attempt, episode

    def _probe_path(self) -> str:
        return ".ephemeralos/sweevo-mock/probe.txt"

    def _absolute_probe_path(self, path: str) -> str:
        if path.startswith("/"):
            return path
        return f"{self._repo_dir.rstrip('/')}/{path}"

    @staticmethod
    def _require_sandbox_id(metadata: ExecutionMetadata) -> str:
        sandbox_id = str(metadata.get("sandbox_id") or "").strip()
        if not sandbox_id:
            raise RuntimeError("Sandbox id is required for SWE-EVO sandbox checks.")
        return sandbox_id

    def _caller(self, metadata: ExecutionMetadata) -> SandboxCaller:
        return SandboxCaller(
            agent_id=str(metadata.agent_name or "sweevo-mock"),
            run_id=str(metadata.get("run_id") or ""),
            agent_run_id=str(metadata.agent_run_id or ""),
            task_id=str(metadata.get("task_center_task_id") or ""),
        )


async def run_sweevo_task_center_with_mock_agent_execution(
    *,
    instance: SWEEvoInstance,
    user_prompt: str,
    sandbox_id: str,
    repo_dir: str = _REPO_DIR,
    stores: TaskCenterStoreBundle | None = None,
) -> dict[str, Any]:
    """Run one SWE-EVO prompt through real TaskCenter with mocked agent execution."""
    owns_stores = stores is None
    bundle = stores or create_in_memory_task_center_stores()
    squad = MockSWEvoAgentExecution(instance=instance, repo_dir=repo_dir)
    started = time.perf_counter()
    try:
        with registered_mock_sweevo_agents():
            handle = start_task_center_entry_run(
                config=SimpleNamespace(cwd=repo_dir),
                prompt=user_prompt,
                sandbox_id=sandbox_id,
                on_agent_event=None,
                task_store=bundle.task_store,
                mission_store=bundle.mission_store,
                episode_store=bundle.episode_store,
                attempt_store=bundle.attempt_store,
                runner=squad,
                context_packet_store=bundle.context_packet_store,
                sandbox_bridge=TaskCenterSandboxBridge(
                    start_fn=lambda existing_id: {"id": existing_id}
                ),
            )
            await handle.launcher.wait_for_idle()

        run = bundle.task_store.get_run(handle.task_center_run_id) or {}
        tasks = bundle.task_store.list_tasks_for_run(handle.task_center_run_id)
        failed_prompt_reviews = [
            item.as_dict() for item in squad.prompt_inspections if not item.passed
        ]
        failed_sandbox_checks = [
            item.as_dict() for item in squad.sandbox_checks if not item.passed
        ]
        if failed_prompt_reviews:
            raise RuntimeError(
                "SWE-EVO prompt context inspection failed: "
                f"{failed_prompt_reviews}"
            )
        if failed_sandbox_checks:
            raise RuntimeError(
                "SWE-EVO sandbox integrity checks failed: "
                f"{failed_sandbox_checks}"
            )

        return {
            "instance": summarize_sweevo_instance(instance),
            "request_id": handle.request_id,
            "task_center_run_id": handle.task_center_run_id,
            "task_center_status": run.get("status"),
            "sandbox_id": handle.sandbox_id,
            "duration_s": time.perf_counter() - started,
            "task_count": len(tasks),
            "tasks_completed": sum(1 for task in tasks if task.get("status") == "done"),
            "tasks_failed": sum(1 for task in tasks if task.get("status") == "failed"),
            "agent_events": len(squad.launches) + len(squad.tool_calls),
            "launches": [item.as_dict() for item in squad.launches],
            "tool_calls": [item.as_dict() for item in squad.tool_calls],
            "prompt_inspections": [
                item.as_dict() for item in squad.prompt_inspections
            ],
            "sandbox_checks": [item.as_dict() for item in squad.sandbox_checks],
            "graph": _graph_summary(bundle, handle.task_center_run_id),
        }
    finally:
        if owns_stores:
            bundle.close()


def _graph_summary(
    stores: TaskCenterStoreBundle,
    task_center_run_id: str,
) -> dict[str, Any]:
    missions: list[dict[str, Any]] = []
    for mission in stores.mission_store.list_for_run(task_center_run_id):
        episodes: list[dict[str, Any]] = []
        for episode in stores.episode_store.list_for_mission(mission.id):
            attempts: list[dict[str, Any]] = []
            for attempt in stores.attempt_store.list_for_episode(episode.id):
                attempts.append(
                    {
                        "id": attempt.id,
                        "sequence_no": attempt.attempt_sequence_no,
                        "stage": attempt.stage.value,
                        "status": attempt.status.value,
                        "fail_reason": (
                            attempt.fail_reason.value
                            if attempt.fail_reason is not None
                            else None
                        ),
                        "continuation_goal": attempt.continuation_goal,
                        "task_ids": list(attempt.generator_task_ids),
                    }
                )
            episodes.append(
                {
                    "id": episode.id,
                    "sequence_no": episode.sequence_no,
                    "creation_reason": episode.creation_reason.value,
                    "status": episode.status.value,
                    "goal": episode.goal,
                    "continuation_goal": episode.continuation_goal,
                    "attempts": attempts,
                }
            )
        missions.append(
            {
                "id": mission.id,
                "status": mission.status.value,
                "requested_by_task_id": mission.requested_by_task_id,
                "final_outcome": mission.final_outcome,
                "episodes": episodes,
            }
        )
    return {"missions": missions}


__all__ = [
    "MockSWEvoAgentExecution",
    "PromptInspection",
    "SandboxCheck",
    "TaskCenterStoreBundle",
    "create_in_memory_task_center_stores",
    "registered_mock_sweevo_agents",
    "run_sweevo_task_center_with_mock_agent_execution",
]
