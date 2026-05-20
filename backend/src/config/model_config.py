"""Active-model resolver — DB ``model_registrations`` is the sole source of truth."""

from __future__ import annotations

import logging
from typing import Any

logger = logging.getLogger(__name__)
_MODEL_ID_KEYS = ("model", "id", "model_id")


class NoActiveModelError(RuntimeError):
    """Raised when no active model registration is available."""


def _resolve_store() -> Any:
    from runtime.app_factory import ensure_runtime_stores_ready, model_store

    if store_unavailable := (model_store is None or not getattr(model_store, "is_ready", False)):
        try:
            ensure_runtime_stores_ready()
        except Exception:
            logger.debug("Failed to bootstrap runtime stores for model resolution", exc_info=True)
        if store_unavailable and getattr(model_store, "is_ready", False):
            return model_store

    return model_store


def get_active_model_kwargs() -> dict[str, Any]:
    """Return resolved active-model kwargs from the DB, with ``class_path``
    threaded in from the row's column.

    Plan §A1/A5: ``class_path`` is the dispatch discriminator used by
    :func:`providers.provider.make_api_client` and the
    ``coding_plan_mode_active`` derivation in ``task_center_runner.core.engine``.
    Both call sites read it from this dict, so we inject it from the row
    record rather than letting it stay siloed on the column.

    Raises :class:`NoActiveModelError` if the store is uninitialised or
    no active row exists.
    """
    store = _resolve_store()
    if store is None or not getattr(store, "is_ready", False):
        raise NoActiveModelError("ModelStore is not initialised")
    active = store.get_active_resolved()
    if not active:
        raise NoActiveModelError("No active model registration")
    kwargs = dict(active.get("kwargs") or {})
    class_path = active.get("class_path", "") or ""
    if class_path:
        kwargs["class_path"] = class_path
    return kwargs


def try_get_active_model_kwargs() -> dict[str, Any] | None:
    """Non-raising variant — returns ``None`` when unavailable."""
    try:
        return get_active_model_kwargs()
    except NoActiveModelError:
        return None


def get_active_model_id() -> str:
    kwargs = get_active_model_kwargs()
    model = _first_present(kwargs, _MODEL_ID_KEYS)
    if model is None or not str(model).strip():
        raise NoActiveModelError("Active model registration has no 'model' id")
    return str(model)


def _first_present(kwargs: dict[str, Any], keys: tuple[str, ...]) -> Any:
    for key in keys:
        if key in kwargs:
            return kwargs[key]
    return None


def get_active_max_tokens(default: int = 16384) -> int:
    kwargs = try_get_active_model_kwargs() or {}
    value = kwargs.get("max_tokens", default)
    try:
        return int(value)
    except (TypeError, ValueError):
        return default


def get_active_api_key() -> str:
    kwargs = get_active_model_kwargs()
    key = kwargs.get("api_key") or ""
    if not key:
        raise NoActiveModelError("Active model registration has no api_key")
    return str(key)


def get_active_base_url() -> str | None:
    kwargs = try_get_active_model_kwargs() or {}
    base = kwargs.get("base_url")
    return str(base) if base else None


__all__ = [
    "NoActiveModelError",
    "get_active_model_kwargs",
    "try_get_active_model_kwargs",
    "get_active_model_id",
    "get_active_max_tokens",
    "get_active_api_key",
    "get_active_base_url",
]
