# Edge-Case Merge Load Experimental Report

## Summary

This experiment evaluated the new sandbox API edge-case merge load suite for
deletes, renames, file/directory replacement, symlink mode changes, and
package-lock style same-file semantics. The suite is separate from the general
`test_load.py` suite and focuses on correctness-sensitive filesystem shape
changes at concurrency levels `3` and `5`.

The result is healthy for the target workload:

- all 6 tests passed
- all shell edge cases completed under 212 ms p95 at concurrency 5
- expected conflicts produced exactly one successful commit and
  `aborted_version` losers
- package-lock full-file shell writes did not silently merge
- package-lock edit-based same-file dependency updates merged successfully
- no async resume-delay regression appeared in these runs

The slowest case was file-to-directory replacement at c=5:
`api.shell.total_s` p95 was 211.5 ms. The cost was real overlay and OCC work,
not process execution or event-loop resume delay.

## Scope

New test suite:

```text
backend/tests/test_sandbox/test_api/test_shell_merge_edge_cases_load.py
```

Covered cases:

| Area | Success Case | Conflict Case |
|---|---|---|
| Delete | Concurrent deletes on disjoint files | Concurrent delete of the same file |
| Rename | Concurrent renames from disjoint sources | Concurrent rename from the same source |
| Dir/file replacement | File -> directory and directory -> file | Not a same-path conflict suite |
| Symlink mode changes | File -> symlink and symlink -> file | Not a same-path conflict suite |
| Package-lock shell writes | Not expected to merge | Concurrent full-file writes conflict |
| Package-lock edit merge | Concurrent dependency-slot edits merge | Not a conflict case |

The package-lock split is intentional. Shell capture sees full-file writes, so
same-file shell updates must conflict rather than silently perform a semantic
JSON merge. The edit API can merge independent exact-anchor edits in the same
file because each edit is re-applied against the current active content.

## Test Command

Final run:

```bash
uv run pytest backend/tests/test_sandbox/test_api/test_shell_merge_edge_cases_load.py -q \
  --log-cli-level=INFO \
  --log-cli-format='%(message)s' \
  --log-file=/tmp/eos_shell_merge_edge_cases_load_final.log \
  --log-file-level=INFO \
  --log-file-format='%(message)s'
```

Result:

```text
6 passed in 3.20s
```

Reference log:

```text
/tmp/eos_shell_merge_edge_cases_load_final.log
```

## Performance Results

### Batch-level latency

| Case | Concurrency | Successes | Expected Conflicts | Wall | p50 | p95 | Parallel Factor |
|---|---:|---:|---:|---:|---:|---:|---:|
| delete_disjoint | 3 | 3 | 0 | 103.0 ms | 101.8 ms | 102.7 ms | 2.97 |
| delete_conflict | 3 | 1 | 2 | 100.8 ms | 97.9 ms | 100.4 ms | 2.92 |
| delete_disjoint | 5 | 5 | 0 | 147.0 ms | 145.6 ms | 146.4 ms | 4.92 |
| delete_conflict | 5 | 1 | 4 | 125.2 ms | 123.8 ms | 125.0 ms | 4.92 |
| rename_disjoint | 3 | 3 | 0 | 104.9 ms | 104.4 ms | 104.5 ms | 2.98 |
| rename_conflict | 3 | 1 | 2 | 116.3 ms | 111.3 ms | 115.7 ms | 2.89 |
| rename_disjoint | 5 | 5 | 0 | 185.8 ms | 178.4 ms | 184.9 ms | 4.76 |
| rename_conflict | 5 | 1 | 4 | 167.3 ms | 163.6 ms | 166.4 ms | 4.87 |
| file_to_dir | 3 | 3 | 0 | 115.9 ms | 115.1 ms | 115.3 ms | 2.98 |
| dir_to_file | 3 | 3 | 0 | 113.4 ms | 112.7 ms | 113.1 ms | 2.94 |
| file_to_dir | 5 | 5 | 0 | 212.6 ms | 210.7 ms | 211.8 ms | 4.87 |
| dir_to_file | 5 | 5 | 0 | 178.1 ms | 176.8 ms | 177.3 ms | 4.91 |
| file_to_symlink | 3 | 3 | 0 | 106.4 ms | 105.7 ms | 105.8 ms | 2.97 |
| symlink_to_file | 3 | 3 | 0 | 105.4 ms | 105.0 ms | 105.0 ms | 2.98 |
| file_to_symlink | 5 | 5 | 0 | 179.1 ms | 178.0 ms | 178.6 ms | 4.97 |
| symlink_to_file | 5 | 5 | 0 | 173.5 ms | 171.9 ms | 173.0 ms | 4.86 |
| package_lock_conflict | 3 | 1 | 2 | 91.1 ms | 90.4 ms | 90.6 ms | 2.97 |
| package_lock_conflict | 5 | 1 | 4 | 106.6 ms | 105.4 ms | 106.1 ms | 4.88 |
| package_lock_dependency_merge | 3 | 3 | 0 | 5.2 ms | 3.4 ms | 4.8 ms | 1.95 |
| package_lock_dependency_merge | 5 | 5 | 0 | 9.1 ms | 6.3 ms | 8.4 ms | 3.23 |

### Shell timing split at c=5

| Case | API Total p95 | Overlay p95 | OCC Apply p95 | Mount p95 | Command p95 | Capture p95 |
|---|---:|---:|---:|---:|---:|---:|
| delete_disjoint | 146.4 ms | 138.6 ms | 20.8 ms | 43.9 ms | 81.1 ms | 9.7 ms |
| delete_conflict | 124.9 ms | 119.5 ms | 14.1 ms | 33.5 ms | 81.1 ms | 4.9 ms |
| rename_disjoint | 184.9 ms | 170.1 ms | 25.4 ms | 48.4 ms | 78.3 ms | 35.0 ms |
| rename_conflict | 166.4 ms | 157.3 ms | 16.0 ms | 45.3 ms | 81.1 ms | 28.4 ms |
| file_to_dir | 211.5 ms | 199.3 ms | 37.0 ms | 79.7 ms | 77.8 ms | 30.7 ms |
| dir_to_file | 177.3 ms | 170.4 ms | 14.1 ms | 56.5 ms | 79.3 ms | 30.5 ms |
| file_to_symlink | 178.5 ms | 171.4 ms | 8.5 ms | 63.8 ms | 79.4 ms | 29.4 ms |
| symlink_to_file | 173.0 ms | 159.5 ms | 16.5 ms | 42.5 ms | 85.1 ms | 30.2 ms |
| package_lock_conflict | 106.1 ms | 101.2 ms | 11.2 ms | 15.0 ms | 79.4 ms | 5.2 ms |

The shell commands intentionally include `sleep 0.05`, so the command p95 has a
hard floor near 50 ms. The observed c=5 command p95 is about 78 to 85 ms. The
remaining wall time is primarily snapshot materialization, upperdir capture, and
OCC publication.

### Package-lock edit merge timing

| Concurrency | API Edit p95 | OCC Apply p95 | OCC Commit p95 | Lock Wait p95 | Publish p95 |
|---|---:|---:|---:|---:|---:|
| 3 | 4.8 ms | 4.8 ms | 4.0 ms | 2.6 ms | 0.8 ms |
| 5 | 8.4 ms | 8.4 ms | 7.1 ms | 6.0 ms | 1.0 ms |

The edit merge case is much faster than shell because it does not mount or
capture an overlay. The lower parallel factor is expected for such short
operations: serialized commit lock waiting is a larger fraction of the total
runtime.

## Bottleneck Assessment

Performance is acceptable for this edge-case suite.

The shell path does not show the earlier async resume-delay bottleneck. At c=5,
all shell cases are below 212 ms p95, and parallel factors stay close to the
requested concurrency: about 4.76 to 4.97 for successful shell batches.

The slowest path is file-to-directory replacement. Its c=5 p95 split is:

```text
api.shell.total_s          211.5 ms
api.shell.overlay_s        199.3 ms
api.shell.occ_apply_s       37.0 ms
overlay.mount_snapshot_s    79.7 ms
overlay.run_command_s       77.8 ms
overlay.capture_changes_s   30.7 ms
```

This is a real filesystem-shape workload. It exercises lower materialization,
replacement capture, whiteout handling, and layer publication. The numbers are
reasonable for a correctness-heavy local overlay simulation and do not point to
a scheduler or async bridge issue.

Conflict cases are also inexpensive. Same-path delete, rename, and package-lock
full-file write conflicts all complete with one winner and expected losers. The
serial batch size is usually `1` in conflict cases, which is correct because
the path groups cannot be merged as disjoint changes.

## Correctness Findings

The new suite found a real replacement bug before the final passing run:

```text
directory -> file replacement captured changed path as case/dir/dir
```

The fix made three behaviors explicit:

- upperdir diff capture skips lower-child whiteouts when a new file payload
  replaces an ancestor path
- merged reads treat a file or symlink ancestor in a newer layer as shadowing
  lower descendants
- layer publication and materialization remove same-path directory/file
  collisions before writing the replacement

Changed implementation files:

```text
backend/src/sandbox/overlay/capture/upperdir.py
backend/src/sandbox/layer_stack/merged_view.py
backend/src/sandbox/layer_stack/publisher.py
```

## Validation

Commands run after the implementation:

```bash
uv run pytest backend/tests/test_sandbox/test_api/test_shell_merge_edge_cases_load.py -q \
  --log-cli-level=INFO \
  --log-cli-format='%(message)s' \
  --log-file=/tmp/eos_shell_merge_edge_cases_load_final.log \
  --log-file-level=INFO \
  --log-file-format='%(message)s'

uv run pytest backend/tests/test_sandbox/test_overlay/test_upperdir_capture.py \
  backend/tests/test_sandbox/test_layer_stack -q

uv run ruff check \
  backend/src/sandbox/overlay/capture/upperdir.py \
  backend/src/sandbox/layer_stack/merged_view.py \
  backend/src/sandbox/layer_stack/publisher.py \
  backend/tests/test_sandbox/test_api/test_shell_merge_edge_cases_load.py

uv run python -m py_compile \
  backend/src/sandbox/overlay/capture/upperdir.py \
  backend/src/sandbox/layer_stack/merged_view.py \
  backend/src/sandbox/layer_stack/publisher.py \
  backend/tests/test_sandbox/test_api/test_shell_merge_edge_cases_load.py

uv run pytest --collect-only \
  backend/tests/test_sandbox/test_api/test_shell_merge_edge_cases_load.py -q

git diff --check -- \
  backend/src/sandbox/overlay/capture/upperdir.py \
  backend/src/sandbox/layer_stack/merged_view.py \
  backend/src/sandbox/layer_stack/publisher.py \
  backend/tests/test_sandbox/test_api/test_shell_merge_edge_cases_load.py
```

Results:

```text
edge-case load suite                                  6 passed in 3.20s
upperdir capture + layer-stack focused unit tests    25 passed in 0.20s
ruff                                                  passed
py_compile                                            passed
collect-only                                          6 tests collected
git diff --check                                      passed
```

## Conclusion

The edge-case merge behavior is sound for the tested c=3 and c=5 workloads.
Performance is within the expected range for shell-backed filesystem mutation
tests, and the only observed slow path is actual overlay/capture/publish work
for file/directory replacement. The package-lock semantics are also correctly
split: shell full-file updates conflict, while edit API same-file dependency
updates can merge when their anchors are independent.
