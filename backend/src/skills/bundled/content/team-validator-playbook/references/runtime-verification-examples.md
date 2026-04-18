# Runtime Verification Examples

Use this reference before the first `daytona_codeact` verification command on a benchmark lane.

## Task/Goal

- You are about to run the first benchmark-lane verification command.

## Avoid

- Must not switch to `subprocess.run(...)`, `subprocess.Popen(...)`, or helper Python wrappers just because direct command output is short.
- Must not edit or persist output through CodeAct; no `sed -i`, `tee file`, output redirects, shell mutation commands, or inline Python writes.
- Must not inspect source through CodeAct; no `cat`, `sed -n`, `grep`/`rg`, `head`/`tail`/`nl`, Python file reads, or source introspection.
- Must not rerun a green command with `--collect-only`, `ls`, or extra probes to "confirm" the pass, and must not rerun a failing broad regression command once it already printed the failing ids or original-message producer you need.
- After one exact-command `transient_runtime` failure with no failing ids, may shard only the same owned payload targets into disjoint equivalent chunks.
- Do not fall back to `subprocess.run(...)` or `subprocess.Popen(...)` to work around timeouts, and do not leave a clearly red background suite running after a progress check already exposed the decisive failure.

## Workflow

- Must run the exact payload command through `daytona_codeact(command="...", timeout=N)`, use that same run's exit code and returned output, and return PASS immediately when it exits `0`.
- Must treat wrapper success, manifest output, and `__CODEX_EXIT_CODE__` as wrapper health only; the verdict comes from the returned exit code.
- Must turn the first red run into a root-cause packet with `phase`, `boundary`, and `next_question`. Never replace that packet with vibes.
- For large suites, use `background=true` on `daytona_codeact`, call `check_background_progress(task_id="bg_1", last_n_lines=20)` before any wait, and alternate with short `wait_for_background_task(timeout=120)` calls only after at least one poll.
- If a progress check already shows a deterministic failure id, `FAILED`, `ERROR`, `ImportError`, or traceback, cancel the task and use that partial output as the runtime evidence.

## Expected Outcome

- The validator returns one verdict backed by exact runtime evidence from the owned command surface.
