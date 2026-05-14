"""Natural layer-stack squash trigger coverage for OCC publications."""

from __future__ import annotations

import asyncio
import concurrent.futures
import threading

from sandbox.layer_stack.manifest import LayerRef, Manifest
from sandbox.layer_stack.manager import LayerStackManager
from sandbox.occ.changeset.types import ChangesetResult, WriteChange
from sandbox.occ.maintenance import AutoSquashMaintenancePolicy
from sandbox.occ.service import Service
from sandbox.occ.timing_keys import TimingKey


class _Gitignore:
    def is_ignored(self, _path: str) -> bool:
        return False

    def is_ignored_in_snapshot(self, path: str, _snapshot: object) -> bool:
        return self.is_ignored(path)


def _auto_squash_service(
    stack: LayerStackManager,
    *,
    max_depth: int,
) -> Service:
    return Service(
        gitignore=_Gitignore(),
        layer_stack=stack,
        maintenance=AutoSquashMaintenancePolicy(
            snapshot_reader=stack, squasher=stack, max_depth=max_depth
        ),
    )


def test_occ_publications_auto_squash_without_direct_squash_call(
    tmp_path,
) -> None:
    max_depth = 4
    stack = LayerStackManager(tmp_path / "stack")
    service = _auto_squash_service(stack, max_depth=max_depth)

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


def test_auto_squash_preserves_active_lease_view(tmp_path) -> None:
    max_depth = 3
    stack = LayerStackManager(tmp_path / "stack")
    service = _auto_squash_service(stack, max_depth=max_depth)

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


def test_auto_squash_uses_fixed_constant_depth(tmp_path) -> None:
    max_depth = 6
    stack = LayerStackManager(tmp_path / "stack")
    service = _auto_squash_service(stack, max_depth=max_depth)
    observed_max_depths: list[float] = []

    for index in range(10):
        result = asyncio.run(
            service.apply_changeset(
                [
                    WriteChange(
                        path=f"tracked/constant-depth/{index:02d}.txt",
                        final_content=f"constant-depth-{index:02d}\n".encode(),
                    )
                ],
                snapshot=stack.read_active_manifest(),
            )
        )
        max_depth = result.timings.get(TimingKey.LAYER_AUTO_SQUASH_MAX_DEPTH)
        if max_depth is not None:
            observed_max_depths.append(max_depth)

    assert observed_max_depths
    assert set(observed_max_depths) == {max_depth}
    assert stack.read_active_manifest().depth <= max_depth


def test_default_mode_skips_in_flight_squash_and_rechecks() -> None:
    max_depth = 4
    stack = _AutoSquashOnlyLayerStack(
        depth=7,
        depth_after_first_squash=6,
        depth_after_later_squash=1,
    )
    policy = AutoSquashMaintenancePolicy(
        snapshot_reader=stack,
        squasher=stack,
        max_depth=max_depth,
    )
    published = ChangesetResult(files=(), published_manifest_version=1)

    with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
        owner = pool.submit(policy.after_publish_sync, published)
        assert stack.squash_entered.wait(timeout=2)
        skipped = pool.submit(policy.after_publish_sync, published)
        skipped_timings = skipped.result(timeout=2)
        stack.release_squash.set()
        owner_timings = owner.result(timeout=2)

    assert stack.squash_calls == 2
    assert skipped_timings[TimingKey.LAYER_AUTO_SQUASH_SKIPPED_IN_FLIGHT] == 1.0
    assert owner_timings[TimingKey.LAYER_AUTO_SQUASH_RECHECK_TRIGGERED] == 1.0
    assert owner_timings[TimingKey.LAYER_AUTO_SQUASH_DEPTH_BEFORE] == 7.0
    assert owner_timings[TimingKey.LAYER_AUTO_SQUASH_DEPTH_AFTER] == 1.0


def test_removed_mode_env_is_ignored(monkeypatch) -> None:
    monkeypatch.setenv("EOS_OCC_SQUASH_MODE", "async")
    max_depth = 4
    stack = _AutoSquashOnlyLayerStack(
        depth=7,
        depth_after_first_squash=1,
    )
    policy = AutoSquashMaintenancePolicy(
        snapshot_reader=stack,
        squasher=stack,
        max_depth=max_depth,
    )
    published = ChangesetResult(files=(), published_manifest_version=1)

    stack.release_squash.set()
    timings = policy.after_publish_sync(published)

    assert "layer_stack.auto_squash.enqueued" not in timings
    assert timings[TimingKey.LAYER_AUTO_SQUASH_DEPTH_AFTER] == 1.0
    assert stack.squash_calls == 1


class _AutoSquashOnlyLayerStack:
    def __init__(
        self,
        *,
        depth: int,
        depth_after_first_squash: int | None = None,
        depth_after_later_squash: int = 1,
    ) -> None:
        self._lock = threading.Lock()
        self._manifest = _manifest(depth)
        self._depth_after_first_squash = depth_after_first_squash
        self._depth_after_later_squash = depth_after_later_squash
        self.squash_calls = 0
        self.squash_entered = threading.Event()
        self.release_squash = threading.Event()

    def read_active_manifest(self) -> Manifest:
        with self._lock:
            return self._manifest

    def squash(self, *, max_depth: int) -> Manifest | None:
        del max_depth
        self.squash_entered.set()
        assert self.release_squash.wait(timeout=2)
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
            LayerRef(layer_id=f"L{index:06d}", path=f"layers/{index:06d}") for index in range(depth)
        ),
    )
