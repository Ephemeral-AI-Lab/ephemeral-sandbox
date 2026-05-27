"""Provider-aware health gate for SWE-EVO image-backed tests."""

from __future__ import annotations

import importlib.util
import os
import sys
from pathlib import Path
from typing import Any

import pytest

from task_center_runner.benchmarks.sweevo.models import SWEEvoInstance
from config import get_central_config


def require_sweevo_image_provider_healthy(instance: SWEEvoInstance) -> None:
    """Skip cleanly when the selected live sandbox provider is unavailable.

    Daytona uses the existing Daytona tier-0 probe. Docker uses the same
    SWE-EVO image that the scenario fixture will create the sandbox from, so
    Docker runs do not depend on the Daytona-local registry alias.
    """
    module = _load_tier0_health_module()
    provider = (
        os.environ.get("EOS_SANDBOX_PROVIDER")
        or get_central_config().sandbox.default_provider
    ).strip().lower()
    if provider == "docker":
        result = module.probe_tier0_docker(image=instance.docker_image)
    else:
        result = module.probe_tier0()
    if not result.passed:
        pytest.skip(
            f"Tier-0 health gate failed: api_health={result.api_health!r} "
            f"notes={result.notes!r}"
        )


def _load_tier0_health_module() -> Any:
    repo_root = Path(__file__).resolve().parents[5]
    tier0_path = (
        repo_root
        / "backend"
        / "tests"
        / "live_e2e_test"
        / "_tools"
        / "tier0_health.py"
    )
    if not tier0_path.exists():
        pytest.skip(f"tier0_health module not found at {tier0_path}")
    spec = importlib.util.spec_from_file_location(
        "_sweevo_tier0_health",
        tier0_path,
    )
    if spec is None or spec.loader is None:
        pytest.skip(f"tier0_health module not loadable from {tier0_path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules.setdefault(spec.name, module)
    spec.loader.exec_module(module)
    return module
