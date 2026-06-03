"""Natural layer-stack squash trigger coverage for OCC publications."""

from __future__ import annotations

from tests.occ_change_helpers import write_change

import asyncio
import concurrent.futures
import threading
from pathlib import Path

from sandbox.layer_stack import WriteLayerChange
from sandbox.layer_stack.manifest import LayerRef, Manifest, manifest_root_hash
from sandbox.layer_stack.stack import LayerStack
from sandbox.occ.changeset import ChangesetResult
from sandbox.occ.maintenance import AutoSquashMaintenancePolicy
from sandbox.occ.service import OccService
from sandbox._shared.timing_keys import TimingKey


class _Gitignore:
    def is_ignored(self, _path: str) -> bool:
        return False

    def is_ignored_in_snapshot(self, path: str, _snapshot: object) -> bool:
        return self.is_ignored(path)


def _auto_squash_service(
    stack: LayerStack,
    *,
    max_depth: int,
) -> OccService:
    return OccService(
        gitignore=_Gitignore(),
        layer_stack=stack,
        maintenance=AutoSquashMaintenancePolicy(
            snapshot_reader=stack,
            squasher=stack,
            max_depth=max_depth,
        ),
    )


def _source(tmp_path: Path, name: str, content: bytes) -> str:
    path = tmp_path / "sources" / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(content)
    return str(path)


def _publish_direct(stack: LayerStack, tmp_path: Path, index: int) -> None:
    stack.publish_changes(
        [
            WriteLayerChange(
                path=f"tracked/direct/{index:02d}.txt",
                source_path=_source(
                    tmp_path,
                    f"direct-{index:02d}.txt",
                    f"direct-{index:02d}\n".encode(),
                ),
            )
        ]
    )


def test_occ_publications_auto_squash_without_direct_squash_call(
    tmp_path,
) -> None:
    max_depth = 4
    stack = LayerStack(tmp_path / "stack")
    service = _auto_squash_service(stack, max_depth=max_depth)

    for index in range(8):
        result = asyncio.run(
            service.apply_changeset(
                [
                    write_change(
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
    stack = LayerStack(tmp_path / "stack")
    service = _auto_squash_service(stack, max_depth=max_depth)

    seed = asyncio.run(
        service.apply_changeset(
            [write_change(path="tracked/value.txt", final_content=b"base\n")],
            snapshot=stack.read_active_manifest(),
        )
    )
    assert isinstance(seed, ChangesetResult)
    assert seed.published_manifest_version is not None

    lease = stack.acquire_lease_record("held-before-auto-squash")
    try:
        for index in range(6):
            result = asyncio.run(
                service.apply_changeset(
                    [
                        write_change(
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


def test_auto_squash_collapses_active_manifest_even_with_active_lease(
    tmp_path: Path,
) -> None:
    stack = LayerStack(tmp_path / "stack")
    for index in range(4):
        _publish_direct(stack, tmp_path, index)
    lease = stack.acquire_lease_record("held-reader")
    try:
        _publish_direct(stack, tmp_path, 4)
        _publish_direct(stack, tmp_path, 5)
        policy = AutoSquashMaintenancePolicy(
            snapshot_reader=stack,
            squasher=stack,
            max_depth=3,
        )

        timings = policy.after_publish_sync(
            ChangesetResult(files=(), published_manifest_version=6)
        )

        assert timings[TimingKey.LAYER_AUTO_SQUASH_DEPTH_BEFORE] == 6.0
        assert timings[TimingKey.LAYER_AUTO_SQUASH_DEPTH_AFTER] == 3.0
        assert stack.read_active_manifest().depth == 3
        assert stack.read_text("tracked/direct/03.txt", lease.manifest) == (
            "direct-03\n",
            True,
        )
    finally:
        assert stack.release_lease(lease.lease_id) is True

    assert stack.active_lease_count() == 0


def test_auto_squash_uses_fixed_constant_depth(tmp_path) -> None:
    configured_max_depth = 6
    stack = LayerStack(tmp_path / "stack")
    service = _auto_squash_service(stack, max_depth=configured_max_depth)
    observed_max_depths: list[float] = []

    for index in range(10):
        result = asyncio.run(
            service.apply_changeset(
                [
                    write_change(
                        path=f"tracked/constant-depth/{index:02d}.txt",
                        final_content=f"constant-depth-{index:02d}\n".encode(),
                    )
                ],
                snapshot=stack.read_active_manifest(),
            )
        )
        observed_max_depth = result.timings.get(TimingKey.LAYER_AUTO_SQUASH_MAX_DEPTH)
        if observed_max_depth is not None:
            observed_max_depths.append(observed_max_depth)

    assert observed_max_depths
    assert set(observed_max_depths) == {configured_max_depth}
    assert stack.read_active_manifest().depth <= configured_max_depth


def test_auto_squash_policy_serializes_in_flight_squash_workers() -> None:
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
        peer = pool.submit(policy.after_publish_sync, published)
        assert stack.concurrent_squash_calls == 1
        stack.release_squash.set()
        peer_timings = peer.result(timeout=2)
        owner_timings = owner.result(timeout=2)

    assert stack.squash_calls == 2
    assert stack.max_concurrent_squash_calls == 1
    assert TimingKey.LAYER_AUTO_SQUASH_SKIPPED_IN_FLIGHT not in peer_timings
    assert TimingKey.LAYER_AUTO_SQUASH_SKIPPED_IN_FLIGHT not in owner_timings
    assert TimingKey.LAYER_AUTO_SQUASH_RECHECK_TRIGGERED not in peer_timings
    assert TimingKey.LAYER_AUTO_SQUASH_RECHECK_TRIGGERED not in owner_timings
    assert {
        owner_timings[TimingKey.LAYER_AUTO_SQUASH_DEPTH_AFTER],
        peer_timings[TimingKey.LAYER_AUTO_SQUASH_DEPTH_AFTER],
    } == {1.0, 6.0}


def test_auto_squash_policy_rechecks_after_wait_and_skips_when_recovered() -> None:
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

    with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
        owner = pool.submit(policy.after_publish_sync, published)
        assert stack.squash_entered.wait(timeout=2)
        peer = pool.submit(policy.after_publish_sync, published)
        stack.release_squash.set()
        peer_timings = peer.result(timeout=2)
        owner_timings = owner.result(timeout=2)

    assert stack.squash_calls == 1
    assert stack.max_concurrent_squash_calls == 1
    assert owner_timings[TimingKey.LAYER_AUTO_SQUASH_DEPTH_AFTER] == 1.0
    assert peer_timings == {}


def test_auto_squash_policy_emits_focused_squash_audit() -> None:
    max_depth = 4
    stack = _AutoSquashOnlyLayerStack(depth=7, depth_after_first_squash=1)
    stack.release_squash.set()
    events = []
    policy = AutoSquashMaintenancePolicy(
        snapshot_reader=stack,
        squasher=stack,
        max_depth=max_depth,
        audit=lambda **payload: events.append(payload),
    )
    published = ChangesetResult(files=(), published_manifest_version=1)

    timings = policy.after_publish_sync(published)

    assert timings[TimingKey.LAYER_AUTO_SQUASH_DEPTH_BEFORE] == 7.0
    assert timings[TimingKey.LAYER_AUTO_SQUASH_DEPTH_AFTER] == 1.0
    assert events == [
        {
            "triggered": True,
            "trigger_reason": "post_publish_depth",
            "input_layers": 7,
        },
        {
            "completed": True,
            "input_layers": 7,
            "result_layers": 1,
            "manifest_root_hash_value": manifest_root_hash(stack.read_active_manifest()),
        },
    ]


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
        self.concurrent_squash_calls = 0
        self.max_concurrent_squash_calls = 0
        self.squash_entered = threading.Event()
        self.release_squash = threading.Event()

    def read_active_manifest(self) -> Manifest:
        with self._lock:
            return self._manifest

    def can_squash(self, *, max_depth: int) -> bool:
        with self._lock:
            return self._manifest.depth > max_depth

    def squash(self, *, max_depth: int) -> Manifest | None:
        del max_depth
        self.squash_entered.set()
        with self._lock:
            self.concurrent_squash_calls += 1
            self.max_concurrent_squash_calls = max(
                self.max_concurrent_squash_calls,
                self.concurrent_squash_calls,
            )
        try:
            assert self.release_squash.wait(timeout=2)
            with self._lock:
                self.squash_calls += 1
                if self.squash_calls == 1 and self._depth_after_first_squash is not None:
                    next_depth = self._depth_after_first_squash
                else:
                    next_depth = self._depth_after_later_squash
                self._manifest = _manifest(
                    next_depth, version=self._manifest.version + 1
                )
                return self._manifest
        finally:
            with self._lock:
                self.concurrent_squash_calls -= 1


def _manifest(depth: int, *, version: int | None = None) -> Manifest:
    return Manifest(
        version=depth if version is None else version,
        layers=tuple(
            LayerRef(layer_id=f"L{index:06d}", path=f"layers/{index:06d}") for index in range(depth)
        ),
    )
