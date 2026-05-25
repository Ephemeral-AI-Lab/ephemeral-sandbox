"""Focused scenarios for the 3.5 plugin/LSP live tier."""

from __future__ import annotations

from collections.abc import Sequence
from typing import Any

from tools.submission.evaluator import submit_evaluation_success
from tools.submission.planner import submit_plan_closes_goal

from task_center_runner.audit.events import EventType
from task_center_runner.scenarios.base import (
    ScenarioBase,
    ScenarioContext,
    ToolCallSpec,
)


def _plan(action_id: str, action_spec: str, summary_hint: str) -> dict[str, Any]:
    return {
        "plan_spec": (
            f"Single-task plan that drives the {action_id} plugin/LSP probe "
            "through the mock-agent harness."
        ),
        "evaluation_criteria": [
            f"Plugin probe '{action_id}' wrote its summary to {summary_hint}.",
            "Plugin READ_ONLY service latency, WRITE_ALLOWED overlay/OCC "
            "behavior, and isolated-workspace policy matched the 3.5 live "
            "E2E contract.",
        ],
        "tasks": [{"id": action_id, "agent_name": "executor", "deps": []}],
        "task_specs": {action_id: action_spec},
    }


class _PluginScenarioBase(ScenarioBase):
    expected_event_sequence: tuple[EventType, ...] = (
        EventType.PLANNER_INVOKED,
        EventType.PLANNER_COMPLETES_GOAL_PLAN,
        EventType.EXECUTOR_INVOKED,
        EventType.EXECUTOR_SUCCESS,
        EventType.EVALUATOR_INVOKED,
        EventType.EVALUATOR_SUCCESS,
    )

    action_id: str = ""
    action_spec: str = ""
    summary_path_hint: str = ""

    def planner_response(self, ctx: ScenarioContext) -> ToolCallSpec:  # noqa: ARG002
        return ToolCallSpec(
            submit_plan_closes_goal,
            _plan(self.action_id, self.action_spec, self.summary_path_hint),
        )

    def executor_actions(self, ctx: ScenarioContext) -> Sequence[str]:
        context_message = ctx.context_message or ctx.prompt or ""
        if f"ACTION {self.action_id}" in context_message:
            return (self.action_id,)
        return ()

    def evaluator_response(self, ctx: ScenarioContext) -> ToolCallSpec:
        return ToolCallSpec(
            submit_evaluation_success,
            {
                "summary": f"{self.action_id} plugin/LSP scenario completed.",
                "passed_criteria": list(ctx.attempt.evaluation_criteria),
            },
        )


class PluginReadOnlyLspRefresh(_PluginScenarioBase):
    name = "sandbox.plugin_read_only_lsp_refresh"
    action_id = "plugin_read_only_lsp_refresh"
    action_spec = (
        "ACTION plugin_read_only_lsp_refresh. Seed a Python module, run "
        "READ_ONLY LSP hover/definition/diagnostics, edit the module through "
        "the default file path, and run diagnostics again to prove warm "
        "service refresh without per-call publish."
    )
    summary_path_hint = (
        "/testbed/.ephemeralos/sweevo-mock/plugin/read_only_lsp_refresh/summary.json"
    )


class PluginWriteAllowedPublish(_PluginScenarioBase):
    name = "sandbox.plugin_write_allowed_publish"
    action_id = "plugin_write_allowed_publish"
    action_spec = (
        "ACTION plugin_write_allowed_publish. Seed a Python file, apply an "
        "LSP WorkspaceEdit through the WRITE_ALLOWED plugin path, and read "
        "the committed change through the normal sandbox API."
    )
    summary_path_hint = (
        "/testbed/.ephemeralos/sweevo-mock/plugin/write_allowed_publish/summary.json"
    )


class PluginIntentContract(_PluginScenarioBase):
    name = "sandbox.plugin_intent_contract"
    action_id = "plugin_intent_contract"
    action_spec = (
        "ACTION plugin_intent_contract. Register synthetic plugin controls "
        "to prove missing intent and lifecycle intent fail fast while "
        "READ_ONLY dispatch stays in-process and WRITE_ALLOWED dispatch uses "
        "the overlay runner."
    )
    summary_path_hint = (
        "/testbed/.ephemeralos/sweevo-mock/plugin/intent_contract/summary.json"
    )


class PluginIwsPolicy(_PluginScenarioBase):
    name = "sandbox.plugin_iws_policy"
    action_id = "plugin_iws_policy"
    action_spec = (
        "ACTION plugin_iws_policy. Enter isolated_workspace for the executor "
        "agent, prove generic and dynamic plugin daemon ops are blocked with "
        "forbidden_in_isolated_workspace, exit, and verify default mode "
        "permits plugin status."
    )
    summary_path_hint = (
        "/testbed/.ephemeralos/sweevo-mock/plugin/iws_policy/summary.json"
    )


class PluginSetupFailure(_PluginScenarioBase):
    name = "sandbox.plugin_setup_failure"
    action_id = "plugin_setup_failure"
    action_spec = (
        "ACTION plugin_setup_failure. Force a synthetic setup/network failure "
        "through call_plugin, assert the structured setup failure payload, "
        "then retry successfully to prove no stale loaded state remains."
    )
    summary_path_hint = (
        "/testbed/.ephemeralos/sweevo-mock/plugin/setup_failure/summary.json"
    )


class PluginServiceEvict(_PluginScenarioBase):
    name = "sandbox.plugin_service_evict"
    action_id = "plugin_service_evict"
    action_spec = (
        "ACTION plugin_service_evict. Start the Pyright service, publish "
        "several peer edits, verify warm refresh/remount, force plugin "
        "runtime eviction through api.plugin.ensure digest churn, and verify "
        "a clean service restart."
    )
    summary_path_hint = (
        "/testbed/.ephemeralos/sweevo-mock/plugin/service_evict/summary.json"
    )


__all__ = [
    "PluginIntentContract",
    "PluginIwsPolicy",
    "PluginReadOnlyLspRefresh",
    "PluginServiceEvict",
    "PluginSetupFailure",
    "PluginWriteAllowedPublish",
]
