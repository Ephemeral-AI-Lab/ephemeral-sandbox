"""Medium tier for the LayerStack squash live-Docker catalog."""

import pytest

from manager.management.squash.helpers import cases_for_tier, run_case


pytestmark = [pytest.mark.squash, pytest.mark.medium]


@pytest.mark.parametrize("case", cases_for_tier("medium"), ids=lambda case: case["id"])
def test_squash_medium_catalog(case, squash_preconditions, squash_sandbox_factory):
    run_case(case, squash_sandbox_factory)
