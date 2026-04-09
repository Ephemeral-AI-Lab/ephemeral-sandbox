"""SWE-EVO evaluation harness — test running, patch extraction, scoring."""

from __future__ import annotations

import json
import logging
import re

from benchmarks.sweevo.models import (
    SWEEvoInstance,
    SWEEvoResult,
    _CONDA_ACTIVATE,
    _DEFAULT_SWEEVO_TEST_TIMEOUT,
    _REPO_DIR,
)
from benchmarks.sweevo.sandbox import _exec, ensure_sweevo_test_patch

logger = logging.getLogger(__name__)


async def _extract_combined_patch(sandbox_id: str, repo_dir: str) -> str:
    """Extract combined diff of all agent changes against base commit."""
    patch = await _exec(
        sandbox_id,
        f"cd {repo_dir} && git add -A && git diff HEAD 2>/dev/null || "
        f"git diff 2>/dev/null || echo ''",
    )
    return patch.strip()


async def evaluate_sweevo_result(
    instance: SWEEvoInstance,
    result: SWEEvoResult,
    sandbox_id: str,
    repo_dir: str = _REPO_DIR,
) -> SWEEvoResult:
    """Run FAIL_TO_PASS and PASS_TO_PASS tests to score the result."""
    # Step 1: Apply test patch
    await ensure_sweevo_test_patch(instance, sandbox_id, repo_dir)

    # Step 2: Run FAIL_TO_PASS tests
    f2p_passed = 0
    f2p_total = len(instance.fail_to_pass)
    if f2p_total > 0:
        f2p_passed = await _run_test_set(
            sandbox_id, repo_dir, instance.fail_to_pass, instance.test_cmds
        )

    # Step 3: Run PASS_TO_PASS tests
    p2p_total = len(instance.pass_to_pass)
    p2p_passed = 0
    if p2p_total > 0:
        p2p_passed = await _run_test_set(
            sandbox_id, repo_dir, instance.pass_to_pass, instance.test_cmds
        )

    p2p_broken = p2p_total - p2p_passed

    # Step 4: Compute metrics
    result.fail_to_pass_passed = f2p_passed
    result.fail_to_pass_total = f2p_total
    result.pass_to_pass_broken = p2p_broken
    result.pass_to_pass_total = p2p_total
    result.fix_rate = f2p_passed / max(f2p_total, 1)
    result.resolved = (f2p_passed == f2p_total) and (p2p_broken == 0)

    logger.info(
        "SWE-EVO %s: resolved=%s fix_rate=%.2f F2P=%d/%d P2P_broken=%d/%d",
        instance.instance_id,
        result.resolved,
        result.fix_rate,
        f2p_passed,
        f2p_total,
        p2p_broken,
        p2p_total,
    )

    return result

async def _run_test_set(
    sandbox_id: str,
    repo_dir: str,
    test_ids: list[str],
    test_cmds: str,
    *,
    timeout: int = _DEFAULT_SWEEVO_TEST_TIMEOUT,
) -> int:
    """Run a set of tests and return the number that passed."""
    if not test_ids:
        return 0

    cmd = _build_test_set_command(repo_dir, test_ids, test_cmds)

    try:
        output = await _exec(sandbox_id, cmd, timeout=timeout, check=False)
    except Exception as exc:
        logger.warning("Test execution failed: %s", exc)
        return 0

    if "EXIT_CODE=0" in output:
        return len(test_ids)

    passed = _parse_pytest_passed_count(output, len(test_ids))
    return passed


def _build_test_set_command(repo_dir: str, test_ids: list[str], test_cmds: str) -> str:
    """Build a shell command that replays pytest IDs without shell-quoting them."""
    script = (
        "import shlex, subprocess\n"
        f"test_cmd = {json.dumps(test_cmds)}\n"
        f"test_ids = {json.dumps(test_ids)}\n"
        "argv = shlex.split(test_cmd) + test_ids\n"
        "proc = subprocess.run(argv, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)\n"
        "print(proc.stdout, end='')\n"
        "print(f'EXIT_CODE={proc.returncode}')\n"
    )
    return f"{_CONDA_ACTIVATE} && cd {repo_dir} && python - <<'PY'\n{script}PY"


def _parse_pytest_passed_count(output: str, total: int) -> int:
    """Parse pytest summary line to extract passed count."""
    m = re.search(r"(\d+) passed", output)
    if m:
        return int(m.group(1))
    return 0
