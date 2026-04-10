"""SWE-EVO sandbox provisioning, setup, and command execution."""

from __future__ import annotations

import base64
import logging
import shlex
from typing import Any
from uuid import uuid4

from sandbox.async_client import get_async_sandbox
from tools.daytona_toolkit.tools import _upload_file_compat

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


def _default_sweevo_sandbox_name(instance: SWEEvoInstance) -> str:
    """Return a unique sandbox name for a fresh SWE-EVO run."""
    return _truncate_dns_label(f"sweevo-test-{instance.instance_id}-{uuid4().hex[:8]}")


def _find_existing_sandbox_by_name(service: Any, name: str) -> dict[str, Any] | None:
    """Return an existing sandbox record matching ``name`` if present."""
    for sandbox in service.list_sandboxes():
        if sandbox.get("name") == name:
            return sandbox
    return None


def _find_latest_started_sweevo_sandbox(
    service: Any,
    instance: SWEEvoInstance,
    *,
    exclude_name: str = "",
) -> dict[str, Any] | None:
    """Return the newest started sandbox for this SWE-EVO instance, if any."""
    expected_prefix = f"sweevo-test-{instance.instance_id}-"
    for sandbox in service.list_sandboxes():
        name = str(sandbox.get("name") or "")
        if exclude_name and name == exclude_name:
            continue
        if str(sandbox.get("state") or "") != "started":
            continue
        labels = sandbox.get("labels") or {}
        same_instance = str(labels.get("sweevo_instance") or "") == instance.instance_id
        same_name_family = name.startswith(expected_prefix)
        if same_instance or same_name_family:
            return sandbox
    return None


def _is_timeout_error(exc: Exception) -> bool:
    name = exc.__class__.__name__.lower()
    text = str(exc).lower()
    return "timeout" in name or "timeout" in text or "timed out" in text


def _log_sandbox_creation_failure(
    service: Any,
    *,
    sandbox_name: str,
    instance: SWEEvoInstance,
    exc: Exception,
) -> None:
    pending = _find_existing_sandbox_by_name(service, sandbox_name)
    if pending is None:
        logger.warning(
            "Fresh SWE-EVO sandbox %s for %s failed before the sandbox became discoverable: %s",
            sandbox_name,
            instance.instance_id,
            exc,
        )
        return
    build_logs_url = None
    try:
        build_logs_url = service.get_build_logs_url(str(pending["id"]))
    except Exception:
        logger.debug(
            "Failed to fetch build logs URL for sandbox %s",
            pending.get("id", ""),
            exc_info=True,
        )
    logger.warning(
        "Fresh SWE-EVO sandbox %s (%s) failed in state=%s build_logs_url=%s error=%s",
        sandbox_name,
        pending.get("id", ""),
        pending.get("state", "unknown"),
        build_logs_url or "-",
        exc,
    )


def _cleanup_failed_sandbox(service: Any, sandbox_name: str) -> None:
    pending = _find_existing_sandbox_by_name(service, sandbox_name)
    if pending is None:
        return
    state = str(pending.get("state") or "")
    if state not in {"pending_build", "build_failed", "error"}:
        return
    try:
        service.delete_sandbox(str(pending["id"]))
    except Exception:
        logger.debug(
            "Failed to delete unhealthy SWE-EVO sandbox %s",
            pending.get("id", ""),
            exc_info=True,
        )


# ---------------------------------------------------------------------------
# Sandbox command execution
# ---------------------------------------------------------------------------


async def _get_sandbox(sandbox_id: str) -> Any:
    """Get the async Daytona sandbox object."""
    return await get_async_sandbox(sandbox_id)


async def _upload_file_compat(sandbox: Any, content: bytes, path: str) -> None:
    """Upload using the SDK's content-first signature with a stale-mock fallback."""
    try:
        await sandbox.fs.upload_file(content, path)
    except (AttributeError, TypeError) as exc:
        if "decode" not in str(exc) and "bytes-like object" not in str(exc):
            raise
        await sandbox.fs.upload_file(path, content)


async def _write_file_via_chunked_base64_exec(
    sandbox_id: str,
    path: str,
    content: bytes,
    *,
    chunk_size: int = 4096,
) -> None:
    """Write a file via repeated short exec calls when direct upload is unavailable."""
    encoded = base64.b64encode(content).decode("ascii")
    encoded_path = f"{path}.b64"
    await _exec(sandbox_id, f": > {shlex.quote(encoded_path)}")
    for start in range(0, len(encoded), chunk_size):
        chunk = encoded[start:start + chunk_size]
        await _exec(
            sandbox_id,
            f"printf %s {shlex.quote(chunk)} >> {shlex.quote(encoded_path)}",
        )
    await _exec(
        sandbox_id,
        f"base64 -d {shlex.quote(encoded_path)} > {shlex.quote(path)} && rm -f {shlex.quote(encoded_path)}",
    )


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
    wrapped_cmd = f"bash -lc {shlex.quote(cmd)}"
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
    # Retry runs may reuse the same named sandbox. Always restore the repo to
    # a clean benchmark checkpoint before reapplying the SWE-EVO test patch so
    # failed edits from earlier attempts do not contaminate the next run.
    await _exec(sandbox_id, f"cd {repo_dir} && git reset --hard HEAD 2>/dev/null")
    await _exec(sandbox_id, f"cd {repo_dir} && git clean -fd 2>/dev/null")
    await _exec(sandbox_id, f"cd {repo_dir} && git checkout -f {instance.base_commit} 2>/dev/null")
    await _exec(sandbox_id, f"cd {repo_dir} && git checkout -B sweevo-work {instance.base_commit} 2>/dev/null")
    await _exec(
        sandbox_id,
        f"{_CONDA_ACTIVATE} && cd {repo_dir} && pip install -e . -q 2>/dev/null || true",
        timeout=_DEFAULT_SANDBOX_SETUP_TIMEOUT,
    )

    try:
        sandbox = _service().get_sandbox_object(sandbox_id)
        existing_labels = getattr(sandbox, "labels", None) or {}
        merged_labels = {str(k): str(v) for k, v in dict(existing_labels).items()}
        merged_labels["project_dir"] = repo_dir
        sandbox.set_labels(merged_labels)
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


async def ensure_sweevo_test_patch(
    instance: SWEEvoInstance,
    sandbox_id: str,
    repo_dir: str = _REPO_DIR,
) -> None:
    """Apply the SWE-EVO test patch so planner, workers, and grader share one test surface."""
    test_patch = instance.test_patch
    if not test_patch:
        logger.warning(
            "No test patch for %s — F2P tests may not exist",
            instance.instance_id,
        )
        return

    patch_path = "/tmp/sweevo_test.patch"
    try:
        sandbox = await _get_sandbox(sandbox_id)
        await _upload_file_compat(sandbox, test_patch.encode("utf-8"), patch_path)
    except Exception:
        await _write_file_via_chunked_base64_exec(
            sandbox_id,
            patch_path,
            test_patch.encode("utf-8"),
        )

    patch_status = await _exec(
        sandbox_id,
        (
            f"cd {repo_dir} && "
            f"if git apply --check {patch_path} >/dev/null 2>&1; then "
            f"echo APPLYABLE; "
            f"elif git apply -R --check {patch_path} >/dev/null 2>&1; then "
            f"echo ALREADY_APPLIED; "
            f"else "
            f"git apply --check {patch_path} 2>&1; "
            f"fi"
        ),
        check=False,
    )
    normalized_status = patch_status.strip()
    if normalized_status == "APPLYABLE":
        out = await _exec(
            sandbox_id,
            f"cd {repo_dir} && git apply {patch_path} 2>&1",
            check=False,
        )
        lower = out.lower()
        if "error" in lower and "already applied" not in lower:
            logger.warning(
                "Test patch for %s had issues: %s",
                instance.instance_id,
                out[:300],
            )
        else:
            logger.info("Ensured test patch for %s", instance.instance_id)
    elif normalized_status == "ALREADY_APPLIED":
        logger.info("Test patch for %s already applied", instance.instance_id)
    else:
        logger.warning(
            "Test patch for %s had issues: %s",
            instance.instance_id,
            patch_status[:300],
        )

    try:
        from code_intelligence.routing.service import dispose_code_intelligence

        dispose_code_intelligence(sandbox_id)
    except Exception:
        logger.debug(
            "CI disposal skipped after test patch for sandbox %s",
            sandbox_id,
            exc_info=True,
        )


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

    resolved_name = _truncate_dns_label(sandbox_name) if sandbox_name else _default_sweevo_sandbox_name(instance)
    if sandbox_name:
        existing = _find_existing_sandbox_by_name(service, resolved_name)
        if existing:
            state = str(existing.get("state") or "")
            if state == "stopped":
                try:
                    existing = service.start_sandbox(str(existing["id"]))
                except Exception:
                    logger.warning(
                        "Failed to start stopped SWE-EVO sandbox %s (%s)",
                        resolved_name,
                        existing.get("id", ""),
                    )
                    existing = None
            elif state not in {"started", ""}:
                logger.warning(
                    "Ignoring named SWE-EVO sandbox %s in non-reusable state %s",
                    resolved_name,
                    state,
                )
                existing = None
        if existing:
            logger.info(
                "Reusing existing SWE-EVO sandbox %s (%s) for retry",
                resolved_name,
                existing.get("id", ""),
            )
            await setup_sweevo_sandbox(instance, existing["id"], repo_dir)
            await ensure_sweevo_test_patch(instance, existing["id"], repo_dir)
            return {
                "sandbox_id": existing["id"],
                "sandbox": existing,
                "snapshot_name": "",
                "repo_dir": repo_dir,
                "reused_existing": True,
            }

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

    try:
        result = service.create_sandbox(
            name=resolved_name,
            language="python",
            labels={
                "purpose": "sweevo-test",
                "sweevo_instance": instance.instance_id,
                "sweevo_repo": instance.repo,
            },
            **create_kwargs,
        )
    except Exception as exc:
        _log_sandbox_creation_failure(
            service,
            sandbox_name=resolved_name,
            instance=instance,
            exc=exc,
        )
        if _is_timeout_error(exc):
            fallback = _find_latest_started_sweevo_sandbox(
                service,
                instance,
                exclude_name=resolved_name,
            )
            if fallback is not None:
                logger.warning(
                    "Falling back to started SWE-EVO sandbox %s (%s) after fresh build failure",
                    fallback.get("name", ""),
                    fallback.get("id", ""),
                )
                await setup_sweevo_sandbox(instance, str(fallback["id"]), repo_dir)
                await ensure_sweevo_test_patch(instance, str(fallback["id"]), repo_dir)
                _cleanup_failed_sandbox(service, resolved_name)
                return {
                    "sandbox_id": fallback["id"],
                    "sandbox": fallback,
                    "snapshot_name": "",
                    "repo_dir": repo_dir,
                    "reused_existing": True,
                    "fallback_reason": "fresh_create_timeout_reused_started_sandbox",
                    "fresh_create_error": str(exc),
                }
        _cleanup_failed_sandbox(service, resolved_name)
        raise
    sandbox_id = result["id"]
    await setup_sweevo_sandbox(instance, sandbox_id, repo_dir)
    await ensure_sweevo_test_patch(instance, sandbox_id, repo_dir)
    sandbox_info = service.get_sandbox(sandbox_id)
    return {
        "sandbox_id": sandbox_id,
        "sandbox": sandbox_info,
        "snapshot_name": resolved_snapshot,
        "repo_dir": repo_dir,
        "reused_existing": False,
    }


async def run_sweevo_required_test(
    instance: SWEEvoInstance,
    sandbox_id: str,
    *,
    repo_dir: str = _REPO_DIR,
    test_command: str | None = None,
    timeout: int = _DEFAULT_SWEEVO_TEST_TIMEOUT,
    on_line: "callable | None" = None,
    poll_interval: float = 1.5,
) -> dict[str, Any]:
    """Run the instance's required test command inside the prepared sandbox.

    If ``on_line`` is provided, stream stdout/stderr line-by-line as the test
    runs (via background shell + ``tail -c +N`` polling on a log file). Without
    it, behaves like the original one-shot exec.
    """
    import asyncio
    import re
    import time as _time

    resolved_command = (test_command or instance.test_cmds).strip()
    if not resolved_command:
        raise ValueError(f"Instance {instance.instance_id} has no test command.")

    if on_line is None:
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

    # ---- Streaming mode ---------------------------------------------------
    log_path = f"/tmp/sweevo_run_{int(_time.time() * 1000)}.log"
    pid_path = f"{log_path}.pid"
    done_path = f"{log_path}.done"

    spawn_cmd = (
        f"rm -f {log_path} {pid_path} {done_path} && "
        f"( {_CONDA_ACTIVATE} && cd {repo_dir} && {resolved_command} > {log_path} 2>&1; "
        f"echo $? > {done_path} ) & "
        f"echo $! > {pid_path}"
    )
    await _exec(sandbox_id, spawn_cmd, check=False)

    offset = 1  # tail -c +N is 1-indexed
    buf = ""
    collected: list[str] = []
    deadline = _time.monotonic() + timeout
    exit_code: int | None = None

    while True:
        poll_cmd = (
            f'tail -c +{offset} {log_path} 2>/dev/null; '
            f'printf "\\n__SWEEVO_MARK__"; '
            f'if [ -f {done_path} ]; then cat {done_path}; fi'
        )
        chunk = await _exec(sandbox_id, poll_cmd, check=False)
        marker_idx = chunk.rfind("__SWEEVO_MARK__")
        if marker_idx >= 0:
            data = chunk[:marker_idx]
            tail = chunk[marker_idx + len("__SWEEVO_MARK__"):].strip()
        else:
            data = chunk
            tail = ""

        if data:
            # strip the trailing newline we injected before the marker
            if data.endswith("\n"):
                data = data[:-1]
            offset += len(data.encode("utf-8", errors="replace"))
            buf += data
            while "\n" in buf:
                line, buf = buf.split("\n", 1)
                collected.append(line)
                try:
                    on_line(line)
                except Exception:
                    logger.debug("on_line callback raised", exc_info=True)

        if tail and tail.isdigit():
            exit_code = int(tail)
            if buf:
                collected.append(buf)
                try:
                    on_line(buf)
                except Exception:
                    pass
                buf = ""
            break

        if _time.monotonic() > deadline:
            logger.warning("Streaming test run exceeded timeout %ss", timeout)
            break

        await asyncio.sleep(poll_interval)

    return {
        "command": resolved_command,
        "exit_code": exit_code,
        "output": "\n".join(collected),
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
    on_line: "callable | None" = None,
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
        on_line=on_line,
    )
    return {
        "instance": summarize_sweevo_instance(instance),
        "snapshot_name": sandbox_result["snapshot_name"],
        "sandbox": sandbox_result["sandbox"],
        "repo_dir": repo_dir,
        "test": test_result,
    }
