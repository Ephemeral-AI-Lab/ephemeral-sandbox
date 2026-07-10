"""Live e2e: Correctness: Layerstack, Mount, Conflict — Sessionless (18 cases)."""

import pytest

from runtime.file.helpers import (
    assert_blame_owners,
    assert_blame_ranges,
    assert_blame_tiling,
    assert_content,
    assert_error,
    assert_manifest_delta,
    assert_ok,
    assert_single_owner,
    edit,
    exec_command,
    file_blame,
    file_edit,
    file_read,
    file_write,
    layer_ids,
    layerstack,
    owners_by_line,
    sandbox_from_workspace,
)


def _exec_ok(sandbox, command, **kwargs):
    kwargs.setdefault("yield_time_ms", 30_000)
    result = exec_command(sandbox, command, **kwargs)
    assert result["status"] == "ok", result
    assert result["exit_code"] == 0, result
    return result


def _assert_stack_unchanged(sandbox, before):
    after = layerstack(sandbox)
    assert after["manifest_version"] == before["manifest_version"], after
    assert after["root_hash"] == before["root_hash"], after
    assert layer_ids(sandbox) == [layer["layer_id"] for layer in before["layers"]]
    return after


def _assert_publish_rejection(result, reason):
    error = assert_error(result, "operation_failed")
    rejection = (error.get("details") or {}).get("publish_rejection")
    if rejection is not None:
        assert rejection["reason"] == reason, error
    else:
        expected = "".join(ch for ch in reason.lower() if ch.isalnum())
        actual = "".join(ch for ch in error.get("message", "").lower() if ch.isalnum())
        assert expected in actual, error
    return error


def _bulk_path(index):
    return f"bulk/dir{index % 10:02d}/sub{index % 5:02d}/file-{index}.txt"


def test_three_distinct_sessionless_writes_prepend_three_layers(sandbox):
    """Three sessionless `file_write` calls to three distinct new paths, then
    read each back.
    Expected: `observability layerstack` shows `manifest_version` advanced by
    exactly 3 and three new `layer_id` entries prepended (newest first); each
    `file_read` returns the written `content` with correct
    `total_lines`/`total_bytes`."""
    before = layerstack(sandbox)
    before_layers = layer_ids(sandbox)
    writes = {
        "correctness/one.txt": "one\nuno",
        "correctness/two.txt": "two\ndos\nzwei",
        "correctness/three.txt": "three",
    }

    for path, content in writes.items():
        result = file_write(sandbox, path, content)
        assert result["type"] == "create", result
        read = assert_content(file_read(sandbox, path), content)
        assert read["total_lines"] == content.count("\n") + 1, read
        assert read["total_bytes"] == len(content.encode("utf-8")), read
        assert_single_owner(sandbox, path, prefix="operation:")

    after = assert_manifest_delta(sandbox, before, 3)
    after_layers = [layer["layer_id"] for layer in after["layers"]]
    assert len(after_layers) == len(before_layers) + 3, after
    assert after_layers[3:] == before_layers, after
    assert len(set(after_layers[:3])) == 3, after


def test_blame_ladder_on_one_path_tracks_three_publish_owners(sandbox):
    """Blame ladder on one path: sessionless `file_write` creates a 3-line file
    (request A), then `file_edit` rewrites line 2 (request B), then
    `file_edit` rewrites line 3 (request C).
    Expected: `file_blame` tiles line 1 to `operation:<A>`, line 2 to
    `operation:<B>`, line 3 to `operation:<C>` as three coalesced ranges;
    `manifest_version` advanced by exactly 3."""
    path = "correctness/blame-ladder.txt"
    before = layerstack(sandbox)

    assert_ok(file_write(sandbox, path, "line-1\nline-2\nline-3"))
    owner_a = assert_single_owner(sandbox, path, prefix="operation:")
    assert_ok(file_edit(sandbox, path, [edit("line-2", "line-2-b")]))
    owner_b = owners_by_line(assert_blame_tiling(sandbox, path))[1]
    assert owner_b.startswith("operation:")
    assert owner_b != owner_a
    assert_ok(file_edit(sandbox, path, [edit("line-3", "line-3-c")]))
    owner_c = owners_by_line(assert_blame_tiling(sandbox, path))[2]
    assert owner_c.startswith("operation:")
    assert owner_c not in {owner_a, owner_b}

    assert_content(file_read(sandbox, path), "line-1\nline-2-b\nline-3-c")
    assert_blame_ranges(
        sandbox,
        path,
        [(1, 1, owner_a), (2, 1, owner_b), (3, 1, owner_c)],
    )
    assert_manifest_delta(sandbox, before, 3)


def test_identical_content_repeated_at_head_is_noop_update(sandbox):
    """Identical-content `file_write` immediately repeated on the same path
    (digest matches the head layer).
    Expected: second write returns `type = update` but publishes no layer —
    `manifest_version` and `root_hash` unchanged, layer list identical, and
    `file_blame` owners unchanged."""
    path = "correctness/head-identical.txt"
    assert_ok(file_write(sandbox, path, "same\ncontent"))
    owner = assert_single_owner(sandbox, path, prefix="operation:")
    before = layerstack(sandbox)
    before_blame = assert_blame_tiling(sandbox, path)

    result = file_write(sandbox, path, "same\ncontent")
    assert result["type"] == "update", result
    assert_content(file_read(sandbox, path), "same\ncontent")
    _assert_stack_unchanged(sandbox, before)
    assert_blame_ranges(sandbox, path, [(1, 2, owner)], before_blame)


def test_identical_content_rewrite_not_at_head_commits_but_keeps_original_owner(
    sandbox,
):
    """Identical-content rewrite not at head: write X to `a.txt` (request A),
    write to `b.txt`, then rewrite `a.txt` with byte-identical X (request C).
    Expected: the third write commits a new layer (`manifest_version` +1)
    because the head digest differs, but `file_blame a.txt` still shows every
    line owned by `operation:<A>` (all lines resolve as inherited/active, none
    as command lines of C)."""
    path = "correctness/not-head-a.txt"
    assert_ok(file_write(sandbox, path, "alpha\nbeta"))
    owner_a = assert_single_owner(sandbox, path, prefix="operation:")
    assert_ok(file_write(sandbox, "correctness/not-head-b.txt", "other"))
    assert_single_owner(sandbox, "correctness/not-head-b.txt", prefix="operation:")
    before = layerstack(sandbox)

    result = file_write(sandbox, path, "alpha\nbeta")
    assert result["type"] == "update", result
    assert_content(file_read(sandbox, path), "alpha\nbeta")
    assert_manifest_delta(sandbox, before, 1)
    assert_blame_ranges(sandbox, path, [(1, 2, owner_a)])


def test_delete_via_exec_then_recreate_via_file_op(sandbox):
    """Delete via exec then re-create via file op: one-shot
    `exec_command "rm f.txt"` publishes a whiteout layer; then sessionless
    `file_write` re-creates `f.txt`.
    Expected: after the exec, `file_read f.txt` faults `not_found`; the write
    returns `type = create`; `file_read` then returns the new content (upper
    layer resolves before the whiteout) and `file_blame` shows all lines owned
    by the new `operation:<request_id>`."""
    path = "correctness/recreate.txt"
    before = layerstack(sandbox)
    assert_ok(file_write(sandbox, path, "old\ntext"))
    old_owner = assert_single_owner(sandbox, path, prefix="operation:")

    _exec_ok(sandbox, f"rm {path}")
    assert_error(file_read(sandbox, path), "not_found")

    result = file_write(sandbox, path, "new\ntext")
    assert result["type"] == "create", result
    assert_content(file_read(sandbox, path), "new\ntext")
    new_owner = assert_single_owner(sandbox, path, prefix="operation:")
    assert new_owner != old_owner
    assert_manifest_delta(sandbox, before, 3)


def test_parent_hidden_by_whiteout_can_be_recreated(tmp_path):
    """Parent hidden by whiteout: one-shot `exec_command "rm -rf dir"` (dir has
    files in lower layers), then `file_write dir/new.txt`.
    Expected: `file_read dir/old.txt` is `not_found` (never the lower-layer
    object); the write succeeds as `type = create` with parents re-created in
    the new layer; `file_read dir/new.txt` returns the content while
    `file_read dir/old.txt` stays `not_found`."""
    with sandbox_from_workspace(tmp_path, files={"dir/old.txt": "old"}) as sandbox:
        before = layerstack(sandbox)
        _exec_ok(sandbox, "rm -rf dir")
        assert_error(file_read(sandbox, "dir/old.txt"), "not_found")

        result = file_write(sandbox, "dir/new.txt", "new")
        assert result["type"] == "create", result
        assert_content(file_read(sandbox, "dir/new.txt"), "new")
        assert_single_owner(sandbox, "dir/new.txt", prefix="operation:")
        assert_error(file_read(sandbox, "dir/old.txt"), "not_found")
        assert_manifest_delta(sandbox, before, 2)


def test_opaque_directory_hides_lower_children_and_allows_update(tmp_path):
    """Opaque directory: one-shot
    `exec_command "rm -rf dir && mkdir dir && echo fresh > dir/only.txt"` over
    a dir with lower-layer children.
    Expected: one captured layer carries `OpaqueDir(dir)` plus the write;
    `file_read dir/only.txt` returns `fresh`, every pre-existing `dir/*` path
    reads `not_found` through the merged manifest, and a
    `file_write dir/only.txt` update classifies the target as an existing
    regular file (`type = update`)."""
    files = {
        "dir/old.txt": "old",
        "dir/nested/old.txt": "nested",
    }
    with sandbox_from_workspace(tmp_path, files=files) as sandbox:
        before = layerstack(sandbox)
        _exec_ok(sandbox, "rm -rf dir && mkdir dir && printf 'fresh' > dir/only.txt")
        assert_content(file_read(sandbox, "dir/only.txt"), "fresh")
        assert_single_owner(sandbox, "dir/only.txt", prefix="workspace_session:")
        assert_error(file_read(sandbox, "dir/old.txt"), "not_found")
        assert_error(file_read(sandbox, "dir/nested/old.txt"), "not_found")
        assert_manifest_delta(sandbox, before, 1)

        before_update = layerstack(sandbox)
        result = file_write(sandbox, "dir/only.txt", "fresh update")
        assert result["type"] == "update", result
        assert_content(file_read(sandbox, "dir/only.txt"), "fresh update")
        assert_single_owner(sandbox, "dir/only.txt", prefix="operation:")
        assert_error(file_read(sandbox, "dir/old.txt"), "not_found")
        assert_manifest_delta(sandbox, before_update, 1)


@pytest.mark.slow
def test_complex_deep_whiteout_opaque_hierarchy(sandbox):
    """[complex] Deep whiteout/opaque hierarchy: seed `a/b/c/d` with files at
    each depth (one exec layer), publish `rm -rf a/b` plus a re-created
    `a/b/x/new.txt` via a second exec, then sessionless
    `file_write a/b/c/d/deep.txt`.
    Expected: reads under the old `a/b/c` subtree are `not_found` at every
    depth; `a/keep.txt` and `a/b/x/new.txt` read correctly; the final write
    returns `type = create` and only the explicitly re-created paths are
    visible in the merged view."""
    before = layerstack(sandbox)
    _exec_ok(
        sandbox,
        "mkdir -p a/b/c/d && "
        "printf 'keep' > a/keep.txt && "
        "printf 'old-b' > a/b/old-b.txt && "
        "printf 'old-c' > a/b/c/old-c.txt && "
        "printf 'old-d' > a/b/c/d/old-d.txt",
    )
    assert_single_owner(sandbox, "a/keep.txt", prefix="workspace_session:")

    _exec_ok(
        sandbox,
        "rm -rf a/b && mkdir -p a/b/x && printf 'new' > a/b/x/new.txt",
    )
    assert_content(file_read(sandbox, "a/keep.txt"), "keep")
    assert_content(file_read(sandbox, "a/b/x/new.txt"), "new")
    assert_single_owner(sandbox, "a/b/x/new.txt", prefix="workspace_session:")
    for path in ["a/b/old-b.txt", "a/b/c/old-c.txt", "a/b/c/d/old-d.txt"]:
        assert_error(file_read(sandbox, path), "not_found")

    result = file_write(sandbox, "a/b/c/d/deep.txt", "deep")
    assert result["type"] == "create", result
    assert_content(file_read(sandbox, "a/b/c/d/deep.txt"), "deep")
    assert_single_owner(sandbox, "a/b/c/d/deep.txt", prefix="operation:")
    assert_content(file_read(sandbox, "a/keep.txt"), "keep")
    assert_content(file_read(sandbox, "a/b/x/new.txt"), "new")
    for path in ["a/b/old-b.txt", "a/b/c/old-c.txt", "a/b/c/d/old-d.txt"]:
        assert_error(file_read(sandbox, path), "not_found")
    assert_manifest_delta(sandbox, before, 3)


@pytest.mark.slow
def test_complex_deep_layer_stack_multi_path(sandbox):
    """[complex] Deep layer stack, multi-path: 60 sequential sessionless
    `file_write` calls cycling over 20 paths (3 generations each).
    Expected: `manifest_version` advanced by exactly 60 with 60 new layers;
    `file_read` of every path returns its last-written generation;
    `file_blame` of each path shows all lines owned by the
    `operation:<request_id>` of its final write."""
    before = layerstack(sandbox)
    before_layers = layer_ids(sandbox)
    final_content = {}
    final_owner = {}

    for index in range(60):
        path = f"correctness/multi/path-{index % 20:02d}.txt"
        generation = index // 20 + 1
        content = f"path-{index % 20:02d}-generation-{generation}\ngeneration-{generation}"
        assert_ok(file_write(sandbox, path, content))
        final_content[path] = content
        final_owner[path] = assert_single_owner(sandbox, path, prefix="operation:")

    after = assert_manifest_delta(sandbox, before, 60)
    after_layers = [layer["layer_id"] for layer in after["layers"]]
    assert len(after_layers) == len(before_layers) + 60, after
    new_layers = set(after_layers) - set(before_layers)
    assert len(new_layers) == 60, after

    for path, content in final_content.items():
        assert_content(file_read(sandbox, path), content)
        assert_single_owner(sandbox, path, owner=final_owner[path])


@pytest.mark.slow
def test_complex_deep_layer_stack_single_path_50_line_edits(sandbox):
    """[complex] Deep layer stack, single path: `file_write` a 50-line file of
    unique markers, then 50 sequential `file_edit` calls, each replacing
    exactly one marker line.
    Expected: all 50 publishes commit (`manifest_version` +51 total); the
    final `file_read` shows all replacements applied cumulatively;
    `file_blame` maps each line to the specific `operation:<request_id>` that
    last edited it."""
    path = "correctness/single-path.txt"
    lines = [f"marker-{index:02d}" for index in range(1, 51)]
    before = layerstack(sandbox)
    assert_ok(file_write(sandbox, path, "\n".join(lines)))
    owners = owners_by_line(assert_blame_tiling(sandbox, path))

    for line_number in range(1, 51):
        old = f"marker-{line_number:02d}"
        new = f"replacement-{line_number:02d}"
        assert_ok(file_edit(sandbox, path, [edit(old, new)]))
        lines[line_number - 1] = new
        owner = owners_by_line(assert_blame_tiling(sandbox, path))[line_number - 1]
        assert owner.startswith("operation:")
        owners[line_number - 1] = owner

    assert_content(file_read(sandbox, path), "\n".join(lines))
    assert_blame_owners(sandbox, path, owners)
    assert_manifest_delta(sandbox, before, 51)


@pytest.mark.slow
def test_complex_large_file_windowed_reads_over_layer_boundary(sandbox):
    """[complex] Large file windowed reads over a layer boundary: generate a
    ~15,000-line file with ~200-byte lines in one exec-published layer, then
    sessionless `file_edit` a unique marker near line 10,000 (new layer).
    Expected: `file_read` with default `limit` faults `invalid_request`
    (selected output over the 256 KiB `MAX_OUTPUT_BYTES` cap, not
    `FileTooLarge`); `--offset 1 --limit 500` and a window over line 10,000
    succeed with correct `start_line`/`num_lines`/`next_offset`/`total_lines`,
    the edited window shows the replacement while early windows are
    byte-identical to the pre-edit read, and `file_blame` shows the edited
    line as `operation:<request_id>` with surrounding lines owned by the
    exec's `workspace_session:<id>`."""
    path = "large/window.txt"
    before = layerstack(sandbox)
    _exec_ok(
        sandbox,
        "mkdir -p large && : > large/window.txt && "
        "for i in $(seq 1 15000); do "
        "printf 'line-%05d-%0190d\\n' \"$i\" 0 >> large/window.txt; "
        "done",
        timeout=240,
    )
    assert_manifest_delta(sandbox, before, 1)
    assert_error(file_read(sandbox, path), "invalid_request")

    early_before = assert_ok(file_read(sandbox, path, offset=1, limit=500))
    assert early_before["start_line"] == 1, early_before
    assert early_before["num_lines"] == 500, early_before
    assert early_before["next_offset"] == 501, early_before
    assert early_before["total_lines"] == 15000, early_before

    old_line = f"line-10000-{'0' * 190}"
    new_line = f"line-10000-EDITED-{'1' * 183}"
    assert_ok(file_edit(sandbox, path, [edit(old_line, new_line)]))
    assert_manifest_delta(sandbox, before, 2)

    early_after = assert_ok(file_read(sandbox, path, offset=1, limit=500))
    assert early_after["content"] == early_before["content"], early_after
    window = assert_ok(file_read(sandbox, path, offset=9998, limit=5))
    assert window["start_line"] == 9998, window
    assert window["num_lines"] == 5, window
    assert window["next_offset"] == 10003, window
    assert window["total_lines"] == 15000, window
    assert new_line in window["content"], window
    assert old_line not in window["content"], window

    blame = assert_ok(file_blame(sandbox, path))
    expected_start = 1
    for item in blame["ranges"]:
        assert item["start_line"] == expected_start, blame
        assert item["line_count"] > 0, blame
        expected_start += item["line_count"]
    assert expected_start == 15001, blame
    owners = owners_by_line(blame)
    assert owners[9999].startswith("operation:"), owners[9997:10002]
    for index in [9997, 9998, 10000, 10001]:
        assert owners[index].startswith("workspace_session:"), owners[9997:10002]


@pytest.mark.slow
def test_complex_hundreds_of_files_in_one_captured_layer(sandbox):
    """[complex] Hundreds of files in one captured layer: a one-shot exec creates
    300 files across nested directories, then sessionless `file_write` updates
    3 of them.
    Expected: the exec adds exactly one layer (`manifest_version` +1, not
    +300); spot `file_read` of 10 files across the tree returns exec content;
    the 3 updates add 3 layers, and `file_blame` shows updated files owned by
    `operation:<request_id>` while untouched files stay
    `workspace_session:<id>`."""
    before = layerstack(sandbox)
    _exec_ok(
        sandbox,
        "for i in $(seq 1 300); do "
        "d=$(printf 'bulk/dir%02d/sub%02d' $((i % 10)) $((i % 5))); "
        "mkdir -p \"$d\"; "
        "printf 'exec-%03d' \"$i\" > \"$d/file-$i.txt\"; "
        "done",
        timeout=240,
    )
    assert_manifest_delta(sandbox, before, 1)

    for index in [1, 2, 3, 17, 42, 88, 101, 149, 233, 300]:
        path = _bulk_path(index)
        assert_content(file_read(sandbox, path), f"exec-{index:03d}")
        assert_single_owner(sandbox, path, prefix="workspace_session:")

    updated = [17, 149, 300]
    update_owners = {}
    for index in updated:
        path = _bulk_path(index)
        assert_ok(file_write(sandbox, path, f"updated-{index:03d}"))
        update_owners[path] = assert_single_owner(sandbox, path, prefix="operation:")

    assert_manifest_delta(sandbox, before, 4)
    for path, owner in update_owners.items():
        assert_single_owner(sandbox, path, owner=owner)
    for index in [1, 2, 3, 42, 88, 101, 233]:
        assert_single_owner(sandbox, _bulk_path(index), prefix="workspace_session:")


@pytest.mark.slow
def test_complex_mixed_history_blame_at_scale(tmp_path):
    """[complex] Mixed-history blame at scale: on one seeded 20-line base file,
    alternate 5 sessionless `file_edit` single-line replacements with 5
    one-shot exec single-line rewrites (10 publishes, disjoint lines).
    Expected: final `file_read` shows all 10 modified lines; `file_blame`
    tiles each modified line to its exact actor (`operation:<request_id>` or
    `workspace_session:<id>` respectively) and every untouched line to
    `original`."""
    path = "mixed/base.txt"
    base_lines = [f"base-{line:02d}" for line in range(1, 21)]
    with sandbox_from_workspace(tmp_path, files={path: "\n".join(base_lines)}) as sandbox:
        before = layerstack(sandbox)
        lines = list(base_lines)
        expected = ["original"] * 20
        edit_lines = [1, 3, 5, 7, 9]
        exec_lines = [2, 4, 6, 8, 10]

        for step, (edit_line, exec_line) in enumerate(zip(edit_lines, exec_lines), 1):
            edit_text = f"edit-{step:02d}"
            assert_ok(file_edit(sandbox, path, [edit(lines[edit_line - 1], edit_text)]))
            lines[edit_line - 1] = edit_text
            expected[edit_line - 1] = owners_by_line(
                assert_blame_tiling(sandbox, path)
            )[edit_line - 1]

            exec_text = f"exec-{step:02d}"
            _exec_ok(sandbox, f"sed -i '{exec_line}s/.*/{exec_text}/' {path}")
            lines[exec_line - 1] = exec_text
            expected[exec_line - 1] = owners_by_line(
                assert_blame_tiling(sandbox, path)
            )[exec_line - 1]

        assert_content(file_read(sandbox, path), "\n".join(lines))
        owners = owners_by_line(assert_blame_tiling(sandbox, path))
        assert owners == expected, owners
        assert_manifest_delta(sandbox, before, 10)


def test_first_sessionless_edit_of_workspace_base_uses_original_for_untouched(
    tmp_path,
):
    """First sessionless `file_edit` of a file that shipped in the workspace
    base, replacing one unique line.
    Expected: `file_blame` shows the changed line owned by
    `operation:<request_id>` and all untouched lines owned by `original`."""
    path = "base/edit-me.txt"
    with sandbox_from_workspace(
        tmp_path,
        files={path: "one\ntwo unique\nthree"},
    ) as sandbox:
        before = layerstack(sandbox)
        assert_ok(file_edit(sandbox, path, [edit("two unique", "TWO")]))
        assert_content(file_read(sandbox, path), "one\nTWO\nthree")
        owner = owners_by_line(assert_blame_tiling(sandbox, path))[1]
        assert owner.startswith("operation:")
        assert_blame_owners(sandbox, path, ["original", owner, "original"])
        assert_manifest_delta(sandbox, before, 1)


def test_blame_survives_deletion(sandbox):
    """Blame survives deletion: sessionless `file_write` a file (audit event
    recorded), then one-shot `exec_command "rm <path>"` publishes the
    whiteout.
    Expected: `file_read` faults `not_found`, but `file_blame` (a pure store
    read) still returns the pre-delete ranges owned by
    `operation:<request_id>` — a delete appends no audit event."""
    path = "correctness/delete-keeps-blame.txt"
    before = layerstack(sandbox)
    assert_ok(file_write(sandbox, path, "owned\nlines"))
    owner = assert_single_owner(sandbox, path, prefix="operation:")

    _exec_ok(sandbox, f"rm {path}")
    assert_error(file_read(sandbox, path), "not_found")
    blame = assert_ok(file_blame(sandbox, path))
    actual = [
        (item["start_line"], item["line_count"], item["owner"])
        for item in blame["ranges"]
    ]
    assert actual == [(1, 2, owner)], blame
    assert_manifest_delta(sandbox, before, 2)


def test_forbidden_publishes_do_not_advance_manifest(sandbox):
    """Forbidden publishes: sessionless `file_write` to `layers/evil.txt` and
    `manifest.json` (layerstack-internal paths).
    Expected: both fault `operation_failed` with publish rejection
    `protected_path`; `manifest_version`/`root_hash` are unchanged and no layer
    is added. (`.git` is no longer special-cased — see the git-policy suite.)"""
    before = layerstack(sandbox)
    _assert_publish_rejection(
        file_write(sandbox, "layers/evil.txt", "evil"),
        "protected_path",
    )
    _assert_publish_rejection(
        file_write(sandbox, "manifest.json", "{}"),
        "protected_path",
    )
    _assert_stack_unchanged(sandbox, before)


def test_gitignored_route_blame_is_wholesale(tmp_path):
    """Gitignored route: with `logs/` in the base `.gitignore`, a one-shot exec
    writes a multi-line `logs/app.log`.
    Expected: the file is committed on the `ignored` route and
    `file_read logs/app.log` returns its content, but `file_blame` returns a
    single wholesale range (`start_line = 1`, `line_count = 1`) owned by
    `workspace_session:<id>` rather than per-line tiling."""
    with sandbox_from_workspace(tmp_path, files={".gitignore": "logs/"}) as sandbox:
        before = layerstack(sandbox)
        _exec_ok(sandbox, "mkdir -p logs && printf 'a\\nb\\nc' > logs/app.log")
        assert_content(file_read(sandbox, "logs/app.log"), "a\nb\nc")
        blame = assert_ok(file_blame(sandbox, "logs/app.log"))
        assert len(blame["ranges"]) == 1, blame
        assert blame["ranges"][0]["start_line"] == 1, blame
        assert blame["ranges"][0]["line_count"] == 1, blame
        assert blame["ranges"][0]["owner"].startswith("workspace_session:"), blame
        assert_manifest_delta(sandbox, before, 1)


def test_no_change_capture_is_noop(sandbox):
    """No-change capture: one-shot `exec_command "true"` completes.
    Expected: the capture publish is a `no_op` — `manifest_version`,
    `root_hash`, and the layer list are unchanged after the command reaches
    `status = ok`, and `active_lease_count` returns to its prior value once
    the one-shot workspace is destroyed."""
    before = layerstack(sandbox)
    _exec_ok(sandbox, "true")
    after = _assert_stack_unchanged(sandbox, before)
    assert after["active_lease_count"] == before["active_lease_count"], after
