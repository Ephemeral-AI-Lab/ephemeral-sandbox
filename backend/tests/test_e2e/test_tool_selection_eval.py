# ruff: noqa
"""LLM Tool Selection Eval — tests whether the model picks the right tool.

Each test sends a natural-language intent to the configured LLM with the
full Daytona toolkit, then verifies:
  1. The model selected the correct tool(s)
  2. The input parameters are well-formed

Uses EvalAgent for credential loading and agent configuration.
Run with:
    .venv/bin/python -m pytest backend/tests/test_e2e/test_tool_selection_eval.py -v -s
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field

import pytest

from engine.testing.eval_agent import EvalAgent
from tests.test_e2e.conftest import (
    create_eval_agent,
    create_test_sandbox,
    delete_test_sandbox,
    populate_sandbox_files,
)

pytestmark = [pytest.mark.e2e, pytest.mark.live]

HAS_CREDENTIALS = EvalAgent.has_credentials()


# ---------------------------------------------------------------------------
# Eval case definitions
# ---------------------------------------------------------------------------


@dataclass
class EvalCase:
    name: str
    prompt: str
    expected_tools: list[str]
    required_params: dict[str, list[str]] = field(default_factory=dict)
    description: str = ""


EVAL_CASES = [
    # -- File operations --
    EvalCase(
        name="list_directory",
        prompt="Show me what files are in the src directory.",
        expected_tools=["daytona_shell"],
        required_params={"daytona_shell": ["command"]},
    ),
    EvalCase(
        name="read_file",
        prompt="Read the contents of src/main.py",
        expected_tools=["daytona_read_file"],
        required_params={"daytona_read_file": ["file_path"]},
    ),
    EvalCase(
        name="read_file_range",
        prompt="Show me lines 10 through 25 of src/utils.py",
        expected_tools=["daytona_read_file"],
        required_params={"daytona_read_file": ["file_path"]},
    ),
    EvalCase(
        name="write_file",
        prompt="Create a new file at src/config.py with the content:\n\nDEBUG = True\nPORT = 8080",
        expected_tools=["daytona_write_file"],
        required_params={"daytona_write_file": ["file_path", "content"]},
    ),
    EvalCase(
        name="edit_file",
        prompt="In src/main.py, replace 'DEBUG = False' with 'DEBUG = True'",
        expected_tools=["daytona_edit_file", "daytona_read_file"],
        required_params={},
    ),
    # -- Search operations --
    EvalCase(
        name="grep_search",
        prompt="Search for all occurrences of 'TODO' in the src directory.",
        expected_tools=["daytona_grep"],
        required_params={"daytona_grep": ["pattern"]},
    ),
    EvalCase(
        name="glob_search",
        prompt="Find all Python files in the workspace.",
        expected_tools=["daytona_glob"],
        required_params={"daytona_glob": ["pattern"]},
    ),
    # -- Shell execution --
    EvalCase(
        name="run_command",
        prompt="Run 'python -m pytest tests/ -v' in the sandbox.",
        expected_tools=["daytona_shell"],
        required_params={"daytona_shell": ["command"]},
    ),
    EvalCase(
        name="install_package",
        prompt="Install the requests library using pip.",
        expected_tools=["daytona_shell"],
        required_params={"daytona_shell": ["command"]},
    ),
    # -- Code intelligence operations --
    EvalCase(
        name="check_diagnostics",
        prompt="Check if src/main.py has any syntax or type errors.",
        expected_tools=["ci_diagnostics"],
        required_params={"ci_diagnostics": ["file_path"]},
    ),
    EvalCase(
        name="goto_definition",
        prompt="Find the definition of build_app in src/app.py",
        expected_tools=["ci_query_symbol", "daytona_read_file"],
        required_params={"ci_query_symbol": ["query"]},
    ),
    EvalCase(
        name="find_references",
        prompt="Find all usages of the User symbol across the codebase.",
        expected_tools=["ci_query_symbol"],
        required_params={"ci_query_symbol": ["query"]},
    ),
    EvalCase(
        name="check_errors",
        prompt="Check src/main.py for syntax errors and type errors.",
        expected_tools=["ci_diagnostics"],
        required_params={"ci_diagnostics": ["file_path"]},
    ),
    # -- Behavioral --
    EvalCase(
        name="read_before_edit",
        prompt="I need to understand how the login function works in src/auth.py before I modify it. Read the file first.",
        expected_tools=["daytona_read_file"],
        required_params={"daytona_read_file": ["file_path"]},
    ),
]


# ---------------------------------------------------------------------------
# Scoring
# ---------------------------------------------------------------------------


@dataclass
class EvalScore:
    case: EvalCase
    tool_names_called: list[str]
    tool_selection_correct: bool
    params_correct: bool
    errors: list[str]
    latency_ms: float

    @property
    def passed(self) -> bool:
        return self.tool_selection_correct and self.params_correct


def _score(case: EvalCase, result) -> EvalScore:
    errors: list[str] = []
    called = result.tool_names

    tool_ok = any(t in called for t in case.expected_tools)
    if not tool_ok:
        errors.append(f"Expected {case.expected_tools}, got {called or '(no tool calls)'}")

    params_ok = True
    for tool_name, required in case.required_params.items():
        matching = [tc for tc in result.tool_calls if tc.name == tool_name]
        if not matching and tool_name in case.expected_tools:
            params_ok = False
            errors.append(f"{tool_name} not called — cannot check params")
            continue
        for tc in matching:
            for param in required:
                if param not in tc.input or tc.input[param] in (None, "", {}):
                    params_ok = False
                    errors.append(f"{tool_name}: missing '{param}', got {tc.input}")

    return EvalScore(
        case=case,
        tool_names_called=called,
        tool_selection_correct=tool_ok,
        params_correct=params_ok,
        errors=errors,
        latency_ms=result.latency_ms,
    )


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


@pytest.fixture(scope="module")
def eval_agent():
    if not HAS_CREDENTIALS:
        pytest.skip("No LLM credentials configured")

    sandbox_id = None
    if EvalAgent.has_daytona():
        sb = create_test_sandbox("tool-eval")
        sandbox_id = sb["id"]
        populate_sandbox_files(sandbox_id)

    agent = create_eval_agent(sandbox_id=sandbox_id, tool_call_limit=100)
    yield agent

    if sandbox_id:
        delete_test_sandbox(sandbox_id)


# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------


@pytest.mark.skipif(not HAS_CREDENTIALS, reason="No LLM credentials configured")
@pytest.mark.asyncio
@pytest.mark.parametrize("case", EVAL_CASES, ids=[c.name for c in EVAL_CASES])
async def test_tool_selection(case: EvalCase, eval_agent):
    result = await eval_agent.invoke(case.prompt)
    score = _score(case, result)

    status = "PASS" if score.passed else "FAIL"
    print(
        f"\n  [{status}] {case.name}: expected={case.expected_tools}, "
        f"got={score.tool_names_called}, {score.latency_ms:.0f}ms"
    )
    if result.tool_calls:
        for tc in result.tool_calls:
            print(f"    -> {tc.name}({json.dumps(tc.input, default=str)[:200]})")
    if score.errors:
        for err in score.errors:
            print(f"    x {err}")

    assert score.tool_selection_correct, f"Tool selection: {score.errors}"
    assert score.params_correct, f"Params: {score.errors}"


@pytest.mark.skipif(not HAS_CREDENTIALS, reason="No LLM credentials configured")
@pytest.mark.asyncio
async def test_full_eval_report(eval_agent):
    """Run all cases and print a summary. Fails if <80% pass."""
    scores: list[EvalScore] = []
    for case in EVAL_CASES:
        result = await eval_agent.invoke(case.prompt)
        scores.append(_score(case, result))

    total = len(scores)
    passed = sum(1 for s in scores if s.passed)
    tool_ok = sum(1 for s in scores if s.tool_selection_correct)
    params_ok = sum(1 for s in scores if s.params_correct)
    avg_ms = sum(s.latency_ms for s in scores) / total

    print(f"\n{'=' * 70}")
    print(f"TOOL SELECTION EVAL — {eval_agent.model}")
    print(f"{'=' * 70}")
    print(f"Overall:   {passed}/{total} ({passed / total * 100:.0f}%)")
    print(f"Tool sel:  {tool_ok}/{total} ({tool_ok / total * 100:.0f}%)")
    print(f"Params:    {params_ok}/{total} ({params_ok / total * 100:.0f}%)")
    print(f"Avg latency: {avg_ms:.0f}ms")
    print(f"{'-' * 70}")
    for s in scores:
        mark = "PASS" if s.passed else "FAIL"
        called = ",".join(s.tool_names_called) or "(none)"
        print(f"  [{mark}] {s.case.name:<25} -> {called:<35} {s.latency_ms:.0f}ms")
        for err in s.errors:
            print(f"         x {err}")
    print(f"{'=' * 70}")

    assert passed / total >= 0.80, f"Pass rate {passed / total:.0%} < 80%"
