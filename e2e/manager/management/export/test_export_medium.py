"""Medium tier (MED-01..10): one interaction dimension per case. MED-04
(opaque-clear ordering) is the must-not-regress case.
"""

import pytest

from manager.management.export.helpers import cases_for_tier, run_case


pytestmark = [pytest.mark.export, pytest.mark.medium]


@pytest.mark.parametrize("case", cases_for_tier("medium"), ids=lambda case: case["id"])
def test_export_medium_catalog(case, export_preconditions):
    run_case(case)
