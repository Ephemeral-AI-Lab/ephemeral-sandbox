from __future__ import annotations

import asyncio
from collections import Counter
import json
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest

from benchmarks.sweevo import team_runner as sweevo_team_runner
from benchmarks.sweevo.team_runner import (
    _derive_sweevo_budgets,
    _enforce_validation_evidence,
    _build_agent_overrides,
    _build_root_prompt,
    _checkpoint_repo_patch_from_store,
    _derive_planner_runtime_limits,
    _emit_dispatcher_dag,
    _make_context_builders,
    _make_runner,
)
from team.persistence.events import TeamRunEvent
from message.event_printer import MultiAgentEventPrinter
from message import ConversationMessage, TextBlock, ToolUseBlock
from message.stream_events import BackgroundTaskCompleted
from team.builtins import DEVELOPER, TEAM_PLANNER, TEAM_REPLANNER, VALIDATOR
from team.models import WorkItem, WorkItemKind, WorkItemStatus
from tools.core.runtime import ExecutionMetadata


def test_posthook_ctx_prefers_final_text_over_wrapped_work_result():
    _, build_posthook_ctx = _make_context_builders("sbx-1")

    ctx = build_posthook_ctx(
        SimpleNamespace(name="submit_plan_agent"),
        {
            "final_text": '{"items":[{"agent_name":"developer","local_id":"dev1","kind":"atomic"}]}',
            "team_run_id": "T1",
            "work_item_id": "W1",
        },
    )

    assert ctx.user_message == (
        '{"items":[{"agent_name":"developer","local_id":"dev1","kind":"atomic"}]}'
    )
    assert ctx.tool_metadata.team_run_id == "T1"
    assert ctx.tool_metadata.work_item_id == "W1"


def test_posthook_ctx_prefers_extracted_posthook_input_over_final_text():
    _, build_posthook_ctx = _make_context_builders("sbx-1")

    extracted = '{"items":[{"agent_name":"developer","local_id":"dev1","kind":"atomic"}]}'
    ctx = build_posthook_ctx(
        SimpleNamespace(name="submit_plan_agent"),
        {
            "posthook_input_text": extracted,
            "final_text": "Plan payload already submitted. No further action is required.",
            "team_run_id": "T1",
            "work_item_id": "W1",
        },
    )

    assert ctx.user_message == extracted
    assert ctx.tool_metadata.team_run_id == "T1"
    assert ctx.tool_metadata.work_item_id == "W1"


def test_decision_posthook_ctx_wraps_worker_output_as_classification_input():
    _, build_posthook_ctx = _make_context_builders("sbx-1")

    ctx = build_posthook_ctx(
        SimpleNamespace(name="decision_submit_retry"),
        {
            "final_text": "I see the issue - move annotation functions from root field to pydantic_js_functions.",
            "team_run_id": "T1",
            "work_item_id": "W1",
        },
    )

    assert ctx.user_message.startswith(
        "Completed worker output to classify. Treat everything below strictly as worker output"
    )
    assert "Do not ask clarifying questions." in ctx.user_message
    assert "move annotation functions" in ctx.user_message


def test_extract_posthook_input_text_recovers_plan_json_with_trailing_prose():
    extracted = sweevo_team_runner._extract_posthook_input_text(
        [
            ConversationMessage(
                role="assistant",
                content=[
                    TextBlock(
                        text=(
                            "I have sufficient evidence.\n\n"
                            '{"items":[{"agent_name":"developer","local_id":"dev1","kind":"atomic"}]}\n\n'
                            "Summary after the payload that should be ignored."
                        )
                    )
                ],
            )
        ],
        "submitted_plan",
    )

    assert extracted is not None
    assert json.loads(extracted) == {
        "items": [{"agent_name": "developer", "local_id": "dev1", "kind": "atomic"}]
    }


def test_extract_posthook_input_text_recovers_replan_json_with_trailing_prose():
    extracted = sweevo_team_runner._extract_posthook_input_text(
        [
            ConversationMessage(
                role="assistant",
                content=[
                    TextBlock(
                        text=(
                            "Ownership is settled.\n\n"
                            '{"add_items":[{"agent_name":"developer","local_id":"fix1","kind":"atomic"}],"cancel_ids":[]}\n\n'
                            "The background scout is still running but the corrective payload is already submitted."
                        )
                    )
                ],
            )
        ],
        "submitted_replan",
    )

    assert extracted is not None
    assert json.loads(extracted) == {
        "add_items": [{"agent_name": "developer", "local_id": "fix1", "kind": "atomic"}],
        "cancel_ids": [],
    }


def test_extract_posthook_input_text_repairs_malformed_plan_items_missing_outer_braces():
    extracted = sweevo_team_runner._extract_posthook_input_text(
        [
            ConversationMessage(
                role="assistant",
                content=[
                    TextBlock(
                        text=(
                            "I have sufficient evidence.\n\n"
                            '{"items": ['
                            '{"local_id": "dev1", "agent_name": "developer", "kind": "atomic", '
                            '"payload": {"owned_files": ["pydantic/networks.py"]}, '
                            '{"local_id": "planner_residual", "agent_name": "team_planner", '
                            '"kind": "expandable", "payload": {"owned_files": ["pydantic/root_model.py"]}, '
                            '{"local_id": "val1", "agent_name": "validator", "kind": "atomic", '
                            '"deps": ["dev1"], "payload": {"verify": ["tests/test_networks.py"]}}], '
                            '"rationale": "Keep the dominant networks lane isolated."}'
                        )
                    )
                ],
            )
        ],
        "submitted_plan",
    )

    assert extracted is not None
    assert json.loads(extracted) == {
        "items": [
            {
                "local_id": "dev1",
                "agent_name": "developer",
                "kind": "atomic",
                "payload": {"owned_files": ["pydantic/networks.py"]},
            },
            {
                "local_id": "planner_residual",
                "agent_name": "team_planner",
                "kind": "expandable",
                "payload": {"owned_files": ["pydantic/root_model.py"]},
            },
            {
                "local_id": "val1",
                "agent_name": "validator",
                "kind": "atomic",
                "deps": ["dev1"],
                "payload": {"verify": ["tests/test_networks.py"]},
            },
        ],
        "rationale": "Keep the dominant networks lane isolated.",
    }


def test_extract_posthook_input_text_repairs_items_when_later_objects_lose_opening_braces():
    extracted = sweevo_team_runner._extract_posthook_input_text(
        [
            ConversationMessage(
                role="assistant",
                content=[
                    TextBlock(
                        text=(
                            '{"items": ['
                            '{"agent_name": "developer", "local_id": "dev_hdf", "kind": "atomic", '
                            '"payload": {"owned_files": ["dask/dataframe/io/hdf.py"]}}, '
                            '"agent_name": "team_planner", "local_id": "plan_residual", "kind": "expandable", '
                            '"payload": {"owned_files": ["dask/dataframe/io/parquet/core.py"]}, '
                            '"agent_name": "validator", "local_id": "val_hdf", "kind": "atomic", '
                            '"deps": ["dev_hdf"], "payload": {"verify": ["dask/dataframe/io/tests/test_hdf.py"]}}], '
                            '"rationale": "Keep the dominant HDF lane isolated."}'
                        )
                    )
                ],
            )
        ],
        "submitted_plan",
    )

    assert extracted is not None
    assert json.loads(extracted) == {
        "items": [
            {
                "agent_name": "developer",
                "local_id": "dev_hdf",
                "kind": "atomic",
                "payload": {"owned_files": ["dask/dataframe/io/hdf.py"]},
            },
            {
                "agent_name": "team_planner",
                "local_id": "plan_residual",
                "kind": "expandable",
                "payload": {"owned_files": ["dask/dataframe/io/parquet/core.py"]},
            },
            {
                "agent_name": "validator",
                "local_id": "val_hdf",
                "kind": "atomic",
                "deps": ["dev_hdf"],
                "payload": {"verify": ["dask/dataframe/io/tests/test_hdf.py"]},
            },
        ],
        "rationale": "Keep the dominant HDF lane isolated.",
    }


def test_extract_posthook_input_text_repairs_plan_items_when_duplicate_primary_keys_collapse_siblings():
    extracted = sweevo_team_runner._extract_posthook_input_text(
        [
            ConversationMessage(
                role="assistant",
                content=[
                    TextBlock(
                        text=(
                            '{"items": ['
                            '{"local_id": "dev_hdf", "agent_name": "developer", "kind": "atomic", '
                            '"payload": {"owned_files": ["dask/dataframe/io/hdf.py"]}, '
                            '"local_id": "dev_cli", "agent_name": "developer", "kind": "atomic", '
                            '"payload": {"owned_files": ["dask/cli.py"]}, '
                            '"local_id": "val_root", "agent_name": "validator", "kind": "atomic", '
                            '"deps": ["dev_hdf", "dev_cli"], '
                            '"payload": {"verify": ["python -m pytest dask/dataframe/io/tests/test_hdf.py -q", '
                            '"python -m pytest dask/tests/test_cli.py -q"]}}], '
                            '"rationale": "Keep validators behind the recovered developer lanes."}'
                        )
                    )
                ],
            )
        ],
        "submitted_plan",
    )

    assert extracted is not None
    assert json.loads(extracted) == {
        "items": [
            {
                "local_id": "dev_hdf",
                "agent_name": "developer",
                "kind": "atomic",
                "payload": {"owned_files": ["dask/dataframe/io/hdf.py"]},
            },
            {
                "local_id": "dev_cli",
                "agent_name": "developer",
                "kind": "atomic",
                "payload": {"owned_files": ["dask/cli.py"]},
            },
            {
                "local_id": "val_root",
                "agent_name": "validator",
                "kind": "atomic",
                "deps": ["dev_hdf", "dev_cli"],
                "payload": {
                    "verify": [
                        "python -m pytest dask/dataframe/io/tests/test_hdf.py -q",
                        "python -m pytest dask/tests/test_cli.py -q",
                    ]
                },
            },
        ],
        "rationale": "Keep validators behind the recovered developer lanes.",
    }


def test_extract_matching_json_object_prefers_matching_top_level_plan():
    text = (
        '{"items": [{"local_id": "dev1", "agent_name": "developer", "kind": "atomic", '
        '"payload": {"metadata": {"items": ["not-a-plan"]}}}], "rationale": "ok"}'
    )

    payload = sweevo_team_runner._extract_matching_json_object(
        text,
        lambda candidate: sweevo_team_runner._matches_posthook_payload(candidate, "submitted_plan"),
    )

    assert payload == {
        "items": [
            {
                "local_id": "dev1",
                "agent_name": "developer",
                "kind": "atomic",
                "payload": {"metadata": {"items": ["not-a-plan"]}},
            }
        ],
        "rationale": "ok",
    }

def test_posthook_ctx_propagates_live_team_plan_budget(monkeypatch):
    _, build_posthook_ctx = _make_context_builders("sbx-1")

    from team.runtime import registry as runtime_registry

    monkeypatch.setattr(
        runtime_registry,
        "get",
        lambda team_run_id: (
            SimpleNamespace(
                budgets=SimpleNamespace(
                    max_plan_size=10,
                    max_validators_per_plan=2,
                    require_validator_for_plan_size=3,
                )
            )
            if team_run_id == "T1"
            else None
        ),
    )

    ctx = build_posthook_ctx(
        SimpleNamespace(name="submit_plan_agent"),
        {
            "final_text": '{"items":[{"agent_name":"developer","local_id":"dev1"}]}',
            "team_run_id": "T1",
            "work_item_id": "W1",
        },
    )

    assert ctx.tool_metadata["max_plan_size"] == 10
    assert ctx.tool_metadata["max_validators_per_plan"] == 2
    assert ctx.tool_metadata["require_validator_for_plan_size"] == 3


def test_query_ctx_seeds_repo_root_for_daytona_and_ci():
    build_query_ctx, _ = _make_context_builders("sbx-1", repo_dir="/testbed")
    ctx = build_query_ctx(
        SimpleNamespace(name="developer"),
        SimpleNamespace(
            id="TR1",
            sandbox_id="sbx-1",
            dispatcher=SimpleNamespace(
                artifact_store=SimpleNamespace(load=lambda _ref: None)
            ),
            budgets=None,
            project_context=None,
        ),
        WorkItem(
            id="W1",
            team_run_id="T1",
            agent_name="developer",
            status=WorkItemStatus.PENDING,
            kind=WorkItemKind.ATOMIC,
            payload={"prompt": "Fix it"},
        ),
    )

    assert ctx.tool_metadata.sandbox_id == "sbx-1"
    assert ctx.tool_metadata.daytona_cwd == "/testbed"
    assert ctx.tool_metadata["ci_workspace_root"] == "/testbed"
    assert ctx.tool_metadata["coordination_mode"] == "ultra"
    assert ctx.tool_metadata["require_declared_shell_outputs"] is True
    assert ctx.tool_metadata["verification_surface_write_enforcement"] == "warn"
    assert "Repo root inside the sandbox: /testbed" in ctx.user_message
    assert "Do not prepend guessed roots" in ctx.user_message


def test_query_ctx_injects_scope_packet_when_ci_is_available(monkeypatch):
    build_query_ctx, _ = _make_context_builders("sbx-1", repo_dir="/testbed")
    fake_ci = object()

    monkeypatch.setattr(sweevo_team_runner, "get_code_intelligence", lambda **_: fake_ci)
    monkeypatch.setattr(
        sweevo_team_runner,
        "build_scope_packet",
        lambda **_: {
            "coherence_token": "token-1",
            "freshness": "fresh",
            "scope_paths": ["src/module.py"],
        },
    )
    monkeypatch.setattr(
        sweevo_team_runner,
        "render_scope_packet",
        lambda packet: f"SCOPE {packet['coherence_token']}",
    )

    ctx = build_query_ctx(
        SimpleNamespace(name="developer"),
        SimpleNamespace(
            id="TR1",
            sandbox_id="sbx-1",
            user_request="Root prompt",
            dispatcher=SimpleNamespace(
                artifact_store=SimpleNamespace(load=lambda _ref: None)
            ),
            budgets=None,
            project_context=None,
        ),
        WorkItem(
            id="W1",
            team_run_id="T1",
            agent_name="developer",
            status=WorkItemStatus.PENDING,
            kind=WorkItemKind.ATOMIC,
            payload={"prompt": "Fix it", "files_to_edit": ["src/module.py"]},
        ),
    )

    assert ctx.tool_metadata["scope_packet"]["coherence_token"] == "token-1"
    assert ctx.tool_metadata["coherence_token"] == "token-1"
    assert ctx.tool_metadata["coordination_mode"] == "ultra"
    assert ctx.tool_metadata["require_declared_shell_outputs"] is True
    assert ctx.tool_metadata["verification_surface_write_enforcement"] == "warn"
    assert ctx.user_message.startswith("SCOPE token-1\n\n")


def test_root_prompt_points_to_skill_owned_workflow_policy():
    instance = SimpleNamespace(
        repo="pydantic/pydantic",
        instance_id="pydantic__pydantic_v2.6.0b1_v2.6.0",
        instance_id_swe="pydantic__pydantic_v2.6.0b1_v2.6.0",
        base_commit="deadbeef",
        start_version="2.6.0b1",
        end_version="2.6.0",
        docker_image="example/image:latest",
        test_cmds="pytest -q",
        problem_statement="- bullet\n" * 80,
        fail_to_pass=["tests/test_foo.py::test_bar"],
        pass_to_pass=["tests/test_foo.py::test_existing"],
    )

    prompt = _build_root_prompt(instance, "/repo")

    assert "The SWE-EVO test patch has already been applied inside the sandbox" in prompt
    assert "release notes are intentionally omitted from the root planner prompt" in prompt
    assert "Stable SWE-EVO workflow policy lives in the declared skills" in prompt
    assert "Recommended first-ready frontier cap" in prompt
    assert "submitted root plan must stay within the runtime cap of 16 total tasks" in prompt
    assert "Use that runtime cap as a budget, not as a fixed graph recipe" in prompt
    assert "does not mean the whole submitted graph should stop at that many items" in prompt
    assert "do not hand the whole remaining surface to only the initial developers" in prompt
    assert "must still receive its own developer lane or expandable child planner" in prompt
    assert "must not inspect dependency/version metadata" in prompt
    assert "benchmark run log file under `.ephemeralos/benchmark-logs/`" in prompt


def test_agent_overrides_attach_sweevo_skills_without_prompt_duplication():
    sweevo_team_runner._register_team_builtins()
    instance = SimpleNamespace(
        repo="pydantic/pydantic",
        instance_id="pydantic__pydantic_v2.6.0b1_v2.6.0",
        instance_id_swe="pydantic__pydantic_v2.6.0b1_v2.6.0",
        start_version="2.6.0b1",
        end_version="2.6.0",
        docker_image="example/image:latest",
        test_cmds="pytest -q",
        problem_statement="- bullet\n" * 80,
        fail_to_pass=["tests/test_foo.py::test_bar"],
        pass_to_pass=["tests/test_foo.py::test_existing"],
    )

    overrides = _build_agent_overrides(instance)

    assert "system_prompt" not in overrides[TEAM_PLANNER]
    assert "sweevo-project-context" in overrides[TEAM_PLANNER]["skills"]
    assert "context_inheritance" in overrides[TEAM_PLANNER]["toolkits"]
    assert "context_sharing" not in overrides[TEAM_PLANNER]["toolkits"]
    assert "team_context" not in overrides[TEAM_PLANNER]["toolkits"]
    assert overrides[TEAM_PLANNER]["tool_call_limit"] == 100
    assert "system_prompt" not in overrides[DEVELOPER]
    assert "sweevo-project-context" in overrides[DEVELOPER]["skills"]
    assert "system_prompt" not in overrides[VALIDATOR]
    assert "sweevo-project-context" in overrides[VALIDATOR]["skills"]
    assert "verification-replan" in overrides[VALIDATOR]["skills"]
    assert "system_prompt" not in overrides[TEAM_REPLANNER]
    assert "sweevo-project-context" in overrides[TEAM_REPLANNER]["skills"]


def test_planner_runtime_limits_preserve_shared_agent_budget():
    large_single_target = SimpleNamespace(
        instance_id="large-one",
        instance_id_swe="large-one",
        repo="example/repo",
        start_version="1.0.0",
        end_version="1.0.1",
        docker_image="example/image:latest",
        test_cmds="pytest -q",
        fail_to_pass=["tests/test_foo.py::test_bar"],
        pass_to_pass=[],
        problem_statement="- bullet\n" * 80,
    )
    assert _derive_planner_runtime_limits(large_single_target) == {
        "tool_call_limit": 100,
    }

    medium_multi_target = SimpleNamespace(
        instance_id="medium-three",
        instance_id_swe="medium-three",
        repo="example/repo",
        start_version="1.0.0",
        end_version="1.0.1",
        docker_image="example/image:latest",
        test_cmds="pytest -q",
        fail_to_pass=["a", "b", "c"],
        pass_to_pass=[],
        problem_statement="- bullet\n" * 10,
    )
    assert _derive_planner_runtime_limits(medium_multi_target) == {
        "tool_call_limit": 100,
    }


def test_sweevo_budgets_follow_instance_size_ceiling():
    instance = SimpleNamespace(
        repo="pydantic/pydantic",
        instance_id="wide-plan",
        instance_id_swe="wide-plan",
        start_version="2.6.0b1",
        end_version="2.6.0",
        docker_image="example/image:latest",
        test_cmds="pytest -q",
        problem_statement="- bullet\n" * 80,
        fail_to_pass=[f"tests/test_{i}.py::test_case" for i in range(20)],
        pass_to_pass=["tests/test_guard.py::test_existing"],
    )

    budgets = _derive_sweevo_budgets(instance)

    assert budgets.max_plan_size == 16


def test_checkpoint_repo_patch_from_store_returns_latest_matching_patch():
    store = SimpleNamespace(
        load_run=lambda _team_run_id: [
            TeamRunEvent(
                team_run_id="T1",
                kind="checkpoint_repo_state",
                data={"checkpoint_id": "cp-1", "repo_patch": "patch-a"},
            ),
            TeamRunEvent(
                team_run_id="T1",
                kind="checkpoint_repo_state",
                data={"checkpoint_id": "cp-2", "repo_patch": "patch-b"},
            ),
            TeamRunEvent(
                team_run_id="T1",
                kind="checkpoint_repo_state",
                data={"checkpoint_id": "cp-1", "repo_patch": "patch-a2"},
            ),
        ]
    )

    assert _checkpoint_repo_patch_from_store(store, "T1", "cp-1") == "patch-a2"
    assert _checkpoint_repo_patch_from_store(store, "T1", "cp-2") == "patch-b"
    assert _checkpoint_repo_patch_from_store(store, "T1", "missing") == ""

def test_enforce_validation_evidence_requires_daytona_bash():
    with pytest.raises(RuntimeError, match="validator_missing_tool_evidence"):
        _enforce_validation_evidence(
            "validator",
            [ConversationMessage(role="assistant", content=[TextBlock(text="VERDICT: PASS")])],
        )

    _enforce_validation_evidence(
        "validator",
        [
            ConversationMessage(
                role="assistant",
                content=[
                    ToolUseBlock(
                        id="tc1",
                        name="daytona_bash",
                        input={"command": "pytest -q"},
                    )
                ],
            )
        ],
    )


def test_resume_sweevo_team_uses_default_executor_factory_signature(monkeypatch):
    instance = SimpleNamespace(
        repo="pydantic/pydantic",
        instance_id="pydantic__pydantic_v2.6.0b1_v2.6.0",
        instance_id_swe="pydantic__pydantic_v2.6.0b1_v2.6.0",
        start_version="2.6.0b1",
        end_version="2.6.0",
        docker_image="example/image:latest",
        test_cmds="pytest -q",
        problem_statement="- bullet\n" * 80,
        fail_to_pass=["tests/test_foo.py::test_bar"],
        pass_to_pass=["tests/test_foo.py::test_existing"],
    )
    fake_tr = SimpleNamespace(
        id="team-run-1",
        sandbox_id="sbx-1",
        session_id="sess-1",
        budgets=SimpleNamespace(),
        dispatcher=SimpleNamespace(graph={}, list_checkpoints=lambda: []),
        resume=AsyncMock(),
        wait=AsyncMock(),
    )

    monkeypatch.setattr(sweevo_team_runner, "_register_team_builtins", lambda: None)
    monkeypatch.setattr(sweevo_team_runner, "_build_benchmark_event_store", lambda **_: object())
    monkeypatch.setattr(
        sweevo_team_runner,
        "_prepare_benchmark_session",
        lambda **_: (SimpleNamespace(session_id="sess-1"), object()),
    )
    monkeypatch.setattr(sweevo_team_runner, "_build_agent_overrides", lambda _instance: {})
    monkeypatch.setattr(sweevo_team_runner, "_build_team_metrics", lambda: {})
    monkeypatch.setattr(sweevo_team_runner, "_emit_team_runtime_banner", lambda *args, **kwargs: None)
    monkeypatch.setattr(sweevo_team_runner, "_checkpoint_records_from_store", lambda *args, **kwargs: [])
    monkeypatch.setattr(sweevo_team_runner, "_checkpoint_repo_patch_from_store", lambda *args, **kwargs: "")
    def fake_resume_from(_store, _team_run_id, *, checkpoint_id=None):
        assert checkpoint_id is None
        return fake_tr

    monkeypatch.setattr(
        sweevo_team_runner.TeamRun,
        "resume_from",
        staticmethod(fake_resume_from),
    )

    seen_factory_calls: list[dict[str, object]] = []

    def fake_make_executor_factory(
        session_config,
        sandbox_id,
        printer,
        *,
        repo_dir="/testbed",
        team_metrics=None,
        agent_overrides=None,
    ):
        seen_factory_calls.append(
            {
                "session_config": session_config,
                "sandbox_id": sandbox_id,
                "printer": printer,
                "agent_overrides": agent_overrides,
            }
        )
        return "executor-factory"

    monkeypatch.setattr(sweevo_team_runner, "_make_executor_factory", fake_make_executor_factory)
    monkeypatch.setattr(
        sweevo_team_runner,
        "_finalize_team_result",
        lambda **_: {"status": "ok"},
    )

    result = asyncio.run(
        sweevo_team_runner.resume_sweevo_team(
            instance,
            "team-run-1",
        )
    )

    assert result == {"status": "ok"}
    assert seen_factory_calls and seen_factory_calls[0]["sandbox_id"] == "sbx-1"
    assert seen_factory_calls[0]["agent_overrides"] == {}
    fake_tr.resume.assert_awaited_once_with(
        executor_factory="executor-factory",
        num_executors=sweevo_team_runner._DEFAULT_NUM_EXECUTORS,
        resumed_from="team-run-1",
        resumed_from_checkpoint=None,
    )


def test_resume_sweevo_team_restores_checkpoint_repo_patch(monkeypatch):
    instance = SimpleNamespace(
        repo="pydantic/pydantic",
        instance_id="pydantic__pydantic_v2.6.0b1_v2.6.0",
        instance_id_swe="pydantic__pydantic_v2.6.0b1_v2.6.0",
        start_version="2.6.0b1",
        end_version="2.6.0",
        docker_image="example/image:latest",
        test_cmds="pytest -q",
        problem_statement="- bullet\n" * 80,
        fail_to_pass=["tests/test_foo.py::test_bar"],
        pass_to_pass=["tests/test_foo.py::test_existing"],
    )
    fake_tr = SimpleNamespace(
        id="team-run-1",
        sandbox_id="sbx-1",
        session_id="sess-1",
        budgets=SimpleNamespace(),
        dispatcher=SimpleNamespace(graph={}, list_checkpoints=lambda: []),
        resume=AsyncMock(),
        wait=AsyncMock(),
    )

    monkeypatch.setattr(sweevo_team_runner, "_register_team_builtins", lambda: None)
    monkeypatch.setattr(sweevo_team_runner, "_build_benchmark_event_store", lambda **_: object())
    monkeypatch.setattr(
        sweevo_team_runner,
        "_prepare_benchmark_session",
        lambda **_: (SimpleNamespace(session_id="sess-1"), object()),
    )
    monkeypatch.setattr(sweevo_team_runner, "_build_agent_overrides", lambda _instance: {})
    monkeypatch.setattr(sweevo_team_runner, "_build_team_metrics", lambda: {})
    monkeypatch.setattr(sweevo_team_runner, "_emit_team_runtime_banner", lambda *args, **kwargs: None)
    monkeypatch.setattr(
        sweevo_team_runner,
        "_checkpoint_records_from_store",
        lambda *args, **kwargs: [{"id": "cp-1", "label": "durable:complete:developer:dev1", "sequence": 1}],
    )
    monkeypatch.setattr(
        sweevo_team_runner,
        "_checkpoint_repo_patch_from_store",
        lambda *args, **kwargs: "diff --git a/x b/x",
    )
    monkeypatch.setattr(
        sweevo_team_runner.TeamRun,
        "resume_from",
        staticmethod(lambda *_args, **_kwargs: fake_tr),
    )
    monkeypatch.setattr(sweevo_team_runner, "setup_sweevo_sandbox", AsyncMock())
    monkeypatch.setattr(sweevo_team_runner, "apply_sweevo_repo_patch", AsyncMock())
    monkeypatch.setattr(sweevo_team_runner, "_make_executor_factory", lambda *args, **kwargs: "executor-factory")
    monkeypatch.setattr(
        sweevo_team_runner,
        "_finalize_team_result",
        lambda **_: {"status": "ok"},
    )

    result = asyncio.run(
        sweevo_team_runner.resume_sweevo_team(
            instance,
            "team-run-1",
            checkpoint_id="cp-1",
        )
    )

    assert result == {"status": "ok"}
    sweevo_team_runner.setup_sweevo_sandbox.assert_awaited_once_with(instance, "sbx-1", "/testbed")
    sweevo_team_runner.apply_sweevo_repo_patch.assert_awaited_once_with(
        "sbx-1",
        "diff --git a/x b/x",
        "/testbed",
    )
    fake_tr.resume.assert_awaited_once()


def test_make_runner_uses_agent_definition_limits(monkeypatch):
    captured_agents: list[SimpleNamespace] = []

    class _Tracker:
        def __init__(self) -> None:
            self.run_id = "run-1"

        def finish(self, **_: object) -> None:
            return None

    async def _fake_run(_prompt: str):
        if False:
            yield None

    def fake_spawn_agent(*_args, **_kwargs):
        agent = SimpleNamespace(
            query_context=SimpleNamespace(
                tool_metadata=ExecutionMetadata(session_config="cfg", sandbox_id="sbx-1"),
                run_id="",
                tool_call_limit=_kwargs["agent_def"].tool_call_limit,
                api_messages_snapshot=None,
            ),
            display_messages=[],
            total_usage=None,
            model="test-model",
            run=_fake_run,
        )
        captured_agents.append(agent)
        return agent

    monkeypatch.setattr(
        sweevo_team_runner,
        "AgentRunTracker",
        SimpleNamespace(create=lambda **_: _Tracker()),
    )
    monkeypatch.setattr(sweevo_team_runner, "spawn_agent", fake_spawn_agent)

    runner = _make_runner(
        session_config=SimpleNamespace(session_id="sess-1"),
        sandbox_id="sbx-1",
        printer=None,
        agent_overrides={"team_planner": {"tool_call_limit": 50}},
    )
    ctx = sweevo_team_runner.TeamAgentContext(
        user_message="Plan it",
        tool_metadata=ExecutionMetadata(team_run_id="TR1", work_item_id="W1"),
    )

    asyncio.run(
        runner(
            SimpleNamespace(
                name="team_planner",
                model_copy=lambda update: SimpleNamespace(name="team_planner", **update),
            ),
            ctx,
        )
    )

    assert captured_agents
    assert captured_agents[0].query_context.tool_metadata.agent_name == "team_planner"
    assert captured_agents[0].query_context.tool_call_limit == 50


def test_make_runner_persists_full_compaction_delta(monkeypatch):
    tracker_finishes: list[dict[str, object]] = []
    printed: list[tuple[str, str]] = []

    class _Tracker:
        run_id = "run-1"

        def finish(self, **kwargs: object) -> None:
            tracker_finishes.append(kwargs)

    async def _fake_run(_prompt: str):
        state.compacted = 3
        query_context.tool_calls_used = 4
        if False:
            yield None

    state = SimpleNamespace(compacted=1)
    query_context = SimpleNamespace(
        tool_metadata=ExecutionMetadata(session_config="cfg", sandbox_id="sbx-1"),
        run_id="",
        tool_call_limit=10,
        tool_calls_used=0,
        session_state=state,
        api_messages_snapshot=["snapshot"],
    )
    agent = SimpleNamespace(
        query_context=query_context,
        display_messages=[],
        total_usage=SimpleNamespace(input_tokens=12, output_tokens=8),
        model="test-model",
        run=_fake_run,
    )

    monkeypatch.setattr(
        sweevo_team_runner,
        "AgentRunTracker",
        SimpleNamespace(create=lambda **_: _Tracker()),
    )
    monkeypatch.setattr(sweevo_team_runner, "spawn_agent", lambda *_args, **_kwargs: agent)
    monkeypatch.setattr(sweevo_team_runner, "_estimate_final_context", lambda _messages: 321)
    monkeypatch.setattr(sweevo_team_runner, "_persist_benchmark_session", lambda **_: None)

    runner = _make_runner(
        session_config=SimpleNamespace(session_id="sess-1"),
        sandbox_id="sbx-1",
        printer=SimpleNamespace(
            raw_line=lambda who, body: printed.append((who, body)),
            emit=lambda _event: None,
        ),
    )
    ctx = sweevo_team_runner.TeamAgentContext(
        user_message="Ship it",
        tool_metadata=ExecutionMetadata(team_run_id="TR1", work_item_id="W1"),
    )

    asyncio.run(
        runner(
            SimpleNamespace(name="developer", model_copy=lambda update: SimpleNamespace(name="developer", **update)),
            ctx,
        )
    )

    assert tracker_finishes
    response = tracker_finishes[0]["response"]
    assert isinstance(response, dict)
    assert response["tool_calls_used"] == 4
    assert response["tool_call_limit"] == 10
    assert response["final_context_tokens"] == 321
    assert response["compactions_added"] == 2
    assert response["compacted"] == 3
    assert any(
        body == "[usage] prompt=12 completion=8 total=20 tool_calls=4/10 final_context=321 compactions=+2(total=3)"
        for _, body in printed
    )


def test_finalize_team_result_surfaces_retry_replan_and_checkpoint_metadata(monkeypatch):
    printed: list[tuple[str, str]] = []
    fake_usage_store = SimpleNamespace(
        is_ready=True,
        get_session_usage=lambda _session_id: {
            "prompt_tokens": 11,
            "completion_tokens": 7,
            "total_tokens": 18,
            "run_count": 2,
        },
        get_usage_by_model=lambda _session_id: [{"model_id": "test-model", "total_tokens": 18}],
    )
    monkeypatch.setattr("server.app_factory.usage_store", fake_usage_store, raising=False)

    result = sweevo_team_runner._finalize_team_result(
        tr=SimpleNamespace(
            id="TR1",
            status=sweevo_team_runner.TeamRunStatus.SUCCEEDED,
            sandbox_id="sbx-1",
            budget_state=SimpleNamespace(replans_used=2),
            dispatcher=SimpleNamespace(
                graph={
                    "A": WorkItem(
                        id="A",
                        team_run_id="TR1",
                        agent_name="developer",
                        status=WorkItemStatus.DONE,
                        kind=WorkItemKind.ATOMIC,
                        retry_count=1,
                    ),
                    "B": WorkItem(
                        id="B",
                        team_run_id="TR1",
                        agent_name="validator",
                        status=WorkItemStatus.DONE,
                        kind=WorkItemKind.ATOMIC,
                        retry_count=2,
                        depth=1,
                    ),
                },
                list_checkpoints=lambda: [],
            ),
        ),
        session_config=SimpleNamespace(session_id="sess-1"),
        team_metrics={
            "agent_runs": 4,
            "agent_counts": Counter({"developer": 2, "validator": 2}),
            "checkpoint_ids": [],
            "checkpoints": [],
        },
        budgets=SimpleNamespace(
            max_work_items=10,
            max_depth=5,
            max_plan_size=6,
            max_shared_briefings=100,
            max_briefing_bytes=4096,
        ),
        printer=SimpleNamespace(raw_line=lambda who, body: printed.append((who, body))),
        checkpoint_records=[
            {"id": "cp-1", "label": "planner:W1", "sequence": 1},
            {"id": "cp-2", "label": "durable:complete:developer:A", "sequence": 2},
        ],
        resumed_from="TR0",
        resumed_from_checkpoint="cp-1",
    )

    assert result["retry_count_total"] == 3
    assert result["replans_used"] == 2
    assert result["checkpoints"][-1]["label"] == "durable:complete:developer:A"
    assert result["latest_checkpoint_id"] == "cp-2"
    assert result["latest_checkpoint_label"] == "durable:complete:developer:A"
    assert any(
        body == "[team_stats] work_items=2 max_depth=1 agent_runs=4 checkpoints=2 retries=3 replans=2"
        for _, body in printed
    )


def test_emit_dispatcher_dag_logs_graph_lines():
    lines: list[tuple[str, str]] = []
    printer = SimpleNamespace(raw_line=lambda agent, body: lines.append((agent, body)))
    root = WorkItem(
        id="root-1",
        team_run_id="TR1",
        agent_name="team_planner",
        status=WorkItemStatus.DONE,
        kind=WorkItemKind.EXPANDABLE,
        local_id="plan1",
        depth=0,
    )
    child = WorkItem(
        id="child-1",
        team_run_id="TR1",
        agent_name="developer",
        status=WorkItemStatus.READY,
        kind=WorkItemKind.ATOMIC,
        deps=["root-1"],
        local_id="dev1",
        depth=1,
    )
    team_run = SimpleNamespace(dispatcher=SimpleNamespace(graph={root.id: root, child.id: child}))

    _emit_dispatcher_dag(printer, team_run, trigger_agent="team_planner")

    assert lines[0] == ("team", "[dag] after=team_planner nodes=2")
    assert any("plan1 agent=team_planner" in body for _, body in lines[1:])
    assert any("dev1 agent=developer" in body and "deps=['plan1']" in body for _, body in lines[1:])


def test_sweevo_printer_surfaces_scout_triggered_atlas_info() -> None:
    lines: list[str] = []
    printer = MultiAgentEventPrinter(color=False, sink=lines.append)

    printer.emit(
        BackgroundTaskCompleted(
            task_id="bg_scout",
            tool_name="run_subagent",
            output=json.dumps(
                {
                    "kind": "brief",
                    "run_id": "run-1",
                    "summary": "Scout summary",
                    "artifact_ref": "scout:src/auth",
                    "payload": {"target_paths": ["src/auth"]},
                    "atlas": {
                        "subsystem": "src/auth",
                        "persisted": True,
                        "promoted": True,
                        "artifact_ref": "scout:src/auth",
                        "reason": "run_subagent:scout-complete",
                    },
                }
            ),
            agent_name="team_planner",
            work_id="planner123",
        )
    )

    assert any(
        line == (
            "[team_planner  ] [planner123] [atlas] subsystem=src/auth persisted=true "
            "promoted=true artifact=scout:src/auth reason=run_subagent:scout-complete"
        )
        for line in lines
    )
