"""Stage 2 — wire the agent runner and build the ``RunConfig`` for ``run_pipeline``.

Replaces the legacy ``provisioner.py`` (verify-only provisioner) and
``agent_runner.py`` (triple-factory) with the smallest amount of code that
satisfies the ``run_pipeline`` contract.
"""

from __future__ import annotations

from collections.abc import Awaitable, Callable
from pathlib import Path
from typing import TYPE_CHECKING, Any

from task_center_runner.benchmarks.sweevo._provision import setup_sweevo_sandbox
from task_center_runner.benchmarks.sweevo.eval import SweevoLifecycle
from task_center_runner.benchmarks.sweevo.models import PreContext, SWEEvoInstance
from task_center_runner.core.sandbox import SandboxLease
from tools._framework.core.runtime import ExecutionMetadata

if TYPE_CHECKING:
    from task_center.attempt.launch import AttemptAgentRunner
    from task_center_runner.core.config import RunConfig, RunContext


class SweevoProvisioner:
    """Verify-only provisioner — the caller owns the container lifecycle.

    ``provision_sandbox`` already created/resumed the container; this hook just
    re-runs ``setup_sweevo_sandbox`` inside ``run_pipeline``'s provisioner
    contract so retry attempts get a clean checkout. Release is a no-op since
    the container persists across runs.
    """

    def __init__(
        self,
        instance: SWEEvoInstance,
        sandbox_id: str,
        *,
        repo_dir: str,
        install_lsp: bool = False,
    ) -> None:
        self._instance = instance
        self._sandbox_id = sandbox_id
        self._repo_dir = repo_dir
        self._install_lsp = install_lsp

    async def provision(self, ctx: "RunContext") -> SandboxLease:
        await setup_sweevo_sandbox(
            self._instance,
            self._sandbox_id,
            repo_dir=self._repo_dir,
            install_lsp=self._install_lsp,
        )
        return SandboxLease(
            sandbox_id=self._sandbox_id,
            metadata={
                "instance_id": self._instance.instance_id,
                "repo_dir": self._repo_dir,
            },
        )

    async def release(self, lease: SandboxLease) -> None:
        return None


def build_agent_delegate(
    *,
    repo_dir: str,
) -> Callable[["RunContext"], "AttemptAgentRunner"]:
    """Return a ``RunConfig.runner_factory`` that delegates to the real runner."""

    async def _delegate(
        config: Any,
        prompt: str,
        *,
        agent_def: Any,
        sandbox_id: str | None,
        persist_agent_run: bool,
        task_id: str,
        on_event: Callable[[Any], Awaitable[None]] | None,
        extra_tool_metadata: Any,
        initial_messages: Any = None,
    ) -> Any:
        from engine.api import run_ephemeral_agent

        if isinstance(extra_tool_metadata, ExecutionMetadata):
            metadata = extra_tool_metadata.copy()
        else:
            metadata = ExecutionMetadata()
            metadata.update(extra_tool_metadata or {})
        metadata = metadata.with_overrides(
            sandbox_id=str(sandbox_id or ""),
            agent_name=str(getattr(agent_def, "name", "") or ""),
            repo_root=repo_dir,
            exec_cwd=repo_dir,
        )
        return await run_ephemeral_agent(
            config,
            prompt,
            agent_def=agent_def,
            sandbox_id=sandbox_id,
            persist_agent_run=persist_agent_run,
            task_id=task_id,
            on_event=on_event,
            extra_tool_metadata=metadata,
            initial_messages=initial_messages,
        )

    def _factory(_ctx: "RunContext") -> "AttemptAgentRunner":
        async def runner(
            config: Any,
            prompt: str,
            *,
            agent_def: Any,
            sandbox_id: str | None = None,
            persist_agent_run: bool = True,
            task_id: str = "",
            on_event: Callable[[Any], Awaitable[None]] | None = None,
            extra_tool_metadata: Any = None,
            initial_messages: Any = None,
            **_ignored: Any,
        ) -> Any:
            return await _delegate(
                config,
                prompt,
                agent_def=agent_def,
                sandbox_id=sandbox_id,
                persist_agent_run=persist_agent_run,
                task_id=task_id,
                on_event=on_event,
                extra_tool_metadata=extra_tool_metadata,
                initial_messages=initial_messages,
            )

        return runner

    return _factory


def build_run_config(ctx: PreContext, sandbox_id: str) -> "RunConfig":
    """Assemble the ``RunConfig`` consumed by ``run_pipeline``."""
    from runtime.app_factory import RuntimeConfig
    from task_center_runner.core.bootstrap import bootstrap_real_agent_runtime
    from task_center_runner.core.config import RunConfig

    runtime_cfg = RuntimeConfig(cwd=str(Path.cwd()), external_api_client=None)
    return RunConfig(
        entry_prompt=ctx.workflow,
        repo_dir=ctx.repo_dir,
        sandbox=SweevoProvisioner(
            ctx.instance,
            sandbox_id,
            repo_dir=ctx.repo_dir,
            install_lsp=True,
        ),
        runner_factory=build_agent_delegate(repo_dir=ctx.repo_dir),
        lifecycle=SweevoLifecycle(
            ctx.instance,
            repo_dir=ctx.repo_dir,
            aggregate_jsonl_path=ctx.audit_dir / "aggregate.jsonl",
        ),
        bootstrap=bootstrap_real_agent_runtime,
        audit_dir=ctx.audit_dir,
        run_label=f"benchmark/sweevo/{ctx.instance.instance_id}",
        instance_id=ctx.instance.instance_id,
        max_duration_s=ctx.max_duration_s,
        extras={"runtime_config": runtime_cfg},
    )


__all__ = ["SweevoProvisioner", "build_agent_delegate", "build_run_config"]
