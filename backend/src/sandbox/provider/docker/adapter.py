"""Docker implementation of :class:`sandbox.provider.protocol.ProviderAdapter`.

All Docker SDK access is lazy via :func:`sandbox.provider.docker.client.get_docker_client`
so this module imports cleanly without the ``docker`` package installed.
"""

from __future__ import annotations

import asyncio
import logging
import os
import secrets
import shlex
from typing import Any

from sandbox._shared.models import RawExecResult
from sandbox.provider.docker.client import (
    get_async_docker_client,
    get_docker_client,
    host_config_kwargs,
)
from sandbox.provider._payloads import normalize_string_dict

logger = logging.getLogger(__name__)

APP_MANAGED_BY = "eos"
APP_CREATED_VIA = "ephemeral_os"
DAEMON_TCP_INTERNAL_PORT = 37657
DAEMON_TCP_ENABLED_LABEL = "eos.daemon.tcp.enabled"
DAEMON_TCP_PORT_LABEL = "eos.daemon.tcp.port"
DAEMON_TCP_ENV_HOST = "EOS_DAEMON_TCP_HOST"
DAEMON_TCP_ENV_PORT = "EOS_DAEMON_TCP_PORT"
DAEMON_AUTH_ENV = "EOS_DAEMON_AUTH_TOKEN"
DOCKER_INIT_ENABLED_LABEL = "eos.docker.init.enabled"


def _serialize_container(container: Any) -> dict[str, Any]:
    """Translate ``docker.models.containers.Container`` into our canonical dict."""
    attrs = getattr(container, "attrs", None) or {}
    config = attrs.get("Config") or {}
    host_config = attrs.get("HostConfig") or {}
    state = attrs.get("State") or {}
    labels = config.get("Labels") or {}

    return {
        "id": getattr(container, "id", None) or attrs.get("Id"),
        "name": (getattr(container, "name", None) or attrs.get("Name") or "").lstrip("/"),
        "image": config.get("Image"),
        "snapshot": labels.get("snapshot"),
        "status": state.get("Status") or getattr(container, "status", None),
        "labels": dict(labels),
        "project_dir": labels.get("project_dir") or config.get("WorkingDir"),
        "docker_init": host_config.get("Init"),
    }


def _container_env(container: Any) -> dict[str, str]:
    attrs = getattr(container, "attrs", None) or {}
    config = attrs.get("Config") or {}
    env: dict[str, str] = {}
    for item in config.get("Env") or []:
        if not isinstance(item, str) or "=" not in item:
            continue
        key, value = item.split("=", 1)
        env[key] = value
    return env


def _docker_daemon_tcp_enabled() -> bool:
    raw = os.environ.get("EOS_DOCKER_DAEMON_TCP", "1").strip().lower()
    return raw not in {"0", "false", "no", "off"}


def _serialize_image(image: Any) -> dict[str, Any]:
    """Translate ``docker.models.images.Image`` into a Daytona-snapshot-shaped dict."""
    tags = list(getattr(image, "tags", None) or [])
    primary = tags[0] if tags else None
    attrs = getattr(image, "attrs", None) or {}
    return {
        "name": primary,
        "image": primary,
        "id": getattr(image, "id", None) or attrs.get("Id"),
        "tags": tags,
    }


def _is_image_not_found(exc: Exception) -> bool:
    if type(exc).__name__ == "ImageNotFound":
        return True
    detail = str(getattr(exc, "explanation", "") or exc).lower()
    return "no such image" in detail or "image not found" in detail


class DockerProviderAdapter:
    """Docker SDK-backed implementation of ``ProviderAdapter``."""

    name = "docker"

    def __init__(self) -> None:
        # Client is constructed lazily so imports succeed without the docker SDK.
        self._client: Any | None = None

    # -- Client wiring -------------------------------------------------------

    def _get_client(self) -> Any:
        if self._client is None:
            self._client = get_docker_client()
        return self._client

    async def _get_async_client(self) -> Any:
        if self._client is None:
            self._client = await asyncio.to_thread(get_async_docker_client)
        return self._client

    # -- Health / discovery --------------------------------------------------

    def get_health(self) -> dict[str, Any]:
        try:
            info = self._get_client().info()
        except Exception as exc:  # pragma: no cover - depends on local daemon
            return {"provider": "docker", "healthy": False, "error": str(exc)}
        return {
            "provider": "docker",
            "healthy": True,
            "server_version": info.get("ServerVersion"),
            "containers_running": info.get("ContainersRunning"),
            "kernel_version": info.get("KernelVersion"),
            "operating_system": info.get("OperatingSystem"),
        }

    def list_snapshots(self) -> list[dict[str, Any]]:
        try:
            images = self._get_client().images.list()
        except Exception:
            logger.warning("docker.images.list() failed", exc_info=True)
            return []
        return [_serialize_image(img) for img in images]

    # -- Container CRUD ------------------------------------------------------

    def create(
        self,
        *,
        name: str,
        snapshot: str | None = None,
        image: str | None = None,
        language: str = "python",
        env_vars: dict[str, str] | None = None,
        labels: dict[str, str] | None = None,
        platform: str | None = None,
    ) -> dict[str, Any]:
        image_ref = (image or snapshot or "").strip()
        if not image_ref:
            raise ValueError("DockerProviderAdapter.create requires `image` or `snapshot`")

        client = self._get_client()

        merged_labels = {
            "managed_by": APP_MANAGED_BY,
            "created_via": APP_CREATED_VIA,
            "language": language,
        }
        if snapshot:
            merged_labels["snapshot"] = snapshot
        merged_labels.update(normalize_string_dict(labels))
        merged_labels[DOCKER_INIT_ENABLED_LABEL] = "1"

        environment = normalize_string_dict(env_vars)
        host_kwargs = host_config_kwargs()
        if _docker_daemon_tcp_enabled():
            environment.update(
                {
                    DAEMON_TCP_ENV_HOST: "0.0.0.0",
                    DAEMON_TCP_ENV_PORT: str(DAEMON_TCP_INTERNAL_PORT),
                    DAEMON_AUTH_ENV: secrets.token_urlsafe(32),
                }
            )
            merged_labels[DAEMON_TCP_ENABLED_LABEL] = "1"
            merged_labels[DAEMON_TCP_PORT_LABEL] = str(DAEMON_TCP_INTERNAL_PORT)
            host_kwargs.setdefault(
                "ports",
                {f"{DAEMON_TCP_INTERNAL_PORT}/tcp": ("127.0.0.1", None)},
            )

        create_kwargs = {
            "image": image_ref,
            "name": name,
            "command": ["sleep", "infinity"],
            "detach": True,
            "init": True,
            "tty": False,
            "environment": environment,
            "labels": merged_labels,
            **host_kwargs,
        }
        if platform:
            create_kwargs["platform"] = platform
        try:
            container = client.containers.create(**create_kwargs)
        except Exception as exc:
            if not _is_image_not_found(exc):
                raise
            logger.info("Docker image %s missing locally; pulling before create", image_ref)
            if platform:
                client.images.pull(image_ref, platform=platform)
            else:
                client.images.pull(image_ref)
            container = client.containers.create(**create_kwargs)
        container.start()
        container.reload()
        return _serialize_container(container)

    def get(self, sandbox_id: str) -> dict[str, Any]:
        container = self._get_client().containers.get(sandbox_id)
        container.reload()
        return _serialize_container(container)

    def list(self) -> list[dict[str, Any]]:
        try:
            containers = self._get_client().containers.list(
                all=True, filters={"label": f"managed_by={APP_MANAGED_BY}"}
            )
        except Exception:
            logger.warning("docker.containers.list() failed", exc_info=True)
            return []
        return [_serialize_container(c) for c in containers]

    def start(self, sandbox_id: str) -> dict[str, Any]:
        container = self._get_client().containers.get(sandbox_id)
        container.start()
        container.reload()
        return _serialize_container(container)

    def stop(self, sandbox_id: str) -> dict[str, Any]:
        container = self._get_client().containers.get(sandbox_id)
        container.stop()
        container.reload()
        return _serialize_container(container)

    def delete(self, sandbox_id: str) -> None:
        try:
            container = self._get_client().containers.get(sandbox_id)
        except Exception:
            return
        try:
            container.remove(force=True)
        except Exception:
            logger.warning(
                "docker container remove failed for %s", sandbox_id, exc_info=True
            )

    def set_labels(self, sandbox_id: str, labels: dict[str, str]) -> dict[str, Any]:
        """Update container labels.

        Docker does not support live label mutation. Recreating the container
        changes its id, but the sandbox lifecycle treats ids as stable, so this
        method preserves the existing container and logs ignored label changes.
        """
        existing = self._get_client().containers.get(sandbox_id)
        existing.reload()
        attrs = existing.attrs or {}
        config = attrs.get("Config") or {}
        current_labels = {
            str(key): str(value)
            for key, value in (config.get("Labels") or {}).items()
        }
        requested_labels = normalize_string_dict(labels)
        merged_labels = {**current_labels, **requested_labels}
        if merged_labels != current_labels:
            logger.warning(
                "Docker provider cannot mutate labels for existing container %s; "
                "ignoring label changes for keys=%s",
                sandbox_id,
                sorted(requested_labels),
            )
        return _serialize_container(existing)

    # -- Preview / observability --------------------------------------------

    def get_signed_preview_url(self, sandbox_id: str, port: int) -> dict[str, Any]:
        return {
            "url": None,
            "reason": "docker provider has no signed preview URL",
        }

    def get_build_logs_url(self, sandbox_id: str) -> str | None:
        return None

    def get_daemon_tcp_endpoint(self, sandbox_id: str) -> dict[str, Any] | None:
        """Return the host-side TCP endpoint for the resident daemon, if mapped."""
        if not _docker_daemon_tcp_enabled():
            return None
        container = self._get_client().containers.get(sandbox_id)
        container.reload()
        attrs = getattr(container, "attrs", None) or {}
        config = attrs.get("Config") or {}
        labels = config.get("Labels") or {}
        if labels.get(DAEMON_TCP_ENABLED_LABEL) != "1":
            return None

        raw_internal_port = labels.get(DAEMON_TCP_PORT_LABEL) or str(
            DAEMON_TCP_INTERNAL_PORT
        )
        try:
            internal_port = int(raw_internal_port)
        except (TypeError, ValueError):
            return None

        bindings = (
            (attrs.get("NetworkSettings") or {})
            .get("Ports", {})
            .get(f"{internal_port}/tcp")
        )
        if not bindings:
            return None
        binding = next(
            (
                item
                for item in bindings
                if isinstance(item, dict) and item.get("HostPort")
            ),
            None,
        )
        if binding is None:
            return None
        host = str(binding.get("HostIp") or "127.0.0.1")
        if host in {"0.0.0.0", "::"}:
            host = "127.0.0.1"
        try:
            host_port = int(str(binding["HostPort"]))
        except (TypeError, ValueError):
            return None
        token = _container_env(container).get(DAEMON_AUTH_ENV, "")
        return {
            "host": host,
            "port": host_port,
            "internal_port": internal_port,
            "auth_token": token,
        }

    # -- Exec ----------------------------------------------------------------

    async def exec(
        self,
        sandbox_id: str,
        command: str,
        *,
        cwd: str | None = None,
        timeout: int | None = None,
    ) -> RawExecResult:
        client = await self._get_async_client()

        def _run() -> tuple[int, bytes, bytes]:
            container = client.containers.get(sandbox_id)
            wrapped = command
            if cwd:
                # Use newlines around the subshell parens so multi-line
                # commands (e.g. heredocs) keep their terminator on its own
                # line. `(cmd)` on one line glues `)` to the last cmd line,
                # which breaks bash `<<'EOF'` heredoc terminators.
                wrapped = f"cd {shlex.quote(cwd)} && (\n{command}\n)"
            exit_code, output = container.exec_run(
                cmd=["/bin/bash", "-lc", wrapped],
                demux=True,
                tty=False,
            )
            stdout_b: bytes
            stderr_b: bytes
            if isinstance(output, tuple) and len(output) == 2:
                stdout_b = output[0] or b""
                stderr_b = output[1] or b""
            else:
                stdout_b = output if isinstance(output, (bytes, bytearray)) else b""
                stderr_b = b""
            return int(exit_code or 0), bytes(stdout_b), bytes(stderr_b)

        if timeout is not None:
            exit_code, stdout_b, stderr_b = await asyncio.wait_for(
                asyncio.to_thread(_run), timeout=timeout
            )
        else:
            exit_code, stdout_b, stderr_b = await asyncio.to_thread(_run)

        return RawExecResult(
            success=exit_code == 0,
            exit_code=exit_code,
            stdout=stdout_b.decode("utf-8", errors="replace"),
            stderr=stderr_b.decode("utf-8", errors="replace"),
        )

    # -- Upload --------------------------------------------------------------

    async def put_archive(
        self,
        sandbox_id: str,
        *,
        tar_stream: bytes,
        dest_dir: str,
    ) -> None:
        client = await self._get_async_client()

        def _run() -> None:
            container = client.containers.get(sandbox_id)
            ok = container.put_archive(path=dest_dir, data=tar_stream)
            if not ok:
                raise RuntimeError(
                    f"docker put_archive returned False "
                    f"(sandbox={sandbox_id!r}, dest_dir={dest_dir!r})"
                )

        await asyncio.to_thread(_run)

    # -- Context preparation -------------------------------------------------

    def context_preparer(self, sandbox_id: str) -> Any:
        from sandbox.provider.docker.context_preparer import DockerContextPreparer

        return DockerContextPreparer(sandbox_id)


__all__ = ["DockerProviderAdapter"]
