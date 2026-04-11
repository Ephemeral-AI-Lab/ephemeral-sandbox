# Runtime Verification Examples
Use this reference before the first `daytona_codeact` verification command on a benchmark lane.
## Rules
- Must run the exact payload command through `shell("...")` inside `daytona_codeact`.
- Must print or persist the first command's exit code and relevant output from that same `shell(...)` run.
- If the exact payload command exits `0`, must return PASS from that evidence.
- If output capture is awkward, may redirect inside `shell("...")` and inspect the saved file with a structured read.
- Must not switch to `subprocess.run(...)`, `subprocess.Popen(...)`, or helper Python wrappers just because `shell(...)` output is short.
- Must not rerun a green command with `--collect-only`, `ls`, or extra probes to "confirm" the pass.
- Must not rerun a failing broad regression command once it already printed the failing ids you need.
- After one exact-command `transient_runtime` failure with no failing ids, may shard only the same owned payload targets into disjoint equivalent chunks.
- Must turn the first red run into a root-cause packet with `phase`, `boundary`, and `next_question`. Never replace that packet with vibes.
## Few-shot examples
- Example: payload verify is `pytest dask/tests/test_cli.py dask/tests/test_compatibility.py --continue-on-collection-errors -n0 -rA --color=no`.
  Run `result = shell("pytest dask/tests/test_cli.py dask/tests/test_compatibility.py --continue-on-collection-errors -n0 -rA --color=no", timeout=180)`.
  Print `result.exit_code` plus a short stdout or stderr tail from that same run, then decide PASS or FAIL.
- Example: the first `daytona_codeact` attempt is rejected because you wrote `subprocess.run(...)`.
  Retry once with `result = shell("pytest ...", timeout=180)` and use that shell result for the verdict.
  Do not inspect the rejection and then keep using Python process wrappers.
- Example: the exact broad command fails during collection with `ImportError: cannot import name foo from pkg.bar`.
  Report `phase=collection`, boundary `pkg.bar`, and the next corrective question around that import edge from the first run.
  Do not narrow to a single test just to get cleaner text.
- Example: the exact payload suite is `pytest pkg/tests/test_big_file.py -q`, it runs hundreds of cases, and dies with `SIGKILL` before printing any failing ids.
  Keep that first run as the runtime evidence, then shard only `pkg/tests/test_big_file.py` into disjoint equivalent chunks on the retry path.
  PASS only if every shard passes; if any shard fails, return that shard's exact failure evidence.
