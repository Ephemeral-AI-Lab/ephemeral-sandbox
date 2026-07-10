"""Smoke tier for the LayerStack squash live-Docker catalog."""

import pytest

from manager.management.squash.helpers import cases_for_tier, run_case


pytestmark = [pytest.mark.squash, pytest.mark.smoke]


@pytest.mark.parametrize("case", cases_for_tier("smoke"), ids=lambda case: case["id"])
def test_squash_smoke_catalog(case, squash_preconditions, squash_sandbox_factory):
    run_case(case, squash_sandbox_factory)
