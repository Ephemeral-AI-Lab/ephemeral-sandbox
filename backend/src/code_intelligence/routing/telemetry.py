"""Status and telemetry shaping for code intelligence services."""

from __future__ import annotations

import threading
from dataclasses import dataclass
from typing import Any

from code_intelligence.types import CITelemetry


@dataclass
class OverlayCounters:
    """Per-process counters for the overlay shell backend.

    See ``docs/architecture/overlay-sandbox-plan.md`` §6. Incremented by
    :class:`OverlayAuditor` when it finishes one op; surfaced on the
    service status for the operator dashboard.
    """

    snap_build_ms: int = 0
    mount_setup_ms: int = 0
    cmd_ms: int = 0
    diff_ms: int = 0
    merge_back_ms: int = 0
    upper_bytes: int = 0
    upper_files: int = 0
    gitinclude_changes: int = 0
    gitignore_changes: int = 0
    direct_merged_bytes: int = 0
    whiteouts_gitinclude: int = 0
    whiteouts_gitignore_refused: int = 0
    dotgit_rejects: int = 0
    upper_full_failures: int = 0
    gitignore_changes_after_aborted_gitinclude: int = 0
    mixed_gitinclude_gitignore_ops: int = 0
    mixed_partial_apply_ops: int = 0
    ops_total: int = 0
    ops_rejected: int = 0


_OVERLAY_COUNTERS = OverlayCounters()
_OVERLAY_LOCK = threading.Lock()


def overlay_counters_snapshot() -> OverlayCounters:
    """Return a consistent copy of the overlay counter state."""
    with _OVERLAY_LOCK:
        return OverlayCounters(**_OVERLAY_COUNTERS.__dict__)


def record_overlay_op(**fields: int) -> None:
    """Additive increment for one overlay op's counters.

    Unknown keys are ignored so the auditor can evolve its metadata
    without tripping the telemetry recorder.
    """
    with _OVERLAY_LOCK:
        for key, value in fields.items():
            if hasattr(_OVERLAY_COUNTERS, key):
                setattr(
                    _OVERLAY_COUNTERS, key, getattr(_OVERLAY_COUNTERS, key) + int(value)
                )


def build_status(
    *,
    sandbox_id: str,
    workspace_root: str,
    initialized: bool,
    symbol_index: Any,
    arbiter: Any,
    tree_cache: Any,
    lsp_client: Any,
    rename_cache_stats: dict[str, int],
    rename_preview_fast_fallbacks: int,
) -> dict[str, Any]:
    """Return service status summary."""
    lsp = lsp_telemetry_fields(lsp_client)
    overlay = overlay_counters_snapshot()
    return {
        "sandbox_id": sandbox_id,
        "initialized": initialized,
        "workspace_root": workspace_root,
        "symbol_index": {
            "built": symbol_index.is_built,
            "files": symbol_index.indexed_files,
            "symbols": symbol_index.size,
            "generation": symbol_index.generation,
        },
        "arbiter": arbiter.status(),
        "edit_buffer": {
            "entries": arbiter.metrics.total_edits,
            "generation": arbiter.generation,
        },
        "tree_cache": tree_cache.stats,
        "rename_preview_cache": rename_cache_stats,
        "rename_preview_fast_fallbacks": rename_preview_fast_fallbacks,
        "lsp": lsp,
        "overlay": overlay.__dict__,
    }


def build_telemetry(*, symbol_index: Any, arbiter: Any, lsp_client: Any) -> CITelemetry:
    lsp = lsp_telemetry_fields(lsp_client)
    return CITelemetry(
        symbol_index_size=symbol_index.size,
        symbol_index_generation=symbol_index.generation,
        indexed_files=symbol_index.indexed_files,
        lsp_connected=lsp["connected"],
        lsp_query_count=lsp["queries"],
        lsp_cache_hits=lsp["cache_hits"],
        arbiter_active_locks=arbiter.active_lock_count,
        total_edits=arbiter.metrics.total_edits,
    )


def lsp_telemetry_fields(lsp_client: Any) -> dict[str, Any]:
    tel = lsp_client.telemetry
    return {
        "connected": lsp_client.connected,
        "queries": tel.queries,
        "successes": tel.successes,
        "errors": tel.errors,
        "cache_hits": tel.cache_hits,
        "script_runs": tel.script_runs,
        "script_successes": tel.script_successes,
        "script_errors": tel.script_errors,
    }
