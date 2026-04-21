# Scout Launch Contract
Use this reference immediately before the first scout wave. For the entry/root planner, this is the first exploration reference after the main playbook; do not do Task Center graph/detail/note setup before it.

## Task/Goal

- You are about to launch the first useful scout wave.

## Avoid

- Never launch explorers for benchmark tests when a plausible production owner already exists.
- Never use a scout to locate or correct a benchmark test path mismatch; put the literal test path in task prose and scout the production owner path instead. Never use scouts to locate or correct benchmark test path mismatches; do not use scouts to repair benchmark test paths.
- Never pass `*/tests/*`, `test_*.py`, an unconfirmed test-derived path, or a missing test-derived path in scout `target_paths` when production owners exist.
- Never pass an exact file to a scout after a file-symbol query found no indexed symbols and workspace structure shows a live directory or nested files for that same owner family.
- Never bundle unrelated exact files or the whole first-wave ledger into one explorer.
- Never check or wait on a scout id after it reports `delivered`, `Posted.`, `[COMPLETED]`, `[ALREADY_COMPLETED]`, or `[NO TASKS RUNNING]`. `Posted.` means findings were posted to Task Center notes, not in the envelope.
- Do not launch a second scout wave just to repair weak or contradictory scout notes when CI/file notes already identify usable production owner boundaries; put the uncertainty in task specs.
- Do not launch scouts when the assigned task already names concrete owner files and the child lane split; read inherited task/file notes and submit the DAG with remaining uncertainty in task specs.

## Workflow

1. Scrub `target_paths` first: every entry should be a live production owner file/directory unless tests are explicitly the owner surface; do not use scouts to repair benchmark test paths.
2. Call `run_subagent(agent_name="scout", input={"target_paths": [...]}, task_note="...")` with one unresolved owner slice per scout.
3. Queue the whole useful wave before any progress check or wait.
4. After the wave, read scout findings with `read_file_note(file_path="...")` for each exact scout `target_paths` entry you launched. Do not drop file extensions, reuse an unrelated prior path, or skip a scout path. Scouts/subagents are not Task Center tasks. Do not call `read_task_graph()` or `read_task_details(...)` to retrieve scout results, and do not pass `bg_*` background ids, planner slugs, short prefixes, or fabricated ids as task ids.
5. On cold CI or a disproved exact file, fall back to the nearest stable production boundary instead of preserving a guessed exact path.

## Expected Outcome

- The full useful scout wave is queued once, terminal scout ids are retired, note review happens before DAG shaping, and residual uncertainty moves into task specs instead of another scout wave.
