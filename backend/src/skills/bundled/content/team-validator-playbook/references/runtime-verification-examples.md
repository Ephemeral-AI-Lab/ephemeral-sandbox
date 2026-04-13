# Runtime Verification Examples
Use this reference before the first `daytona_codeact` verification command on a benchmark lane.

## Rules
- Must run the exact payload command through `shell("...")` inside `daytona_codeact`.
- Must print or persist the first command's exit code and relevant output from that same `shell(...)` run.
- If the exact payload command exits `0`, must return PASS from that evidence.
- Must treat wrapper success, manifest output, and `__CODEX_EXIT_CODE__` as wrapper health only; the verdict comes from `shell(...)["exit_code"]`.
- If output capture is awkward, may redirect inside `shell("...")` and inspect the saved file with a structured read.
- Must not switch to `subprocess.run(...)`, `subprocess.Popen(...)`, or helper Python wrappers just because `shell(...)` output is short.
- Must not rerun a green command with `--collect-only`, `ls`, or extra probes to "confirm" the pass.
- Must not rerun a failing broad regression command once it already printed the failing ids or original-message producer you need.
- After one exact-command `transient_runtime` failure with no failing ids, may shard only the same owned payload targets into disjoint equivalent chunks.
- Must turn the first red run into a root-cause packet with `phase`, `boundary`, and `next_question`. Never replace that packet with vibes.

## Few-shot examples
- Example: payload verify is `pytest dask/tests/test_cli.py dask/tests/test_compatibility.py --continue-on-collection-errors -n0 -rA --color=no`.
  Run `result = shell("pytest dask/tests/test_cli.py dask/tests/test_compatibility.py --continue-on-collection-errors -n0 -rA --color=no", timeout=600)`. Print `result["exit_code"]` plus a short stdout or stderr tail from that same run, then decide PASS or FAIL.
- Example: the first `daytona_codeact` attempt is rejected because you wrote `subprocess.run(...)`.
  Retry once with `result = shell("pytest ...", timeout=180)` and use that shell result for the verdict.
  Do not inspect the rejection and then keep using Python process wrappers.
- Example: the exact broad command fails during collection with `ImportError: cannot import name foo from pkg.bar`.
  Report `{"phase":"collection","boundary":"pkg.bar","next_question":"which import edge owns foo?"}` from the first run.
  Do not narrow to a single test just to get cleaner text.
- Example: the shell output contains `Killed`, `timeout`, or an inline `EXIT_CODE=137`, but the outer tool still says `status: ok`.
  Return the runtime verdict from the shell exit code and that same output tail.
  Do not call PASS from the wrapper manifest.
- Example: the exact payload suite is `pytest pkg/tests/test_big_file.py -q`, it runs hundreds of cases, and dies with `SIGKILL` before printing any failing ids.
  Keep that first run as the runtime evidence, then shard only `pkg/tests/test_big_file.py` into disjoint equivalent chunks on the retry path.
  PASS only if every shard passes; if any shard fails, return that shard's exact failure evidence.

## Large test suites (>10 tests or known-slow modules)

When the payload command runs a broad test suite that may exceed 5 minutes, use `background=true` on `daytona_codeact` to avoid hitting the exec timeout.

### Step 1 — Launch as background

```python
daytona_codeact(
  code='result = shell("pytest tests/ --continue-on-collection-errors -n0 -rA --color=no", timeout=900)\nprint(result["exit_code"])\nprint(result["stdout"][-2000:])',
  background=true
)
```
The engine returns immediately with a `task_id` (e.g. `"bg_1"`). Save it.

### Step 2 — Poll with check_background_progress

```python
check_background_progress(task_id="bg_1", last_n_lines=20)
```
- Non-blocking. Returns current status (`running`, `completed`, `failed`, `cancelled`) and recent output lines.
- Call this periodically while doing other work. Do not tight-loop — space checks out.
- You **must** call this at least once before `wait_for_background_task` will accept the task.

### Step 3 — Collect with wait_for_background_task

```python
wait_for_background_task(task_id="bg_1", timeout=120)
```
- Blocks server-side up to `timeout` seconds (max 300s). Use only when no foreground work remains.
- If the test suite runs longer than 300s, do **not** set `timeout=900` — it caps at 300. Instead, alternate between `check_background_progress` and short `wait_for_background_task(timeout=120)` calls until status is `completed`.

### Step 4 — Cancel if stuck

```python
cancel_background_task(task_id="bg_1", reason="test suite hung after 10 minutes")
```
- Requires the exact `task_id` — `"all"` is not accepted.
- Use `"auto"` only when exactly one task is running.
- After cancellation, partial output is still available via `check_background_progress`.

### Rules
- The `shell()` timeout default is 900s (15 min). For suites known to run longer, pass an explicit `timeout=` to `shell()`.
- Do not fall back to `subprocess.run(...)` or `subprocess.Popen(...)` to work around timeouts — use `shell()` with a higher timeout or background execution.
- Do not call `wait_for_background_task` immediately after launch — the engine will reject it. Do other work or call `check_background_progress` first.
