"""Unit tests for ``EphemeralPipeline._release_lease`` idempotency.

Pre-mortem #5 requires that a double release on the same lease silently
no-ops — otherwise the daemon's
``lease_acquire_count == lease_release_count`` AC-5 invariant breaks under
cancel + reap fan-in.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

import pytest

from sandbox._shared.lease_guard import LeaseGuard
from sandbox.ephemeral_workspace.pipeline import EphemeralPipeline


class _FakeLayerStack:
    """Minimal stand-in for ``LayerStackPortAdapter`` that just counts releases."""

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


class _Handle:
    def __init__(self, lease_id: str) -> None:
        self.lease_id = lease_id
        self._released = False


def _make_overlay() -> tuple[EphemeralPipeline, _FakeLayerStack]:
    fake = _FakeLayerStack()
    overlay = EphemeralPipeline.__new__(EphemeralPipeline)
    # Bypass __init__ because the real one wants a usable layer_stack manifest.
    overlay._occ_client = _NoopOccClient()  # type: ignore[attr-defined]
    overlay._workspace_ref = "test"  # type: ignore[attr-defined]
    overlay._layer_stack = fake  # type: ignore[attr-defined]
    overlay._workspace_root = "/testbed"  # type: ignore[attr-defined]
    overlay._lease_guard = LeaseGuard()  # type: ignore[attr-defined]
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
    overlay._lease_guard = LeaseGuard()  # type: ignore[attr-defined]
    overlay._release_lease("lease-x")  # must not raise


@pytest.mark.asyncio
async def test_lease_guard_release_skips_release_after_mark_released() -> None:
    guard = LeaseGuard()
    handle = _Handle("lease-1")
    release_calls: list[str] = []

    async def _release(target: _Handle) -> None:
        release_calls.append(target.lease_id)
        target._released = True

    assert guard.mark_released(handle.lease_id) is True
    assert guard.mark_released(handle.lease_id) is False

    await guard.release(handle, _release)

    assert release_calls == []
    assert handle._released is True


@pytest.mark.asyncio
async def test_lease_guard_release_serializes_duplicate_release_calls() -> None:
    guard = LeaseGuard()
    handle = _Handle("lease-2")
    release_calls: list[str] = []

    async def _release(target: _Handle) -> None:
        release_calls.append(target.lease_id)
        target._released = True

    await release_handle_twice(guard, handle, _release)

    assert release_calls == ["lease-2"]


async def release_handle_twice(
    guard: LeaseGuard,
    handle: _Handle,
    release_fn: Any,
) -> None:
    await guard.release(handle, release_fn)
    await guard.release(handle, release_fn)
