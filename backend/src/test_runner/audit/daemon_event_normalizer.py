"""Normalize pulled daemon audit events for the canonical JSONL sink.

This is the **only** writer of ``payload["daemon_event"]`` — the forensic raw
field, env-gated by ``EOS_AUDIT_FORENSIC_RAW_ENABLED=true`` (default off). A
CI grep test (`test_daemon_event_writer_module_boundary`) fails if any other
file outside this module references that key.

See ``docs/daemon-audit-pull-consolidation-v3/README.md#dual-write-authoritativeness``.
"""

from __future__ import annotations

import os
from collections.abc import Iterable
from typing import Any

FORENSIC_RAW_ENV = "EOS_AUDIT_FORENSIC_RAW_ENABLED"


def forensic_raw_enabled() -> bool:
    return os.environ.get(FORENSIC_RAW_ENV, "").strip().lower() == "true"


_SECTION_KEYS = frozenset(
    {
        "daemon",
        "layer_stack",
        "overlay_workspace",
        "occ",
        "isolated_workspace",
        "os_resource",
        "plugin",
        "background_tool",
        "tool_call",
    }
)


def _section_keys_of(payload: dict[str, Any]) -> list[str]:
    return [key for key in payload if key in _SECTION_KEYS]


def normalize_pulled_event(
    raw: dict[str, Any],
    *,
    boot_epoch_id: int | None = None,
    request_id: str = "",
) -> dict[str, Any]:
    """Promote subsystem sections to ``payload[<section>]``; optionally retain raw.

    The pulled event already carries the promoted sections (emitters construct
    them via dataclass helpers in :mod:`sandbox.audit.schema`). This
    function reshapes the wire format into the JSONL row schema
    (``ts``, ``event_type``, ``seq``, ``payload``) and conditionally tacks on
    ``payload["daemon_event"]`` when forensic raw is enabled.
    """
    inner = raw.get("payload") if isinstance(raw.get("payload"), dict) else {}
    event_type = str(raw.get("type") or raw.get("event_type") or "daemon.unknown")
    seq_value = raw.get("seq")
    seq = int(seq_value) if isinstance(seq_value, int) else None
    lane = str(raw.get("lane") or "")

    payload: dict[str, Any] = {}
    for key in _section_keys_of(inner):
        payload[key] = inner[key]

    if forensic_raw_enabled():
        payload["daemon_event"] = dict(raw)

    if boot_epoch_id is not None:
        payload.setdefault("daemon", {})["boot_epoch_id"] = boot_epoch_id

    row: dict[str, Any] = {
        "event_type": event_type,
        "schema": raw.get("schema") or "sandbox.daemon.audit.pull.v1",
        "lane": lane,
        "payload": payload,
    }
    if seq is not None:
        row["seq"] = seq
    if request_id:
        row["request_id"] = request_id
    return row


def collect_forensic_deltas(
    rows: Iterable[dict[str, Any]],
) -> dict[str, Any] | None:
    """Phase 3 deferral D15 — surface forensic-raw drift rows.

    Compares promoted ``payload[<section>]`` values against the
    ``payload["daemon_event"]`` forensic raw stash. Returns ``None``
    unless :func:`forensic_raw_enabled` so the happy-path JSON shape
    is unchanged. The only reader of the ``daemon_event`` key — the
    module-boundary CI lint enforces that other modules never touch it.
    """
    if not forensic_raw_enabled():
        return None
    drift_rows: list[dict[str, Any]] = []
    for row in rows:
        if not isinstance(row, dict):
            continue
        payload = row.get("payload")
        if not isinstance(payload, dict):
            continue
        raw_event = payload.get("daemon_event")
        if not isinstance(raw_event, dict):
            continue
        raw_inner = raw_event.get("payload")
        if not isinstance(raw_inner, dict):
            continue
        for key, value in payload.items():
            if key == "daemon_event" or not isinstance(value, dict):
                continue
            raw_section = raw_inner.get(key)
            if not isinstance(raw_section, dict):
                continue
            for field_name, promoted_value in value.items():
                if isinstance(promoted_value, dict):
                    continue
                raw_value = raw_section.get(field_name)
                if raw_value is not None and raw_value != promoted_value:
                    drift_rows.append(
                        {
                            "seq": row.get("seq"),
                            "key": f"{key}.{field_name}",
                            "promoted_value": promoted_value,
                            "daemon_event_value": raw_value,
                        }
                    )
    return {"rows": drift_rows}


__all__ = [
    "FORENSIC_RAW_ENV",
    "collect_forensic_deltas",
    "forensic_raw_enabled",
    "normalize_pulled_event",
]
