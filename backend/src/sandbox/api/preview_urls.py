"""Public sandbox preview and observability URL verbs."""

from __future__ import annotations

from typing import Any

from sandbox.provider.registry import get_adapter


def get_signed_preview_url(sandbox_id: str, port: int) -> dict[str, Any]:
    return get_adapter(sandbox_id).get_signed_preview_url(sandbox_id, port)


def get_build_logs_url(sandbox_id: str) -> str | None:
    return get_adapter(sandbox_id).get_build_logs_url(sandbox_id)


__all__ = [
    "get_build_logs_url",
    "get_signed_preview_url",
]
