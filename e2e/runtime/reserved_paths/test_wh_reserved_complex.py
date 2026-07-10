"""Reserved `.wh.` namespace live catalog — complex tier (CX-01…04).

Catalog: docs/obsidian/ephemeral-os/implementation_plan/wh-reserved-namespace/test-case.md §3.3.
Definition order follows the §5 execution order: CX-01, CX-02, CX-04, then the
CX-03 storm last.
"""

from __future__ import annotations

import os
import random
import time
import zlib

import pytest

from runtime.reserved_paths.helpers import (
    REJECT_CLASS,
    CaseRecorder,
    RUN_ID,
    _assert_stack_unchanged,
    assert_no_wh_visible,
    assert_read_equals,
    assert_read_not_found,
    squash_layerstacks,
    daemon_fd_count,
    exec_publish_ok,
    exec_publish_reject,
    exec_read,
    file_write,
    layerstack,
    release_gated,
    start_gated,
    wh_case,
)
from runtime.file.helpers import assert_manifest_delta, assert_ok

pytestmark = [pytest.mark.whreserved, pytest.mark.complex]


def test_CX_01_poisoned_session_never_disturbs_concurrent_session(tmp_path):
    """CX-01 — poisoned session B never disturbs concurrent session A."""
    with CaseRecorder("CX-01") as rec:
        rec.axis("isolation", False)
        with wh_case(tmp_path, rec, files={"shared.txt": "base\n"}) as sandbox:
            before = assert_ok(layerstack(sandbox))

            agent_a = start_gated(
                sandbox, rec, "printf 'A\\n' >> /workspace/shared.txt", "agentA-start"
            )
            agent_b = start_gated(
                sandbox, rec, "printf 'evil' > /workspace/.wh.shared.txt", "agentB-start"
            )
            assert (
                agent_a["workspace_session_id"] != agent_b["workspace_session_id"]
            ), {"a": agent_a, "b": agent_b}

            release_gated(sandbox, rec, agent_a, "agentA-finalize")
            after_a = assert_manifest_delta(sandbox, before, 1)
            rec.expect_stack(after_a)

            release_gated(
                sandbox, rec, agent_b, "agentB-finalize", expect_reject=REJECT_CLASS
            )
            assert_manifest_delta(sandbox, before, 1)
            _assert_stack_unchanged(sandbox, after_a)
            rec.axis(
                "correctness",
                True,
                reject_class=REJECT_CLASS,
                manifest_delta=1,
            )

            assert_read_equals(sandbox, rec, "shared.txt", "base\nA\n", "read-shared")
            assert_read_not_found(sandbox, rec, ".wh.shared.txt", "read-wh-shared")
            assert_no_wh_visible(sandbox, rec, "wh-sweep")
            rec.axis("data_safety", True, base_paths_verified=1, masked=False)

            fresh = exec_read(
                sandbox, rec, "cat /workspace/shared.txt && ls -a /workspace", "fresh-exec"
            )
            listing = fresh["output"].split()
            assert "base" in listing and "A" in listing, fresh
            assert "shared.txt" in listing, fresh
            assert ".wh.shared.txt" not in listing, fresh
            rec.axis("isolation", True, fresh_view="agent A's world only")


def test_CX_02_squash_interplay_reservation_survives_squash(tmp_path):
    """CX-02 — internal markers squash cleanly; the reservation survives squash."""
    with CaseRecorder("CX-02") as rec:
        files = {"d/x.txt": "X", "d/y.txt": "Y", "solo.txt": "S"}
        with wh_case(tmp_path, rec, files=files) as sandbox:
            before = assert_ok(layerstack(sandbox))

            exec_publish_ok(sandbox, rec, "rm /workspace/solo.txt", "churn-delete")
            exec_publish_ok(
                sandbox,
                rec,
                "rm -rf /workspace/d && mkdir -p /workspace/d "
                "&& printf 'new' > /workspace/d/new.txt",
                "churn-recreate",
            )
            pre_squash = assert_manifest_delta(sandbox, before, 2)

            def verify_merged(tag):
                assert_read_not_found(sandbox, rec, "solo.txt", f"{tag}-read-solo")
                assert_read_not_found(sandbox, rec, "d/x.txt", f"{tag}-read-x")
                assert_read_not_found(sandbox, rec, "d/y.txt", f"{tag}-read-y")
                assert_read_equals(sandbox, rec, "d/new.txt", "new", f"{tag}-read-new")
                assert_read_not_found(sandbox, rec, "d/.wh..wh..opq", f"{tag}-read-marker")
                session = exec_read(
                    sandbox,
                    rec,
                    "cat /workspace/d/new.txt && test ! -e /workspace/d/x.txt "
                    "&& test ! -e /workspace/d/y.txt && test ! -e /workspace/solo.txt "
                    "&& echo masked-ok",
                    f"{tag}-session-view",
                )
                assert "new" in session["output"], session
                assert "masked-ok" in session["output"], session

            verify_merged("pre-squash")

            squashed = squash_layerstacks(sandbox)
            rec.record("squash-layerstacks.json", squashed)
            assert_ok(squashed)
            post_squash = rec.expect_stack(assert_ok(layerstack(sandbox)))
            rec.record("post-squash-layerstack.json", post_squash)

            verify_merged("post-squash")

            reject = exec_publish_reject(
                sandbox, rec, "printf 'z' > /workspace/.wh.z", "post-squash-poison"
            )
            _assert_stack_unchanged(sandbox, post_squash)
            rec.axis(
                "correctness",
                True,
                reject_class=reject["publish_reject_class"],
                squash="ok",
                post_squash_manifest_delta=0,
                pre_squash_manifest_version=pre_squash["manifest_version"],
                post_squash_manifest_version=post_squash["manifest_version"],
            )
            rec.axis("data_safety", True, base_paths_verified=4, masked=False)


def test_CX_04_reject_on_a_deep_stack_is_prompt_and_clean(tmp_path):
    """CX-04 — reject on a 50-layer stack is prompt; teardown contract holds at depth."""
    with CaseRecorder("CX-04") as rec:
        with wh_case(tmp_path, rec, files={}) as sandbox:
            before = assert_ok(layerstack(sandbox))
            layer_count = 50

            for index in range(layer_count):
                written = file_write(
                    sandbox, f"deep/f{index:03d}.txt", f"payload-{index}"
                )
                assert_ok(written)
            deep = assert_manifest_delta(sandbox, before, layer_count)
            rec.expect_stack(deep)

            started = time.monotonic()
            reject = exec_publish_reject(
                sandbox, rec, "touch /workspace/.wh.deep", "poison"
            )
            rec.timer("finalize_to_reject_ms", (time.monotonic() - started) * 1000.0)
            assert_manifest_delta(sandbox, deep, 0)
            _assert_stack_unchanged(sandbox, deep)
            rec.axis(
                "correctness",
                True,
                reject_class=reject["publish_reject_class"],
                manifest_delta=0,
                stack_depth=layer_count,
            )

            for index in (0, layer_count // 2, layer_count - 1):
                assert_read_equals(
                    sandbox,
                    rec,
                    f"deep/f{index:03d}.txt",
                    f"payload-{index}",
                    f"read-deep-{index:03d}",
                )
            assert_read_not_found(sandbox, rec, ".wh.deep", "read-wh-deep")
            assert_no_wh_visible(sandbox, rec, "wh-sweep")
            rec.axis("data_safety", True, base_paths_verified=3, masked=False)


POISON_SHAPES = (
    ("wh-name", lambda i: f"printf 'p' > '/workspace/.wh.storm{i}'"),
    ("opaque-marker", lambda i: f"mkdir -p /workspace/sd{i} && printf 'x' > '/workspace/sd{i}/.wh..wh..opq'"),
    ("nested-component", lambda i: f"mkdir -p '/workspace/na{i}/.wh.d' && printf 'x' > '/workspace/na{i}/.wh.d/f'"),
    ("symlink", lambda i: f"ln -s target '/workspace/.wh.l{i}'"),
    ("bare-wh", lambda _i: "touch '/workspace/.wh.'"),
)


def test_CX_03_reserved_name_storm_standing_invariants(tmp_path):
    """CX-03 — randomized reserved-name storm: standing invariants ×10."""
    with CaseRecorder("CX-03") as rec:
        rec.axis("isolation", False)
        iterations = 10
        seed = int(
            os.environ.get("WH_STORM_SEED", zlib.crc32(RUN_ID.encode("utf-8")))
        )
        rng = random.Random(seed)
        rec.record("storm-seed.json", {"seed": seed, "iterations": iterations})

        base_files = {f"k{i}.txt": str(i) for i in range(10)}
        with wh_case(tmp_path, rec, files=base_files) as sandbox:
            live = dict(base_files)
            deleted = set()
            extras = {}
            expected_version = assert_ok(layerstack(sandbox))["manifest_version"]
            fd_base = daemon_fd_count(sandbox)
            fd_samples = [fd_base]
            sessions_started = 0

            for iteration in range(iterations):
                shape_name, shape = rng.choice(POISON_SHAPES)
                poison_cmd = shape(iteration)

                if live and rng.random() < 0.4:
                    target = rng.choice(sorted(live))
                    legit_cmd = f"rm /workspace/{target}"
                    legit_kind = f"rm:{target}"
                else:
                    target = None
                    legit_cmd = f"printf 'v{iteration}' > /workspace/legit-{iteration}.txt"
                    legit_kind = f"write:legit-{iteration}.txt"

                poison = start_gated(
                    sandbox, rec, poison_cmd, f"iter{iteration:02d}-poison-start"
                )
                legit = start_gated(
                    sandbox, rec, legit_cmd, f"iter{iteration:02d}-legit-start"
                )
                releases = [("poison", poison), ("legit", legit)]
                rng.shuffle(releases)
                for kind, started in releases:
                    release_gated(
                        sandbox,
                        rec,
                        started,
                        f"iter{iteration:02d}-{kind}-finalize",
                        expect_reject=REJECT_CLASS if kind == "poison" else None,
                    )

                expected_version += 1
                stack = assert_ok(layerstack(sandbox))
                assert stack["manifest_version"] == expected_version, {
                    "iteration": iteration,
                    "expected_manifest_version": expected_version,
                    "layerstack": stack,
                }
                rec.expect_stack(stack)

                if target is not None:
                    del live[target]
                    deleted.add(target)
                else:
                    extras[f"legit-{iteration}.txt"] = f"v{iteration}"

                for name, content in live.items():
                    assert_read_equals(sandbox, rec, name, content)
                for name in deleted:
                    assert_read_not_found(sandbox, rec, name)
                assert_no_wh_visible(sandbox, rec, f"iter{iteration:02d}-wh-sweep")
                sessions_started += 3

                fd_now = daemon_fd_count(sandbox)
                fd_samples.append(fd_now)
                assert fd_now - fd_base <= sessions_started + 16, {
                    "iteration": iteration,
                    "fd_base": fd_base,
                    "fd_now": fd_now,
                    "sessions_started": sessions_started,
                }
                rec.record(
                    f"iter{iteration:02d}.json",
                    {
                        "poison_shape": shape_name,
                        "poison_cmd": poison_cmd,
                        "legit": legit_kind,
                        "manifest_version": stack["manifest_version"],
                        "fd_count": fd_now,
                        "sessions_started": sessions_started,
                    },
                )

            for name, content in extras.items():
                assert_read_equals(sandbox, rec, name, content)
            rec.axis(
                "correctness",
                True,
                reject_class=REJECT_CLASS,
                iterations=iterations,
                accepted_publishes=iterations,
                seed=seed,
            )
            rec.axis(
                "data_safety",
                True,
                base_paths_verified=len(live) + len(deleted),
                masked=False,
            )
            rec.axis(
                "isolation",
                True,
                fd_base=fd_base,
                fd_samples=fd_samples,
                fd_drift_max=max(abs(sample - fd_base) for sample in fd_samples),
                sessions_started=sessions_started,
            )
            rec.note(
                "fd sentinel bound is sessions_started + 16: completed command "
                "sessions retain their pty fd by design until LRU eviction at "
                "the engine cap (catalog corrected accordingly)."
            )
