"""Daytona implementation of the provider adapter seam."""

from __future__ import annotations

import logging
from collections.abc import Awaitable, Callable
from typing import Any, ClassVar

from sandbox.shared.models import RawExecResult
from sandbox.provider.daytona.bash_command import extract_exit_code, wrap_bash_command
from sandbox.provider.daytona.client import (
    APP_CREATED_VIA,
    APP_MANAGED_BY,
    HEALTH_TIMEOUT_SECONDS,
    IMAGE_LABEL,
    LIST_PAGE_LIMIT,
    SANDBOX_TIMEOUT_SECONDS,
    SNAPSHOT_LABEL,
    SNAPSHOT_PAGE_LIMIT,
    call_with_optional_timeout,
    get_sync_daytona_client,
    creation_param_classes,
    fetch_sandbox,
    get_async_sandbox,
    load_credentials,
    paginate_all,
)
from sandbox.provider._payloads import normalize_string_dict

logger = logging.getLogger(__name__)


def _normalize_optional_text(value: str | None) -> str | None:
    if value is None:
        return None
    normalized = value.strip()
    return normalized or None


def _serialize_raw(raw: Any, *, assigned_agents: list[str] | None = None) -> dict[str, Any]:
    """Convert a Daytona SDK sandbox object into the canonical dict shape."""
    sandbox_id = getattr(raw, "id", "")
    name = getattr(raw, "name", "")
    created_at = getattr(raw, "created_at", None)

    raw_labels = getattr(raw, "labels", None) or {}
    labels = (
        {str(k): str(v) for k, v in raw_labels.items()} if isinstance(raw_labels, dict) else {}
    )

    raw_state = getattr(raw, "state", None)
    if raw_state is None:
        state = "unknown"
    else:
        s = str(getattr(raw_state, "value", raw_state)).strip()
        if not s:
            state = "unknown"
        elif s.lower().startswith("sandboxstate."):
            state = s.split(".", 1)[1].lower()
        else:
            state = s.lower()

    image: str | None = None
    for key in (SNAPSHOT_LABEL, IMAGE_LABEL):
        if labels.get(key):
            image = labels[key]
            break
    if image is None:
        for attr in ("image", "image_name", "snapshot"):
            val = _normalize_optional_text(getattr(raw, attr, None))
            if val:
                image = val
                break

    project_dir = _sandbox_project_root(raw, labels)

    return {
        "id": sandbox_id,
        "name": name,
        "state": state,
        "image": image,
        "labels": labels,
        "created_at": created_at,
        "managed_by_app": labels.get("managed_by") == APP_MANAGED_BY,
        "project_dir": project_dir,
        "assigned_agents": list(assigned_agents or []),
    }


def _sandbox_project_root(raw: Any, labels: dict[str, str]) -> str | None:
    project_dir = getattr(raw, "project_dir", None)
    if isinstance(project_dir, str) and project_dir.strip():
        return project_dir.strip()
    label_dir = labels.get("project_dir")
    if isinstance(label_dir, str) and label_dir.strip():
        return label_dir.strip()
    return None


def _refresh(raw: Any) -> Any:
    fn = getattr(raw, "refresh_data", None)
    if callable(fn):
        fn()
    return raw


class DaytonaProviderAdapter:
    """Provider adapter backed directly by the AsyncDaytona SDK."""

    name: ClassVar[str] = "daytona"

    def __init__(
        self,
        *,
        sandbox_resolver: Callable[[str], Awaitable[Any]] | None = None,
    ) -> None:
        self._resolver = sandbox_resolver or get_async_sandbox

    # -- Health / discovery ---------------------------------------------------

    def get_health(self) -> dict[str, Any]:
        api_key, api_url, target = load_credentials()
        if not api_key or not api_url:
            return {
                "configured": False,
                "available": False,
                "api_url": api_url or None,
                "target": target or None,
                "detail": "Set DAYTONA_API_KEY and DAYTONA_API_URL to connect.",
                "default_image": None,
            }
        try:
            client = get_sync_daytona_client()
            call_with_optional_timeout(
                client.list,
                limit=1,
                timeout=HEALTH_TIMEOUT_SECONDS,
            )
            return {
                "configured": True,
                "available": True,
                "api_url": api_url,
                "target": target or None,
                "detail": None,
                "default_image": None,
            }
        except Exception as exc:
            # IN-02: log full SDK error server-side; return generic detail
            # (SDK exceptions can leak URLs/request IDs/response bodies).
            logger.warning(
                "Daytona health probe failed: %s", exc, exc_info=True
            )
            return {
                "configured": True,
                "available": False,
                "api_url": api_url,
                "target": target or None,
                "detail": "provider unavailable",
                "default_image": None,
            }

    def list_snapshots(self) -> list[dict[str, Any]]:
        client = get_sync_daytona_client()
        snapshot_api = getattr(client, "snapshot", None)
        if snapshot_api and hasattr(snapshot_api, "list"):
            items = paginate_all(snapshot_api.list, SNAPSHOT_PAGE_LIMIT)
        elif hasattr(client, "list_snapshots"):
            items = paginate_all(client.list_snapshots, SNAPSHOT_PAGE_LIMIT)
        else:
            logger.warning("Daytona client has no snapshot listing API")
            return []
        return [
            {
                "name": getattr(s, "name", ""),
                "state": str(getattr(s, "state", "unknown")),
                "image_name": getattr(s, "image_name", None),
            }
            for s in items
        ]

    # -- Container CRUD -------------------------------------------------------

    def create(
        self,
        *,
        name: str,
        snapshot: str | None = None,
        image: str | None = None,
        language: str = "python",
        env_vars: dict[str, str] | None = None,
        labels: dict[str, str] | None = None,
    ) -> dict[str, Any]:
        normalized_name = _normalize_optional_text(name)
        normalized_snapshot = _normalize_optional_text(snapshot)
        normalized_image = _normalize_optional_text(image)
        if not normalized_name:
            raise ValueError("Sandbox name is required")
        if normalized_snapshot and normalized_image:
            raise ValueError("Pass either snapshot or image, not both.")

        clean_env = normalize_string_dict(env_vars)
        clean_labels = normalize_string_dict(labels)
        clean_labels["managed_by"] = APP_MANAGED_BY
        clean_labels["created_via"] = APP_CREATED_VIA
        if normalized_snapshot:
            clean_labels[SNAPSHOT_LABEL] = normalized_snapshot
        if normalized_image:
            clean_labels[IMAGE_LABEL] = normalized_image

        client = get_sync_daytona_client()
        CreateSandboxFromSnapshotParams, CreateSandboxFromImageParams = (
            creation_param_classes()
        )

        if normalized_image:
            params = CreateSandboxFromImageParams(
                name=normalized_name,
                image=normalized_image,
                language=language,
                auto_stop_interval=0,
                env_vars=clean_env or None,
                labels=clean_labels,
                ephemeral=False,
            )
        else:
            params = CreateSandboxFromSnapshotParams(
                name=normalized_name,
                snapshot=normalized_snapshot,
                language=language,
                auto_stop_interval=0,
                env_vars=clean_env or None,
                labels=clean_labels,
                ephemeral=False,
            )

        logger.info("create_sandbox(%s): Daytona create starting", normalized_name)
        raw = client.create(params, timeout=SANDBOX_TIMEOUT_SECONDS)
        logger.info("create_sandbox(%s): Daytona create returned", normalized_name)
        _refresh(raw)
        return _serialize_raw(raw, assigned_agents=[])

    def get(self, sandbox_id: str) -> dict[str, Any]:
        return _serialize_raw(fetch_sandbox(sandbox_id))

    def list(self) -> list[dict[str, Any]]:
        client = get_sync_daytona_client()
        items = paginate_all(client.list, LIST_PAGE_LIMIT)
        sandboxes = [_serialize_raw(sb) for sb in items]
        sandboxes.sort(key=lambda item: item.get("created_at") or "", reverse=True)
        return sandboxes

    def start(self, sandbox_id: str) -> dict[str, Any]:
        raw = fetch_sandbox(sandbox_id)
        state_attr = getattr(raw, "state", None)
        state = (
            str(getattr(state_attr, "value", state_attr) or "unknown").lower()
        )
        if not state.startswith("started"):
            raw.start(timeout=SANDBOX_TIMEOUT_SECONDS)
            _refresh(raw)
        return _serialize_raw(raw)

    def stop(self, sandbox_id: str) -> dict[str, Any]:
        raw = fetch_sandbox(sandbox_id)
        # WR-03: module-wide timeout (configurable via EPHEMERALOS_SANDBOX_TIMEOUT_SECONDS)
        # so degraded-scheduler stops don't fail at 60s while start/delete tolerate longer.
        raw.stop(timeout=SANDBOX_TIMEOUT_SECONDS)
        _refresh(raw)
        return _serialize_raw(raw)

    def delete(self, sandbox_id: str) -> None:
        raw = fetch_sandbox(sandbox_id)
        raw.delete(timeout=SANDBOX_TIMEOUT_SECONDS)
        logger.info("Sandbox deleted: %s", sandbox_id)

    def set_labels(self, sandbox_id: str, labels: dict[str, str]) -> dict[str, Any]:
        raw = fetch_sandbox(sandbox_id)
        raw.set_labels(normalize_string_dict(labels))
        _refresh(raw)
        return _serialize_raw(raw)

    # -- Preview / observability ---------------------------------------------

    def get_signed_preview_url(self, sandbox_id: str, port: int) -> dict[str, Any]:
        raw = fetch_sandbox(sandbox_id)
        try:
            result = raw.create_signed_preview_url(port)
            return {
                "url": result.url,
                "token": result.token,
                "port": result.port,
            }
        except AttributeError:
            url = raw.get_preview_url(port)
            return {"url": url, "token": "", "port": port}

    def get_build_logs_url(self, sandbox_id: str) -> str | None:
        raw = fetch_sandbox(sandbox_id)
        # Daytona SDK 0.23.x does not expose build-log URLs publicly; keep the
        # private access grep-visible so SDK upgrades fail loudly in review.
        daytona_api = getattr(raw, "_sandbox_api", None)  # noqa: SLF001
        if daytona_api is None or not hasattr(daytona_api, "get_build_logs_url"):
            return None
        try:
            result = daytona_api.get_build_logs_url(sandbox_id)
        except Exception:
            logger.debug(
                "Failed to fetch build logs URL for sandbox %s", sandbox_id, exc_info=True
            )
            return None
        url = getattr(result, "url", None)
        return str(url).strip() or None

    # -- Exec ----------------------------------------------------------------

    async def exec(
        self,
        sandbox_id: str,
        command: str,
        *,
        cwd: str | None = None,
        timeout: int | None = None,
    ) -> RawExecResult:
        sandbox = await self._resolver(sandbox_id)
        wrapped = wrap_bash_command(command, cwd=cwd)
        kwargs: dict[str, Any] = {}
        if timeout is not None:
            kwargs["timeout"] = timeout
        response = await sandbox.process.exec(wrapped, **kwargs)
        stdout, exit_code = extract_exit_code(
            getattr(response, "result", "") or "",
            fallback_exit_code=getattr(response, "exit_code", None),
        )
        return RawExecResult(
            success=exit_code == 0,
            exit_code=exit_code,
            stdout=stdout,
            stderr=str(getattr(response, "stderr", "") or ""),
        )

    # -- Hook ----------------------------------------------------------------

    def context_preparer(self, sandbox_id: str) -> Any:
        """Return the daytona-specific context preparer for *sandbox_id*."""
        from sandbox.provider.daytona.context_preparer import DaytonaContextPreparer

        return DaytonaContextPreparer(sandbox_id)


__all__ = [
    "DaytonaProviderAdapter",
]
