"""SWE-EVO image-backed fixtures for task-center-runner suites.

Mocked-agent and real-agent tests both use the SWE-EVO Docker image as an
environment. Full benchmark orchestration remains under
``task_center_runner.benchmarks.sweevo``.
"""

from __future__ import annotations

import os
import re
from collections.abc import AsyncIterator
from collections.abc import Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import IO

import pytest

from task_center_runner.benchmarks.sweevo.models import SWEEvoInstance, _REPO_DIR
from task_center_runner.benchmarks.sweevo.setup import (
    build_sweevo_user_prompt,
    select_sweevo_instance,
)
from task_center_runner.core.runner import RunReport
from task_center_runner.core.runner import run_scenario as _generic_run_scenario
from task_center_runner.core.stores import TaskCenterStoreBundle
from task_center_runner.hooks.registry import Hook
from task_center_runner.scenarios.base import Scenario

_DEFAULT_INSTANCE_ID = "dask__dask_2023.3.2_2023.4.0"
_SESSION_WORKSPACE_USED_ATTR = "_ephemeralos_sweevo_workspace_used_sandboxes"
_REPO_ROOT = Path(__file__).resolve().parents[3]
_LOCK_DIR = _REPO_ROOT / ".sweevo_runs" / "locks"
_HELD_SWEEVO_LOCKS: dict[Path, tuple[IO[str], int]] = {}


@dataclass(frozen=True, slots=True)
class _SweevoSessionLock:
    path: Path


async def run_scenario_on_sweevo_image(
    scenario: Scenario,
    *,
    instance: SWEEvoInstance,
    sandbox_id: str,
    audit_dir: Path,
    stores: TaskCenterStoreBundle | None = None,
    repo_dir: str = _REPO_DIR,
    extra_hooks: Sequence[Hook] = (),
    user_prompt: str | None = None,
    commit_to_workspace: bool = False,
) -> RunReport:
    """Run a mocked-agent scenario inside a SWE-EVO image workspace.

    When *commit_to_workspace* is true, the active layer-stack overlay is
    projected onto ``repo_dir`` after the run via the same
    ``apply_layerstack_to_repo`` step sweevo uses in
    :meth:`SweevoLifecycle.after_run`, so a host-side / raw ``git`` invocation on
    ``repo_dir`` sees the mock agent's edits and ``repo_dir/.git`` survives.
    Defaults to false: existing callers read final state through the
    daemon-backed ``sandbox_api`` overlay (which already reflects committed OCC
    layers) and must keep their current behavior.
    """
    entry_prompt = (
        user_prompt
        if user_prompt is not None
        else build_sweevo_user_prompt(instance, repo_dir=repo_dir)
    )
    report = await _generic_run_scenario(
        scenario,
        sandbox_id=sandbox_id,
        audit_dir=audit_dir,
        repo_dir=repo_dir,
        entry_prompt=entry_prompt,
        stores=stores,
        extra_hooks=extra_hooks,
        instance_id=instance.instance_id,
    )
    if commit_to_workspace:
        from task_center_runner.benchmarks.sweevo.eval import apply_layerstack_to_repo

        await apply_layerstack_to_repo(sandbox_id, repo_dir)
    return report


@pytest.fixture(scope="session")
def sweevo_image_instance() -> SWEEvoInstance:
    instance_id = os.getenv("EOS_SWEEVO_INSTANCE", _DEFAULT_INSTANCE_ID)
    return select_sweevo_instance(instance_id=instance_id)


@pytest.fixture(scope="session")
async def sweevo_image_sandbox(
    sweevo_image_instance: SWEEvoInstance,
) -> AsyncIterator[dict[str, object]]:
    """Provision the persistent sweevo container for the configured instance."""
    from sandbox.provider.bootstrap import bootstrap_sandbox_provider

    from task_center_runner.benchmarks.sweevo._provision import (
        _create_sandbox,
        _find_existing_sandbox_by_name,
        _resume_sandbox,
        _service,
        setup_sweevo_sandbox,
    )
    from task_center_runner.benchmarks.sweevo.models import _sweevo_sandbox_name
    from task_center_runner.environments.sweevo_image.health import (
        require_sweevo_image_provider_healthy,
    )

    worker = os.environ.get("PYTEST_XDIST_WORKER")
    lock = _acquire_sweevo_session_lock(sweevo_image_instance.instance_id, worker)
    try:
        bootstrap_sandbox_provider()
        require_sweevo_image_provider_healthy(sweevo_image_instance)
        name = _sweevo_sandbox_name(sweevo_image_instance, worker)
        service = _service()
        existing = _find_existing_sandbox_by_name(service, name)
        reused_existing = existing is not None
        if existing is None:
            sandbox_id = await _create_sandbox(
                sweevo_image_instance, name, _REPO_DIR,
            )
        else:
            sandbox_id = await _resume_sandbox(
                existing, name, sweevo_image_instance, _REPO_DIR,
            )
        await setup_sweevo_sandbox(
            sweevo_image_instance,
            sandbox_id,
            _REPO_DIR,
            install_lsp=True,
        )
        sandbox_info = service.get_sandbox(sandbox_id)
        yield {
            "sandbox_id": sandbox_id,
            "sandbox": sandbox_info,
            "snapshot_name": "",
            "repo_dir": _REPO_DIR,
            "reused_existing": reused_existing,
        }
    finally:
        _release_sweevo_session_lock(lock)


@pytest.fixture
async def workspace(
    sweevo_image_sandbox: dict[str, object],
    request: pytest.FixtureRequest,
) -> dict[str, object]:
    """Return a SWE-EVO workspace with per-test reset isolation."""
    sandbox_id = str(sweevo_image_sandbox["sandbox_id"])
    used_sandboxes = _session_workspace_used_sandboxes(request.session)
    first_use = sandbox_id not in used_sandboxes
    should_reset = (not first_use) or bool(sweevo_image_sandbox.get("reused_existing"))
    if should_reset:
        from task_center_runner.benchmarks.sweevo._provision import (
            reset_sweevo_workspace,
        )

        await reset_sweevo_workspace(sandbox_id, install_lsp=True)
    used_sandboxes.add(sandbox_id)
    return sweevo_image_sandbox


def _acquire_sweevo_session_lock(instance_id: str, worker: str | None = None) -> _SweevoSessionLock:
    """Serialize live SWE-EVO runs that may reuse the same Daytona sandbox.

    Several scenarios intentionally rebind the public-tool workspace root
    during execution. Running two live pytest sessions for the same SWE-EVO
    instance against a reusable sandbox can make one session observe the
    other's binding. A host-side flock keeps setup and the test session
    isolated without adding a dependency.
    """
    import fcntl

    _LOCK_DIR.mkdir(parents=True, exist_ok=True)
    key = instance_id if not worker else f"{instance_id}-{worker}"
    lock_path = _LOCK_DIR / f"sweevo-{_lock_slug(key)}.lock"
    held = _HELD_SWEEVO_LOCKS.get(lock_path)
    if held is not None:
        handle, count = held
        _HELD_SWEEVO_LOCKS[lock_path] = (handle, count + 1)
        return _SweevoSessionLock(lock_path)

    handle = lock_path.open("a+", encoding="utf-8")
    fcntl.flock(handle.fileno(), fcntl.LOCK_EX)
    handle.seek(0)
    handle.truncate()
    handle.write(f"pid={os.getpid()}\ninstance={instance_id}\n")
    handle.flush()
    _HELD_SWEEVO_LOCKS[lock_path] = (handle, 1)
    return _SweevoSessionLock(lock_path)


def _release_sweevo_session_lock(lock: _SweevoSessionLock) -> None:
    import fcntl

    held = _HELD_SWEEVO_LOCKS.get(lock.path)
    if held is None:
        return
    handle, count = held
    if count > 1:
        _HELD_SWEEVO_LOCKS[lock.path] = (handle, count - 1)
        return
    try:
        fcntl.flock(handle.fileno(), fcntl.LOCK_UN)
    finally:
        _HELD_SWEEVO_LOCKS.pop(lock.path, None)
        handle.close()


def _lock_slug(value: str) -> str:
    slug = re.sub(r"[^A-Za-z0-9_.-]+", "-", value.strip()).strip("-")
    return slug or "default"


def _session_workspace_used_sandboxes(session: object) -> set[str]:
    """Track workspace use in the current pytest process only."""
    current = getattr(session, _SESSION_WORKSPACE_USED_ATTR, None)
    if isinstance(current, set):
        return current
    used: set[str] = set()
    setattr(session, _SESSION_WORKSPACE_USED_ATTR, used)
    return used


__all__ = [
    "RunReport",
    "run_scenario_on_sweevo_image",
    "sweevo_image_instance",
    "sweevo_image_sandbox",
    "workspace",
]
