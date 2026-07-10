"""Benchmark tier for the LayerStack squash remount-sweep.

Deterministic (N, M, B) topology cases used by the A/B driver and the
sweep-width tuning sweep. Run explicitly (not part of smoke/medium/hard); the
sweep width the daemon uses is set out of band via the daemon config key
``runtime.layerstack.remount_sweep_width`` (see ``config/bench.yml`` +
``ab_driver.py``).
"""

import pytest

from manager.management.squash.helpers import cases_for_tier, run_case


pytestmark = [pytest.mark.squash, pytest.mark.bench]


@pytest.mark.parametrize("case", cases_for_tier("bench"), ids=lambda case: case["id"])
def test_squash_bench_catalog(case, squash_preconditions, squash_sandbox_factory):
    run_case(case, squash_sandbox_factory)
