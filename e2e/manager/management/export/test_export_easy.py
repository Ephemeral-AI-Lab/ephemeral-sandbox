"""Easy tier (EZ-01..10) for the Manager Export Changes live-Docker catalog.

The whole tier is the rebuild gate (``pytest -m "export and easy"``); EZ-01 —
the primary B1 workflow — runs first.
"""

import pytest

from manager.management.export.helpers import cases_for_tier, run_case


pytestmark = [pytest.mark.export, pytest.mark.easy]


@pytest.mark.parametrize("case", cases_for_tier("easy"), ids=lambda case: case["id"])
def test_export_easy_catalog(case, export_preconditions):
    run_case(case)
