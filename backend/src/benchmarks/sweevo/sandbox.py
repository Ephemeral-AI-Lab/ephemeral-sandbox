"""SWE-EVO sandbox provisioning, setup, and command execution."""

from __future__ import annotations

import base64
import logging
import shlex
import time
from collections.abc import Callable
from typing import Any
from uuid import uuid4

from sandbox.async_client import get_async_sandbox
from sandbox.daytona_utils import _build_write_text_file_command, _wrap_bash_command

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
    _has_explicit_sweevo_image_version,
    _normalize_sweevo_image_ref,
    _strip_exit_code_marker,
    _truncate_dns_label,
)

logger = logging.getLogger(__name__)

ProgressCallback = Callable[[str], None]


def _progress(on_progress: ProgressCallback | None, message: str) -> None:
    if on_progress is None:
        return
    try:
        on_progress(message)
    except Exception:
        logger.debug("SWE-EVO progress callback failed", exc_info=True)


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


def _safe_list_sandboxes(
    service: Any,
    *,
    attempts: int = 6,
    delay_s: float = 1.0,
    max_delay_s: float = 4.0,
) -> list[dict[str, Any]]:
    """List sandboxes with bounded backoff for transient Daytona API resets."""
    last_exc: Exception | None = None
    for attempt in range(1, attempts + 1):
        try:
            return list(service.list_sandboxes())
        except Exception as exc:
            last_exc = exc
            if attempt < attempts:
                logger.warning(
                    "Listing SWE-EVO sandboxes failed (attempt %s/%s): %s",
                    attempt,
                    attempts,
                    exc,
                )
                time.sleep(min(delay_s * attempt, max_delay_s))
    logger.warning("Listing SWE-EVO sandboxes failed after %s attempts: %s", attempts, last_exc)
    return []


def _find_existing_sandbox_by_name(service: Any, name: str) -> dict[str, Any] | None:
    """Return an existing sandbox record matching ``name`` if present."""
    for sandbox in _safe_list_sandboxes(service):
        if sandbox.get("name") == name:
            return sandbox
    return None


def _prune_auto_sweevo_sandboxes_for_fresh_run(
    service: Any,
    instance: SWEEvoInstance,
) -> list[str]:
    """Delete older inactive auto-generated sandboxes before a fresh run.

    Fresh runs must not delete active sandboxes that may still back another
    in-flight run for the same instance.
    """
    expected_prefix = f"sweevo-test-{instance.instance_id}-"
    deleted: list[str] = []
    for sandbox in _safe_list_sandboxes(service):
        name = str(sandbox.get("name") or "")
        if not name.startswith(expected_prefix):
            continue
        state = str(sandbox.get("state") or "")
        if state not in {"stopped", "build_failed", "error"}:
            continue
        sandbox_id = str(sandbox.get("id") or "")
        if not sandbox_id:
            continue
        try:
            service.delete_sandbox(sandbox_id)
            deleted.append(sandbox_id)
        except Exception:
            logger.warning(
                "Failed to delete stale SWE-EVO sandbox %s (%s) during fresh-run prune",
                name,
                sandbox_id,
                exc_info=True,
            )
    return deleted


def _log_sandbox_creation_failure(
    service: Any,
    *,
    sandbox_name: str,
    instance: SWEEvoInstance,
    exc: Exception,
    sandbox: dict[str, Any] | None = None,
) -> None:
    if sandbox is None:
        logger.warning(
            "Fresh SWE-EVO sandbox %s for %s failed before the sandbox became discoverable: %s",
            sandbox_name,
            instance.instance_id,
            exc,
        )
        return
    build_logs_url = None
    try:
        build_logs_url = service.get_build_logs_url(str(sandbox["id"]))
    except Exception:
        logger.debug(
            "Failed to fetch build logs URL for sandbox %s",
            sandbox.get("id", ""),
            exc_info=True,
        )
    logger.warning(
        "Fresh SWE-EVO sandbox %s (%s) failed in state=%s build_logs_url=%s error=%s",
        sandbox_name,
        sandbox.get("id", ""),
        sandbox.get("state", "unknown"),
        build_logs_url or "-",
        exc,
    )


def _cleanup_failed_sandbox(service: Any, sandbox: dict[str, Any] | None) -> None:
    if sandbox is None:
        return
    state = str(sandbox.get("state") or "")
    if state not in {"pending_build", "build_failed", "error"}:
        return
    try:
        service.delete_sandbox(str(sandbox["id"]))
    except Exception:
        logger.debug(
            "Failed to delete unhealthy SWE-EVO sandbox %s",
            sandbox.get("id", ""),
            exc_info=True,
        )


# ---------------------------------------------------------------------------
# Sandbox command execution
# ---------------------------------------------------------------------------


async def _get_sandbox(sandbox_id: str) -> Any:
    """Get the async Daytona sandbox object."""
    import asyncio

    last_exc: Exception | None = None
    for attempt in range(1, 4):
        try:
            return await get_async_sandbox(sandbox_id)
        except Exception as exc:
            last_exc = exc
            if attempt < 3:
                logger.warning(
                    "Fetching SWE-EVO async sandbox %s failed (attempt %s/3): %s",
                    sandbox_id,
                    attempt,
                    exc,
                )
                await asyncio.sleep(1.0)
    assert last_exc is not None
    raise last_exc


async def _upload_file_with_fallback(
    sandbox_id: str,
    path: str,
    content: bytes,
) -> None:
    """Upload a text file to the sandbox via exec, falling back to chunked exec."""
    try:
        sandbox = await _get_sandbox(sandbox_id)
        await _upload_file_compat(sandbox, content, path)
    except UnicodeDecodeError:
        await _write_file_via_chunked_base64_exec(sandbox_id, path, content)
    except Exception:
        await _write_file_via_chunked_base64_exec(sandbox_id, path, content)


async def _upload_file_compat(
    sandbox: Any,
    content: bytes,
    path: str,
) -> None:
    """Upload a text file via exec when available, otherwise use sandbox.fs."""
    process = getattr(sandbox, "process", None)
    if callable(getattr(process, "exec", None)):
        text = content.decode("utf-8")
        response = await process.exec(
            _wrap_bash_command(_build_write_text_file_command(path, text)),
            timeout=60,
        )
        if getattr(response, "exit_code", 0) not in (0, None):
            raise RuntimeError(getattr(response, "result", "") or f"write failed for {path}")
        return
    fs = getattr(sandbox, "fs", None)
    upload_fn = getattr(fs, "upload_file", None)
    if not callable(upload_fn):
        raise RuntimeError("Sandbox text upload transport is unavailable")
    await upload_fn(content, path)


def _dispose_code_intelligence_quietly(sandbox_id: str, context: str) -> None:
    """Dispose code intelligence for a sandbox, logging debug on failure."""
    try:
        from code_intelligence.service import dispose_code_intelligence

        dispose_code_intelligence(sandbox_id)
    except Exception:
        logger.debug(
            "CI disposal skipped after %s for sandbox %s",
            context,
            sandbox_id,
            exc_info=True,
        )


def _is_transient_sandbox_exec_error(exc: Exception) -> bool:
    text = str(exc).lower()
    return (
        "connection reset" in text
        or "connection refused" in text
        or "server disconnected" in text
        or "failed to execute command" in text
        or "clientoserror" in text
        or "temporarily unavailable" in text
    )


async def _wait_for_sandbox_exec_ready(
    sandbox_id: str,
    *,
    attempts: int = 6,
    delay_s: float = 1.0,
) -> None:
    """Wait until a started sandbox accepts toolbox exec requests."""
    import asyncio

    last_exc: Exception | None = None
    for attempt in range(1, attempts + 1):
        try:
            sandbox = await _get_sandbox(sandbox_id)
            response = await sandbox.process.exec("pwd", cwd="/", timeout=10)
            exit_code = getattr(response, "exit_code", None)
            if exit_code in (None, 0):
                return
            last_exc = RuntimeError(
                f"Sandbox readiness probe exited with code {exit_code}: "
                f"{getattr(response, 'result', '')}"
            )
        except Exception as exc:
            last_exc = exc
            if not _is_transient_sandbox_exec_error(exc):
                raise

        if attempt < attempts:
            logger.warning(
                "SWE-EVO sandbox %s exec readiness probe failed (attempt %s/%s): %s",
                sandbox_id,
                attempt,
                attempts,
                last_exc,
            )
            await asyncio.sleep(delay_s)

    assert last_exc is not None
    raise RuntimeError(f"SWE-EVO sandbox {sandbox_id} did not become exec-ready") from last_exc


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
    *,
    on_progress: ProgressCallback | None = None,
) -> str:
    """Prepare the sandbox by checking out the repo at the base commit."""
    _progress(on_progress, f"[setup] waiting for sandbox exec readiness sandbox_id={sandbox_id}")
    await _wait_for_sandbox_exec_ready(sandbox_id)
    _progress(on_progress, f"[setup] checking repository at {repo_dir}")
    await _exec(sandbox_id, f"test -d {repo_dir} && test -d {repo_dir}/.git")
    await _exec(sandbox_id, f"{_CONDA_ACTIVATE} && python --version")
    # Retry runs may reuse the same named sandbox. Always restore the repo to
    # the base commit before reapplying the SWE-EVO test patch so failed edits
    # from earlier attempts do not contaminate the next run.
    _progress(on_progress, f"[setup] resetting checkout to {instance.base_commit[:12]}")
    await _exec(sandbox_id, f"cd {repo_dir} && git reset --hard HEAD 2>/dev/null")
    await _exec(sandbox_id, f"cd {repo_dir} && git clean -fd 2>/dev/null")
    await _exec(sandbox_id, f"cd {repo_dir} && git checkout -f {instance.base_commit} 2>/dev/null")
    await _exec(sandbox_id, f"cd {repo_dir} && git checkout -B sweevo-work {instance.base_commit} 2>/dev/null")
    _progress(on_progress, "[setup] installing repository in editable mode")
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
    _progress(
        on_progress,
        f"[setup] sandbox ready sandbox_id={sandbox_id} repo={instance.repo} "
        f"base={instance.base_commit[:12]}",
    )
    return repo_dir


async def ensure_sweevo_test_patch(
    instance: SWEEvoInstance,
    sandbox_id: str,
    repo_dir: str = _REPO_DIR,
) -> None:
    """Apply the SWE-EVO test patch so the grader uses the expected test surface."""
    test_patch = instance.test_patch
    if not test_patch:
        logger.warning(
            "No test patch for %s — F2P tests may not exist",
            instance.instance_id,
        )
        return

    patch_path = "/tmp/sweevo_test.patch"
    await _upload_file_with_fallback(sandbox_id, patch_path, test_patch.encode("utf-8"))

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

    _dispose_code_intelligence_quietly(sandbox_id, "test patch")


async def create_sweevo_test_sandbox(
    instance: SWEEvoInstance,
    *,
    snapshot_name: str = "",
    sandbox_name: str = "",
    register_snapshot: bool = True,
    cpu: int = 2,
    disk: int = 10,
    repo_dir: str = _REPO_DIR,
    on_progress: ProgressCallback | None = None,
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
            _progress(
                on_progress,
                f"[setup] reusing sandbox name={resolved_name} "
                f"sandbox_id={existing.get('id', '')}",
            )
            await setup_sweevo_sandbox(
                instance,
                existing["id"],
                repo_dir,
                on_progress=on_progress,
            )
            return {
                "sandbox_id": existing["id"],
                "sandbox": existing,
                "snapshot_name": "",
                "repo_dir": repo_dir,
                "reused_existing": True,
            }
    else:
        deleted = _prune_auto_sweevo_sandboxes_for_fresh_run(service, instance)
        if deleted:
            logger.info(
                "Pruned %s stale SWE-EVO sandboxes before fresh run for %s",
                len(deleted),
                instance.instance_id,
            )

    create_kwargs: dict[str, Any] = {}
    resolved_snapshot = ""
    fallback_reason = ""
    if register_snapshot:
        if _has_explicit_sweevo_image_version(instance.docker_image):
            _progress(
                on_progress,
                f"[setup] registering snapshot "
                f"image={_normalize_sweevo_image_ref(instance.docker_image)}",
            )
            resolved_snapshot = resolve_sweevo_snapshot(
                instance,
                snapshot_name=snapshot_name,
                register_snapshot=True,
                cpu=cpu,
                disk=disk,
            )
            create_kwargs["snapshot"] = resolved_snapshot
            _progress(on_progress, f"[setup] snapshot ready name={resolved_snapshot}")
        else:
            fallback_reason = "snapshot_requires_explicit_image_version"
            logger.info(
                "Skipping SWE-EVO snapshot registration for %s because image %s "
                "has no explicit non-latest version",
                instance.instance_id,
                instance.docker_image,
            )
            create_kwargs["image"] = _normalize_sweevo_image_ref(instance.docker_image)
            _progress(
                on_progress,
                f"[setup] using image directly image={create_kwargs['image']}",
            )
    elif snapshot_name:
        resolved_snapshot = snapshot_name
        create_kwargs["snapshot"] = resolved_snapshot
        _progress(on_progress, f"[setup] using snapshot name={resolved_snapshot}")
    else:
        create_kwargs["image"] = _normalize_sweevo_image_ref(instance.docker_image)
        _progress(on_progress, f"[setup] using image directly image={create_kwargs['image']}")

    try:
        _progress(on_progress, f"[setup] creating sandbox name={resolved_name}")
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
        fresh = _find_existing_sandbox_by_name(service, resolved_name)
        _log_sandbox_creation_failure(
            service,
            sandbox_name=resolved_name,
            instance=instance,
            exc=exc,
            sandbox=fresh,
        )
        if fresh is not None and str(fresh.get("state") or "") == "started":
            logger.warning(
                "Recovered fresh SWE-EVO sandbox %s (%s) after transient create failure",
                resolved_name,
                fresh.get("id", ""),
            )
            _progress(
                on_progress,
                f"[setup] recovered sandbox name={resolved_name} "
                f"sandbox_id={fresh.get('id', '')}",
            )
            await setup_sweevo_sandbox(
                instance,
                fresh["id"],
                repo_dir,
                on_progress=on_progress,
            )
            recover_reason = "fresh_create_recovered_started_sandbox"
            if fallback_reason:
                recover_reason = f"{fallback_reason};{recover_reason}"
            return {
                "sandbox_id": fresh["id"],
                "sandbox": fresh,
                "snapshot_name": resolved_snapshot,
                "repo_dir": repo_dir,
                "reused_existing": False,
                "fallback_reason": recover_reason,
            }
        _cleanup_failed_sandbox(service, fresh)
        raise
    sandbox_id = result["id"]
    _progress(on_progress, f"[setup] created sandbox sandbox_id={sandbox_id}")
    await setup_sweevo_sandbox(instance, sandbox_id, repo_dir, on_progress=on_progress)
    sandbox_info = service.get_sandbox(sandbox_id)
    return {
        "sandbox_id": sandbox_id,
        "sandbox": sandbox_info,
        "snapshot_name": resolved_snapshot,
        "repo_dir": repo_dir,
        "reused_existing": False,
        "fallback_reason": fallback_reason,
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
