"""Audit-signal derivation from sandbox timing payloads.

``timing_audit_signals`` inspects a result's ``timings`` map and classifies
which lifecycle facts (OCC prepared, overlay executed, layer published, …) the
caller can claim happened. Clock primitives live in ``sandbox._shared.clock``;
this module is intentionally narrow to audit-facing concerns.
"""

from __future__ import annotations

from collections.abc import Mapping
from typing import Literal

TimingAuditSignal = Literal[
    "occ_prepared",
    "occ_committed",
    "occ_conflicted",
    "overlay_executed",
    "layer_stack_lease_acquired",
    "layer_stack_layer_published",
    "layer_stack_auto_squashed",
    "resource_snapshot",
]


def timing_audit_signals(
    timings: Mapping[str, object],
    *,
    status: object,
    payload: Mapping[str, object] | None = None,
) -> tuple[TimingAuditSignal, ...]:
    """Return audit signal names implied by sandbox timing keys."""
    if not timings:
        return ()

    emitted: list[TimingAuditSignal] = []
    if _has_any_timing(
        timings,
        (
            "occ.prepare.",
            "occ.apply.",
            "api.write.occ_apply_s",
            "api.edit.occ_apply_s",
            "command_exec.occ_apply_s",
        ),
    ):
        emitted.append("occ_prepared")
    if _has_timing(timings, "occ.") and status == "conflict":
        emitted.append("occ_conflicted")
    elif _has_any_timing(
        timings,
        (
            "occ.commit.",
            "occ.apply.",
            "api.write.occ_apply_s",
            "api.edit.occ_apply_s",
            "command_exec.occ_apply_s",
        ),
    ) and status == "ok":
        emitted.append("occ_committed")

    if _has_any_timing(timings, ("workspace.", "overlay.", "command_exec.")):
        emitted.append("overlay_executed")

    if _has_any_timing(
        timings,
        (
            "layer_stack.lease_",
            "layer_stack.transaction_lock_wait",
            "layer_stack.transaction_lock_held",
            "layer_stack.transaction.lock_wait",
            "layer_stack.transaction.lock_held",
        ),
    ):
        emitted.append("layer_stack_lease_acquired")
    if _has_any_timing(
        timings,
        (
            "layer_stack.publish",
            "layer_stack.layer_",
            "occ.commit.publish_layer",
        ),
    ):
        emitted.append("layer_stack_layer_published")
    if _has_auto_squash_fact(timings, payload or {}):
        emitted.append("layer_stack_auto_squashed")
    if _has_timing(timings, "resource."):
        emitted.append("resource_snapshot")
    return tuple(emitted)


def _has_timing(timings: Mapping[str, object], prefix: str) -> bool:
    return any(key.startswith(prefix) for key in timings)


def _has_any_timing(timings: Mapping[str, object], prefixes: tuple[str, ...]) -> bool:
    return any(_has_timing(timings, prefix) for prefix in prefixes)


def _has_auto_squash_fact(
    timings: Mapping[str, object],
    payload: Mapping[str, object],
) -> bool:
    if any("auto_squash" in key.lower() for key in timings):
        return True
    return any("auto_squash" in str(key).lower() for key in payload)


__all__ = [
    "TimingAuditSignal",
    "timing_audit_signals",
]
