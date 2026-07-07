"""Reserved `.wh.` namespace live catalog — medium tier (MED-01…06).

Catalog: docs/obsidian/ephemeral-os/implementation_plan/wh-reserved-namespace/test-case.md §3.2.
"""

from __future__ import annotations

import pytest

from runtime.reserved_paths.helpers import (
    CaseRecorder,
    _assert_stack_unchanged,
    assert_no_wh_visible,
    assert_read_equals,
    assert_read_not_found,
    create_session,
    destroy_session,
    exec_in,
    exec_publish_ok,
    exec_publish_reject,
    exec_read,
    file_write,
    layerstack,
    wh_case,
)
from runtime.file.helpers import assert_manifest_delta, assert_ok

pytestmark = [pytest.mark.whreserved, pytest.mark.medium]


def test_MED_01_mixed_changeset_is_atomic(tmp_path):
    """MED-01 — one reserved name discards the whole changeset, no partial publish."""
    with CaseRecorder("MED-01") as rec:
        with wh_case(tmp_path, rec, files={}) as sandbox:
            before = assert_ok(layerstack(sandbox))

            reject = exec_publish_reject(
                sandbox,
                rec,
                "printf 'good' > /workspace/good.txt && printf 'bad' > /workspace/.wh.bad",
                "poison",
            )
            assert_manifest_delta(sandbox, before, 0)
            _assert_stack_unchanged(sandbox, before)
            rec.axis(
                "correctness",
                True,
                reject_class=reject["publish_reject_class"],
                manifest_delta=0,
            )

            assert_read_not_found(sandbox, rec, "good.txt", "read-good")
            assert_read_not_found(sandbox, rec, ".wh.bad", "read-wh-bad")
            assert_no_wh_visible(sandbox, rec, "wh-sweep")
            rec.axis("data_safety", True, base_paths_verified=0, masked=False)


def test_MED_02_rule_is_per_component_not_per_basename(tmp_path):
    """MED-02 — `a/.wh.d/f.txt` rejects: the reserved rule is component-wise."""
    with CaseRecorder("MED-02") as rec:
        with wh_case(tmp_path, rec, files={"a/d/keep.txt": "K\n"}) as sandbox:
            before = assert_ok(layerstack(sandbox))

            reject = exec_publish_reject(
                sandbox,
                rec,
                "mkdir -p /workspace/a/.wh.d && printf 'x' > /workspace/a/.wh.d/f.txt",
                "poison",
            )
            assert_manifest_delta(sandbox, before, 0)
            _assert_stack_unchanged(sandbox, before)
            rec.axis(
                "correctness",
                True,
                reject_class=reject["publish_reject_class"],
                manifest_delta=0,
            )

            assert_read_equals(sandbox, rec, "a/d/keep.txt", "K\n", "read-keep")
            fresh = exec_read(sandbox, rec, "ls -a /workspace/a", "fresh-exec")
            listing = fresh["output"].split()
            assert "d" in listing, fresh
            assert ".wh.d" not in listing, fresh
            assert_no_wh_visible(sandbox, rec, "wh-sweep")
            rec.axis("data_safety", True, base_paths_verified=1, masked=False)


def test_MED_03_symlink_named_wh_link_rejects(tmp_path):
    """MED-03 — a symlink named `.wh.link` rejects like every other change kind."""
    with CaseRecorder("MED-03") as rec:
        with wh_case(tmp_path, rec, files={}) as sandbox:
            before = assert_ok(layerstack(sandbox))

            reject = exec_publish_reject(
                sandbox, rec, "ln -s target /workspace/.wh.link", "poison"
            )
            assert_manifest_delta(sandbox, before, 0)
            _assert_stack_unchanged(sandbox, before)
            rec.axis(
                "correctness",
                True,
                reject_class=reject["publish_reject_class"],
                manifest_delta=0,
            )

            assert_read_not_found(sandbox, rec, ".wh.link", "read-wh-link")
            fresh = exec_read(sandbox, rec, "ls -a /workspace", "fresh-exec")
            assert ".wh.link" not in fresh["output"].split(), fresh
            assert_no_wh_visible(sandbox, rec, "wh-sweep")
            rec.axis("data_safety", True, base_paths_verified=0, masked=False)


def test_MED_04_bare_wh_file_rejects_and_base_materialization_stays_intact(tmp_path):
    """MED-04 — bare `.wh.` rejects; a fresh session still sees the whole base."""
    with CaseRecorder("MED-04") as rec:
        files = {"root.txt": "R\n", "sub/leaf.txt": "L\n"}
        with wh_case(tmp_path, rec, files=files) as sandbox:
            before = assert_ok(layerstack(sandbox))

            reject = exec_publish_reject(
                sandbox, rec, "touch '/workspace/.wh.'", "poison"
            )
            assert_manifest_delta(sandbox, before, 0)
            _assert_stack_unchanged(sandbox, before)
            rec.axis(
                "correctness",
                True,
                reject_class=reject["publish_reject_class"],
                manifest_delta=0,
            )

            session = create_session(sandbox)["workspace_session_id"]
            try:
                fresh = exec_in(
                    sandbox,
                    session,
                    "cat /workspace/root.txt /workspace/sub/leaf.txt",
                    yield_time_ms=60_000,
                )
                rec.record("fresh-session-read.json", fresh)
                assert_ok(fresh)
                assert fresh["status"] == "ok" and fresh["exit_code"] == 0, fresh
                assert fresh["output"] == "R\nL", fresh
            finally:
                assert_ok(destroy_session(sandbox, session, grace_s=1))
            assert_read_equals(sandbox, rec, "root.txt", "R\n", "read-root")
            assert_read_equals(sandbox, rec, "sub/leaf.txt", "L\n", "read-leaf")
            assert_read_not_found(sandbox, rec, ".wh.", "read-bare-wh")
            assert_no_wh_visible(sandbox, rec, "wh-sweep")
            rec.note(
                "Harness has no per-sandbox daemon restart; base rematerialization "
                "is exercised via a fresh workspace session, and the projection "
                "guard itself is pinned by the unit test "
                "bare_wh_layer_entry_projects_as_file_not_directory_clear."
            )
            rec.axis("data_safety", True, base_paths_verified=2, masked=False)


def test_MED_05_wh_protected_name_is_one_clean_reject(tmp_path):
    """MED-05 — `.wh.manifest.json` is one clean reject, not a fabricated protected delete."""
    with CaseRecorder("MED-05") as rec:
        with wh_case(tmp_path, rec, files={}) as sandbox:
            before = assert_ok(layerstack(sandbox))

            reject = exec_publish_reject(
                sandbox, rec, "printf 'x' > /workspace/.wh.manifest.json", "poison"
            )
            assert_manifest_delta(sandbox, before, 0)
            _assert_stack_unchanged(sandbox, before)

            named = file_write(sandbox, ".wh.manifest.json", "x")
            rec.record("file-write-naming-probe.json", named)
            error = named.get("error") or {}
            assert error.get("kind") == "operation_failed", named
            assert ".wh.manifest.json" in error.get("message", ""), named
            assert "ProtectedPath" in error.get("message", ""), named
            rec.note(
                "Terminal exec responses carry publish_reject_class only; the "
                "path-naming half of the F3 regression is pinned via the "
                "sessionless file_write error, which names the literal user path."
            )
            rec.axis(
                "correctness",
                True,
                reject_class=reject["publish_reject_class"],
                manifest_delta=0,
                reject_names_user_path=True,
            )

            stack = assert_ok(layerstack(sandbox))
            rec.record("post-reject-layerstack.json", stack)
            assert stack["manifest_version"] == before["manifest_version"], stack
            assert stack["root_hash"] == before["root_hash"], stack
            assert_read_not_found(sandbox, rec, ".wh.manifest.json", "read-wh-manifest")
            assert_no_wh_visible(sandbox, rec, "wh-sweep")
            rec.axis("data_safety", True, base_paths_verified=0, masked=False)


def test_MED_06_rm_rf_and_recreate_round_trips(tmp_path):
    """MED-06 — `rm -rf` + recreate (kernel opaque/whiteout flow) still round-trips."""
    with CaseRecorder("MED-06") as rec:
        with wh_case(tmp_path, rec, files={"reports/daily/r1.txt": "r1"}) as sandbox:
            before = assert_ok(layerstack(sandbox))

            publish = exec_publish_ok(
                sandbox,
                rec,
                "rm -rf /workspace/reports && mkdir -p /workspace/reports/daily "
                "&& printf 'r2' > /workspace/reports/daily/r2.txt",
                "churn",
            )
            after = assert_manifest_delta(sandbox, before, 1)
            rec.expect_stack(after)
            rec.axis("correctness", True, manifest_delta=1)

            assert_read_not_found(sandbox, rec, "reports/daily/r1.txt", "read-r1")
            assert_read_equals(sandbox, rec, "reports/daily/r2.txt", "r2", "read-r2")
            fresh = exec_read(sandbox, rec, "ls -a /workspace/reports/daily", "fresh-exec")
            listing = fresh["output"].split()
            assert "r2.txt" in listing, fresh
            assert not any(name.startswith(".wh.") for name in listing), fresh
            assert_no_wh_visible(sandbox, rec, "wh-sweep")
            rec.axis("data_safety", True, base_paths_verified=2, masked=False)
