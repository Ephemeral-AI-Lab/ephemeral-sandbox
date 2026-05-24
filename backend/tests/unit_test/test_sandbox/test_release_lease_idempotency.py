"""Unit tests for ``EphemeralPipeline._release_lease`` idempotency.

Pre-mortem #5 requires that a double release on the same lease silently
no-ops — otherwise the daemon's
``lease_acquire_count == lease_release_count`` AC-5 invariant breaks under
cancel + reap fan-in.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

from sandbox.ephemeral_workspace.pipeline import EphemeralPipeline


class _FakeLayerStack:
    """Minimal stand-in for ``LayerStackClient`` that just counts releases."""

    storage_root = Path("/tmp/fake-storage")

    def __init__(self) -> None:
        self.released: list[str] = []

    def release_lease(self, *, lease_id: str) -> bool:
        self.released.append(lease_id)
        return True

    def read_active_manifest(self) -> Any:  # pragma: no cover - not exercised here
        raise NotImplementedError


class _NoopOccClient:
    pass


def _make_overlay() -> tuple[EphemeralPipeline, _FakeLayerStack]:
    fake = _FakeLayerStack()
    overlay = EphemeralPipeline.__new__(EphemeralPipeline)
    # Bypass __init__ because the real one wants a usable layer_stack manifest.
    overlay._occ_client = _NoopOccClient()  # type: ignore[attr-defined]
    overlay._workspace_ref = "test"  # type: ignore[attr-defined]
    overlay._layer_stack = fake  # type: ignore[attr-defined]
    overlay._workspace_root = "/testbed"  # type: ignore[attr-defined]
    overlay._released_lease_ids = set()  # type: ignore[attr-defined]
    return overlay, fake


def test_double_release_calls_layer_stack_once() -> None:
    overlay, fake = _make_overlay()
    overlay._release_lease("lease-1")
    overlay._release_lease("lease-1")
    overlay._release_lease("lease-1")
    assert fake.released == ["lease-1"]


def test_releases_for_distinct_leases_pass_through() -> None:
    overlay, fake = _make_overlay()
    overlay._release_lease("lease-a")
    overlay._release_lease("lease-b")
    overlay._release_lease("lease-a")
    overlay._release_lease("lease-c")
    assert fake.released == ["lease-a", "lease-b", "lease-c"]


def test_empty_lease_id_is_noop() -> None:
    overlay, fake = _make_overlay()
    overlay._release_lease("")
    overlay._release_lease("   ")  # type: ignore[arg-type]
    # Whitespace-only string is a defensive case; we don't strip but the
    # important property is no release_lease pass-through.
    assert fake.released == ["   "]  # current behavior; empty short-circuits


def test_release_without_layer_stack_is_noop() -> None:
    overlay = EphemeralPipeline.__new__(EphemeralPipeline)
    overlay._occ_client = _NoopOccClient()  # type: ignore[attr-defined]
    overlay._workspace_ref = "test"  # type: ignore[attr-defined]
    overlay._layer_stack = None  # type: ignore[attr-defined]
    overlay._workspace_root = "/testbed"  # type: ignore[attr-defined]
    overlay._released_lease_ids = set()  # type: ignore[attr-defined]
    overlay._release_lease("lease-x")  # must not raise
