# ruff: noqa
"""Live E2E: Supernova — autonomous debug-fix-retest cycles.

The agent receives buggy code and a failing test suite. No step-by-step guidance.
It must autonomously: run tests in background, wait for results, read output,
diagnose failures, write fixes, and re-run until all tests pass.

These tests validate the full background task workflow under realistic conditions:
long waits, multi-iteration fix cycles, and autonomous decision-making.

Run with: .venv/bin/python -m pytest backend/tests/test_e2e/test_bg_supernova_live.py -v -s --log-cli-level=INFO
"""
from __future__ import annotations

import json
import logging
import textwrap

import pytest

from engine.testing.eval_agent import EvalAgent
from tests.test_e2e.conftest import create_eval_agent, create_test_sandbox, delete_test_sandbox

logger = logging.getLogger(__name__)

pytestmark = [pytest.mark.e2e, pytest.mark.live]

AGENT_PROMPT = """\
You are a senior developer with a remote Daytona sandbox.

You MUST use tools for every action. Never describe what you'd do — execute it.
Use whichever tools are appropriate for the task.

For long-running commands (tests, builds), run them in background with "background": true,
then use wait_for_background_task to wait for results.

You are an autonomous agent. Analyze failures, reason about root causes, apply fixes,
and verify your fixes work. Keep iterating until the problem is solved.
"""


def _verify_tests_pass(
    sandbox_id: str, command: str, marker: str, timeout: int = 300
) -> tuple[bool, str]:
    """Run the test command in the sandbox and check for a success marker in the output.

    Returns (passed, output). This is the ground truth — it re-runs the test
    after the agent is done to verify the fixes actually work.
    """
    from sandbox.testing import get_sandbox_service

    svc = get_sandbox_service()
    sb = svc.get_sandbox_object(sandbox_id)
    resp = sb.process.exec(command, timeout=timeout)
    output = getattr(resp, "result", "") or getattr(resp, "stdout", "") or ""
    exit_code = getattr(resp, "exit_code", None)
    return (marker in output and (exit_code == 0 or exit_code is None)), output


def _log_result(result, label: str) -> None:
    waits = result.tool_count("wait_for_background_task")
    checks = result.tool_count("check_background_progress")
    writes = result.tool_count("daytona_write_file")
    bg_started = len(result.background_started())
    bg_completed = len(result.background_completed())

    logger.info(
        f"\n{'='*60}\n[{label}] Supernova summary:\n"
        f"  Tools used: {len(result.tool_calls)}\n"
        f"  Background started: {bg_started}\n"
        f"  Background completed: {bg_completed}\n"
        f"  Progress checks: {checks}\n"
        f"  Wait calls: {waits}\n"
        f"  File writes: {writes}\n"
        f"  Tool sequence: {result.tool_names}\n"
        f"{'='*60}"
    )


# ===========================================================================
# Buggy project source code — injected into sandbox, NOT shown to agent
# ===========================================================================

# A calculator module with 3 cascading bugs:
# Bug 1: divide() doesn't handle zero → ZeroDivisionError crashes the import test
# Bug 2: multiply() has an off-by-one (a * b + 1)
# Bug 3: subtract() has swapped operands (b - a instead of a - b)
# Each bug is masked by the previous — they surface one at a time as fixes are applied.

CALC_MODULE = textwrap.dedent("""\
    \"\"\"Calculator module.\"\"\"

    def add(a, b):
        return a + b

    def subtract(a, b):
        return b - a

    def multiply(a, b):
        return a * b + 1

    def divide(a, b):
        return a / b
""")

CALC_TESTS = textwrap.dedent("""\
    #!/usr/bin/env python3
    \"\"\"Test suite for calculator module.\"\"\"
    import sys
    import time

    sys.path.insert(0, "/home/daytona/project")

    passed = 0
    failed = 0
    errors = []

    def check(name, got, expected):
        global passed, failed
        time.sleep(2)  # simulate real test execution time
        if got == expected:
            print(f"  PASS: {name} — got {got}")
            passed += 1
        else:
            print(f"  FAIL: {name} — expected {expected}, got {got}")
            failed += 1
            errors.append(f"{name}: expected {expected}, got {got}")

    print("=" * 50)
    print("Running calculator test suite...")
    print("=" * 50)

    try:
        from calc import add, subtract, multiply, divide
    except Exception as e:
        print(f"IMPORT ERROR: {e}")
        sys.exit(2)

    # Test 1: division by zero handling
    print("\\n[Test Group 1: Division]")
    try:
        result = divide(10, 0)
        print(f"  FAIL: divide(10, 0) — expected ValueError, got {result}")
        failed += 1
        errors.append("divide(10, 0): should raise ValueError")
    except ValueError:
        print("  PASS: divide(10, 0) — correctly raised ValueError")
        passed += 1
    except ZeroDivisionError:
        print("  FAIL: divide(10, 0) — raised ZeroDivisionError instead of ValueError")
        failed += 1
        errors.append("divide(10, 0): raised ZeroDivisionError, should raise ValueError")

    check("divide(10, 2)", divide(10, 2), 5.0)
    check("divide(9, 3)", divide(9, 3), 3.0)

    # Test 2: multiplication
    print("\\n[Test Group 2: Multiplication]")
    check("multiply(3, 4)", multiply(3, 4), 12)
    check("multiply(0, 5)", multiply(0, 5), 0)
    check("multiply(-2, 3)", multiply(-2, 3), -6)

    # Test 3: subtraction
    print("\\n[Test Group 3: Subtraction]")
    check("subtract(10, 3)", subtract(10, 3), 7)
    check("subtract(0, 5)", subtract(0, 5), -5)
    check("subtract(-1, -1)", subtract(-1, -1), 0)

    # Test 4: addition (should pass — no bugs)
    print("\\n[Test Group 4: Addition]")
    check("add(2, 3)", add(2, 3), 5)
    check("add(-1, 1)", add(-1, 1), 0)

    # Summary
    total = passed + failed
    print(f"\\n{'=' * 50}")
    print(f"Results: {passed}/{total} passed, {failed} failed")
    if errors:
        print("\\nFailures:")
        for e in errors:
            print(f"  - {e}")
        print(f"\\nFIX THESE {len(errors)} ISSUE(S) AND RE-RUN")
    else:
        print("\\nALL TESTS PASSED")
    print("=" * 50)
    sys.exit(1 if failed > 0 else 0)
""")

# A build pipeline script with 3 stages — each can fail independently
PIPELINE_SCRIPT = textwrap.dedent("""\
    #!/bin/bash
    set -e
    echo "=============================="
    echo "CI Pipeline — $(date)"
    echo "=============================="

    echo ""
    echo "[Stage 1/3] Lint check..."
    sleep 3
    cd /home/daytona/webapp
    if ! python3 -c "
    import ast, sys
    for f in ['app.py', 'routes.py', 'models.py']:
        try:
            with open(f) as fh:
                ast.parse(fh.read())
            print(f'  lint {f}: OK')
        except SyntaxError as e:
            print(f'  lint {f}: SYNTAX ERROR line {e.lineno}: {e.msg}')
            sys.exit(1)
    "; then
        echo "STAGE FAILED: lint"
        exit 1
    fi
    echo "[Stage 1/3] Lint: PASSED"

    echo ""
    echo "[Stage 2/3] Unit tests..."
    sleep 3
    if ! python3 test_webapp.py; then
        echo "STAGE FAILED: unit tests"
        exit 2
    fi
    echo "[Stage 2/3] Tests: PASSED"

    echo ""
    echo "[Stage 3/3] Integration check..."
    sleep 3
    if ! python3 -c "
    from app import create_app
    app = create_app()
    assert app is not None, 'create_app returned None'
    assert hasattr(app, 'routes'), 'app missing routes attribute'
    print('  integration: app created OK')
    print('  integration: routes registered OK')
    "; then
        echo "STAGE FAILED: integration"
        exit 3
    fi
    echo "[Stage 3/3] Integration: PASSED"

    echo ""
    echo "=============================="
    echo "ALL STAGES PASSED"
    echo "=============================="
""")

# Webapp source — has a syntax error in routes.py, a logic bug in models.py,
# and a missing attribute in app.py. Bugs surface sequentially as pipeline stages run.
WEBAPP_APP = textwrap.dedent("""\
    \"\"\"Main application module.\"\"\"

    class App:
        def __init__(self):
            self.name = "webapp"
            # BUG: missing self.routes — integration check will fail
            from routes import register_routes
            register_routes(self)

    def create_app():
        return App()
""")

WEBAPP_ROUTES = textwrap.dedent("""\
    \"\"\"Route definitions.\"\"\"

    def register_routes(app)
        app.routes = ["/", "/api", "/health"]
        print(f"Registered {len(app.routes)} routes")
""")
# BUG: missing colon after register_routes(app) — syntax error

WEBAPP_MODELS = textwrap.dedent("""\
    \"\"\"Data models.\"\"\"

    class User:
        def __init__(self, name, age):
            self.name = name
            self.age = age

        def is_adult(self):
            return self.age > 21  # BUG: should be >= 18

        def greet(self):
            return f"Hello, {self.name}!"
""")

WEBAPP_TESTS = textwrap.dedent("""\
    #!/usr/bin/env python3
    \"\"\"Unit tests for webapp.\"\"\"
    import sys
    sys.path.insert(0, "/home/daytona/webapp")

    passed = 0
    failed = 0

    def check(name, got, expected):
        global passed, failed
        if got == expected:
            print(f"  PASS: {name}")
            passed += 1
        else:
            print(f"  FAIL: {name} — expected {expected!r}, got {got!r}")
            failed += 1

    from providers import User

    u1 = User("Alice", 18)
    check("User(18).is_adult()", u1.is_adult(), True)

    u2 = User("Bob", 17)
    check("User(17).is_adult()", u2.is_adult(), False)

    u3 = User("Charlie", 25)
    check("User(25).greet()", u3.greet(), "Hello, Charlie!")

    print(f"\\nResults: {passed}/{passed+failed} passed")
    sys.exit(0 if failed == 0 else 1)
""")

# A test suite driven by config.json — 3 config values are wrong,
# causing tests to fail one at a time as each is fixed.
FLAKY_TEST_SCRIPT = textwrap.dedent("""\
    #!/usr/bin/env python3
    \"\"\"A test suite that reads config to decide pass/fail behavior.\"\"\"
    import json
    import sys
    import time

    CONFIG_PATH = "/home/daytona/flaky/config.json"

    try:
        with open(CONFIG_PATH) as f:
            config = json.load(f)
    except FileNotFoundError:
        print("ERROR: config.json not found at", CONFIG_PATH)
        sys.exit(2)

    retries = config.get("max_retries", 1)
    timeout = config.get("timeout_sec", 5)
    db_host = config.get("db_host", "localhost")
    db_port = config.get("db_port", 5432)

    print("=" * 50)
    print("Integration Test Suite")
    print(f"Config: retries={retries}, timeout={timeout}s, db={db_host}:{db_port}")
    print("=" * 50)

    # Test 1: always passes
    print("\\n[1/4] test_health_check...", end=" ")
    time.sleep(2)
    print("PASS")

    # Test 2: fails if timeout < 10
    print("[2/4] test_db_connection...", end=" ")
    time.sleep(3)
    if timeout < 10:
        print(f"FAIL — connection timed out after {timeout}s (need >= 10s)")
        sys.exit(1)
    print("PASS")

    # Test 3: fails if db_port != 5433
    print("[3/4] test_db_query...", end=" ")
    time.sleep(3)
    if db_port != 5433:
        print(f"FAIL — connection refused on port {db_port} (service runs on 5433)")
        sys.exit(1)
    print("PASS")

    # Test 4: fails if max_retries < 3
    print("[4/4] test_retry_logic...", end=" ")
    time.sleep(3)
    if retries < 3:
        print(f"FAIL — retry budget exhausted ({retries} retries insufficient, need >= 3)")
        sys.exit(1)
    print("PASS")

    print(f"\\n{'=' * 50}")
    print("ALL 4 TESTS PASSED")
    print("=" * 50)
    sys.exit(0)
""")

FLAKY_INITIAL_CONFIG = json.dumps({
    "max_retries": 1,
    "timeout_sec": 5,
    "db_host": "localhost",
    "db_port": 5432,
}, indent=2)


# ===========================================================================
# Test 1: Cascading calculator bugs — 3 iterations to fix all
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestSupernovaCascadingBugs:
    """Agent fixes 3 cascading bugs in a calculator, one per test run iteration."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("nova-cascade")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.fixture(scope="class", autouse=True)
    def seed_files(self, sandbox):
        """Pre-populate sandbox with buggy project files."""
        from sandbox.testing import get_sandbox_service

        svc = get_sandbox_service()
        sb = svc.get_sandbox_object(sandbox["id"])
        sb.process.exec("mkdir -p /home/daytona/project")
        sb.fs.upload_file(CALC_MODULE.encode(), "/home/daytona/project/calc.py")
        sb.fs.upload_file(CALC_TESTS.encode(), "/home/daytona/project/test_calc.py")
        sb.process.exec("chmod +x /home/daytona/project/test_calc.py")

    @pytest.mark.asyncio
    async def test_autonomous_cascading_fix_cycle(self, sandbox):
        """Agent gets buggy code + failing tests. No guidance. Fix all bugs."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "There is a Python project at /home/daytona/project/ with:\n"
            "- calc.py — a calculator module with add, subtract, multiply, divide\n"
            "- test_calc.py — a test suite that validates all operations\n\n"
            "The tests are currently failing. Your job:\n"
            "1. Run the test suite in background and wait for results\n"
            "2. Read the output, diagnose what's wrong\n"
            "3. Fix the bug in calc.py\n"
            "4. Re-run the tests and wait for results\n"
            "5. Repeat until ALL tests pass\n\n"
            "The test suite takes ~20 seconds to run. Use background execution "
            "and wait_for_background_task."
        )
        _log_result(result, "cascading_bugs")

        # Ground truth: re-run the tests and verify they actually pass
        passed, output = _verify_tests_pass(
            sandbox["id"],
            "cd /home/daytona/project && python3 test_calc.py",
            "ALL TESTS PASSED",
        )
        assert passed, (
            f"Tests still failing after agent iteration. Output:\n{output[-1500:]}"
        )


# ===========================================================================
# Test 2: Multi-stage CI pipeline — fix bugs across 3 pipeline stages
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestSupernovaPipelineDebug:
    """Agent debugs a 3-stage CI pipeline that fails at different stages."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("nova-pipeline")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.fixture(scope="class", autouse=True)
    def seed_files(self, sandbox):
        """Pre-populate sandbox with buggy webapp + pipeline."""
        from sandbox.testing import get_sandbox_service

        svc = get_sandbox_service()
        sb = svc.get_sandbox_object(sandbox["id"])
        sb.process.exec("mkdir -p /home/daytona/webapp")
        sb.fs.upload_file(WEBAPP_APP.encode(), "/home/daytona/webapp/app.py")
        sb.fs.upload_file(WEBAPP_ROUTES.encode(), "/home/daytona/webapp/routes.py")
        sb.fs.upload_file(WEBAPP_MODELS.encode(), "/home/daytona/webapp/models.py")
        sb.fs.upload_file(WEBAPP_TESTS.encode(), "/home/daytona/webapp/test_webapp.py")
        sb.fs.upload_file(PIPELINE_SCRIPT.encode(), "/home/daytona/webapp/pipeline.sh")
        sb.process.exec("chmod +x /home/daytona/webapp/pipeline.sh /home/daytona/webapp/test_webapp.py")

    @pytest.mark.asyncio
    async def test_autonomous_pipeline_debug(self, sandbox):
        """Agent gets a failing CI pipeline. Must fix bugs stage by stage."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "There is a webapp project at /home/daytona/webapp/ with:\n"
            "- app.py, routes.py, models.py — source code\n"
            "- test_webapp.py — unit tests\n"
            "- pipeline.sh — a CI pipeline that runs lint, tests, then integration check\n\n"
            "The pipeline is failing. Run it and fix all issues until it passes.\n"
            "The pipeline takes ~10 seconds per run. Use background execution."
        )
        _log_result(result, "pipeline_debug")

        # Ground truth: re-run the pipeline and verify it actually passes
        passed, output = _verify_tests_pass(
            sandbox["id"],
            "cd /home/daytona/webapp && bash pipeline.sh",
            "ALL STAGES PASSED",
        )
        assert passed, (
            f"Pipeline still failing after agent iteration. Output:\n{output[-1500:]}"
        )


# ===========================================================================
# Test 3: Config tuning — agent reads test output, adjusts config, retests
# ===========================================================================


@pytest.mark.skipif(not EvalAgent.has_all(), reason="API + Daytona both required")
class TestSupernovaConfigTuning:
    """Agent iteratively tunes config by reading test output until all tests pass."""

    @pytest.fixture(scope="class")
    def sandbox(self):
        sb = create_test_sandbox("nova-config")
        yield sb
        delete_test_sandbox(sb["id"])

    @pytest.fixture(scope="class", autouse=True)
    def seed_files(self, sandbox):
        """Pre-populate sandbox with flaky test suite and bad config."""
        from sandbox.testing import get_sandbox_service

        svc = get_sandbox_service()
        sb = svc.get_sandbox_object(sandbox["id"])
        sb.process.exec("mkdir -p /home/daytona/flaky")
        sb.fs.upload_file(
            FLAKY_TEST_SCRIPT.encode(), "/home/daytona/flaky/run_tests.py"
        )
        sb.fs.upload_file(
            FLAKY_INITIAL_CONFIG.encode(), "/home/daytona/flaky/config.json"
        )
        sb.process.exec("chmod +x /home/daytona/flaky/run_tests.py")

    @pytest.mark.asyncio
    async def test_autonomous_config_tuning_cycle(self, sandbox):
        """Agent reads test failures, adjusts config, re-runs. 3 config bugs to fix."""
        agent = create_eval_agent(
            system_prompt=AGENT_PROMPT,
            sandbox_id=sandbox["id"],
            enable_background_tasks=True,
        )
        result = await agent.invoke(
            "There is a test suite at /home/daytona/flaky/ with:\n"
            "- run_tests.py — integration tests that read config.json\n"
            "- config.json — configuration that controls test behavior\n\n"
            "The tests are failing. Run the test suite, read the output to understand "
            "why each test fails, update config.json to fix the issue, and re-run.\n"
            "Keep iterating until all 4 tests pass.\n\n"
            "The test suite takes ~12 seconds per run. Use background execution "
            "and wait_for_background_task."
        )
        _log_result(result, "config_tuning")

        # Ground truth: re-run the tests and verify they actually pass
        passed, output = _verify_tests_pass(
            sandbox["id"],
            "cd /home/daytona/flaky && python3 run_tests.py",
            "ALL 4 TESTS PASSED",
        )
        assert passed, (
            f"Tests still failing after agent iteration. Output:\n{output[-1500:]}"
        )
