"""R1 kwarg-drift guard for ``EphemeralAttemptAgentLauncher._run_launch``.

The ``benchmark_sweevo`` agent runner delegates non-entry agent
launches to ``engine.api.run_ephemeral_agent`` with the exact frozen
kwarg set the production launcher passes. If the launcher's kwarg set
ever drifts, this test fails loudly so the delegate can be updated in
lockstep.

Risk register R1 in
``.omc/plans/sweevo-csv-real-agent-benchmarker-20260516.md``.
"""

from __future__ import annotations

import inspect
import re

from task_center.attempt.launch import EphemeralAttemptAgentLauncher

_FROZEN_KWARG_SET: frozenset[str] = frozenset(
    {
        "agent_def",
        "sandbox_id",
        "persist_agent_run",
        "task_id",
        "on_event",
        "extra_tool_metadata",
        "initial_messages",
    }
)


def test_run_launch_calls_runner_with_frozen_kwarg_set() -> None:
    """Extract the runner(...) call inside ``_run_launch`` and assert kwargs match.

    The frozen set is exactly what the benchmark_sweevo runner delegate forwards
    in ``task_center_runner.benchmarks.sweevo.run.``
    ``_delegate_to_real_runner``. ``config`` and ``prompt`` are
    positional, so they are intentionally NOT in the kwarg set.
    """
    source = inspect.getsource(EphemeralAttemptAgentLauncher._run_launch)

    # Find the ``await runner(...)`` call. Use a non-greedy regex that
    # stops at the closing paren of the call (no nested parens in the
    # current source — see launch.py:136-145).
    match = re.search(r"await runner\((?P<body>.*?)\)\n", source, re.DOTALL)
    assert match is not None, (
        "Could not find ``await runner(...)`` in EphemeralAttemptAgentLauncher._run_launch — "
        "the launcher may have been refactored. Update the SWE-EVO runner delegate to match."
    )

    body = match.group("body")
    kwargs = set(re.findall(r"\b(\w+)\s*=", body))

    assert kwargs == _FROZEN_KWARG_SET, (
        f"EphemeralAttemptAgentLauncher._run_launch's runner() kwargs drifted.\n"
        f"  expected: {sorted(_FROZEN_KWARG_SET)}\n"
        f"  actual:   {sorted(kwargs)}\n"
        f"Update ``_delegate_to_real_runner`` in "
        f"``task_center_runner/benchmarks/sweevo/agent_runner.py`` to forward the new "
        f"kwargs in lockstep, then update _FROZEN_KWARG_SET here."
    )
