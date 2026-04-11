# Corrective Fast Path

Use this reference only when the validator packet already names exact failing pytest ids and exact existing owner files.

## Workflow

1. Must start with `ci_scoped_status(scope_paths=[...])` on the exact owner surface.
2. Must confirm the owner surface is still live.
3. May use `inspect_inherited_context(scope_paths=[...])` to confirm a same-run shared brief on that exact owner surface before asking Atlas or a new scout.
4. Must draft corrective JSON as soon as the failing cluster, owner surface, and retry target are clear.

## Rules

- Must keep owner paths exact.
- May carry one exact missing import-path file when the parent package already exists live.
- If a narrowed pytest node is missing but the parent packet still owns the exact benchmark file, keep the retry surface on that file path.
- If the validator packet already names the live benchmark file and only the current verify command is wrong, correct the retry target and stop.
- Never reopen benchmark test bodies, decorators, parametrization markers, or shared plumbing to re-derive semantics.
- Never merge distinct corrective clusters into one item.
- If the validator failed before the target collected, must keep that failing command visible and route the correction toward the shared owner or runtime-control surface.
- If the same owner cluster was already reopened once and is still clear, must emit JSON now.
- If inherited context for that owner already drifted, must refresh the scoped packet before trusting or re-sharing it.

## Few-shot examples
- Example: the validator packet says `tests/test_hdf.py` fails on `from pkg._compat import X` and live structure shows `pkg/` exists.
  The corrective target may be `pkg/_compat.py`.
  Do not reopen the test body to rediscover the same import failure.
- Example: the validator command never reaches the named test because `pkg/__init__.py` crashes during collection on a missing symbol.
  Keep that exact command in the corrective payload and replan toward the shared import owner.
  Do not "fix" the issue by deleting the failing verification step.
- Example: the validator packet says `pytest pkg/utils.py -x -q` collected zero tests, and the same packet still names `pkg/tests/test_utils_dataframe.py` as the live benchmark file.
  Keep `owned_files=["pkg/utils.py"]`, switch `verify` to `pytest pkg/tests/test_utils_dataframe.py -x -q`, and stop.
  Do not reopen the benchmark test body, and do not escalate to `benchmark_surface_mismatch`.
- Example: the failing owner is still `pkg/groupby.py`, and a same-run shared brief already exists for that file.
  Call `inspect_inherited_context(scope_paths=["pkg/groupby.py"])` once. If it is still fresh, reuse that owner map and emit corrective JSON.
  If it drifted, refresh with `ci_scoped_status(...)` before deciding whether more exploration is necessary.
