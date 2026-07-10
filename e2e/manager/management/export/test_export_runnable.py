"""Runnable-project round-trip (RUN-01..05, runnable-export-test-case.md):
build real Node/Python projects in-sandbox (npm install, tsc, native addon,
venv, numpy wheel), export the built tree, and RUN it — remount-and-run in a
fresh same-image container as the primary proof, best-effort host run as the
secondary. Serial; installs pull over the network (slow).

Order follows §5: RUN-01 smokes the run harness itself, then build/venv
(RUN-02, RUN-04), then the native/wheel boundary cases (RUN-03, RUN-05).
"""

import pytest

from manager.management.export.helpers import cases_for_tier, run_case


pytestmark = [pytest.mark.export, pytest.mark.runnable, pytest.mark.slow]


@pytest.mark.parametrize("case", cases_for_tier("runnable"), ids=lambda case: case["id"])
def test_export_runnable_catalog(case):
    run_case(case)
