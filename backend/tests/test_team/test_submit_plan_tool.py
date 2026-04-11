"""Unit tests for tools.posthook.submit_plan.SubmitPlanTool."""

from __future__ import annotations

import json
from pathlib import Path
from types import SimpleNamespace

import pytest

from team.models import Plan, WorkItemKind
from tools.core.base import ExecutionMetadata, ToolExecutionContext
from tools.posthook import SubmitPlanInput, SubmitPlanTool


@pytest.fixture(autouse=True)
def _all_agents_exist(monkeypatch):
    from team.planning import validation

    monkeypatch.setattr(validation, "_agent_exists", lambda name: True)


def _ctx() -> ToolExecutionContext:
    return ToolExecutionContext(cwd=Path.cwd(), metadata=ExecutionMetadata())


def test_submit_plan_input_accepts_json_string_items() -> None:
    args = SubmitPlanInput.model_validate(
        {"items": json.dumps([{"agent_name": "developer", "local_id": "w1"}])}
    )
    assert len(args.items) == 1
    assert args.items[0].agent_name == "developer"


def test_submit_plan_input_normalizes_legacy_agent_and_id_fields() -> None:
    args = SubmitPlanInput.model_validate(
        {
            "items": [
                {
                    "id": "fix_compat_make_bytes_tuple_version",
                    "agent": "developer",
                    "kind": "atomic",
                    "payload": {"owned_files": ["dask/compatibility.py"]},
                },
                {
                    "id": "validate_all",
                    "agent": "validator",
                    "kind": "atomic",
                    "deps": ["fix_compat_make_bytes_tuple_version"],
                    "payload": {
                        "briefings": [{"name": "b", "source": "inline", "inline": "note"}],
                        "verify": ["pytest dask/tests/test_compatibility.py -q"],
                    },
                },
            ]
        }
    )
    assert args.items[0].agent_name == "developer"
    assert args.items[0].local_id == "fix_compat_make_bytes_tuple_version"
    assert args.items[1].agent_name == "validator"
    assert args.items[1].local_id == "validate_all"
    assert args.items[1].briefings[0].name == "b"
    assert "briefings" not in args.items[1].payload


def test_submit_plan_input_infers_registered_agent_name_from_local_id_like_name() -> None:
    args = SubmitPlanInput.model_validate(
        {
            "items": [
                {
                    "agent_name": "fix_config_cli_integration",
                    "kind": "atomic",
                    "payload": {"owned_files": ["dask/config.py", "dask/cli.py"]},
                },
                {
                    "agent_name": "validate_all",
                    "kind": "atomic",
                    "deps": ["fix_config_cli_integration"],
                    "payload": {"verify": ["pytest dask/tests/test_config.py -q"]},
                },
                {
                    "agent_name": "misc_dask_child_planner",
                    "kind": "expandable",
                    "payload": {"owned_files": ["dask/config.py"]},
                },
            ]
        }
    )
    assert args.items[0].agent_name == "developer"
    assert args.items[0].local_id == "fix_config_cli_integration"
    assert args.items[1].agent_name == "validator"
    assert args.items[1].local_id == "validate_all"
    assert args.items[2].agent_name == "team_planner"
    assert args.items[2].local_id == "misc_dask_child_planner"


@pytest.mark.asyncio
async def test_valid_plan_accepted_and_stashed():
    tool = SubmitPlanTool()
    ctx = _ctx()
    args = SubmitPlanInput.model_validate(
        {
            "items": [
                {"agent_name": "developer", "local_id": "w1"},
                {"agent_name": "validator", "local_id": "w2", "deps": ["w1"]},
            ]
        }
    )
    res = await tool.execute(args, ctx)
    assert not res.is_error
    stashed = ctx.metadata["submitted_plan"]
    assert isinstance(stashed, Plan)
    assert len(stashed.items) == 2


@pytest.mark.asyncio
async def test_invalid_plan_returns_structured_error(monkeypatch):
    from team.planning import validation

    monkeypatch.setattr(validation, "_agent_exists", lambda name: name != "ghost")
    tool = SubmitPlanTool()
    ctx = _ctx()
    args = SubmitPlanInput.model_validate({"items": [{"agent_name": "ghost"}]})
    res = await tool.execute(args, ctx)
    assert res.is_error
    assert "unknown agent" in res.output
    assert "submitted_plan" not in ctx.metadata


@pytest.mark.asyncio
async def test_internal_cycle_rejected():
    tool = SubmitPlanTool()
    ctx = _ctx()
    args = SubmitPlanInput.model_validate(
        {
            "items": [
                {"agent_name": "a", "local_id": "w1", "deps": ["w2"]},
                {"agent_name": "a", "local_id": "w2", "deps": ["w1"]},
            ]
        }
    )
    res = await tool.execute(args, ctx)
    assert res.is_error
    assert "cycle" in res.output


@pytest.mark.asyncio
async def test_single_submission_guard():
    tool = SubmitPlanTool()
    ctx = _ctx()
    args = SubmitPlanInput.model_validate({"items": [{"agent_name": "developer"}]})
    res1 = await tool.execute(args, ctx)
    assert not res1.is_error
    res2 = await tool.execute(args, ctx)
    assert res2.is_error
    assert "already called" in res2.output


@pytest.mark.asyncio
async def test_max_plan_size_respects_metadata_override():
    tool = SubmitPlanTool()
    ctx = _ctx()
    ctx.metadata["max_plan_size"] = 1
    args = SubmitPlanInput.model_validate(
        {
            "items": [
                {"agent_name": "a", "local_id": "w1"},
                {"agent_name": "a", "local_id": "w2"},
            ]
        }
    )
    res = await tool.execute(args, ctx)
    assert res.is_error
    assert "max_plan_size" in res.output


@pytest.mark.asyncio
async def test_submit_plan_accepts_empty_plan_for_non_root_child_planner(monkeypatch):
    tool = SubmitPlanTool()
    ctx = _ctx()
    ctx.metadata["team_run_id"] = "TR_CHILD"
    ctx.metadata["work_item_id"] = "CHILD"

    root = SimpleNamespace(agent_name="team_planner", kind=WorkItemKind.EXPANDABLE)
    child = SimpleNamespace(agent_name="team_planner", kind=WorkItemKind.EXPANDABLE)
    fake_team_run = SimpleNamespace(
        root_work_item_id="ROOT",
        dispatcher=SimpleNamespace(graph={"ROOT": root, "CHILD": child}),
    )

    from team.runtime import registry as runtime_registry

    monkeypatch.setattr(
        runtime_registry,
        "get",
        lambda team_run_id: fake_team_run if team_run_id == "TR_CHILD" else None,
    )

    args = SubmitPlanInput.model_validate({"items": []})

    res = await tool.execute(args, ctx)

    assert not res.is_error
    stashed = ctx.metadata["submitted_plan"]
    assert isinstance(stashed, Plan)
    assert stashed.items == []


@pytest.mark.asyncio
async def test_validator_policy_respects_metadata_overrides():
    tool = SubmitPlanTool()
    ctx = _ctx()
    ctx.metadata["max_validators_per_plan"] = 1
    ctx.metadata["require_validator_for_plan_size"] = 3

    no_validator = SubmitPlanInput.model_validate(
        {
            "items": [
                {"agent_name": "a", "local_id": "w1"},
                {"agent_name": "a", "local_id": "w2"},
                {"agent_name": "a", "local_id": "w3"},
            ]
        }
    )
    res = await tool.execute(no_validator, ctx)
    assert res.is_error
    assert "3 or more concrete non-planner items must include at least one terminal validator" in res.output

    too_many_validators = SubmitPlanInput.model_validate(
        {
            "items": [
                {"agent_name": "validator", "local_id": "v1"},
                {"agent_name": "validator", "local_id": "v2"},
            ]
        }
    )
    fresh_ctx = _ctx()
    fresh_ctx.metadata["max_validators_per_plan"] = 1
    res = await tool.execute(too_many_validators, fresh_ctx)
    assert res.is_error
    assert "submitted plans may have at most 1" in res.output


@pytest.mark.asyncio
async def test_submit_plan_requires_terminal_validator_for_three_developers():
    tool = SubmitPlanTool()
    ctx = _ctx()
    args = SubmitPlanInput.model_validate(
        {
            "items": [
                {"agent_name": "developer", "local_id": "dev1"},
                {"agent_name": "developer", "local_id": "dev2"},
                {"agent_name": "developer", "local_id": "dev3"},
            ]
        }
    )

    res = await tool.execute(args, ctx)

    assert res.is_error
    assert "plans with 3 or more concrete non-planner items must include at least one terminal validator" in res.output


@pytest.mark.asyncio
async def test_submit_plan_rejects_terminal_validator_without_deps() -> None:
    tool = SubmitPlanTool()
    ctx = _ctx()
    args = SubmitPlanInput.model_validate(
        {
            "items": [
                {"agent_name": "developer", "local_id": "dev1"},
                {"agent_name": "developer", "local_id": "dev2"},
                {"agent_name": "developer", "local_id": "dev3"},
                {"agent_name": "validator", "local_id": "val"},
            ]
        }
    )

    res = await tool.execute(args, ctx)

    assert res.is_error
    assert "validator items must depend on at least one upstream sibling" in res.output


@pytest.mark.asyncio
async def test_submit_plan_rejects_terminal_validator_missing_terminal_leaf_deps() -> None:
    tool = SubmitPlanTool()
    ctx = _ctx()
    args = SubmitPlanInput.model_validate(
        {
            "items": [
                {"agent_name": "developer", "local_id": "dev1"},
                {"agent_name": "developer", "local_id": "dev2"},
                {"agent_name": "developer", "local_id": "dev3"},
                {"agent_name": "validator", "local_id": "val", "deps": ["dev1"]},
            ]
        }
    )

    res = await tool.execute(args, ctx)

    assert res.is_error
    assert "terminal validator must depend on every terminal concrete sibling" in res.output


@pytest.mark.asyncio
async def test_submit_plan_allows_two_developers_plus_child_planner_without_parent_validator():
    tool = SubmitPlanTool()
    ctx = _ctx()
    args = SubmitPlanInput.model_validate(
        {
            "items": [
                {"agent_name": "developer", "local_id": "dev1"},
                {"agent_name": "developer", "local_id": "dev2"},
                {"agent_name": "team_planner", "local_id": "child", "kind": "expandable"},
            ]
        }
    )

    res = await tool.execute(args, ctx)

    assert not res.is_error


@pytest.mark.asyncio
async def test_submit_plan_rejects_three_validators():
    tool = SubmitPlanTool()
    ctx = _ctx()
    args = SubmitPlanInput.model_validate(
        {
            "items": [
                {"agent_name": "validator", "local_id": "val1"},
                {"agent_name": "validator", "local_id": "val2"},
                {"agent_name": "validator", "local_id": "val3"},
            ]
        }
    )

    res = await tool.execute(args, ctx)

    assert res.is_error
    assert "plan has 3 validator items; submitted plans may have at most 2" in res.output


@pytest.mark.asyncio
async def test_submit_plan_requires_terminal_validator_when_any_validator_exists():
    tool = SubmitPlanTool()
    ctx = _ctx()
    args = SubmitPlanInput.model_validate(
        {
            "items": [
                {"agent_name": "developer", "local_id": "dev1"},
                {"agent_name": "validator", "local_id": "val1", "deps": ["dev1"]},
                {"agent_name": "a", "local_id": "followup", "deps": ["val1"]},
            ]
        }
    )

    res = await tool.execute(args, ctx)

    assert res.is_error
    assert (
        "plans with validator items must leave at least one validator as a terminal end-of-chain guard"
        in res.output
    )


@pytest.mark.asyncio
async def test_submit_plan_rejects_multiple_terminal_validators():
    tool = SubmitPlanTool()
    ctx = _ctx()
    args = SubmitPlanInput.model_validate(
        {
            "items": [
                {"agent_name": "developer", "local_id": "dev1"},
                {"agent_name": "developer", "local_id": "dev2"},
                {"agent_name": "validator", "local_id": "val1", "deps": ["dev1"]},
                {"agent_name": "validator", "local_id": "val2", "deps": ["dev2"]},
            ]
        }
    )

    res = await tool.execute(args, ctx)

    assert res.is_error
    assert (
        "plans with validator items must keep exactly one validator as the terminal end-of-chain guard"
        in res.output
    )


@pytest.mark.asyncio
async def test_submit_plan_rejects_validator_depending_on_expandable_sibling():
    tool = SubmitPlanTool()
    ctx = _ctx()
    args = SubmitPlanInput.model_validate(
        {
            "items": [
                {"agent_name": "developer", "local_id": "dev1"},
                {"agent_name": "team_planner", "local_id": "child", "kind": "expandable"},
                {"agent_name": "validator", "local_id": "val1", "deps": ["child"]},
            ]
        }
    )

    res = await tool.execute(args, ctx)

    assert res.is_error
    assert "validator items must not depend on expandable siblings" in res.output


@pytest.mark.asyncio
async def test_submit_plan_rejects_unknown_dep_against_live_team_run(monkeypatch):
    tool = SubmitPlanTool()
    ctx = _ctx()
    ctx.metadata["team_run_id"] = "TR1"

    fake_team_run = type(
        "FakeTeamRun",
        (),
        {"dispatcher": type("FakeDispatcher", (), {"graph": {"ROOT": object()}})()},
    )()

    from team.runtime import registry as runtime_registry

    monkeypatch.setattr(runtime_registry, "get", lambda team_run_id: fake_team_run if team_run_id == "TR1" else None)

    args = SubmitPlanInput.model_validate(
        {
            "items": [
                {"agent_name": "developer", "local_id": "dev_alpha"},
                {"agent_name": "validator", "local_id": "val", "deps": ["dev1"]},
            ]
        }
    )

    res = await tool.execute(args, ctx)

    assert res.is_error
    assert "unknown dep reference 'dev1'" in res.output


@pytest.mark.asyncio
async def test_submit_plan_rejects_benchmark_ref_aliases_against_root_prompt(monkeypatch):
    tool = SubmitPlanTool()
    ctx = _ctx()
    ctx.metadata["team_run_id"] = "TR1"

    root = SimpleNamespace(
        payload={
            "fail_to_pass": [
                "dask/dataframe/io/tests/test_hdf.py::test_read_hdf",
            ],
            "pass_to_pass": [
                "dask/dataframe/io/tests/test_hdf.py::test_to_hdf",
            ],
        }
    )
    fake_team_run = SimpleNamespace(
        root_work_item_id="ROOT",
        dispatcher=SimpleNamespace(graph={"ROOT": root}),
    )

    from team.runtime import registry as runtime_registry

    monkeypatch.setattr(runtime_registry, "get", lambda team_run_id: fake_team_run if team_run_id == "TR1" else None)

    args = SubmitPlanInput.model_validate(
        {
            "items": [
                {
                    "agent_name": "developer",
                    "local_id": "dev_hdf",
                    "payload": {
                        "owned_failures": ["tests/test_hdf.py"],
                        "reproduction": ["pytest tests/test_hdf.py -q"],
                        "verify": ["pytest tests/test_hdf.py -q"],
                        "retries": ["pytest tests/test_hdf.py::test_read_hdf -q"],
                    },
                }
            ]
        }
    )

    res = await tool.execute(args, ctx)

    assert res.is_error
    assert "benchmark reference must use the exact prompt path/id" in res.output
    assert "expected 'dask/dataframe/io/tests/test_hdf.py'" in res.output
    assert "payload.verify[0]" in res.output
    assert "payload.retries[0]" in res.output


@pytest.mark.asyncio
async def test_submit_plan_accepts_exact_benchmark_refs_against_root_prompt(monkeypatch):
    tool = SubmitPlanTool()
    ctx = _ctx()
    ctx.metadata["team_run_id"] = "TR2"

    root = SimpleNamespace(
        payload={
            "fail_to_pass": [
                "dask/dataframe/io/tests/test_hdf.py::test_read_hdf",
            ],
            "pass_to_pass": [
                "dask/dataframe/io/tests/test_hdf.py::test_to_hdf",
            ],
        }
    )
    fake_team_run = SimpleNamespace(
        root_work_item_id="ROOT",
        dispatcher=SimpleNamespace(graph={"ROOT": root}),
    )

    from team.runtime import registry as runtime_registry

    monkeypatch.setattr(runtime_registry, "get", lambda team_run_id: fake_team_run if team_run_id == "TR2" else None)

    args = SubmitPlanInput.model_validate(
        {
            "items": [
                {
                    "agent_name": "developer",
                    "local_id": "dev_hdf",
                    "payload": {
                        "owned_failures": [
                            "dask/dataframe/io/tests/test_hdf.py::test_read_hdf"
                        ],
                        "verification": [
                            "pytest dask/dataframe/io/tests/test_hdf.py -q"
                        ],
                    },
                }
            ]
        }
    )

    res = await tool.execute(args, ctx)

    assert not res.is_error
    assert isinstance(ctx.metadata["submitted_plan"], Plan)


@pytest.mark.asyncio
async def test_submit_plan_normalizes_guessed_benchmark_repo_root_prefixes(monkeypatch):
    tool = SubmitPlanTool()
    ctx = _ctx()
    ctx.metadata["team_run_id"] = "TR2A"
    ctx.metadata["daytona_cwd"] = "/testbed"

    root = SimpleNamespace(
        payload={
            "fail_to_pass": [
                "dask/dataframe/io/tests/test_hdf.py::test_read_hdf",
            ],
            "pass_to_pass": [
                "dask/dataframe/io/tests/test_hdf.py::test_to_hdf",
            ],
        }
    )
    fake_team_run = SimpleNamespace(
        root_work_item_id="ROOT",
        dispatcher=SimpleNamespace(graph={"ROOT": root}),
    )

    from team.runtime import registry as runtime_registry

    monkeypatch.setattr(
        runtime_registry, "get", lambda team_run_id: fake_team_run if team_run_id == "TR2A" else None
    )

    args = SubmitPlanInput.model_validate(
        {
            "items": [
                {
                    "agent_name": "developer",
                    "local_id": "dev_hdf",
                    "payload": {
                        "owned_failures": [
                            "dask/dataframe/io/tests/test_hdf.py::test_read_hdf"
                        ],
                        "reproduction": [
                            "cd /home && python -m pytest dask/dataframe/io/tests/test_hdf.py -x -q"
                        ],
                        "verify": [
                            "cd /home && python -m pytest dask/dataframe/io/tests/test_hdf.py -q"
                        ],
                    },
                }
            ]
        }
    )

    res = await tool.execute(args, ctx)

    assert not res.is_error
    plan = ctx.metadata["submitted_plan"]
    assert isinstance(plan, Plan)
    assert plan.items[0].payload["reproduction"] == [
        "python -m pytest dask/dataframe/io/tests/test_hdf.py -x -q"
    ]
    assert plan.items[0].payload["verify"] == [
        "python -m pytest dask/dataframe/io/tests/test_hdf.py -q"
    ]


@pytest.mark.asyncio
async def test_submit_plan_normalizes_owned_failures_to_exact_benchmark_file(monkeypatch):
    tool = SubmitPlanTool()
    ctx = _ctx()
    ctx.metadata["team_run_id"] = "TR2C"

    root = SimpleNamespace(
        payload={
            "fail_to_pass": [
                "dask/dataframe/io/tests/test_io.py::test_read_hdf_key_raises[dask]",
            ],
            "pass_to_pass": [],
        }
    )
    fake_team_run = SimpleNamespace(
        root_work_item_id="ROOT",
        dispatcher=SimpleNamespace(graph={"ROOT": root}),
    )

    from team.runtime import registry as runtime_registry

    monkeypatch.setattr(
        runtime_registry, "get", lambda team_run_id: fake_team_run if team_run_id == "TR2C" else None
    )

    args = SubmitPlanInput.model_validate(
        {
            "items": [
                {
                    "agent_name": "developer",
                    "local_id": "dev_io",
                    "payload": {
                        "owned_failures": [
                            "dask/dataframe/tests/test_io.py::test_read_hdf_key_raises[dask]#2-0"
                        ],
                    },
                }
            ]
        }
    )

    res = await tool.execute(args, ctx)

    assert not res.is_error
    plan = ctx.metadata["submitted_plan"]
    assert isinstance(plan, Plan)
    assert plan.items[0].payload["owned_failures"] == ["dask/dataframe/io/tests/test_io.py"]


@pytest.mark.asyncio
async def test_submit_plan_preserves_in_repo_and_relative_benchmark_cd_prefixes(monkeypatch):
    tool = SubmitPlanTool()
    ctx = _ctx()
    ctx.metadata["team_run_id"] = "TR2B"
    ctx.metadata["daytona_cwd"] = "/testbed"

    root = SimpleNamespace(
        payload={
            "fail_to_pass": [
                "dask/dataframe/io/tests/test_hdf.py::test_read_hdf",
            ],
            "pass_to_pass": [
                "dask/dataframe/io/tests/test_hdf.py::test_to_hdf",
            ],
        }
    )
    fake_team_run = SimpleNamespace(
        root_work_item_id="ROOT",
        dispatcher=SimpleNamespace(graph={"ROOT": root}),
    )

    from team.runtime import registry as runtime_registry

    monkeypatch.setattr(
        runtime_registry, "get", lambda team_run_id: fake_team_run if team_run_id == "TR2B" else None
    )

    args = SubmitPlanInput.model_validate(
        {
            "items": [
                {
                    "agent_name": "developer",
                    "local_id": "dev_hdf",
                    "payload": {
                        "owned_failures": [
                            "dask/dataframe/io/tests/test_hdf.py::test_read_hdf"
                        ],
                        "reproduction": [
                            "cd /testbed && python -m pytest dask/dataframe/io/tests/test_hdf.py -x -q"
                        ],
                        "verify": [
                            "cd /testbed/subdir && python -m pytest dask/dataframe/io/tests/test_hdf.py -q"
                        ],
                        "retries": [
                            "cd subdir && python -m pytest dask/dataframe/io/tests/test_hdf.py::test_read_hdf -q"
                        ],
                    },
                }
            ]
        }
    )

    res = await tool.execute(args, ctx)

    assert not res.is_error
    plan = ctx.metadata["submitted_plan"]
    assert isinstance(plan, Plan)
    assert plan.items[0].payload["reproduction"] == [
        "cd /testbed && python -m pytest dask/dataframe/io/tests/test_hdf.py -x -q"
    ]
    assert plan.items[0].payload["verify"] == [
        "cd /testbed/subdir && python -m pytest dask/dataframe/io/tests/test_hdf.py -q"
    ]
    assert plan.items[0].payload["retries"] == [
        "cd subdir && python -m pytest dask/dataframe/io/tests/test_hdf.py::test_read_hdf -q"
    ]


@pytest.mark.asyncio
async def test_submit_plan_suggests_exact_file_path_for_invented_node_on_real_benchmark_file(
    monkeypatch,
):
    tool = SubmitPlanTool()
    ctx = _ctx()
    ctx.metadata["team_run_id"] = "TR3"

    root = SimpleNamespace(
        payload={
            "fail_to_pass": [
                "dask/dataframe/io/tests/test_hdf.py::test_read_hdf",
            ],
            "pass_to_pass": [
                "dask/dataframe/io/tests/test_hdf.py::test_to_hdf",
            ],
        }
    )
    fake_team_run = SimpleNamespace(
        root_work_item_id="ROOT",
        dispatcher=SimpleNamespace(graph={"ROOT": root}),
    )

    from team.runtime import registry as runtime_registry

    monkeypatch.setattr(runtime_registry, "get", lambda team_run_id: fake_team_run if team_run_id == "TR3" else None)

    args = SubmitPlanInput.model_validate(
        {
            "items": [
                {
                    "agent_name": "developer",
                    "local_id": "dev_hdf",
                    "payload": {
                        "owned_failures": [
                            "dask/dataframe/io/tests/test_hdf.py::test_made_up_node"
                        ],
                    },
                }
            ]
        }
    )

    res = await tool.execute(args, ctx)

    assert res.is_error
    assert "benchmark reference must use the exact prompt path/id" in res.output
    assert "expected 'dask/dataframe/io/tests/test_hdf.py'" in res.output
