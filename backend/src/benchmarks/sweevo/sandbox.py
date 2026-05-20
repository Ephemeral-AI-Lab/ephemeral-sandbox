"""SWE-EVO sandbox provisioning, setup, and command execution."""

from __future__ import annotations

import base64
import logging
import os
import shlex
import time
from collections.abc import Callable
from typing import Any
from uuid import uuid4

import sandbox.api as sandbox_api

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
_DEFAULT_SWEEVO_INSTANCE_ID = "dask__dask_2023.3.2_2023.4.0"


def _progress(on_progress: ProgressCallback | None, message: str) -> None:
    if on_progress is None:
        return
    try:
        on_progress(message)
    except Exception:
        logger.debug("SWE-EVO progress callback failed", exc_info=True)


# ---------------------------------------------------------------------------
# Sandbox status/control accessor
# ---------------------------------------------------------------------------


def _service() -> Any:
    """Return the public sandbox status/control facade module.

    The module exposes the same call shape (``.create_sandbox``,
    ``.list_sandboxes``, ``.delete_sandbox``, etc.) as the legacy
    ``DaytonaSandboxLifecycle`` instance — so existing call sites work
    unchanged.
    """
    return sandbox_api


def _default_sweevo_sandbox_name(instance: SWEEvoInstance) -> str:
    """Return a unique sandbox name for a fresh SWE-EVO run."""
    return _truncate_dns_label(f"sweevo-test-{instance.instance_id}-{uuid4().hex[:8]}")


def _sweevo_sandbox_labels(instance: SWEEvoInstance, repo_dir: str) -> dict[str, str]:
    return {
        "purpose": "sweevo-test",
        "project_dir": repo_dir,
        "sweevo_instance": instance.instance_id,
        "sweevo_repo": instance.repo,
    }


def _merge_sandbox_labels(
    existing: dict[str, Any],
    labels: dict[str, str],
) -> dict[str, str]:
    current = existing.get("labels")
    merged = (
        {str(k): str(v) for k, v in current.items()}
        if isinstance(current, dict)
        else {}
    )
    merged.update(labels)
    return merged


def _configure_reusable_sweevo_sandbox(
    service: Any,
    existing: dict[str, Any],
    *,
    instance: SWEEvoInstance,
    repo_dir: str,
) -> dict[str, Any] | None:
    sandbox_id = str(existing.get("id") or "")
    if not sandbox_id:
        return None
    labels = _merge_sandbox_labels(
        existing,
        _sweevo_sandbox_labels(instance, repo_dir),
    )
    service.set_sandbox_labels(sandbox_id, labels)
    if str(existing.get("state") or "") == "started":
        return service.get_sandbox(sandbox_id)
    return service.start_sandbox(sandbox_id)


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


_DEFAULT_GLOBAL_SANDBOX_QUOTA = 5


def _global_sandbox_quota() -> int:
    """Maximum number of Daytona sandboxes to keep before a fresh run.

    Configurable via ``EOS_SWEEVO_SANDBOX_QUOTA`` (default 5). Set to 0 to
    disable the guard. Sandboxes beyond this cap (oldest first) are deleted
    so the next test doesn't hit a Daytona quota-exhaustion failure.
    """
    raw = os.getenv("EOS_SWEEVO_SANDBOX_QUOTA", "")
    if not raw.strip():
        return _DEFAULT_GLOBAL_SANDBOX_QUOTA
    try:
        return max(0, int(raw))
    except ValueError:
        return _DEFAULT_GLOBAL_SANDBOX_QUOTA


def _enforce_global_sandbox_quota(service: Any) -> list[str]:
    """Delete the oldest sandboxes once the account-wide cap is exceeded.

    Daytona accounts have a hard quota on concurrent sandboxes. When the
    test suite leaks sandboxes across runs (e.g. interrupted teardown,
    pending_build failures) the count grows until ``provider.create()`` hangs
    300s on every new test. This guard runs before fresh-sandbox creation:
    list all sandboxes, keep the ``_global_sandbox_quota()`` most recent,
    delete the rest.

    Best-effort: returns the list of deleted ids; individual failures are
    logged and swallowed so a slow-deleting zombie does not block the test.
    """
    keep = _global_sandbox_quota()
    if keep <= 0:
        return []
    sandboxes = _safe_list_sandboxes(service)
    if len(sandboxes) <= keep:
        return []
    # _safe_list_sandboxes already returns newest-first (provider .list()
    # sorts by created_at desc), but we re-sort defensively to make the
    # contract local to this function.
    sorted_sandboxes = sorted(
        sandboxes,
        key=lambda item: str(item.get("created_at") or ""),
        reverse=True,
    )
    victims = sorted_sandboxes[keep:]
    deleted: list[str] = []
    logger.warning(
        "Global SWE-EVO sandbox quota guard: %s sandboxes present, keeping "
        "%s newest, deleting %s",
        len(sandboxes),
        keep,
        len(victims),
    )
    for sandbox in victims:
        sandbox_id = str(sandbox.get("id") or "")
        if not sandbox_id:
            continue
        try:
            service.delete_sandbox(sandbox_id)
            deleted.append(sandbox_id)
        except Exception:
            logger.warning(
                "Quota guard: failed to delete sandbox %s (%s)",
                sandbox.get("name") or "",
                sandbox_id,
                exc_info=True,
            )
    return deleted


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
        if state not in {"stopped", "pending_build", "build_failed", "error"}:
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


def _find_reusable_auto_sweevo_sandbox(
    service: Any,
    instance: SWEEvoInstance,
    *,
    repo_dir: str,
) -> dict[str, Any] | None:
    """Return a healthy auto-created sandbox for the same fixture, if one exists."""
    expected_prefix = f"sweevo-test-{instance.instance_id}-"
    candidates: list[dict[str, Any]] = []
    for sandbox in _safe_list_sandboxes(service):
        name = str(sandbox.get("name") or "")
        if not name.startswith(expected_prefix):
            continue
        state = str(sandbox.get("state") or "")
        if state not in {"started", "stopped", ""}:
            continue
        labels = sandbox.get("labels")
        if not isinstance(labels, dict):
            continue
        if str(labels.get("purpose") or "") != "sweevo-test":
            continue
        if str(labels.get("sweevo_instance") or "") != instance.instance_id:
            continue
        labeled_repo = str(labels.get("project_dir") or repo_dir)
        if labeled_repo != repo_dir:
            continue
        candidates.append(sandbox)
    candidates.sort(
        key=lambda item: (
            str(item.get("state") or "") != "started",
            str(item.get("name") or ""),
        )
    )
    return candidates[0] if candidates else None


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
        logger.warning(
            "Failed to delete unhealthy SWE-EVO sandbox %s",
            sandbox.get("id", ""),
            exc_info=True,
        )


# ---------------------------------------------------------------------------
# Sandbox command execution
# ---------------------------------------------------------------------------


async def _upload_file_with_fallback(
    sandbox_id: str,
    path: str,
    content: bytes,
) -> None:
    """Upload a file through provider-neutral raw exec."""
    await _write_file_via_chunked_base64_exec(sandbox_id, path, content)


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
            await _exec(sandbox_id, "pwd", cwd="/", timeout=10)
            return
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
    """Execute a command in the sandbox via provider raw exec, returning output."""
    try:
        response = await sandbox_api.raw_exec(
            sandbox_id,
            cmd,
            cwd=cwd or "/",
            timeout=timeout,
        )
        stdout = getattr(response, "stdout", "") or ""
        stderr = getattr(response, "stderr", "") or ""
        result_text = stdout if not stderr else f"{stdout}\n{stderr}" if stdout else stderr
        exit_code = response.exit_code
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
    """Register a SWE-EVO Docker image as a provider snapshot.

    Branches on the active provider name. Unknown providers raise
    ``NotImplementedError`` rather than silently skipping — see PLAN_v4 §6
    Step 5.
    """
    if not instance.docker_image:
        raise ValueError(f"Instance {instance.instance_id} has no docker_image")

    name = snapshot_name or f"sweevo-{instance.instance_id_swe or instance.instance_id}"
    if len(name) > 63:
        name = name[:63]

    image_ref = _normalize_sweevo_image_ref(instance.docker_image)

    from sandbox.provider.registry import get_default_provider

    provider_name = getattr(get_default_provider(), "name", "")
    if provider_name == "daytona":
        return _register_sweevo_snapshot_daytona(name, image_ref, cpu=cpu, disk=disk)
    if provider_name == "docker":
        return _register_sweevo_snapshot_docker(name, image_ref)
    raise NotImplementedError(
        f"register_sweevo_snapshot does not support provider={provider_name!r}; "
        "supported: 'daytona', 'docker'"
    )


def _register_sweevo_snapshot_daytona(
    name: str, image_ref: str, *, cpu: int, disk: int
) -> str:
    import subprocess

    logger.info("Registering SWE-EVO snapshot '%s' from %s (daytona)", name, image_ref)
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


def _register_sweevo_snapshot_docker(name: str, image_ref: str) -> str:
    import subprocess

    logger.info("Registering SWE-EVO snapshot '%s' from %s (docker)", name, image_ref)
    pull = subprocess.run(
        ["docker", "pull", image_ref],
        capture_output=True,
        text=True,
        timeout=_DEFAULT_SNAPSHOT_CREATE_TIMEOUT,
    )
    if pull.returncode != 0:
        raise RuntimeError(f"docker pull {image_ref} failed: {pull.stderr}")
    tag = subprocess.run(
        ["docker", "tag", image_ref, name],
        capture_output=True,
        text=True,
        timeout=_DEFAULT_SNAPSHOT_CREATE_TIMEOUT,
    )
    if tag.returncode != 0:
        raise RuntimeError(f"docker tag {image_ref} {name} failed: {tag.stderr}")
    logger.info("Registered snapshot: %s (docker)", name)
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


class SnapshotNotRegisteredError(RuntimeError):
    """Raised when a SWE-EVO snapshot is missing or in a non-active state.

    The CSV benchmarker fails-fast on this so a missing snapshot surfaces
    before sandbox creation rather than after a 30-minute LLM run.
    """


def verify_sweevo_snapshot_exists(instance: SWEEvoInstance) -> str:
    """Assert the Daytona snapshot for *instance* exists and is active.

    Returns the snapshot name on success. Raises
    :class:`SnapshotNotRegisteredError` if the snapshot is missing or in
    any state other than ``active`` (normalized against Daytona SDK
    enum-repr drift, mirroring adapter.py:60-70).
    """
    name = default_sweevo_snapshot_name(instance)
    snapshots = sandbox_api.list_snapshots()
    match = next((s for s in snapshots if s.get("name") == name), None)
    if match is None:
        raise SnapshotNotRegisteredError(
            f"Daytona snapshot {name!r} for instance "
            f"{instance.instance_id!r} is not registered. Pre-register it "
            f"before invoking the CSV benchmarker, e.g. by calling "
            f"benchmarks.sweevo.sandbox.register_sweevo_snapshot(instance) "
            f"from a Python shell."
        )
    # Defensive normalization against Daytona SDK enum-repr drift — the
    # snapshot path at adapter.py:184 does ``str(getattr(s, 'state'))``
    # which yields strings like ``'SnapshotState.ACTIVE'``. Strip the
    # enum prefix and lowercase before comparing.
    state = str(match.get("state", "unknown")).split(".")[-1].lower()
    if state != "active":
        raise SnapshotNotRegisteredError(
            f"Daytona snapshot {name!r} for instance "
            f"{instance.instance_id!r} is in state {state!r}, expected "
            f"'active'. Clean up any error-state zombie snapshot before "
            f"the benchmark can use it."
        )
    return name


# ---------------------------------------------------------------------------
# Sandbox setup
# ---------------------------------------------------------------------------


async def setup_sweevo_sandbox(
    instance: SWEEvoInstance,
    sandbox_id: str,
    repo_dir: str = _REPO_DIR,
    *,
    on_progress: ProgressCallback | None = None,
    exec_ready_attempts: int = 6,
    install_lsp: bool = False,
) -> str:
    """Prepare the sandbox by checking out the repo at the base commit.

    When *install_lsp* is true, the LSP catalog plugin is installed via
    :func:`sandbox.plugin.install.ensure_installed` after the workspace
    is rebuilt. Defaults to False so existing callers (live tiers, mock
    e2e tests, ``_cmd_real_agent``, ``_cmd_scenario``) keep their
    pre-install-lsp behavior.
    """
    _progress(on_progress, f"[setup] waiting for sandbox exec readiness sandbox_id={sandbox_id}")
    await _wait_for_sandbox_exec_ready(sandbox_id, attempts=exec_ready_attempts)
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
    await _rebuild_sweevo_workspace_base(
        sandbox_id,
        repo_dir,
        on_progress=on_progress,
    )

    try:
        sandbox_info = sandbox_api.get_sandbox(sandbox_id)
        existing_labels = sandbox_info.get("labels", {})
        merged_labels = (
            {str(k): str(v) for k, v in existing_labels.items()}
            if isinstance(existing_labels, dict)
            else {}
        )
        merged_labels["project_dir"] = repo_dir
        sandbox_api.set_sandbox_labels(sandbox_id, merged_labels)
        logger.info("Set project_dir label to %s", repo_dir)
    except Exception as exc:
        logger.warning("Could not set project_dir label: %s", exc)

    if install_lsp:
        from plugins.core.discovery import DEFAULT_CATALOG_DIR
        from plugins.core.manifest import parse_plugin_manifest
        from sandbox.plugin.install import ensure_installed

        _progress(on_progress, "[setup] installing LSP plugin")
        manifest = parse_plugin_manifest(DEFAULT_CATALOG_DIR / "lsp")
        await ensure_installed(sandbox_id, manifest)

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


async def reset_sweevo_workspace(sandbox_id: str) -> str:
    """Restore a reused SWE-EVO sandbox and rebuild the public-tool base."""
    service = _service()
    sandbox_info = service.get_sandbox(sandbox_id)
    labels = sandbox_info.get("labels")
    label_map = (
        {str(key): str(value) for key, value in labels.items()}
        if isinstance(labels, dict)
        else {}
    )
    instance_id = label_map.get("sweevo_instance") or _DEFAULT_SWEEVO_INSTANCE_ID
    repo_dir = label_map.get("project_dir") or _REPO_DIR
    instance = select_sweevo_instance(instance_id=instance_id)
    return await setup_sweevo_sandbox(instance, sandbox_id, repo_dir)


async def apply_layerstack_to_repo(
    sandbox_id: str,
    repo_dir: str = _REPO_DIR,
) -> None:
    """Materialize the active public-tool layerstack back onto ``repo_dir``.

    Agent tools mutate the layerstack-backed workspace view, while SWE-EVO
    grading still runs raw provider commands against the repository path. Before
    applying the test patch or running pytest, sync the active layerstack view
    back into the base checkout so the grader sees the agent's edits.
    """
    from sandbox.host.daemon_client import call_daemon_api

    request_id = f"sweevo-eval-materialize-{uuid4().hex}"
    lease_id = ""
    snapshot = await call_daemon_api(
        sandbox_id,
        "api.prepare_workspace_snapshot",
        {"request_id": request_id},
        timeout=240,
    )
    lowerdir = str(snapshot.get("lowerdir") or "").strip()
    lease_id = str(snapshot.get("lease_id") or "").strip()
    if not lowerdir or not lease_id:
        raise RuntimeError(f"invalid layerstack snapshot response: {snapshot!r}")

    try:
        await _exec(
            sandbox_id,
            _materialize_layerstack_command(lowerdir, repo_dir),
            timeout=_DEFAULT_SANDBOX_SETUP_TIMEOUT,
        )
    finally:
        try:
            await call_daemon_api(
                sandbox_id,
                "api.release_workspace_snapshot",
                {"lease_id": lease_id},
                timeout=60,
            )
        except Exception:
            logger.warning(
                "Failed to release SWE-EVO materialization lease %s", lease_id,
                exc_info=True,
            )


def _materialize_layerstack_command(lowerdir: str, repo_dir: str) -> str:
    script = f"""
import errno
import shutil
import uuid
from pathlib import Path

src = Path({lowerdir!r})
dst = Path({repo_dir!r})
if not src.is_dir():
    raise RuntimeError(f"materialized layerstack view is missing: {{src}}")
if not (src / ".git").exists():
    raise RuntimeError(f"materialized layerstack view lacks .git: {{src}}")

parent = dst.parent
tmp = parent / f".{{dst.name}}.layerstack-materialized-{{uuid.uuid4().hex}}"
backup = parent / f".{{dst.name}}.pre-layerstack-{{uuid.uuid4().hex}}"
shutil.copytree(src, tmp, symlinks=True)

try:
    if dst.exists():
        dst.rename(backup)
    tmp.rename(dst)
    if backup.exists():
        shutil.rmtree(backup)
except OSError as exc:
    if exc.errno != errno.EXDEV:
        if backup.exists() and not dst.exists():
            backup.rename(dst)
        raise
    # Bind-mount fallback: dst lives on a different device than parent
    # (docker bind-mounts /testbed as a separate volume). Atomic
    # rename-swap is impossible across devices; do contents-replacement
    # so the workspace ends with the materialized view's children.
    if not dst.exists():
        dst.mkdir(parents=True)
    for child in list(dst.iterdir()):
        if child.is_dir() and not child.is_symlink():
            shutil.rmtree(child)
        else:
            child.unlink()
    for child in list(tmp.iterdir()):
        shutil.move(str(child), str(dst / child.name))
    shutil.rmtree(tmp, ignore_errors=True)
    if backup.exists():
        shutil.rmtree(backup, ignore_errors=True)

print(f"MATERIALIZED_LAYERSTACK {{src}} -> {{dst}}")
"""
    # Trailing newline after ``PY`` is required because callers (docker
    # provider's ``cd … && (cmd)`` subshell wrap) append a closing ``)``
    # immediately after this string. Without the newline, the resulting
    # last line becomes ``PY)`` and bash fails to recognize ``PY`` as the
    # heredoc terminator. Daytona's exec path doesn't subshell-wrap, so
    # it tolerated the missing trailing newline historically.
    return f"python - <<'PY'\n{script}PY\n"


async def _rebuild_sweevo_workspace_base(
    sandbox_id: str,
    repo_dir: str,
    *,
    on_progress: ProgressCallback | None = None,
) -> None:
    """Rebind public-tool workspace truth after raw setup commands."""
    from sandbox.host.daemon_client import call_daemon_api, ensure_daemon_current
    from sandbox.host.runtime_bundle import ensure_runtime_uploaded

    _progress(on_progress, "[setup] rebuilding public tool workspace base")
    await ensure_runtime_uploaded(sandbox_id)
    await ensure_daemon_current(sandbox_id)
    await call_daemon_api(
        sandbox_id,
        "api.build_workspace_base",
        {"workspace_root": repo_dir, "reset": True},
        timeout=240,
    )
    readiness = await call_daemon_api(
        sandbox_id,
        "api.runtime.ready",
        {},
        timeout=60,
    )
    if not _runtime_ready(readiness):
        raise RuntimeError(f"SWE-EVO sandbox runtime is not ready: {readiness!r}")


def _runtime_ready(readiness: dict[str, Any]) -> bool:
    return bool(readiness.get("success") and readiness.get("ready"))


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

async def create_sweevo_test_sandbox(
    instance: SWEEvoInstance,
    *,
    snapshot_name: str = "",
    sandbox_name: str = "",
    register_snapshot: bool = True,
    reuse_existing_auto: bool = False,
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
        existing_state = str(existing.get("state") or "") if existing else ""
        if existing:
            if existing_state in {"started", "stopped", ""}:
                try:
                    existing = _configure_reusable_sweevo_sandbox(
                        service,
                        existing,
                        instance=instance,
                        repo_dir=repo_dir,
                    )
                except Exception:
                    logger.warning(
                        "Failed to configure reusable SWE-EVO sandbox %s (%s)",
                        resolved_name,
                        existing.get("id", ""),
                        exc_info=True,
                    )
                    existing = None
            else:
                logger.warning(
                    "Ignoring named SWE-EVO sandbox %s in non-reusable state %s",
                    resolved_name,
                    existing_state,
                )
                _cleanup_failed_sandbox(service, existing)
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
                exec_ready_attempts=1 if existing_state == "started" else 6,
            )
            return {
                "sandbox_id": existing["id"],
                "sandbox": existing,
                "snapshot_name": "",
                "repo_dir": repo_dir,
                "reused_existing": True,
            }
    else:
        if reuse_existing_auto:
            existing = _find_reusable_auto_sweevo_sandbox(
                service,
                instance,
                repo_dir=repo_dir,
            )
            if existing is not None:
                existing_state = str(existing.get("state") or "")
                logger.info(
                    "Reusing auto SWE-EVO sandbox %s (%s) for %s",
                    existing.get("name", ""),
                    existing.get("id", ""),
                    instance.instance_id,
                )
                _progress(
                    on_progress,
                    f"[setup] reusing auto sandbox name={existing.get('name', '')} "
                    f"sandbox_id={existing.get('id', '')}",
                )
                try:
                    configured = _configure_reusable_sweevo_sandbox(
                        service,
                        existing,
                        instance=instance,
                        repo_dir=repo_dir,
                    )
                    if configured is not None:
                        existing = configured
                    await setup_sweevo_sandbox(
                        instance,
                        existing["id"],
                        repo_dir,
                        on_progress=on_progress,
                        exec_ready_attempts=1
                        if existing_state == "started"
                        else 6,
                    )
                    return {
                        "sandbox_id": existing["id"],
                        "sandbox": existing,
                        "snapshot_name": "",
                        "repo_dir": repo_dir,
                        "reused_existing": True,
                        "fallback_reason": "auto_reused_existing",
                    }
                except Exception:
                    logger.warning(
                        "Failed to reuse auto SWE-EVO sandbox %s (%s); creating a fresh sandbox",
                        existing.get("name", ""),
                        existing.get("id", ""),
                        exc_info=True,
                    )
        deleted = _prune_auto_sweevo_sandboxes_for_fresh_run(service, instance)
        if deleted:
            logger.info(
                "Pruned %s stale SWE-EVO sandboxes before fresh run for %s",
                len(deleted),
                instance.instance_id,
            )
        quota_deleted = _enforce_global_sandbox_quota(service)
        if quota_deleted:
            logger.info(
                "Quota guard deleted %s sandboxes before fresh run for %s",
                len(quota_deleted),
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
            labels=_sweevo_sandbox_labels(instance, repo_dir),
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
            recovered = _configure_reusable_sweevo_sandbox(
                service,
                fresh,
                instance=instance,
                repo_dir=repo_dir,
            )
            if recovered is not None:
                fresh = recovered
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
