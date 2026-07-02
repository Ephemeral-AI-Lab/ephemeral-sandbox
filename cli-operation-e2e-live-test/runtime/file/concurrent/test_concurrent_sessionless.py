"""Live e2e: Concurrent Operations - Sessionless (17 cases)."""

import threading
import time
from concurrent.futures import ThreadPoolExecutor, as_completed

import pytest

from runtime.file.helpers import (
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
    layerstack,
    owners_by_line,
    run_concurrently,
    sandbox_from_workspace,
)


def _is_error(result):
    return isinstance(result, dict) and "error" in result


def _exec_ok(sandbox, command, **kwargs):
    kwargs.setdefault("yield_time_ms", 30_000)
    result = exec_command(sandbox, command, **kwargs)
    assert "error" not in result, result
    assert result["status"] == "ok", result
    assert result["exit_code"] == 0, result
    return result


def _assert_window_content(result, allowed):
    assert_ok(result)
    assert result["content"] in allowed, result
    assert result["total_bytes"] == len(result["content"].encode("utf-8")), result
    assert result["num_lines"] == len(result["content"].splitlines()), result
    return result


def _assert_no_operation_failed(results):
    for result in results:
        if _is_error(result):
            assert result["error"]["kind"] != "operation_failed", result


def test_two_concurrent_sessionless_writes_to_same_path_serialize(sandbox):
    """Two concurrent sessionless `file_write` requests to the same path with
    different multi-line contents (parallel `sandbox-cli runtime file_write`
    invocations).
    Expected: both return `type = create`/`update` with no `operation_failed`;
    the writes serialize under the `amend_path` exclusive writer lock, so a
    final `file_read` returns exactly one writer's complete content (never
    interleaved bytes) and `file_blame` tiles every line with the single
    `operation:<request_id>` owner of the last-committed writer."""
    before = layerstack(sandbox)
    path = "concurrent/same-write.txt"
    contents = ["writer-a\nline-a", "writer-b\nline-b"]

    results = run_concurrently(
        [lambda body=body: file_write(sandbox, path, body) for body in contents],
        max_workers=2,
    )

    assert {assert_ok(result)["type"] for result in results} == {"create", "update"}
    final = file_read(sandbox, path)
    _assert_window_content(final, set(contents))
    assert_single_owner(sandbox, path, prefix="operation:")
    assert_manifest_delta(sandbox, before, 2)


def test_sessionless_write_races_sessionless_edit_same_path(sandbox):
    """Concurrent sessionless `file_write` (rewriting a seeded `alpha\nbeta` file
    to `alpha\nGAMMA`) and `file_edit` (`alpha` -> `ALPHA`) on the same path.
    Expected: each op either succeeds or the edit returns `invalid_request`
    (edit not found); final content is one of the complete serialized outcomes
    (`alpha\nGAMMA` or `ALPHA\nGAMMA`), and `file_blame` owners per line match
    exactly the ops whose text survives - no torn state, no `operation_failed`."""
    path = "concurrent/write-edit.txt"
    assert_ok(file_write(sandbox, path, "alpha\nbeta"))
    before = layerstack(sandbox)

    write_result, edit_result = run_concurrently(
        [
            lambda: file_write(sandbox, path, "alpha\nGAMMA"),
            lambda: file_edit(sandbox, path, [edit("alpha", "ALPHA")]),
        ],
        max_workers=2,
    )

    assert_ok(write_result)
    if _is_error(edit_result):
        assert_error(edit_result, "invalid_request")
    else:
        assert_ok(edit_result)
    _assert_no_operation_failed([write_result, edit_result])
    final = assert_ok(file_read(sandbox, path))
    assert final["content"] in {"alpha\nGAMMA", "ALPHA\nGAMMA"}, final
    owners = owners_by_line(assert_blame_tiling(sandbox, path))
    if final["content"] == "alpha\nGAMMA":
        assert len(set(owners)) == 1 and owners[0].startswith("operation:"), owners
    else:
        assert owners[0].startswith("operation:"), owners
        assert owners[1].startswith("operation:"), owners
        assert owners[0] != owners[1], owners
    assert layerstack(sandbox)["manifest_version"] in {
        before["manifest_version"] + 1,
        before["manifest_version"] + 2,
    }


def test_two_concurrent_sessionless_edits_same_unique_old_string(sandbox):
    """Two concurrent sessionless `file_edit` requests targeting the same unique
    `old_string` on one path (`replace_all` absent).
    Expected: exactly one edit returns `type = edit` with `replacements = 1`;
    the loser reads the post-edit head under the writer lock (no OCC retry)
    and returns `invalid_request` / edit not found; final `file_read` shows a
    single replacement."""
    path = "concurrent/same-edit.txt"
    assert_ok(file_write(sandbox, path, "keep\nTOKEN\nkeep"))

    results = run_concurrently(
        [
            lambda: file_edit(sandbox, path, [edit("TOKEN", "winner-a")]),
            lambda: file_edit(sandbox, path, [edit("TOKEN", "winner-b")]),
        ],
        max_workers=2,
    )

    ok_results = [result for result in results if not _is_error(result)]
    error_results = [result for result in results if _is_error(result)]
    assert len(ok_results) == 1, results
    assert assert_ok(ok_results[0])["replacements"] == 1
    assert len(error_results) == 1, results
    assert_error(error_results[0], "invalid_request")
    final = assert_ok(file_read(sandbox, path))["content"]
    assert final in {"keep\nwinner-a\nkeep", "keep\nwinner-b\nkeep"}, final


def test_two_identical_concurrent_sessionless_writes_publish_once(sandbox):
    """Two concurrent sessionless `file_write` requests with byte-identical
    content to one new path.
    Expected: both return ok (one `type = create`, one `type = update` by
    serialization order); the second publish dedupes against the identical
    head layer (no-op), `observability layerstack` shows `manifest_version`
    advanced by exactly 1, and `file_blame` owner is the first committer's
    `operation:<request_id>`."""
    before = layerstack(sandbox)
    path = "concurrent/identical.txt"
    content = "same\ncontent"

    results = run_concurrently(
        [lambda: file_write(sandbox, path, content), lambda: file_write(sandbox, path, content)],
        max_workers=2,
    )

    assert {assert_ok(result)["type"] for result in results} == {"create", "update"}
    assert_content(file_read(sandbox, path), content)
    assert_single_owner(sandbox, path, prefix="operation:")
    assert_manifest_delta(sandbox, before, 1)


def test_concurrent_reads_race_sessionless_write_complete_snapshots(sandbox):
    """Five concurrent sessionless `file_read` requests of a seeded path racing
    one sessionless `file_write` that replaces its content.
    Expected: every read returns a complete published snapshot -
    `content`/`total_bytes` match either the whole old content or the whole
    new content, never a mix, never `operation_failed`."""
    path = "concurrent/read-race.txt"
    old = "old-a\nold-b\nold-c"
    new = "new-a\nnew-b\nnew-c"
    assert_ok(file_write(sandbox, path, old))

    results = run_concurrently(
        [lambda: file_read(sandbox, path) for _ in range(5)]
        + [lambda: file_write(sandbox, path, new)],
        max_workers=6,
    )

    read_results = results[:5]
    write_result = results[5]
    assert_ok(write_result)
    for result in read_results:
        _assert_window_content(result, {old, new})
    assert_content(file_read(sandbox, path), new)


def test_sessionless_read_races_create_of_brand_new_path(sandbox):
    """Sessionless `file_read` racing a sessionless `file_write` that creates a
    brand-new path.
    Expected: the read returns either `not_found` or the complete new content
    with correct `total_lines`/`total_bytes`; an empty-content success is
    never observed."""
    path = "concurrent/new-path.txt"
    content = "created\nbody"

    read_result, write_result = run_concurrently(
        [lambda: file_read(sandbox, path), lambda: file_write(sandbox, path, content)],
        max_workers=2,
    )

    assert_ok(write_result)
    if _is_error(read_result):
        assert_error(read_result, "not_found")
    else:
        _assert_window_content(read_result, {content})
        assert read_result["content"], read_result
    assert_content(file_read(sandbox, path), content)
    assert_single_owner(sandbox, path, prefix="operation:")


def test_two_concurrent_sessionless_writes_to_disjoint_paths(sandbox):
    """Two concurrent sessionless `file_write` requests to two disjoint paths.
    Expected: both publish independently; each `file_read` returns its full
    content, each `file_blame` shows only its own `operation:<request_id>`,
    and `observability layerstack` shows `manifest_version` advanced by
    exactly 2."""
    before = layerstack(sandbox)
    payloads = {
        "concurrent/disjoint-a.txt": "alpha\nbody",
        "concurrent/disjoint-b.txt": "beta\nbody",
    }

    results = run_concurrently(
        [
            lambda path=path, body=body: file_write(sandbox, path, body)
            for path, body in payloads.items()
        ],
        max_workers=2,
    )

    for result in results:
        assert assert_ok(result)["type"] == "create"
    for path, body in payloads.items():
        assert_content(file_read(sandbox, path), body)
        assert_single_owner(sandbox, path, prefix="operation:")
    assert_manifest_delta(sandbox, before, 2)


def test_file_blame_races_sessionless_write_same_path(sandbox):
    """`file_blame` racing a sessionless `file_write` to the same path.
    Expected: blame returns a fully tiled `ranges` set
    (`start_line`/`line_count`/`owner` covering the whole file) for either the
    pre-write or post-write state - owners drawn from
    `original`/`operation:<request_id>` - never a partially updated tiling."""
    path = "concurrent/blame-race.txt"
    assert_ok(file_write(sandbox, path, "old-1\nold-2"))

    blame_result, write_result = run_concurrently(
        [lambda: file_blame(sandbox, path), lambda: file_write(sandbox, path, "new-1\nnew-2")],
        max_workers=2,
    )

    assert_ok(write_result)
    blame = assert_ok(blame_result)
    assert sum(item["line_count"] for item in blame["ranges"]) == 2, blame
    assert [item["start_line"] for item in blame["ranges"]] == [1], blame
    for owner in owners_by_line(blame):
        assert owner == "original" or owner.startswith("operation:"), blame
    assert_content(file_read(sandbox, path), "new-1\nnew-2")
    assert_single_owner(sandbox, path, prefix="operation:")


def test_sessionless_edit_races_one_shot_exec_disjoint_line_merge(tmp_path):
    """Sessionless `file_edit` changing line 2 of a seeded 4-line file racing a
    one-shot `exec_command` (no `--workspace-session-id`) whose shell command
    sed-edits line 4, so the exec-owned session capture publish three-way
    merges with the amend commit.
    Expected: regardless of commit order both changes land; final `file_read`
    shows line 2 and line 4 changed; `file_blame` shows line 2 owned by
    `operation:<request_id>`, line 4 owned by
    `workspace_session:<one-shot-id>`, and untouched lines `original`."""
    path = "concurrent/merge.txt"
    with sandbox_from_workspace(tmp_path, {path: "one\ntwo\nthree\nfour"}) as sandbox:
        before = layerstack(sandbox)
        edit_result, exec_result = run_concurrently(
            [
                lambda: file_edit(sandbox, path, [edit("two", "TWO")]),
                lambda: _exec_ok(sandbox, "sed -i '4s/four/FOUR/' concurrent/merge.txt"),
            ],
            max_workers=2,
        )

        assert_ok(edit_result)
        assert_ok(exec_result)
        assert_content(file_read(sandbox, path), "one\nTWO\nthree\nFOUR")
        owners = owners_by_line(assert_blame_tiling(sandbox, path))
        assert owners[0] == "original", owners
        assert owners[1].startswith("operation:"), owners
        assert owners[2] == "original", owners
        assert owners[3].startswith("workspace_session:"), owners
        assert_manifest_delta(sandbox, before, 2)


def test_sessionless_write_beats_conflicting_one_shot_capture(tmp_path):
    """Sessionless `file_write` wholesale-rewriting a seeded single-line file
    racing a one-shot `exec_command` that wholesale-rewrites the same file
    with different content (merge-ineligible capture).
    Expected: the amend commit always wins - if the capture publishes second
    its three-way merge conflicts and the publish is dropped as
    `source_conflict` (never surfaced; `exec_command` still reports
    `status = ok`); final content is the sessionless write's payload in both
    orderings and `file_blame` shows only `operation:<request_id>`."""
    path = "concurrent/conflict.txt"
    with sandbox_from_workspace(tmp_path, {path: "base"}) as sandbox:
        before = layerstack(sandbox)
        write_result, exec_result = run_concurrently(
            [
                lambda: file_write(sandbox, path, "amend"),
                lambda: _exec_ok(sandbox, "printf exec > concurrent/conflict.txt"),
            ],
            max_workers=2,
        )

        assert_ok(write_result)
        assert_ok(exec_result)
        assert_content(file_read(sandbox, path), "amend")
        assert_single_owner(sandbox, path, prefix="operation:")
        assert layerstack(sandbox)["manifest_version"] in {
            before["manifest_version"] + 1,
            before["manifest_version"] + 2,
        }


@pytest.mark.slow
def test_complex_twenty_sessionless_writes_to_one_path(sandbox):
    """[complex] 20+ concurrent sessionless `file_write` requests to one path,
    each with a unique multi-line payload.
    Expected: all 20 return ok; final `file_read` returns exactly one writer's
    complete payload, `file_blame` tiles the file with that single
    `operation:<request_id>`, and `observability layerstack` shows
    `manifest_version` advanced by exactly 20 (every serialized commit is a
    full layer, no partial publish)."""
    before = layerstack(sandbox)
    path = "concurrent/hot-write.txt"
    payloads = {f"writer-{index:02d}\nbody-{index:02d}" for index in range(20)}

    results = run_concurrently(
        [lambda body=body: file_write(sandbox, path, body) for body in payloads],
        max_workers=20,
    )

    for result in results:
        assert_ok(result)
    _assert_window_content(file_read(sandbox, path), payloads)
    assert_single_owner(sandbox, path, prefix="operation:")
    assert_manifest_delta(sandbox, before, 20)


@pytest.mark.slow
def test_complex_hundred_sessionless_writes_to_distinct_fanout_paths(sandbox):
    """[complex] 100 concurrent sessionless `file_write` requests, each creating
    its own distinct path under one directory (`fanout/file-<i>.txt`).
    Expected: all 100 return `type = create`; 100 subsequent `file_read` calls
    each return the correct full content, per-path `file_blame` owner matches
    each writer's `operation:<request_id>`, and layer count in
    `observability layerstack` grows by exactly 100 with the daemon still
    serving requests."""
    before = layerstack(sandbox)
    payloads = {
        f"fanout/file-{index:03d}.txt": f"fanout-{index:03d}\nbody"
        for index in range(1, 101)
    }

    results = run_concurrently(
        [
            lambda path=path, body=body: file_write(sandbox, path, body)
            for path, body in payloads.items()
        ],
        max_workers=20,
    )

    for result in results:
        assert assert_ok(result)["type"] == "create"
    for path, body in payloads.items():
        assert_content(file_read(sandbox, path), body)
        assert_single_owner(sandbox, path, prefix="operation:")
    assert_manifest_delta(sandbox, before, 100)


@pytest.mark.slow
def test_complex_mixed_fanout_hot_path_writes_reads_and_blame(sandbox):
    """[complex] Mixed fan-out on one hot path: 10 sessionless writers (unique
    disjoint payloads), 10 sessionless readers, and 10 `file_blame` calls all
    launched concurrently.
    Expected: every read returns one complete committed payload
    (`total_bytes` matches exactly one payload), every blame response is a
    complete single-owner tiling drawn from `original` or one writer's
    `operation:<request_id>`, no request returns `operation_failed`, and the
    final state matches the last-committed writer."""
    path = "concurrent/mixed-hot.txt"
    initial = "seed-a\nseed-b"
    assert_ok(file_write(sandbox, path, initial))
    payloads = {f"payload-{index:02d}\nline-{index:02d}" for index in range(10)}

    calls = [lambda body=body: ("write", file_write(sandbox, path, body)) for body in payloads]
    calls += [lambda: ("read", file_read(sandbox, path)) for _ in range(10)]
    calls += [lambda: ("blame", file_blame(sandbox, path)) for _ in range(10)]
    results = run_concurrently(calls, max_workers=30)

    allowed_content = payloads | {initial}
    for kind, result in results:
        assert not (_is_error(result) and result["error"]["kind"] == "operation_failed"), result
        if kind == "write":
            assert_ok(result)
        elif kind == "read":
            _assert_window_content(result, allowed_content)
        else:
            blame = assert_ok(result)
            assert sum(item["line_count"] for item in blame["ranges"]) == 2, blame
            owners = set(owners_by_line(blame))
            assert len(owners) == 1, blame
            owner = next(iter(owners))
            assert owner.startswith("operation:"), blame

    final = assert_ok(file_read(sandbox, path))["content"]
    assert final in payloads, final
    assert_single_owner(sandbox, path, prefix="operation:")


@pytest.mark.slow
def test_complex_large_same_path_writes_race_windowed_reads(sandbox):
    """[complex] Two concurrent sessionless `file_write` requests each carrying a
    distinct ~1 MiB multi-line payload to the same path, racing five windowed
    `file_read` requests (`--offset`/`--limit` 50-line windows).
    Expected: every windowed read succeeds (no `OutputTooLarge` for the small
    window) and reports `total_bytes` equal to exactly one full payload's size
    with window content from that same payload - never bytes from both; the
    final read and wholesale `file_blame` owner match one writer."""
    path = "concurrent/large-hot.txt"
    line_a = "A" * 200
    line_b = "B" * 200
    line_c = "C" * 200
    payload_a = "\n".join(f"{index:05d}-{line_a}" for index in range(1, 4501))
    payload_b = "\n".join(f"{index:05d}-{line_b}" for index in range(1, 4501))
    payload_c = "\n".join(f"{index:05d}-{line_c}" for index in range(1, 4501))
    payloads = {payload_a, payload_b}
    assert_ok(file_write(sandbox, path, payload_c, timeout=240))
    before = layerstack(sandbox)

    results = run_concurrently(
        [
            lambda: file_write(sandbox, path, payload_a, timeout=240),
            lambda: file_write(sandbox, path, payload_b, timeout=240),
        ]
        + [lambda: file_read(sandbox, path, offset=200, limit=50) for _ in range(5)],
        max_workers=7,
    )

    for result in results[:2]:
        assert_ok(result)
    complete_payload_sizes = {len(payload.encode("utf-8")) for payload in payloads | {payload_c}}
    for result in results[2:]:
        assert_ok(result)
        assert result["total_bytes"] in complete_payload_sizes
        lines = result["content"].splitlines()
        assert lines
        assert all(line.endswith(line_a) for line in lines) or all(
            line.endswith(line_b) for line in lines
        ) or all(
            line.endswith(line_c) for line in lines
        ), result
    final = assert_ok(file_read(sandbox, path, offset=1, limit=50))
    assert final["total_bytes"] in {len(payload.encode("utf-8")) for payload in payloads}
    final_lines = final["content"].splitlines()
    assert all(line.endswith(line_a) for line in final_lines) or all(
        line.endswith(line_b) for line in final_lines
    ), final
    blame = assert_ok(file_blame(sandbox, path))
    assert sum(item["line_count"] for item in blame["ranges"]) == 4500, blame
    owners = set(owners_by_line(blame))
    assert len(owners) == 1, blame
    assert next(iter(owners)).startswith("operation:"), blame
    assert_manifest_delta(sandbox, before, 2)


@pytest.mark.slow
def test_complex_twenty_disjoint_sessionless_edits_one_seeded_file(sandbox):
    """[complex] Seed one file with 20 unique tokens on separate lines, then run
    20 concurrent sessionless `file_edit` requests, each replacing only its
    own token.
    Expected: all 20 return `replacements = 1` (each edit applies to the
    evolving head under the writer lock and its token is untouched by the
    other edits); final content contains all 20 replacements, `file_blame`
    shows each edited line owned by its own `operation:<request_id>`, and
    `manifest_version` advances by exactly 20."""
    path = "concurrent/tokens.txt"
    lines = [f"token-{index:02d}" for index in range(1, 21)]
    assert_ok(file_write(sandbox, path, "\n".join(lines)))
    before = layerstack(sandbox)

    results = run_concurrently(
        [
            lambda index=index: file_edit(
                sandbox,
                path,
                [edit(f"token-{index:02d}", f"edited-{index:02d}")],
            )
            for index in range(1, 21)
        ],
        max_workers=20,
    )

    for result in results:
        assert assert_ok(result)["replacements"] == 1
    expected = "\n".join(f"edited-{index:02d}" for index in range(1, 21))
    assert_content(file_read(sandbox, path), expected)
    owners = owners_by_line(assert_blame_tiling(sandbox, path))
    assert len(set(owners)) == 20, owners
    assert all(owner.startswith("operation:") for owner in owners), owners
    assert_manifest_delta(sandbox, before, 20)


@pytest.mark.slow
def test_complex_forty_way_disjoint_exec_and_file_write_race(sandbox):
    """[complex] 40-way disjoint race: 20 one-shot `exec_command` invocations
    each shell-writing its own path concurrently with 20 sessionless
    `file_write` requests to 20 other paths.
    Expected: all 40 paths are subsequently readable with complete content;
    `file_blame` owners split by origin - `workspace_session:<id>` (20
    distinct one-shot session owners) for exec-created paths and
    `operation:<request_id>` for file-op paths; every capture publish commits
    (disjoint paths, no `source_conflict`)."""
    before = layerstack(sandbox)
    exec_paths = {
        f"race/exec-{index:02d}.txt": f"exec-{index:02d}"
        for index in range(1, 21)
    }
    write_paths = {
        f"race/write-{index:02d}.txt": f"write-{index:02d}"
        for index in range(1, 21)
    }

    calls = [
        lambda path=path, body=body: _exec_ok(
            sandbox,
            f"mkdir -p race && printf {body} > {path}",
            timeout=240,
        )
        for path, body in exec_paths.items()
    ]
    calls += [
        lambda path=path, body=body: file_write(sandbox, path, body)
        for path, body in write_paths.items()
    ]
    results = run_concurrently(calls, max_workers=20)

    for result in results:
        assert_ok(result)
    exec_owners = set()
    for path, body in exec_paths.items():
        assert_content(file_read(sandbox, path), body)
        exec_owners.add(assert_single_owner(sandbox, path, prefix="workspace_session:"))
    assert len(exec_owners) == 20, exec_owners
    for path, body in write_paths.items():
        assert_content(file_read(sandbox, path), body)
        assert_single_owner(sandbox, path, prefix="operation:")
    assert_manifest_delta(sandbox, before, 40)


@pytest.mark.slow
def test_complex_sustained_hot_path_churn_with_layerstack_poller(sandbox):
    """[complex] Sustained hot-path churn: 5 concurrent workers each issue 10
    sequential sessionless `file_write` requests with unique contents to one
    path (50 writes) while a poller repeatedly calls
    `sandbox-cli observability layerstack --sandbox-id ID`.
    Expected: all 50 writes return ok; the poller's `manifest_version` samples
    are strictly non-decreasing and finish at baseline + 50; the final
    `file_read` and `file_blame` owner match exactly one of the 50 request
    ids."""
    before = layerstack(sandbox)
    baseline = before["manifest_version"]
    path = "concurrent/churn.txt"
    payloads = set()
    stop = threading.Event()

    def worker(worker_id):
        results = []
        for iteration in range(10):
            body = f"worker-{worker_id}-iteration-{iteration}\nbody-{worker_id}-{iteration}"
            payloads.add(body)
            results.append(file_write(sandbox, path, body))
        return results

    def poller():
        samples = []
        while not stop.is_set():
            samples.append(layerstack(sandbox)["manifest_version"])
            time.sleep(0.01)
        samples.append(layerstack(sandbox)["manifest_version"])
        return samples

    with ThreadPoolExecutor(max_workers=6) as executor:
        poll_future = executor.submit(poller)
        worker_futures = [executor.submit(worker, index) for index in range(5)]
        worker_results = []
        try:
            for future in as_completed(worker_futures):
                worker_results.extend(future.result())
        finally:
            stop.set()
        samples = poll_future.result()

    for result in worker_results:
        assert_ok(result)
    assert all(earlier <= later for earlier, later in zip(samples, samples[1:])), samples
    assert samples[-1] == baseline + 50, samples
    final = assert_ok(file_read(sandbox, path))["content"]
    assert final in payloads, final
    assert_single_owner(sandbox, path, prefix="operation:")
    assert_manifest_delta(sandbox, before, 50)
