"""Natural layer-stack squash trigger coverage for OCC publications."""

from __future__ import annotations

import asyncio
import concurrent.futures
import threading

import pytest

from sandbox.layer_stack.manifest import LayerRef, Manifest
from sandbox.layer_stack.manager import LayerStackManager
from sandbox.occ.changeset.types import ChangesetResult, WriteChange
import sandbox.occ.service as occ_service_module
from sandbox.occ.service import OccService


class _Gitignore:
    def is_ignored(self, _path: str) -> bool:
        return False


def test_occ_publications_auto_squash_without_direct_squash_call(
    tmp_path,
    monkeypatch,
) -> None:
    monkeypatch.delenv("EOS_OCC_SQUASH_MODE", raising=False)
    monkeypatch.delenv("EOS_OCC_AUTO_SQUASH_MAX_DEPTH", raising=False)
    monkeypatch.setattr(occ_service_module, "AUTO_SQUASH_MAX_DEPTH", 4)
    stack = LayerStackManager(tmp_path / "stack")
    service = OccService(gitignore=_Gitignore(), layer_stack=stack)

    for index in range(8):
        result = asyncio.run(
            service.apply_changeset(
                [
                    WriteChange(
                        path=f"tracked/auto/{index:02d}.txt",
                        final_content=f"auto-{index:02d}\n".encode(),
                    )
                ],
                snapshot=stack.read_active_manifest(),
            )
        )
        assert isinstance(result, ChangesetResult)
        assert result.published_manifest_version is not None

    manifest = stack.read_active_manifest()
    assert manifest.depth <= 4
    assert manifest.version > 8
    for index in range(8):
        assert stack.read_text(f"tracked/auto/{index:02d}.txt") == (
            f"auto-{index:02d}\n",
            True,
        )


def test_auto_squash_preserves_active_lease_view(tmp_path, monkeypatch) -> None:
    monkeypatch.delenv("EOS_OCC_SQUASH_MODE", raising=False)
    monkeypatch.delenv("EOS_OCC_AUTO_SQUASH_MAX_DEPTH", raising=False)
    monkeypatch.setattr(occ_service_module, "AUTO_SQUASH_MAX_DEPTH", 3)
    stack = LayerStackManager(tmp_path / "stack")
    service = OccService(gitignore=_Gitignore(), layer_stack=stack)

    seed = asyncio.run(
        service.apply_changeset(
            [WriteChange(path="tracked/value.txt", final_content=b"base\n")],
            snapshot=stack.read_active_manifest(),
        )
    )
    assert isinstance(seed, ChangesetResult)
    assert seed.published_manifest_version is not None

    lease = stack.acquire_snapshot_lease("held-before-auto-squash")
    try:
        for index in range(6):
            result = asyncio.run(
                service.apply_changeset(
                    [
                        WriteChange(
                            path=f"tracked/burst/{index:02d}.txt",
                            final_content=f"burst-{index:02d}\n".encode(),
                        )
                    ],
                    snapshot=stack.read_active_manifest(),
                )
            )
            assert isinstance(result, ChangesetResult)
            assert result.published_manifest_version is not None

        assert stack.read_active_manifest().depth <= 3
        assert stack.read_text("tracked/value.txt", lease.manifest) == (
            "base\n",
            True,
        )
        assert stack.read_text("tracked/burst/05.txt") == ("burst-05\n", True)
    finally:
        assert stack.release_lease(lease.lease_id) is True

    assert stack.active_lease_count() == 0


def test_auto_squash_depth_is_env_configurable(tmp_path, monkeypatch) -> None:
    monkeypatch.delenv("EOS_OCC_SQUASH_MODE", raising=False)
    monkeypatch.setenv("EOS_OCC_AUTO_SQUASH_MAX_DEPTH", "6")
    stack = LayerStackManager(tmp_path / "stack")
    service = OccService(gitignore=_Gitignore(), layer_stack=stack)
    observed_max_depths: list[float] = []

    for index in range(10):
        result = asyncio.run(
            service.apply_changeset(
                [
                    WriteChange(
                        path=f"tracked/env-depth/{index:02d}.txt",
                        final_content=f"env-depth-{index:02d}\n".encode(),
                    )
                ],
                snapshot=stack.read_active_manifest(),
            )
        )
        max_depth = result.timings.get("layer_stack.auto_squash.max_depth")
        if max_depth is not None:
            observed_max_depths.append(max_depth)

    assert observed_max_depths
    assert set(observed_max_depths) == {6.0}
    assert stack.read_active_manifest().depth <= 6


def test_coalesced_mode_skips_in_flight_squash_and_rechecks(monkeypatch) -> None:
    monkeypatch.setenv("EOS_OCC_SQUASH_MODE", "coalesced")
    monkeypatch.delenv("EOS_OCC_AUTO_SQUASH_MAX_DEPTH", raising=False)
    monkeypatch.setattr(occ_service_module, "AUTO_SQUASH_MAX_DEPTH", 4)
    stack = _AutoSquashOnlyLayerStack(
        depth=7,
        depth_after_first_squash=6,
        depth_after_later_squash=1,
    )
    service = OccService(gitignore=_Gitignore(), layer_stack=stack)
    published = ChangesetResult(files=(), published_manifest_version=1)

    with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
        owner = pool.submit(service._auto_squash_after_publish_sync, published)
        assert stack.squash_entered.wait(timeout=2)
        skipped = pool.submit(service._auto_squash_after_publish_sync, published)
        skipped_timings = skipped.result(timeout=2)
        stack.release_squash.set()
        owner_timings = owner.result(timeout=2)

    assert stack.squash_calls == 2
    assert skipped_timings["layer_stack.auto_squash.skipped_in_flight"] == 1.0
    assert owner_timings["layer_stack.auto_squash.recheck_triggered"] == 1.0
    assert owner_timings["layer_stack.auto_squash.depth_before"] == 7.0
    assert owner_timings["layer_stack.auto_squash.depth_after"] == 1.0


@pytest.mark.asyncio
async def test_async_mode_drains_pending_auto_squash(tmp_path, monkeypatch) -> None:
    monkeypatch.setenv("EOS_OCC_SQUASH_MODE", "async")
    monkeypatch.delenv("EOS_OCC_AUTO_SQUASH_MAX_DEPTH", raising=False)
    monkeypatch.setattr(occ_service_module, "AUTO_SQUASH_MAX_DEPTH", 3)
    stack = LayerStackManager(tmp_path / "stack")
    service = OccService(gitignore=_Gitignore(), layer_stack=stack)
    enqueue_count = 0

    for index in range(8):
        result = await service.apply_changeset(
            [
                WriteChange(
                    path=f"tracked/async/{index:02d}.txt",
                    final_content=f"async-{index:02d}\n".encode(),
                )
            ],
            snapshot=stack.read_active_manifest(),
        )
        enqueue_count += int(
            "layer_stack.auto_squash.enqueued" in result.timings
        )

    status = await service.drain_auto_squash_maintenance(timeout_s=2)

    assert enqueue_count >= 1
    assert status["drain_timed_out"] is False
    assert status["queue_depth"] == 0
    assert status["maintenance_errors"] == 0
    assert stack.read_active_manifest().depth <= 3


@pytest.mark.asyncio
async def test_async_mode_records_squash_failure_without_leaking(
    monkeypatch,
) -> None:
    monkeypatch.setenv("EOS_OCC_SQUASH_MODE", "async")
    monkeypatch.delenv("EOS_OCC_AUTO_SQUASH_MAX_DEPTH", raising=False)
    monkeypatch.setattr(occ_service_module, "AUTO_SQUASH_MAX_DEPTH", 2)
    stack = _AutoSquashOnlyLayerStack(depth=5, fail=True)
    service = OccService(gitignore=_Gitignore(), layer_stack=stack)
    published = ChangesetResult(files=(), published_manifest_version=1)

    timings = await service._auto_squash_after_publish(published)
    status = await service.drain_auto_squash_maintenance(timeout_s=2)

    assert timings["layer_stack.auto_squash.enqueued"] == 1.0
    assert status["drain_timed_out"] is False
    assert status["queue_depth"] == 0
    assert status["maintenance_errors"] == 1
    last_error = status["last_maintenance_error"]
    assert isinstance(last_error, dict)
    assert last_error["error_type"] == "RuntimeError"
    assert last_error["message"] == "forced squash failure"


class _AutoSquashOnlyLayerStack:
    def __init__(
        self,
        *,
        depth: int,
        depth_after_first_squash: int | None = None,
        depth_after_later_squash: int = 1,
        fail: bool = False,
    ) -> None:
        self._lock = threading.Lock()
        self._manifest = _manifest(depth)
        self._depth_after_first_squash = depth_after_first_squash
        self._depth_after_later_squash = depth_after_later_squash
        self._fail = fail
        self.squash_calls = 0
        self.squash_entered = threading.Event()
        self.release_squash = threading.Event()
        if fail:
            self.release_squash.set()

    def read_active_manifest(self) -> Manifest:
        with self._lock:
            return self._manifest

    def squash(self, *, max_depth: int) -> Manifest | None:
        del max_depth
        self.squash_entered.set()
        assert self.release_squash.wait(timeout=2)
        if self._fail:
            raise RuntimeError("forced squash failure")
        with self._lock:
            self.squash_calls += 1
            if self.squash_calls == 1 and self._depth_after_first_squash is not None:
                next_depth = self._depth_after_first_squash
            else:
                next_depth = self._depth_after_later_squash
            self._manifest = _manifest(next_depth, version=self._manifest.version + 1)
            return self._manifest


def _manifest(depth: int, *, version: int | None = None) -> Manifest:
    return Manifest(
        version=depth if version is None else version,
        layers=tuple(
            LayerRef(layer_id=f"L{index:06d}", path=f"layers/{index:06d}")
            for index in range(depth)
        ),
    )
