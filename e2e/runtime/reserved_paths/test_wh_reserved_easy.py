"""Reserved `.wh.` namespace live catalog — easy tier (EZ-01…06).

Catalog: docs/obsidian/ephemeral-os/implementation_plan/wh-reserved-namespace/test-case.md §3.1.
"""

from __future__ import annotations

import pytest

from runtime.reserved_paths.helpers import (
    CaseRecorder,
    REJECT_CLASS,
    _assert_publish_rejection,
    _assert_stack_unchanged,
    assert_error,
    assert_no_wh_visible,
    assert_read_equals,
    assert_read_not_found,
    exec_publish_ok,
    exec_publish_reject,
    exec_read,
    file_read,
    file_write,
    layerstack,
    wh_case,
)
from runtime.file.helpers import assert_manifest_delta, assert_ok, edit, file_edit

pytestmark = [pytest.mark.whreserved, pytest.mark.easy]


def test_EZ_01_session_file_named_wh_foo_never_deletes_foo(tmp_path):
    """EZ-01 — a session file named `.wh.foo` never deletes `foo` (THE regression)."""
    with CaseRecorder("EZ-01") as rec:
        with wh_case(tmp_path, rec, files={"foo": "keep-me\n"}) as sandbox:
            before = assert_ok(layerstack(sandbox))

            reject = exec_publish_reject(
                sandbox, rec, "printf 'payload' > /workspace/.wh.foo", "poison"
            )
            assert_manifest_delta(sandbox, before, 0)
            _assert_stack_unchanged(sandbox, before)
            rec.axis(
                "correctness",
                True,
                reject_class=reject["publish_reject_class"],
                manifest_delta=0,
            )

            assert_read_equals(sandbox, rec, "foo", "keep-me\n", "read-foo")
            fresh = exec_read(
                sandbox, rec, "cat /workspace/foo && ls -a /workspace", "fresh-exec"
            )
            listing = fresh["output"].split()
            assert "keep-me" in fresh["output"], fresh
            assert "foo" in listing, fresh
            assert ".wh.foo" not in listing, fresh
            assert_read_not_found(sandbox, rec, ".wh.foo", "read-wh-foo")
            assert_no_wh_visible(sandbox, rec, "wh-sweep")
            rec.axis("data_safety", True, base_paths_verified=1, masked=False)


def test_EZ_02_one_shot_exec_touch_wh_probe_rejects_and_discards(tmp_path):
    """EZ-02 — one-shot exec `touch .wh.probe` rejects and discards."""
    with CaseRecorder("EZ-02") as rec:
        with wh_case(tmp_path, rec, files={}) as sandbox:
            before = assert_ok(layerstack(sandbox))

            reject = exec_publish_reject(
                sandbox, rec, "touch /workspace/.wh.probe", "poison"
            )
            assert reject["status"] == "ok", reject
            assert_manifest_delta(sandbox, before, 0)
            _assert_stack_unchanged(sandbox, before)
            rec.axis(
                "correctness",
                True,
                reject_class=reject["publish_reject_class"],
                manifest_delta=0,
            )

            fresh = exec_read(sandbox, rec, "ls -a /workspace", "fresh-exec")
            assert ".wh.probe" not in fresh["output"].split(), fresh
            assert_read_not_found(sandbox, rec, ".wh.probe", "read-wh-probe")
            assert_no_wh_visible(sandbox, rec, "wh-sweep")
            rec.axis("data_safety", True, base_paths_verified=0, masked=False)


def test_EZ_03_user_file_named_opaque_marker_never_masks_directory(tmp_path):
    """EZ-03 — a user file named `.wh..wh..opq` never masks its directory."""
    with CaseRecorder("EZ-03") as rec:
        files = {"cfg/a.txt": "A\n", "cfg/b.txt": "B\n"}
        with wh_case(tmp_path, rec, files=files) as sandbox:
            before = assert_ok(layerstack(sandbox))

            reject = exec_publish_reject(
                sandbox, rec, "printf 'x' > /workspace/cfg/.wh..wh..opq", "poison"
            )
            assert_manifest_delta(sandbox, before, 0)
            _assert_stack_unchanged(sandbox, before)
            rec.axis(
                "correctness",
                True,
                reject_class=reject["publish_reject_class"],
                manifest_delta=0,
            )

            assert_read_equals(sandbox, rec, "cfg/a.txt", "A\n", "read-a")
            assert_read_equals(sandbox, rec, "cfg/b.txt", "B\n", "read-b")
            fresh = exec_read(sandbox, rec, "ls /workspace/cfg", "fresh-exec")
            listing = fresh["output"].split()
            assert "a.txt" in listing and "b.txt" in listing, fresh
            assert ".wh..wh..opq" not in listing, fresh
            assert_no_wh_visible(sandbox, rec, "wh-sweep")
            rec.axis("data_safety", True, base_paths_verified=2, masked=False)


def test_EZ_04_sessionless_file_write_to_wh_foo_rejects_without_poisoning(tmp_path):
    """EZ-04 — sessionless `file_write` to `.wh.foo` rejects without poisoning."""
    with CaseRecorder("EZ-04") as rec:
        with wh_case(tmp_path, rec, files={"foo": "keep\n"}) as sandbox:
            before = assert_ok(layerstack(sandbox))

            write = file_write(sandbox, ".wh.foo", "evil")
            rec.record("file-write.json", write)
            _assert_publish_rejection(write, REJECT_CLASS)

            edited = file_edit(sandbox, ".wh.bar", [edit("a", "b")])
            rec.record("file-edit.json", edited)
            assert_error(edited, "not_found")
            rec.note(
                "file_edit pinned to not_found: read-modify-write reads first and "
                "a reserved path can never resolve in the merged view (spec OQ1)."
            )

            assert_manifest_delta(sandbox, before, 0)
            _assert_stack_unchanged(sandbox, before)
            rec.axis(
                "correctness",
                True,
                reject_class=REJECT_CLASS,
                manifest_delta=0,
                edit_error_kind="not_found",
            )

            assert_read_equals(sandbox, rec, "foo", "keep\n", "read-foo")
            assert_read_not_found(sandbox, rec, ".wh.foo", "read-wh-foo")
            assert_no_wh_visible(sandbox, rec, "wh-sweep")
            rec.axis("data_safety", True, base_paths_verified=1, masked=False)


def test_EZ_05_lookalike_names_are_ordinary_paths(tmp_path):
    """EZ-05 — lookalike names (`.wh`, `.whx`, `x.wh.y`, `wh.foo`) are ordinary paths."""
    with CaseRecorder("EZ-05") as rec:
        with wh_case(tmp_path, rec, files={}) as sandbox:
            before = assert_ok(layerstack(sandbox))
            lookalikes = {".wh": "1", ".whx": "2", "x.wh.y": "3", "wh.foo": "4"}

            script = " && ".join(
                f"printf '{content}' > '/workspace/{name}'"
                for name, content in lookalikes.items()
            )
            publish = exec_publish_ok(sandbox, rec, script, "publish")
            assert publish.get("publish_reject_class") is None, publish
            after = assert_manifest_delta(sandbox, before, 1)
            rec.expect_stack(after)
            rec.axis("correctness", True, manifest_delta=1, source_writes=len(lookalikes))

            for name, content in lookalikes.items():
                assert_read_equals(sandbox, rec, name, content)
            fresh = exec_read(sandbox, rec, "ls -a /workspace", "fresh-exec")
            listing = fresh["output"].split()
            for name in lookalikes:
                assert name in listing, fresh
            rec.axis(
                "data_safety",
                True,
                base_paths_verified=len(lookalikes),
                masked=False,
            )


def test_EZ_06_real_deletion_still_works_end_to_end(tmp_path):
    """EZ-06 — real deletion (kernel whiteout) still works end to end."""
    with CaseRecorder("EZ-06") as rec:
        with wh_case(tmp_path, rec, files={"victim.txt": "doomed\n"}) as sandbox:
            before = assert_ok(layerstack(sandbox))

            publish = exec_publish_ok(sandbox, rec, "rm /workspace/victim.txt", "delete")
            after = assert_manifest_delta(sandbox, before, 1)
            rec.expect_stack(after)
            rec.axis("correctness", True, manifest_delta=1)

            assert_read_not_found(sandbox, rec, "victim.txt", "read-victim")
            fresh = exec_read(sandbox, rec, "ls -a /workspace", "fresh-exec")
            listing = fresh["output"].split()
            assert "victim.txt" not in listing, fresh
            assert ".wh.victim.txt" not in listing, fresh
            assert_no_wh_visible(sandbox, rec, "wh-sweep")
            rec.axis("data_safety", True, base_paths_verified=1, masked=False)
