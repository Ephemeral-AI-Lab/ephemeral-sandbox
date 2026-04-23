# ruff: noqa
"""Daytona sandbox load test — raw SDK stress test without LLM.

Directly exercises sandbox.process.exec() under various concurrency
patterns to identify when exit_code: -1 occurs.

Run with:
  .venv/bin/python -m pytest backend/tests/test_e2e/test_daytona_loadtest.py -v -s --log-cli-level=INFO
"""

from __future__ import annotations

import asyncio
import json
import logging
import time
from dataclasses import dataclass, field

import pytest

logger = logging.getLogger(__name__)

pytestmark = [pytest.mark.e2e, pytest.mark.live]


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

@dataclass
class ExecResult:
    command: str
    stdout: str
    exit_code: int
    elapsed_ms: float
    error: str | None = None


@dataclass
class LoadTestReport:
    label: str
    results: list[ExecResult] = field(default_factory=list)

    @property
    def total(self) -> int:
        return len(self.results)

    @property
    def successes(self) -> int:
        return sum(1 for r in self.results if r.exit_code == 0)

    @property
    def failures(self) -> int:
        return sum(1 for r in self.results if r.exit_code != 0)

    @property
    def neg1_count(self) -> int:
        return sum(1 for r in self.results if r.exit_code == -1)

    @property
    def avg_latency_ms(self) -> float:
        if not self.results:
            return 0.0
        return sum(r.elapsed_ms for r in self.results) / len(self.results)

    def summary(self) -> str:
        lines = [
            f"\n{'='*60}",
            f"[{self.label}]",
            f"  Total: {self.total}  |  OK: {self.successes}  |  FAIL: {self.failures}  |  exit_code=-1: {self.neg1_count}",
            f"  Avg latency: {self.avg_latency_ms:.0f}ms",
        ]
        for r in self.results:
            status = "OK" if r.exit_code == 0 else f"FAIL(exit={r.exit_code})"
            lines.append(
                f"    [{status}] {r.elapsed_ms:6.0f}ms  cmd={r.command[:60]}  "
                f"stdout={r.stdout[:40]!r}  err={r.error or ''}"
            )
        lines.append(f"{'='*60}")
        return "\n".join(lines)


async def _exec(sandbox, command: str, timeout: int = 30) -> ExecResult:
    """Execute a command and capture result with timing.

    Wraps in bash -c like the real daytona_shell tool does (tools.py:104),
    so shell operators like && work correctly.
    """
    import shlex

    t0 = time.monotonic()
    try:
        response = await sandbox.process.exec(
            f"bash -c {shlex.quote(command)}", timeout=timeout
        )
        elapsed = (time.monotonic() - t0) * 1000
        exit_code = getattr(response, "exit_code", 0)
        stdout = (response.result or "").strip()
        return ExecResult(
            command=command,
            stdout=stdout,
            exit_code=exit_code,
            elapsed_ms=elapsed,
        )
    except Exception as exc:
        elapsed = (time.monotonic() - t0) * 1000
        return ExecResult(
            command=command,
            stdout="",
            exit_code=-99,
            elapsed_ms=elapsed,
            error=str(exc)[:200],
        )


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------

@pytest.fixture(scope="module")
def sandbox_info():
    """Create a real sandbox and return its metadata dict."""
    from sandbox.testing import create_test_sandbox, delete_test_sandbox

    sb = create_test_sandbox("loadtest")
    yield sb
    delete_test_sandbox(sb["id"])


@pytest.fixture()
async def async_sandbox(sandbox_info):
    """Get an async sandbox handle via the async SDK client."""
    from sandbox.async_client import get_async_sandbox

    sandbox = await get_async_sandbox(sandbox_info["id"])
    return sandbox


# ---------------------------------------------------------------------------
# Test 1: Sequential baseline — single commands one at a time
# ---------------------------------------------------------------------------

class TestSequentialBaseline:
    """Establish baseline: do simple commands work sequentially?"""

    @pytest.mark.asyncio
    async def test_sequential_echo(self, async_sandbox):
        """10 sequential echo commands — should all succeed."""
        report = LoadTestReport("sequential_echo")
        for i in range(10):
            r = await _exec(async_sandbox, f"echo SEQ_{i}")
            report.results.append(r)

        logger.info(report.summary())
        assert report.failures == 0, f"Sequential echo should never fail!\n{report.summary()}"

    @pytest.mark.asyncio
    async def test_sequential_mixed(self, async_sandbox):
        """Sequential mix of echo, pwd, date, sleep 1."""
        report = LoadTestReport("sequential_mixed")
        commands = [
            "echo HELLO", "pwd", "date", "echo WORLD",
            "sleep 1 && echo SLEPT", "ls /", "whoami",
            "echo DONE",
        ]
        for cmd in commands:
            r = await _exec(async_sandbox, cmd)
            report.results.append(r)

        logger.info(report.summary())
        assert report.failures == 0, f"Sequential mixed failed!\n{report.summary()}"


# ---------------------------------------------------------------------------
# Test 2: Parallel burst — fire N commands simultaneously
# ---------------------------------------------------------------------------

class TestParallelBurst:
    """Fire multiple commands at once — the pattern that causes exit_code -1."""

    @pytest.mark.asyncio
    async def test_parallel_2_echo(self, async_sandbox):
        """2 parallel echo commands."""
        report = LoadTestReport("parallel_2")
        tasks = [_exec(async_sandbox, f"echo PAR_{i}") for i in range(2)]
        results = await asyncio.gather(*tasks)
        report.results = list(results)

        logger.info(report.summary())
        assert report.neg1_count == 0, f"exit_code=-1 with just 2 parallel!\n{report.summary()}"

    @pytest.mark.asyncio
    async def test_parallel_5_echo(self, async_sandbox):
        """5 parallel echo commands."""
        report = LoadTestReport("parallel_5")
        tasks = [_exec(async_sandbox, f"echo PAR5_{i}") for i in range(5)]
        results = await asyncio.gather(*tasks)
        report.results = list(results)

        logger.info(report.summary())
        assert report.neg1_count == 0, f"exit_code=-1 with 5 parallel!\n{report.summary()}"

    @pytest.mark.asyncio
    async def test_parallel_10_echo(self, async_sandbox):
        """10 parallel echo commands — stress test."""
        report = LoadTestReport("parallel_10")
        tasks = [_exec(async_sandbox, f"echo PAR10_{i}") for i in range(10)]
        results = await asyncio.gather(*tasks)
        report.results = list(results)

        logger.info(report.summary())
        # Log but don't assert — we want to see the failure rate
        if report.neg1_count > 0:
            logger.warning(f"!! {report.neg1_count}/10 commands got exit_code=-1 under 10x parallel")

    @pytest.mark.asyncio
    async def test_parallel_20_echo(self, async_sandbox):
        """20 parallel echo commands — heavy stress."""
        report = LoadTestReport("parallel_20")
        tasks = [_exec(async_sandbox, f"echo PAR20_{i}") for i in range(20)]
        results = await asyncio.gather(*tasks)
        report.results = list(results)

        logger.info(report.summary())
        if report.neg1_count > 0:
            logger.warning(f"!! {report.neg1_count}/20 commands got exit_code=-1 under 20x parallel")


# ---------------------------------------------------------------------------
# Test 3: Background + foreground interleave (the real failure pattern)
# ---------------------------------------------------------------------------

class TestBackgroundForegroundInterleave:
    """Reproduce the exact pattern from e2e tests: bg sleep + fg echo."""

    @pytest.mark.asyncio
    async def test_bg_sleep_then_fg_echo(self, async_sandbox):
        """Start a background sleep, then immediately run fg echo."""
        report = LoadTestReport("bg_sleep_fg_echo")

        # Fire bg sleep and fg echo simultaneously (like the e2e tests do)
        bg_task = asyncio.create_task(_exec(async_sandbox, "sleep 5 && echo BG_DONE", timeout=30))
        # Small delay to simulate "launch bg first, then fg"
        await asyncio.sleep(0.05)
        fg_result = await _exec(async_sandbox, "echo FG_PREP")
        report.results.append(fg_result)

        # Wait for bg to finish
        bg_result = await bg_task
        report.results.append(bg_result)

        logger.info(report.summary())
        # The fg echo should NOT fail
        assert fg_result.exit_code == 0, f"FG echo failed while BG was running!\n{report.summary()}"

    @pytest.mark.asyncio
    async def test_bg_sleep_then_3_fg_echo(self, async_sandbox):
        """Start bg sleep, then run 3 fg commands while it's running."""
        report = LoadTestReport("bg_sleep_3fg")

        bg_task = asyncio.create_task(_exec(async_sandbox, "sleep 10 && echo BG_DONE", timeout=30))
        await asyncio.sleep(0.05)

        for i in range(3):
            r = await _exec(async_sandbox, f"echo FG_{i}")
            report.results.append(r)

        bg_result = await bg_task
        report.results.append(bg_result)

        logger.info(report.summary())
        fg_failures = [r for r in report.results if r.command.startswith("echo FG") and r.exit_code != 0]
        assert len(fg_failures) == 0, f"FG commands failed during BG!\n{report.summary()}"

    @pytest.mark.asyncio
    async def test_2bg_sleep_then_fg_burst(self, async_sandbox):
        """2 background sleeps + 5 parallel fg commands — max stress."""
        report = LoadTestReport("2bg_5fg_burst")

        bg1 = asyncio.create_task(_exec(async_sandbox, "sleep 5 && echo BG1", timeout=30))
        bg2 = asyncio.create_task(_exec(async_sandbox, "sleep 10 && echo BG2", timeout=30))
        await asyncio.sleep(0.1)

        # Fire 5 fg commands in parallel while bgs are running
        fg_tasks = [_exec(async_sandbox, f"echo BURST_{i}") for i in range(5)]
        fg_results = await asyncio.gather(*fg_tasks)
        report.results.extend(fg_results)

        bg1_result = await bg1
        bg2_result = await bg2
        report.results.append(bg1_result)
        report.results.append(bg2_result)

        logger.info(report.summary())
        fg_neg1 = [r for r in fg_results if r.exit_code == -1]
        if fg_neg1:
            logger.warning(f"!! {len(fg_neg1)}/5 FG commands got -1 during 2 BG tasks")


# ---------------------------------------------------------------------------
# Test 4: Rapid sequential after background — timing sensitivity
# ---------------------------------------------------------------------------

class TestTimingSensitivity:
    """Test if delays between commands affect failure rate."""

    @pytest.mark.asyncio
    async def test_rapid_fire_no_delay(self, async_sandbox):
        """5 commands with zero delay between them."""
        report = LoadTestReport("rapid_no_delay")
        for i in range(5):
            r = await _exec(async_sandbox, f"echo RAPID_{i}")
            report.results.append(r)

        logger.info(report.summary())
        assert report.failures == 0, f"Rapid sequential should work!\n{report.summary()}"

    @pytest.mark.asyncio
    async def test_with_50ms_delay(self, async_sandbox):
        """5 commands with 50ms delay between them."""
        report = LoadTestReport("delay_50ms")
        for i in range(5):
            await asyncio.sleep(0.05)
            r = await _exec(async_sandbox, f"echo DELAY50_{i}")
            report.results.append(r)

        logger.info(report.summary())
        assert report.failures == 0, f"50ms-spaced commands should work!\n{report.summary()}"

    @pytest.mark.asyncio
    async def test_with_200ms_delay(self, async_sandbox):
        """5 commands with 200ms delay between them."""
        report = LoadTestReport("delay_200ms")
        for i in range(5):
            await asyncio.sleep(0.2)
            r = await _exec(async_sandbox, f"echo DELAY200_{i}")
            report.results.append(r)

        logger.info(report.summary())
        assert report.failures == 0, f"200ms-spaced commands should work!\n{report.summary()}"


# ---------------------------------------------------------------------------
# Test 5: Cancel simulation — asyncio.cancel() on running task
# ---------------------------------------------------------------------------

class TestCancelBehavior:
    """Verify that cancelled tasks produce exit_code -1."""

    @pytest.mark.asyncio
    async def test_cancel_sleeping_task(self, async_sandbox):
        """Cancel a sleep command and verify exit_code behavior."""
        report = LoadTestReport("cancel_sleep")

        task = asyncio.create_task(_exec(async_sandbox, "sleep 30 && echo NEVER"))
        await asyncio.sleep(1)
        task.cancel()

        try:
            result = await task
            report.results.append(result)
        except asyncio.CancelledError:
            report.results.append(ExecResult(
                command="sleep 30 (cancelled)",
                stdout="",
                exit_code=-1,
                elapsed_ms=1000,
                error="asyncio.CancelledError",
            ))

        logger.info(report.summary())
        # This just logs — we expect -1 or CancelledError


# ---------------------------------------------------------------------------
# Test 6: Sustained load — repeated bursts with cooldown
# ---------------------------------------------------------------------------

class TestSustainedLoad:
    """Multiple rounds of parallel commands to check if failures accumulate."""

    @pytest.mark.asyncio
    async def test_5_rounds_of_3_parallel(self, async_sandbox):
        """5 rounds × 3 parallel commands with 1s cooldown."""
        all_reports: list[LoadTestReport] = []

        for round_num in range(5):
            report = LoadTestReport(f"round_{round_num}")
            tasks = [_exec(async_sandbox, f"echo R{round_num}_C{i}") for i in range(3)]
            results = await asyncio.gather(*tasks)
            report.results = list(results)
            all_reports.append(report)
            await asyncio.sleep(1)

        total = sum(r.total for r in all_reports)
        total_neg1 = sum(r.neg1_count for r in all_reports)
        for r in all_reports:
            logger.info(r.summary())

        logger.info(
            f"\n{'='*60}\n"
            f"SUSTAINED LOAD SUMMARY: {total} commands, {total_neg1} exit_code=-1\n"
            f"{'='*60}"
        )

    @pytest.mark.asyncio
    async def test_10_rounds_of_5_parallel(self, async_sandbox):
        """10 rounds × 5 parallel commands with 500ms cooldown — heavy sustained."""
        total_ok = 0
        total_neg1 = 0
        total_other_fail = 0

        for round_num in range(10):
            tasks = [_exec(async_sandbox, f"echo S{round_num}_{i}") for i in range(5)]
            results = await asyncio.gather(*tasks)
            for r in results:
                if r.exit_code == 0:
                    total_ok += 1
                elif r.exit_code == -1:
                    total_neg1 += 1
                else:
                    total_other_fail += 1
            await asyncio.sleep(0.5)

        logger.info(
            f"\n{'='*60}\n"
            f"HEAVY SUSTAINED: 50 commands total\n"
            f"  OK: {total_ok}  |  exit=-1: {total_neg1}  |  other_fail: {total_other_fail}\n"
            f"  Failure rate: {(total_neg1 + total_other_fail) / 50 * 100:.1f}%\n"
            f"{'='*60}"
        )
