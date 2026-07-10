"""Bench tier (PERF-*) for the Manager Export Changes live-Docker catalog.

Stage-1 trio benchmark (bench-prompt.md): PERF-0 + 1/5/20 MiB + shape and
compressibility contrasts. Explicit-run only — not part of easy/medium/hard:

    EXPORT_RUN_ID=export-perf-... pytest -m "export and bench"

The only quantity measured is the export operation's client wall clock;
timing is recorded in each case's measurements.json and is never a pass/fail
axis. Run against a RELEASE-profile gateway/daemon or the numbers are junk.
"""

import pytest

from manager.management.export.helpers import cases_for_tier, run_case


pytestmark = [pytest.mark.export, pytest.mark.bench, pytest.mark.slow]


@pytest.mark.parametrize("case", cases_for_tier("bench"), ids=lambda case: case["id"])
def test_export_bench_catalog(case, export_preconditions):
    run_case(case)
