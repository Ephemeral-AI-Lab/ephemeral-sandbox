"""Daytona SDK client lifecycle, credentials, and shared helpers."""

from __future__ import annotations

import asyncio
import concurrent.futures
import inspect
import logging
import os
import threading
import time
import weakref
from hashlib import sha256
from inspect import Parameter, signature
from pathlib import Path
from typing import Any, Literal, TypeAlias

from dotenv import dotenv_values

from sandbox._shared.async_bridge import register_standalone_loop_cleanup
from sandbox.provider.daytona.errors import (
    AsyncDaytonaUnavailableError,
    DaytonaUnavailableError,
)

logger = logging.getLogger(__name__)

DaytonaFactoryName = Literal["Daytona", "AsyncDaytona"]
DaytonaClientCacheKey: TypeAlias = tuple[DaytonaFactoryName, str, str]

APP_MANAGED_BY = "ephemeralos"
APP_CREATED_VIA = "api"
SNAPSHOT_LABEL = "ephemeralos_snapshot"
IMAGE_LABEL = "ephemeralos_image"
LIST_PAGE_LIMIT = 100
SNAPSHOT_PAGE_LIMIT = 100
MAX_PAGINATION_PAGES = 1000


def _find_project_root(start: Path) -> Path:
    for candidate in (start.parent, *start.parents):
        if (candidate / "pyproject.toml").is_file() or (candidate / ".git").exists():
            return candidate
    return start


_PROJECT_ROOT = _find_project_root(Path(__file__).resolve())
_DOTENV_PATH = _PROJECT_ROOT / ".env"


def timeout_seconds_from_env() -> float:
    raw = os.getenv("EPHEMERALOS_SANDBOX_TIMEOUT_SECONDS")
    if not raw:
        return 300.0
    try:
        value = float(raw)
    except ValueError:
        logger.warning("Invalid EPHEMERALOS_SANDBOX_TIMEOUT_SECONDS=%r; using default", raw)
        return 300.0
    return max(value, 1.0)


SANDBOX_TIMEOUT_SECONDS = timeout_seconds_from_env()
HEALTH_TIMEOUT_SECONDS = 30.0


def load_credentials() -> tuple[str, str, str]:
    dotenv_map = _load_dotenv_values()

    api_key = _credential_value("DAYTONA_API_KEY", dotenv_map)
    api_url = _credential_value("DAYTONA_API_URL", dotenv_map)
    target = _credential_value("DAYTONA_TARGET", dotenv_map)

    return api_key, api_url, target


def load_required_credentials(
    *,
    unavailable_cls: type[Exception],
    not_configured_message: str,
) -> tuple[str, str, str]:
    """Load credentials and raise the caller-specific exception if missing."""
    api_key, api_url, target = load_credentials()
    if not api_key or not api_url:
        raise unavailable_cls(not_configured_message)
    return api_key, api_url, target


def client_cache_key(
    factory_name: DaytonaFactoryName,
    *,
    api_key: str,
    api_url: str,
    target: str,
) -> DaytonaClientCacheKey:
    """Return the cache key for one Daytona SDK factory."""
    _validate_factory_name(factory_name)
    credential_hash = sha256(f"{api_key}\0{api_url}".encode()).hexdigest()
    return factory_name, credential_hash, target


def build_sdk_client(
    factory_name: DaytonaFactoryName,
    *,
    api_key: str,
    api_url: str,
    target: str,
    unavailable_cls: type[Exception],
    not_installed_message: str,
) -> Any:
    """Import the Daytona SDK factory and build a configured client."""
    _validate_factory_name(factory_name)
    try:
        import daytona_sdk
    except ImportError as exc:
        raise unavailable_cls(not_installed_message) from exc
    try:
        factory = getattr(daytona_sdk, factory_name)
        config_cls = daytona_sdk.DaytonaConfig
    except AttributeError as exc:
        raise unavailable_cls(not_installed_message) from exc
    cfg_kwargs: dict[str, str] = {"api_key": api_key, "api_url": api_url}
    if target:
        cfg_kwargs["target"] = target
    return factory(config_cls(**cfg_kwargs))


def _client_config(
    factory_name: DaytonaFactoryName,
    *,
    unavailable_cls: type[Exception],
    not_configured_message: str,
) -> tuple[DaytonaClientCacheKey, str, str, str]:
    api_key, api_url, target = load_required_credentials(
        unavailable_cls=unavailable_cls,
        not_configured_message=not_configured_message,
    )
    current_key = client_cache_key(
        factory_name,
        api_key=api_key,
        api_url=api_url,
        target=target,
    )
    return current_key, api_key, api_url, target


def _validate_factory_name(factory_name: str) -> None:
    if factory_name not in ("Daytona", "AsyncDaytona"):
        raise ValueError(f"unsupported Daytona SDK factory: {factory_name!r}")


def _credential_value(
    env_name: str,
    dotenv_map: dict[str, str],
) -> str:
    return os.environ.get(env_name, "").strip() or dotenv_map.get(env_name, "")


def _load_dotenv_values() -> dict[str, str]:
    return {
        str(key): str(value).strip()
        for key, value in dotenv_values(_DOTENV_PATH).items()
        if key and value is not None and str(value).strip()
    }


def creation_param_classes() -> tuple[Any, Any]:
    """Import and return Daytona SDK sandbox creation parameter classes."""
    try:
        from daytona_sdk import (
            CreateSandboxFromImageParams,
            CreateSandboxFromSnapshotParams,
        )
    except ImportError as exc:
        raise DaytonaUnavailableError(
            "Daytona SDK not installed. Run: pip install daytona-sdk"
        ) from exc

    return CreateSandboxFromSnapshotParams, CreateSandboxFromImageParams


def paginate_all(list_fn: Any, limit: int) -> list[Any]:
    """Exhaust a paginated Daytona SDK list method and return all items."""
    first_page = call_with_optional_timeout(
        list_fn,
        limit=limit,
        timeout=SANDBOX_TIMEOUT_SECONDS,
    )
    items = list(getattr(first_page, "items", []) or [])
    current_page = int(getattr(first_page, "page", 1) or 1)
    total_pages = int(getattr(first_page, "total_pages", 1) or 1)
    capped_total = min(total_pages, MAX_PAGINATION_PAGES)
    if total_pages > MAX_PAGINATION_PAGES:
        logger.warning(
            "Truncating Daytona pagination at %d pages (SDK reported %d)",
            MAX_PAGINATION_PAGES,
            total_pages,
        )
    for page in range(current_page + 1, capped_total + 1):
        response = call_with_optional_timeout(
            list_fn,
            page=page,
            limit=limit,
            timeout=SANDBOX_TIMEOUT_SECONDS,
        )
        items.extend(list(getattr(response, "items", []) or []))
    return items


def call_with_optional_timeout(
    fn: Any,
    *args: Any,
    timeout: float,
    **kwargs: Any,
) -> Any:
    """Call a Daytona SDK method, enforcing the caller's timeout."""
    if _accepts_timeout(fn):
        kwargs["timeout"] = timeout
        return fn(*args, **kwargs)
    executor = concurrent.futures.ThreadPoolExecutor(
        max_workers=1,
        thread_name_prefix="daytona-timeout-wrap",
    )
    try:
        future = executor.submit(fn, *args, **kwargs)
        try:
            return future.result(timeout=timeout)
        except concurrent.futures.TimeoutError as exc:
            method = getattr(fn, "__name__", "call")
            raise TimeoutError(
                f"Daytona SDK {method!r} exceeded {timeout:.1f}s"
            ) from exc
    finally:
        executor.shutdown(wait=False)


def _accepts_timeout(fn: Any) -> bool:
    try:
        params = signature(fn).parameters.values()
    except (TypeError, ValueError):
        return True
    for param in params:
        if param.kind is Parameter.VAR_KEYWORD:
            return True
        if param.name == "timeout":
            return True
    return False


_sync_client_lock = threading.Lock()
_cached_client: Any | None = None
_cached_client_key: DaytonaClientCacheKey | None = None


def get_sync_daytona_client() -> Any:
    """Return a cached Daytona client, creating one if config changed."""
    global _cached_client, _cached_client_key

    current_key, api_key, api_url, target = _client_config(
        "Daytona",
        unavailable_cls=DaytonaUnavailableError,
        not_configured_message=(
            "Daytona is not configured. Set DAYTONA_API_KEY and DAYTONA_API_URL."
        ),
    )

    stale_client: Any = None
    with _sync_client_lock:
        if _cached_client is not None and _cached_client_key == current_key:
            return _cached_client

        if _cached_client is not None:
            stale_client = _cached_client

        _cached_client = build_sdk_client(
            "Daytona",
            api_key=api_key,
            api_url=api_url,
            target=target,
            unavailable_cls=DaytonaUnavailableError,
            not_installed_message="Daytona SDK not installed. Run: pip install daytona-sdk",
        )
        _cached_client_key = current_key
        new_client = _cached_client
        logger.info("Daytona client created (api_url=%s)", api_url)

    if stale_client is not None:
        try:
            close_fn = getattr(stale_client, "close", None)
            if callable(close_fn):
                close_fn()
        except Exception:
            logger.debug("Failed to close superseded Daytona client", exc_info=True)
    return new_client


def fetch_sandbox(sandbox_id: str) -> Any:
    """Fetch a pre-created sandbox by ID."""
    client = get_sync_daytona_client()
    sandbox = call_with_optional_timeout(
        client.get,
        sandbox_id,
        timeout=SANDBOX_TIMEOUT_SECONDS,
    )
    if sandbox is None:
        raise ValueError(f"Sandbox '{sandbox_id}' not found")
    return sandbox


_async_client_lock = threading.Lock()
_cached_clients: weakref.WeakKeyDictionary[
    asyncio.AbstractEventLoop,
    tuple[DaytonaClientCacheKey, Any],
] = weakref.WeakKeyDictionary()


def get_async_daytona_client() -> Any:
    """Return a loop-local cached AsyncDaytona client."""
    loop = asyncio.get_running_loop()
    current_key, api_key, api_url, target = _client_config(
        "AsyncDaytona",
        unavailable_cls=AsyncDaytonaUnavailableError,
        not_configured_message=(
            "Async Daytona is not configured. Set DAYTONA_API_KEY and DAYTONA_API_URL."
        ),
    )
    stale_clients: list[Any] = []

    with _async_client_lock:
        for cached_loop, (_, cached_client) in list(_cached_clients.items()):
            if cached_loop.is_closed():
                stale_clients.append(cached_client)
                del _cached_clients[cached_loop]

        cached_entry = _cached_clients.get(loop)
        if cached_entry is not None:
            cached_key, cached_client = cached_entry
            if cached_key == current_key and not loop.is_closed():
                return cached_client
            stale_clients.append(cached_client)
            del _cached_clients[loop]

        client = build_sdk_client(
            "AsyncDaytona",
            api_key=api_key,
            api_url=api_url,
            target=target,
            unavailable_cls=AsyncDaytonaUnavailableError,
            not_installed_message=(
                "Async Daytona SDK is not available. Run: pip install daytona-sdk"
            ),
        )
        _cached_clients[loop] = (current_key, client)

    for stale_client in stale_clients:
        close_client(stale_client)

    logger.info("AsyncDaytona client created (api_url=%s)", api_url)
    return client


async def get_async_sandbox(sandbox_id: str) -> Any:
    """Fetch and start a pre-created sandbox by ID using async client."""
    client = get_async_daytona_client()
    sandbox = await call_with_optional_timeout(
        client.get,
        sandbox_id,
        timeout=SANDBOX_TIMEOUT_SECONDS,
    )
    if sandbox is None:
        raise ValueError(f"Sandbox '{sandbox_id}' not found")
    return sandbox


def close_client(client: Any) -> None:
    closer = _start_async_close_thread(client)
    if closer is not None:
        _join_close_threads([closer], timeout=5.0)


def _start_async_close_thread(client: Any) -> threading.Thread | None:
    if client is None:
        return None
    close_fn = getattr(client, "close", None)
    if not callable(close_fn):
        return None
    try:
        close_result = close_fn()
    except Exception:
        logger.debug("Failed to close cached AsyncDaytona client", exc_info=True)
        return None
    if not inspect.isawaitable(close_result):
        return None

    def _run_close() -> None:
        close_loop: asyncio.AbstractEventLoop | None = None
        try:
            close_loop = asyncio.new_event_loop()
            asyncio.set_event_loop(close_loop)
            close_loop.run_until_complete(close_result)
        except Exception:
            logger.debug("Failed to await AsyncDaytona close", exc_info=True)
        finally:
            if close_loop is not None:
                close_loop.close()

    closer = threading.Thread(
        target=_run_close,
        name="daytona-async-client-close",
        daemon=True,
    )
    closer.start()
    return closer


def _join_close_threads(
    closers: list[threading.Thread],
    *,
    timeout: float,
) -> None:
    deadline = time.monotonic() + timeout
    for closer in closers:
        remaining = max(0.0, deadline - time.monotonic())
        closer.join(timeout=remaining)
    for closer in closers:
        if closer.is_alive():
            logger.warning("Timed out waiting for AsyncDaytona client close")


async def async_close_client(client: Any) -> None:
    """Close an async Daytona client on the currently running event loop."""
    if client is None:
        return
    close_fn = getattr(client, "close", None)
    if not callable(close_fn):
        return
    try:
        close_result = close_fn()
        if inspect.isawaitable(close_result):
            await close_result
    except Exception:
        logger.debug("Failed to close cached AsyncDaytona client", exc_info=True)


async def shutdown_cached_client_async() -> None:
    """Close cached AsyncDaytona clients owned by the active event loop."""
    running_loop = asyncio.get_running_loop()
    active_loop_clients: list[Any] = []
    fallback_clients: list[Any] = []
    with _async_client_lock:
        for loop, (_, client) in list(_cached_clients.items()):
            del _cached_clients[loop]
            if loop is running_loop:
                active_loop_clients.append(client)
            else:
                fallback_clients.append(client)
    for client in active_loop_clients:
        await async_close_client(client)
    fallback_closers = [
        closer
        for client in fallback_clients
        if (closer := _start_async_close_thread(client)) is not None
    ]
    _join_close_threads(fallback_closers, timeout=5.0)


try:
    register_standalone_loop_cleanup(shutdown_cached_client_async)
except Exception:
    logger.debug("Failed to register Daytona async-client cleanup", exc_info=True)


__all__ = [
    "APP_CREATED_VIA",
    "APP_MANAGED_BY",
    "DaytonaClientCacheKey",
    "HEALTH_TIMEOUT_SECONDS",
    "IMAGE_LABEL",
    "LIST_PAGE_LIMIT",
    "SANDBOX_TIMEOUT_SECONDS",
    "SNAPSHOT_LABEL",
    "SNAPSHOT_PAGE_LIMIT",
    "get_sync_daytona_client",
    "async_close_client",
    "build_sdk_client",
    "call_with_optional_timeout",
    "client_cache_key",
    "close_client",
    "creation_param_classes",
    "fetch_sandbox",
    "get_async_daytona_client",
    "get_async_sandbox",
    "load_credentials",
    "load_required_credentials",
    "paginate_all",
    "shutdown_cached_client_async",
    "timeout_seconds_from_env",
]
