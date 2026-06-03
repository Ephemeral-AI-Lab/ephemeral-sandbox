from __future__ import annotations

import os
import time

import pytest

from config import load_settings
import sandbox.api as sandbox_api
from sandbox.host.paths import BUNDLE_REMOTE_DIR
from sandbox.host.runtime_bundle import bundle_hash
from sandbox.host.bootstrap import setup_after_create
from sandbox.provider.daytona.bootstrap import bootstrap_daytona_provider
from sandbox.provider.registry import get_default_provider, register_adapter


pytestmark = [
    pytest.mark.live,
    pytest.mark.skipif(
        os.getenv("EPHEMERALOS_RUN_LIVE_SANDBOX_TESTS") != "1",
        reason="set EPHEMERALOS_RUN_LIVE_SANDBOX_TESTS=1 to run live Daytona sandbox tests",
    ),
]


@pytest.mark.asyncio
async def test_host_setup_prepares_benchmark_runtime() -> None:
    bootstrap_daytona_provider()
    settings = load_settings()
    assert settings.sandbox.daytona.default_image, (
        "live test requires sandbox.daytona.default_image"
    )

    name = f"eos-live-control-setup-{int(time.time())}"
    sandbox_id = ""
    try:
        provider = get_default_provider()
        created = provider.create(
            name=name,
            image=settings.sandbox.daytona.default_image,
            language="python",
            labels={
                "purpose": "live-sandbox-control-setup",
                "project_dir": "/testbed",
            },
        )
        sandbox_id = str(created["id"])
        register_adapter(sandbox_id, provider)
        setup_after_create(sandbox_id, "/testbed")

        assert created["state"] == "started"
        assert created["project_dir"] == "/testbed"
        assert created["image"] == settings.sandbox.daytona.default_image

        probe = await sandbox_api.raw_exec(
            sandbox_id,
            (
                "set -e; "
                "test -d /testbed; "
                "test -d /testbed/.git; "
                "git -C /testbed rev-parse --show-toplevel; "
                "git -C /testbed rev-parse --short HEAD; "
                "python --version; "
                f"test -f {BUNDLE_REMOTE_DIR}/.bundle-hash; "
                f"cat {BUNDLE_REMOTE_DIR}/.bundle-hash"
            ),
            timeout=60,
        )
        assert probe.exit_code == 0, probe.stdout
        assert "/testbed" in probe.stdout
        assert bundle_hash() in probe.stdout

        tests = await sandbox_api.raw_exec(
            sandbox_id,
            "cd /testbed && pytest -q tests/test_structures.py --maxfail=1",
            timeout=120,
        )
        assert tests.exit_code == 0, tests.stdout
        assert "20 passed" in tests.stdout
    finally:
        if sandbox_id:
            sandbox_api.delete_sandbox(sandbox_id)
        else:
            for sandbox in sandbox_api.list_sandboxes():
                if sandbox.get("name") == name and sandbox.get("id"):
                    sandbox_api.delete_sandbox(str(sandbox["id"]))
