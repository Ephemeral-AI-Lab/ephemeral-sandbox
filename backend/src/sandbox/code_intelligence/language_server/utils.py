"""Small helpers for language-server clients."""

from __future__ import annotations

from collections.abc import Sequence
from typing import Any

from sandbox.code_intelligence.core.types import SymbolKind


def _format_transport_exception(exc: Exception) -> str:
    detail = str(exc).strip()
    if not detail:
        detail = repr(exc)
    if detail.rstrip().endswith(":"):
        detail = f"{detail} (no additional detail from Daytona SDK)"
    return f"{detail} [exception_type={type(exc).__name__}]"


def _readiness_targets(languages: Sequence[str] | None) -> set[str]:
    if languages is None:
        return {"python"}
    return {
        str(language).strip().lower()
        for language in languages
        if str(language).strip().lower() == "python"
    }


def _coerce_symbol_kind(raw_kind: Any) -> SymbolKind:
    """Map backend-reported symbol types onto SymbolKind."""
    try:
        return SymbolKind(str(raw_kind))
    except ValueError:
        return SymbolKind.UNKNOWN
