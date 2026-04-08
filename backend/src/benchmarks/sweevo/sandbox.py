"""SWE-EVO sandbox provisioning, setup, and command execution."""

from __future__ import annotations

import logging
from typing import Any

from sandbox.async_client import get_async_sandbox

from benchmarks.sweevo.dataset import (
    default_sweevo_snapshot_name,
    select_sweevo_instance,
    summarize_sweevo_instance,
)
from benchmarks.sweevo.models import (
    SWEEvoInstance,
    _CONDA_ACTIVATE,
    _DEFAULT_DATASET_SOURCE,
    _DEFAULT_SANDBOX_COMMAND_TIMEOUT,
    _DEFAULT_SANDBOX_SETUP_TIMEOUT,
    _DEFAULT_SNAPSHOT_CREATE_TIMEOUT,
    _DEFAULT_SWEEVO_TEST_TIMEOUT,
    _DEFAULT_TARGET_BULLETS,
    _REPO_DIR,
    _normalize_sweevo_image_ref,
    _strip_exit_code_marker,
    _truncate_dns_label,
)

logger = logging.getLogger(__name__)


# ---------------------------------------------------------------------------
# Sandbox service accessor (EphemeralOS SandboxService)
# ---------------------------------------------------------------------------


def _service() -> Any:
    """Return a SandboxService instance (lazy import to avoid cycles)."""
    from sandbox.service import SandboxService

    return SandboxService()


# ---------------------------------------------------------------------------
# Sandbox command execution
# ---------------------------------------------------------------------------


async def _get_sandbox(sandbox_id: str) -> Any:
    """Get the async Daytona sandbox object."""
    return await get_async_sandbox(sandbox_id)


async def _exec(
    sandbox_id: str,
    cmd: str,
    timeout: int = _DEFAULT_SANDBOX_COMMAND_TIMEOUT,
    cwd: str | None = None,
    *,
    check: bool = True,
) -> str:
    """Execute a command in the sandbox via the async Daytona SDK, returning stdout."""
    sandbox = await _get_sandbox(sandbox_id)
    wrapped_cmd = f"bash -c {repr(cmd)}"
    try:
        response = await sandbox.process.exec(
            wrapped_cmd,
            cwd=cwd or "/",
            timeout=timeout,
        )
        result_text = response.result if hasattr(response, "result") else str(response)
        exit_code = getattr(response, "exit_code", None)
        if exit_code not in (None, 0):
            message = (
                f"Sandbox command failed with exit code {exit_code}: {cmd[:100]}\n"
                f"Output: {result_text[:500]}"
            )
            logger.warning(message)
            if check:
                raise RuntimeError(message)
        return result_text
    except Exception as exc:
        if check and isinstance(exc, RuntimeError):
            raise
        logger.warning("Sandbox exec failed: %s\nCommand: %s", exc, cmd[:100])
        if check:
            raise
        return f"ERROR: {exc}"


# ---------------------------------------------------------------------------
# Sandbox provisioning (Daytona)
# ---------------------------------------------------------------------------


def provision_sweevo_sandbox(instance: SWEEvoInstance) -> str:
    """Create a Daytona sandbox from the SWE-EVO Docker image."""
    if not instance.docker_image:
        raise ValueError(
            f"Instance {instance.instance_id} has no docker_image — cannot provision sandbox"
        )

    sandbox_name = f"sweevo-{instance.instance_id}"
    if len(sandbox_name) > 63:
        suffix = sandbox_name[-8:]
        sandbox_name = sandbox_name[:54] + "-" + suffix

    sandbox = _service().create_sandbox(
        name=sandbox_name,
        snapshot=_normalize_sweevo_image_ref(instance.docker_image),
        language="python",
        labels={
            "sweevo_instance": instance.instance_id,
            "sweevo_repo": instance.repo,
        },
    )
    sandbox_id = sandbox["id"]
    logger.info(
        "Provisioned SWE-EVO sandbox %s from image %s",
        sandbox_id,
        instance.docker_image,
    )
    return sandbox_id


def register_sweevo_snapshot(
    instance: SWEEvoInstance,
    *,
    snapshot_name: str = "",
    cpu: int = 2,
    disk: int = 10,
) -> str:
    """Register a SWE-EVO Docker image as a Daytona snapshot."""
    import subprocess

    if not instance.docker_image:
        raise ValueError(f"Instance {instance.instance_id} has no docker_image")

    name = snapshot_name or f"sweevo-{instance.instance_id_swe or instance.instance_id}"
    if len(name) > 63:
        name = name[:63]

    image_ref = _normalize_sweevo_image_ref(instance.docker_image)

    logger.info("Registering SWE-EVO snapshot '%s' from %s", name, image_ref)
    result = subprocess.run(
        [
            "daytona",
            "snapshot",
            "create",
            name,
            "--image",
            image_ref,
            "--entrypoint",
            "sleep infinity",
            "--cpu",
            str(cpu),
            "--disk",
            str(disk),
        ],
        capture_output=True,
        text=True,
        timeout=_DEFAULT_SNAPSHOT_CREATE_TIMEOUT,
    )
    if result.returncode != 0:
        raise RuntimeError(f"Failed to register snapshot {name}: {result.stderr}")
    logger.info("Registered snapshot: %s", name)
    return name


def resolve_sweevo_snapshot(
    instance: SWEEvoInstance,
    *,
    snapshot_name: str = "",
    register_snapshot: bool = True,
    cpu: int = 2,
    disk: int = 10,
) -> str:
    """Resolve the Daytona snapshot identifier to use for a SWE-EVO sandbox."""
    if register_snapshot:
        return register_sweevo_snapshot(
            instance,
            snapshot_name=snapshot_name or default_sweevo_snapshot_name(instance),
            cpu=cpu,
            disk=disk,
        )
    return snapshot_name or instance.docker_image


# ---------------------------------------------------------------------------
# Sandbox setup
# ---------------------------------------------------------------------------


async def setup_sweevo_sandbox(
    instance: SWEEvoInstance,
    sandbox_id: str,
    repo_dir: str = _REPO_DIR,
) -> str:
    """Prepare the sandbox by checking out the repo at the base commit."""
    await _exec(sandbox_id, f"test -d {repo_dir} && test -d {repo_dir}/.git")
    await _exec(sandbox_id, f"{_CONDA_ACTIVATE} && python --version")
    await _exec(sandbox_id, f"cd {repo_dir} && git checkout {instance.base_commit} 2>/dev/null")
    await _exec(sandbox_id, f"cd {repo_dir} && git checkout -b sweevo-work 2>/dev/null || true")
    await _exec(
        sandbox_id,
        f"{_CONDA_ACTIVATE} && cd {repo_dir} && pip install -e . -q 2>/dev/null || true",
        timeout=_DEFAULT_SANDBOX_SETUP_TIMEOUT,
    )

    try:
        sandbox = _service().get_sandbox_object(sandbox_id)
        sandbox.set_labels({"project_dir": repo_dir})
        logger.info("Set project_dir label to %s", repo_dir)
    except Exception as exc:
        logger.warning("Could not set project_dir label: %s", exc)

    logger.info(
        "SWE-EVO sandbox %s ready: %s @ %s",
        sandbox_id,
        instance.repo,
        instance.base_commit[:12],
    )
    return repo_dir


async def create_sweevo_test_sandbox(
    instance: SWEEvoInstance,
    *,
    snapshot_name: str = "",
    sandbox_name: str = "",
    register_snapshot: bool = True,
    cpu: int = 2,
    disk: int = 10,
    repo_dir: str = _REPO_DIR,
) -> dict[str, Any]:
    """Create and prepare a Daytona sandbox for direct SWE-EVO test execution."""
    service = _service()

    resolved_name = sandbox_name or f"sweevo-test-{instance.instance_id}"
    create_kwargs: dict[str, Any] = {}
    resolved_snapshot = ""
    if register_snapshot:
        resolved_snapshot = resolve_sweevo_snapshot(
            instance,
            snapshot_name=snapshot_name,
            register_snapshot=True,
            cpu=cpu,
            disk=disk,
        )
        create_kwargs["snapshot"] = resolved_snapshot
    elif snapshot_name:
        resolved_snapshot = snapshot_name
        create_kwargs["snapshot"] = resolved_snapshot
    else:
        create_kwargs["image"] = _normalize_sweevo_image_ref(instance.docker_image)

    result = service.create_sandbox(
        name=_truncate_dns_label(resolved_name),
        language="python",
        labels={
            "purpose": "sweevo-test",
            "sweevo_instance": instance.instance_id,
            "sweevo_repo": instance.repo,
        },
        **create_kwargs,
    )
    sandbox_id = result["id"]
    await setup_sweevo_sandbox(instance, sandbox_id, repo_dir)
    sandbox_info = service.get_sandbox(sandbox_id)
    return {
        "sandbox_id": sandbox_id,
        "sandbox": sandbox_info,
        "snapshot_name": resolved_snapshot,
        "repo_dir": repo_dir,
    }


async def run_sweevo_required_test(
    instance: SWEEvoInstance,
    sandbox_id: str,
    *,
    repo_dir: str = _REPO_DIR,
    test_command: str | None = None,
    timeout: int = _DEFAULT_SWEEVO_TEST_TIMEOUT,
) -> dict[str, Any]:
    """Run the instance's required test command inside the prepared sandbox."""
    import re

    resolved_command = (test_command or instance.test_cmds).strip()
    if not resolved_command:
        raise ValueError(f"Instance {instance.instance_id} has no test command.")

    output = await _exec(
        sandbox_id,
        (f'{_CONDA_ACTIVATE} && cd {repo_dir} && {resolved_command} 2>&1; echo "EXIT_CODE=$?"'),
        timeout=timeout,
        check=False,
    )
    match = re.search(r"EXIT_CODE=(\d+)", output)
    exit_code = int(match.group(1)) if match else None
    return {
        "command": resolved_command,
        "exit_code": exit_code,
        "output": _strip_exit_code_marker(output),
    }


async def prepare_sweevo_test_run(
    *,
    source: str = _DEFAULT_DATASET_SOURCE,
    instance_id: str | None = None,
    size: str = "medium",
    target_bullets: int = _DEFAULT_TARGET_BULLETS,
    snapshot_name: str = "",
    sandbox_name: str = "",
    register_snapshot: bool = True,
    cpu: int = 2,
    disk: int = 10,
    repo_dir: str = _REPO_DIR,
    test_command: str | None = None,
    test_timeout: int = _DEFAULT_SWEEVO_TEST_TIMEOUT,
) -> dict[str, Any]:
    """Resolve an instance, prepare its sandbox, and run the required test."""
    instance = select_sweevo_instance(
        source=source,
        instance_id=instance_id,
        size=size,
        target_bullets=target_bullets,
    )
    sandbox_result = await create_sweevo_test_sandbox(
        instance,
        snapshot_name=snapshot_name,
        sandbox_name=sandbox_name,
        register_snapshot=register_snapshot,
        cpu=cpu,
        disk=disk,
        repo_dir=repo_dir,
    )
    test_result = await run_sweevo_required_test(
        instance,
        sandbox_result["sandbox_id"],
        repo_dir=repo_dir,
        test_command=test_command,
        timeout=test_timeout,
    )
    return {
        "instance": summarize_sweevo_instance(instance),
        "snapshot_name": sandbox_result["snapshot_name"],
        "sandbox": sandbox_result["sandbox"],
        "repo_dir": repo_dir,
        "test": test_result,
    }
