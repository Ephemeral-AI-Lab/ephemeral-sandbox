"""Dynamic full-case scenario driven by the rendered SWE-EVO user input."""

from __future__ import annotations

from collections.abc import Sequence
from dataclasses import asdict
from typing import Any

from tools.submission.evaluator import submit_evaluation_success
from tools.submission.verifier import (
    submit_verification_failure,
    submit_verification_success,
)
from tools.submission.planner import (
    submit_plan_closes_goal,
    submit_plan_defers_goal,
)

from task_center_runner.audit.events import EventType
from task_center_runner.hooks.registry import Hook
from task_center_runner.scenarios.base import (
    ScenarioBase,
    ScenarioContext,
    ToolCallSpec,
)
from task_center_runner.scenarios._scenario_helpers import (
    context_message_field,
    is_entry_origin_workflow,
    is_recursive_workflow,
)
from task_center_runner.scenarios.user_input import (
    UserInputPlan,
    WorkPackage,
    build_user_input_plan,
)


class FullCaseUserInput(ScenarioBase):
    """Exercise user-input parsing, dynamic DAGs, verifiers, and recursion."""

    name = "full_case_user_input"
    expected_event_sequence: tuple[EventType, ...] = (
        EventType.PLANNER_INVOKED,
        EventType.PLANNER_COMPLETES_GOAL_PLAN,
        EventType.EXECUTOR_INVOKED,
        EventType.EXECUTOR_SUCCESS,
        EventType.VERIFIER_INVOKED,
        EventType.VERIFIER_FAILURE,
        EventType.PLANNER_INVOKED,
        EventType.PLANNER_DEFERS_GOAL_PLAN,
        EventType.EVALUATOR_SUCCESS,
    )

    def __init__(self) -> None:
        self._user_input_plan: UserInputPlan | None = None
        self._entry_prompt: str = ""
        self._recursive_package_id: str | None = None

    @property
    def requirement_ledger(self) -> list[dict[str, Any]]:
        plan = self._user_input_plan
        if plan is None:
            return []
        return [asdict(item) for item in plan.requirements]

    @property
    def package_plan(self) -> list[dict[str, Any]]:
        plan = self._user_input_plan
        if plan is None:
            return []
        return [asdict(package) for package in plan.packages]

    def planner_response(self, ctx: ScenarioContext) -> ToolCallSpec:
        if is_recursive_workflow(ctx):
            return self._recursive_planner_response(ctx)
        return self._entry_origin_planner_response(ctx)

    def executor_actions(self, ctx: ScenarioContext) -> Sequence[str]:
        context_message = ctx.context_message or ctx.prompt or ""
        if "ACTION inspect_user_input" in context_message:
            return ("inspect_user_input",)
        if "ACTION request_recursive_workflow" in context_message:
            package_id = (
                context_message_field(context_message, "package")
                or self._recursive_package_id
                or ""
            )
            return (f"request_recursive_workflow:{package_id}",)
        if "ACTION execute_package" in context_message:
            package_id = context_message_field(context_message, "package") or "unknown"
            return (f"execute_package:{package_id}",)
        if "ACTION final_reconciliation" in context_message:
            return ("final_reconciliation",)
        if "ACTION recursive_" in context_message:
            return ("recursive_step",)
        return ("execute_package:generic",)

    def verifier_response(self, ctx: ScenarioContext) -> ToolCallSpec:
        context_message = ctx.context_message or ""
        checkpoint = context_message_field(context_message, "checkpoint") or "checkpoint"
        failed_by_hook = bool(
            ctx.mutable_state is not None
            and ctx.mutable_state.consume_failure(
                role="verifier",
                attempt_id=str(ctx.attempt.id),
                checkpoint=checkpoint,
            )
        )
        should_fail = failed_by_hook or self._should_fail_verifier(ctx, checkpoint)
        if should_fail:
            return ToolCallSpec(
                submit_verification_failure,
                {
                    "summary": f"Verifier rejected {checkpoint}.",
                    "unresolved_issues": [
                        f"{checkpoint} is missing retry-only evidence.",
                    ],
                },
            )
        return ToolCallSpec(
            submit_verification_success,
            {
                "summary": f"Verifier accepted {checkpoint}.",
                "checks": [
                    f"checkpoint:{checkpoint}",
                    "dependencies:"
                    f"{context_message_field(context_message, 'dependency_count') or '0'}",
                ],
            },
        )

    def evaluator_response(self, ctx: ScenarioContext) -> ToolCallSpec:
        attempt = ctx.attempt
        return ToolCallSpec(
            submit_evaluation_success,
            {
                "summary": "Mock evaluator accepted verifier-gated evidence.",
                "passed_criteria": list(attempt.evaluation_criteria),
            },
        )

    def recursive_handoff_goal(self, ctx: ScenarioContext) -> str | None:
        context_message = ctx.context_message or ""
        package_id = (
            context_message_field(context_message, "package")
            or self._recursive_package_id
        )
        if not package_id:
            return None
        plan = self._ensure_user_input_plan(ctx)
        package = next(
            (item for item in plan.packages if item.id == package_id),
            None,
        )
        if package is None:
            return f"Resolve oversized SWE-EVO package {package_id}."
        requirement_ids = ", ".join(package.item_ids[:12])
        return (
            "Resolve oversized SWE-EVO release package "
            f"{package.id}: {package.title}. "
            f"Representative requirements: {requirement_ids}."
        )

    def hooks(self) -> Sequence[Hook]:
        return ()

    def _entry_origin_planner_response(self, ctx: ScenarioContext) -> ToolCallSpec:
        iteration = ctx.iteration
        attempt = ctx.attempt
        self._ensure_user_input_plan(ctx)
        if iteration.sequence_no == 1 and attempt.attempt_sequence_no == 1:
            return ToolCallSpec(submit_plan_closes_goal, _inventory_plan(kind="completes"))
        if iteration.sequence_no == 1:
            return ToolCallSpec(
                submit_plan_defers_goal,
                _inventory_plan(
                    kind="defers",
                    deferred_goal_for_next_iteration=(
                        "Execute the dynamic package DAG with verifier "
                        "checkpoints and recursive goal handling."
                    ),
                ),
            )
        if iteration.sequence_no == 2:
            args = self._implementation_plan(ctx)
            return ToolCallSpec(submit_plan_defers_goal, args)
        return ToolCallSpec(submit_plan_closes_goal, self._final_reconciliation_plan(ctx))

    def _recursive_planner_response(self, ctx: ScenarioContext) -> ToolCallSpec:
        iteration = ctx.iteration
        if iteration.sequence_no == 1:
            return ToolCallSpec(
                submit_plan_defers_goal,
                {
                    "plan_spec": "Decompose the oversized delegated package.",
                    "evaluation_criteria": [
                        "Recursive package inventory was produced.",
                        "Recursive verifier accepted decomposition coverage.",
                    ],
                    "tasks": [
                        {"id": "recursive_inventory", "agent_name": "executor", "deps": []},
                        {
                            "id": "recursive_inventory_guard",
                            "agent_name": "verifier",
                            "deps": ["recursive_inventory"],
                        },
                    ],
                    "task_specs": {
                        "recursive_inventory": "ACTION recursive_inventory",
                        "recursive_inventory_guard": (
                            "VERIFY checkpoint=recursive_inventory "
                            "dependency_count=1"
                        ),
                    },
                    "deferred_goal_for_next_iteration": (
                        "Execute the delegated package subtasks and verify "
                        "their local integration."
                    ),
                },
            )
        if iteration.sequence_no == 2:
            return ToolCallSpec(
                submit_plan_defers_goal,
                {
                    "plan_spec": "Execute delegated package subtasks.",
                    "evaluation_criteria": [
                        "Recursive package probes completed.",
                        "Recursive wave guard passed.",
                    ],
                    "tasks": [
                        {"id": "recursive_exec_a", "agent_name": "executor", "deps": []},
                        {"id": "recursive_exec_b", "agent_name": "executor", "deps": []},
                        {
                            "id": "recursive_wave_guard",
                            "agent_name": "verifier",
                            "deps": ["recursive_exec_a", "recursive_exec_b"],
                        },
                    ],
                    "task_specs": {
                        "recursive_exec_a": "ACTION recursive_execute slice=a",
                        "recursive_exec_b": "ACTION recursive_execute slice=b",
                        "recursive_wave_guard": (
                            "VERIFY checkpoint=recursive_wave dependency_count=2"
                        ),
                    },
                    "deferred_goal_for_next_iteration": "Reconcile recursive package evidence.",
                },
            )
        return ToolCallSpec(
            submit_plan_closes_goal,
            {
                "plan_spec": "Close the delegated package goal.",
                "evaluation_criteria": [
                    "Recursive close report summarizes package evidence.",
                    "Recursive final verifier passed.",
                ],
                "tasks": [
                    {"id": "recursive_reconcile", "agent_name": "executor", "deps": []},
                    {
                        "id": "recursive_final_guard",
                        "agent_name": "verifier",
                        "deps": ["recursive_reconcile"],
                    },
                ],
                "task_specs": {
                    "recursive_reconcile": "ACTION recursive_reconcile",
                    "recursive_final_guard": (
                        "VERIFY checkpoint=recursive_final dependency_count=1"
                    ),
                },
            },
        )

    def _implementation_plan(self, ctx: ScenarioContext) -> dict[str, Any]:
        plan = self._ensure_user_input_plan(ctx)
        packages = tuple(plan.packages)
        recursive = next((pkg for pkg in packages if pkg.recursive_candidate), None)
        regular = tuple(pkg for pkg in packages if not pkg.recursive_candidate)
        if recursive is None and regular:
            recursive = regular[-1]
            regular = regular[:-1]
        self._recursive_package_id = recursive.id if recursive else None

        tasks: list[dict[str, Any]] = []
        task_specs: dict[str, str] = {}
        previous_guard: str | None = None
        for wave_no, wave in enumerate(_chunked(regular, 8), start=1):
            local_ids: list[str] = []
            deps = [previous_guard] if previous_guard else []
            for package in wave:
                local_id = f"exec_{package.id}"
                local_ids.append(local_id)
                tasks.append({"id": local_id, "agent_name": "executor", "deps": deps})
                task_specs[local_id] = _package_task_spec(package, wave_no)
            guard_id = f"verify_wave_{wave_no}"
            tasks.append(
                {"id": guard_id, "agent_name": "verifier", "deps": local_ids}
            )
            task_specs[guard_id] = (
                f"VERIFY checkpoint=wave_{wave_no} wave={wave_no} "
                f"dependency_count={len(local_ids)}"
            )
            previous_guard = guard_id

        final_deps: list[str] = [previous_guard] if previous_guard else []
        if recursive is not None:
            recursive_deps = [previous_guard] if previous_guard else []
            delegate_id = f"delegate_{recursive.id}"
            tasks.append(
                {"id": delegate_id, "agent_name": "executor", "deps": recursive_deps}
            )
            task_specs[delegate_id] = (
                f"ACTION request_recursive_workflow package={recursive.id} "
                f"risk={recursive.risk}"
            )
            recursive_guard = "verify_recursive_return"
            tasks.append(
                {
                    "id": recursive_guard,
                    "agent_name": "verifier",
                    "deps": [delegate_id],
                }
            )
            task_specs[recursive_guard] = (
                "VERIFY checkpoint=recursive_return dependency_count=1"
            )
            final_deps.append(recursive_guard)

        final_guard = "verify_final_pre_evaluator"
        tasks.append({"id": final_guard, "agent_name": "verifier", "deps": final_deps})
        task_specs[final_guard] = (
            f"VERIFY checkpoint=final_pre_evaluator "
            f"dependency_count={len(final_deps)}"
        )
        return {
            "plan_spec": (
                "Execute dynamic SWE-EVO package DAG from the rendered user input."
            ),
            "evaluation_criteria": [
                "Every generated executor wave is guarded by a verifier.",
                "At least one verifier guards multiple executor tasks.",
                "Recursive package close report is available before parent guard.",
            ],
            "tasks": tasks,
            "task_specs": task_specs,
            "deferred_goal_for_next_iteration": (
                "Run final release-bundle reconciliation after package evidence "
                "and recursive goal output are available."
            ),
        }

    def _final_reconciliation_plan(self, ctx: ScenarioContext) -> dict[str, Any]:
        plan = self._ensure_user_input_plan(ctx)
        high_risk_count = sum(1 for item in plan.requirements if item.risk == "high")
        return {
            "plan_spec": "Reconcile final SWE-EVO coverage evidence.",
            "evaluation_criteria": [
                "High-risk requirement categories have verifier evidence.",
                "Final evaluator runs after the final verifier passes.",
            ],
            "tasks": [
                {"id": "final_coverage_ledger", "agent_name": "executor", "deps": []},
                {
                    "id": "final_readback_probe",
                    "agent_name": "executor",
                    "deps": ["final_coverage_ledger"],
                },
                {
                    "id": "final_release_guard",
                    "agent_name": "verifier",
                    "deps": ["final_coverage_ledger", "final_readback_probe"],
                },
            ],
            "task_specs": {
                "final_coverage_ledger": (
                    "ACTION final_reconciliation stage=coverage "
                    f"high_risk_count={high_risk_count}"
                ),
                "final_readback_probe": "ACTION final_reconciliation stage=readback",
                "final_release_guard": (
                    "VERIFY checkpoint=final_release dependency_count=2"
                ),
            },
        }

    def _ensure_user_input_plan(self, ctx: ScenarioContext) -> UserInputPlan:
        if self._user_input_plan is not None:
            return self._user_input_plan
        prompt = ""
        if ctx.workflow is not None and is_entry_origin_workflow(ctx):
            prompt = str(ctx.workflow.goal or "")
        if not prompt:
            prompt = ctx.prompt or ctx.context_message or ""
        self._entry_prompt = prompt
        self._user_input_plan = build_user_input_plan(prompt)
        return self._user_input_plan

    def _should_fail_verifier(
        self,
        ctx: ScenarioContext,
        checkpoint: str,
    ) -> bool:
        if not is_entry_origin_workflow(ctx):
            return False
        iteration = ctx.iteration
        attempt = ctx.attempt
        return (
            iteration.sequence_no == 1
            and attempt.attempt_sequence_no == 1
            and checkpoint == "inventory"
        ) or (
            iteration.sequence_no == 2
            and attempt.attempt_sequence_no == 1
            and checkpoint == "final_pre_evaluator"
        )


def _inventory_plan(
    *,
    kind: str,
    deferred_goal_for_next_iteration: str | None = None,
) -> dict[str, Any]:
    args: dict[str, Any] = {
        "plan_spec": "Inventory rendered SWE-EVO user-input requirements.",
        "evaluation_criteria": [
            "Requirement ledger was built from the already-rendered user input.",
            "Package DAG policy can be derived from the requirement ledger.",
        ],
        "tasks": [
            {"id": "requirement_inventory", "agent_name": "executor", "deps": []},
            {
                "id": "inventory_guard",
                "agent_name": "verifier",
                "deps": ["requirement_inventory"],
            },
        ],
        "task_specs": {
            "requirement_inventory": "ACTION inspect_user_input",
            "inventory_guard": "VERIFY checkpoint=inventory dependency_count=1",
        },
    }
    if kind == "defers":
        assert deferred_goal_for_next_iteration is not None
        args["deferred_goal_for_next_iteration"] = deferred_goal_for_next_iteration
    return args


def _package_task_spec(package: WorkPackage, wave_no: int) -> str:
    return (
        f"ACTION execute_package package={package.id} wave={wave_no} "
        f"subsystem={package.subsystem} risk={package.risk} "
        f"item_count={len(package.item_ids)}"
    )


def _chunked(
    packages: tuple[WorkPackage, ...],
    size: int,
) -> tuple[tuple[WorkPackage, ...], ...]:
    return tuple(
        tuple(packages[index : index + size])
        for index in range(0, len(packages), size)
    )


__all__ = ["FullCaseUserInput"]
