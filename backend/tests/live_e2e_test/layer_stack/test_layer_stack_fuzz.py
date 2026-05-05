"""Layer-stack property fuzz: publish/lease/release/squash invariants.

Backs §4.1. Drives a random sequence of layer-stack operations and
verifies the snapshot-isolation contract on every leased read:

- A lease pinned at manifest version V must observe the exact path→content
  map that was visible to the active manifest when the lease was acquired,
  regardless of any subsequent ``publish_changes`` or ``squash``.
- After all leases release and a final fsck, the stack reports no orphan
  layers and every active manifest entry resolves to a layer dir on disk.

Determinism: seed via ``EPHEMERALOS_LAYER_FUZZ_SEED`` (default ``17``);
op count via ``EPHEMERALOS_LAYER_FUZZ_OPS`` (default ``300``).
"""

from __future__ import annotations

import os
import random
from pathlib import Path


from .._harness.assertions import (
    assert_manifest_layers_referenced_on_disk,
    assert_no_orphan_layers,
)
from .._harness.sandbox_fixture import SandboxHandle
from .._harness.thresholds import with_thresholds
from .._harness.workload import make_write_change


_PATH_POOL = tuple(f"fuzz/{i:03d}.txt" for i in range(32))


def _ops() -> int:
    return int(os.environ.get("EPHEMERALOS_LAYER_FUZZ_OPS", "300"))


def _seed() -> int:
    return int(os.environ.get("EPHEMERALOS_LAYER_FUZZ_SEED", "17"))


def test_layer_stack_fuzz_pinned_views_match_acquisition_state(
    layer_stack_sandbox: SandboxHandle, tmp_path: Path
) -> None:
    rng = random.Random(_seed())
    ops_total = _ops()

    storage = layer_stack_sandbox.extras["storage_root"]
    payloads = tmp_path / "fuzz_payloads"
    payloads.mkdir()

    with with_thresholds(storage, MAX_DEPTH=24, EMERGENCY_DEPTH=200) as cfg:
        manager = cfg.manager
        # Seed an empty baseline so the first lease has something to pin.
        manager.publish_changes(
            [make_write_change(payloads, "seed", "0\n", layer_path="seed.txt")]
        )

        head: dict[str, str] = {"seed.txt": "0\n"}
        # lease_id -> (manifest pinned, snapshot of head at acquisition)
        pinned: dict[str, tuple[object, dict[str, str]]] = {}

        weights = {
            "publish": 6,
            "acquire": 3,
            "release": 2,
            "squash": 1,
            "read_lease": 4,
        }
        choices: list[str] = []
        for op, w in weights.items():
            choices.extend([op] * w)

        publish_count = squash_count = lease_count = release_count = read_count = 0
        for step in range(ops_total):
            op = rng.choice(choices)
            if op == "publish":
                path = rng.choice(_PATH_POOL)
                content = f"v{step:04d}\n"
                manager.publish_changes(
                    [
                        make_write_change(
                            payloads, f"{step:04d}-{path.replace('/', '_')}",
                            content, layer_path=path,
                        )
                    ]
                )
                head[path] = content
                publish_count += 1
            elif op == "acquire":
                lease = manager.acquire_snapshot_lease(f"fuzz-{step:04d}")
                pinned[lease.lease_id] = (lease.manifest, dict(head))
                lease_count += 1
            elif op == "release":
                if not pinned:
                    continue
                lease_id = rng.choice(list(pinned))
                manager.release_lease(lease_id)
                pinned.pop(lease_id, None)
                release_count += 1
            elif op == "squash":
                manager.squash(max_depth=cfg.max_depth)
                squash_count += 1
            elif op == "read_lease":
                if not pinned:
                    continue
                lease_id = rng.choice(list(pinned))
                manifest, expected = pinned[lease_id]
                # Spot-check up to 4 paths to keep budget bounded.
                sample = rng.sample(list(expected), min(4, len(expected)))
                for path in sample:
                    got, exists = manager.read_text(path, manifest=manifest)
                    assert exists, (
                        f"lease={lease_id} path={path!r} disappeared "
                        f"under pinned manifest version={manifest.version}"
                    )
                    assert got == expected[path], (
                        f"lease={lease_id} path={path!r} torn read: "
                        f"got={got!r} expected={expected[path]!r} "
                        f"manifest_version={manifest.version}"
                    )
                read_count += 1

        # Drain remaining leases and confirm final-state invariants.
        for lease_id in list(pinned):
            manager.release_lease(lease_id)
        pinned.clear()

        assert_manifest_layers_referenced_on_disk(manager)
        assert_no_orphan_layers(manager)

        print(
            f"\n[layer_stack.fuzz.summary] "
            f"ops={ops_total} seed={_seed()} "
            f"publish={publish_count} acquire={lease_count} "
            f"release={release_count} squash={squash_count} "
            f"read_lease={read_count} active_paths={len(head)}"
        )
