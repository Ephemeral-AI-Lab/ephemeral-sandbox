# Phase 6 — Fold daemon-side overlay stages into a single in-namespace process

**Estimated effort:** 1.5-2.5 days (~0.5 day verification + 0.5-1 day engineering + 0.5-1 day E2E)
**Risk profile:** MEDIUM — narrows the daemon-side `svc.cmd` hot path; correctness is well-bounded by parity tests, but the unshare-namespace ownership / `safe.directory` interaction with in-namespace `git_snapshot` is the real risk and only manifests live
**Status:** Implemented (see [`phase-06-implementation-report.md`](./phase-06-implementation-report.md))
**Blocks on:** Phase 5 daemon-default selection lands and remains stable; Phase 4 svc_cmd dispatch remains the daemon entry point

> **Background.** Phase 4 moved `OverlayAuditor.execute` from the orchestrator
> into the daemon. One process.exec-backed daemon command call per `svc.cmd`. The auditor's outer stage
> structure (`git_snapshot`, `upload_runtime`, `run_overlay`, `read_stdout`,
> `read_diff`, `cleanup`) carried forward unchanged from the orchestrator era.
> Each stage that still spawns a sandbox-local subprocess pays a fork/exec +
> bash-wrap tax that wasn't visible when the assumption was "every stage is a
> `transport.exec` round-trip anyway." Recent measurements suggest the
> read/diff/cleanup hops may already be near-zero; this spec opens with a
> verification task before scoping the engineering work.

## Goal

Collapse the auditor's outer stage structure on the daemon-local path so that
`svc.cmd` does **one** in-namespace subprocess invocation (snapshot + overlay
+ user command + walk + classify, all inside one `unshare -Urm` process) plus
pure-Python file I/O for the result envelope. Project the modeled warm-path
10× p50 from **2.61s** (current measured baseline) to **< 2.0s**.

This is a **perf phase**, not a feature phase. Result-shape parity is the
correctness gate; the warm-path 10× latency reduction is the win. The 1×
cold-tax is explicitly out of scope (see §"Out of scope").

## Why now

The two svc_cmd overlay JSONs in `_timings/`:

| run | 1× p50 | 5× p50 | 10× p50 |
|---|---:|---:|---:|
| `…2026-05-02T18-57-05Z.json` (older) | 8.708s | 3.380s | 4.088s |
| `…2026-05-02T19-12-20Z.json` (newer — rebase baseline) | 7.284s | 2.458s | **2.614s** |

Script-internal timings are essentially unchanged between the two runs
(`overlay_run.total` 0.61s → 0.60s, `git_snapshot.total` 0.25s → 0.25s); the
~1.5s/op delta lives in the auditor's outer stage layer, not in the
kernel/git work.

Reviewer-supplied decomposition of the newer baseline (subject to Task 6.0
verification):

| auditor stage | newer 10× | nature |
|---|---:|---|
| `git_snapshot` (auditor wrapper) | ~0.81s | subprocess + bash-wrap + 0.25s real git work |
| `run_overlay` | ~1.21s | subprocess + bash-wrap + 0.60s real overlay work |
| `read_stdout` / `read_diff` / `cleanup` | ~0.000–0.001s each | already pure-Python file I/O if claim holds; subprocess if not |
| in-process OCC commit | ~0.005s | local |
| **sum (matches daemon command total)** | **~2.61s** | |

If the read/diff/cleanup claim holds, only `git_snapshot` and `run_overlay`
remain as subprocess hops on the daemon-local path. Phase 6 folds them into
one unshare invocation (eliminating one subprocess startup + one bash-wrap +
one inter-stage handoff). Modeled save: ~0.6–0.9s. Modeled 10× p50:
**~1.7–2.0s**. If the claim does not hold, scope expands to also fold
read/diff/cleanup, which the spec already covers.

## What is and isn't in scope

**In scope.**
- Verification (Task 6.0) of which auditor stages are still subprocess-based
  on HEAD.
- Inlining `git_snapshot` into the unshare invocation by passing `--snap=""`
  so the runtime builds the snapshot in-namespace
  (`overlay/runtime/runner.py:66-71`).
- Folding `git_snapshot` + `run_overlay` into one unshare process on the
  daemon-local path (one branch inside `OverlayAuditor.execute`, not a new
  public method).
- If Task 6.0 finds read/diff/cleanup are still subprocess-based: extend the
  fold to cover them via pure-Python file I/O.
- Stripping `wrap_bash_command` from any internal hop that survives.
- An atomic `result.json` completion marker — primarily a lifecycle/safety
  signal that the script finished cleanly, not a latency win.

**Out of scope.**
- Replacing overlayfs with a userland CoW (see Appendix A).
- Streaming `on_progress_line`. Phase 4 already documented the contract as
  final-stdout replay; Phase 6 preserves it.
- The orchestrator-side remote fallback path. It was removed in the Phase 5
  cleanup; Phase 6 must not rebuild it.
- The 1× cold-call tax (~4.7s of unaccounted slack at 1× in the older run,
  largely first-op + cold-import). Reducing the warm-path 10× p50 is the
  Phase 6 deliverable; the cold path belongs to a future eager-bootstrap
  follow-up.

## What ships

| Artifact | File | Purpose |
|---|---|---|
| Verification report | (inline in PR description) | Daemon.log evidence from a fresh 19-12-20Z-style run, listing per-stage auditor timings; defines the fold scope. |
| Daemon-local fold | `backend/src/sandbox/code_intelligence/overlay/auditor.py` (modified) | A daemon-local branch inside `OverlayAuditor.execute`: one `subprocess.run` for the unshare invocation (with empty `--snap`), pure-Python `pathlib.Path.read_text()` / `read_bytes()` / `shutil.rmtree` for the rest. No new public method; no constructor flag. |
| Daemon-side wiring | `backend/src/sandbox/code_intelligence/overlay/command_executor.py` (modified) | `AuditedCommandExecutor` learns one constructor argument (`daemon_local: bool = False`) that the auditor uses to pick the branch. Flipped on at the daemon construction site only. |
| Bash-wrap stripping | same auditor file | Internal hops that survive the fold call `subprocess.run` with `shell=False, argv=[...]`, bypassing `wrap_bash_command`. The user command's unshare invocation still wraps for conda activation. |
| Atomic completion marker | `backend/src/sandbox/code_intelligence/overlay/runtime/runner.py` (modified) | `overlay_run.py` writes `<run_dir>/result.json` atomically (via `os.replace`) as its final action. The auditor reads the JSON to confirm clean completion before parsing `diff.ndjson`. Existing `stdout.bin` and `diff.ndjson` semantics unchanged. |
| Unit parity test | `backend/tests/test_sandbox/test_code_intelligence/test_overlay_daemon_local_parity.py` (new) | Five-case corpus (gitinclude / gitignore / mixed / aborted-version / policy-reject) asserting the daemon-local branch produces a `SimpleNamespace` byte-identical to the multi-stage branch (timings normalized to keys-only). |
| Daemon perf E2E | `backend/tests/test_e2e/test_live_ci_phase6_svc_cmd_fold.py` (new) | Live `svc.cmd` at 1×/5×/10× against the same dask sweevo fixture as Phase 3.5 §F. Headline assertion: `svc_cmd_10x_latency.p50 < 2.0s`. Also includes the unshare/git-ownership smoke check that mocked parity cannot cover. |

## Detailed task list

### Task 6.0 — Verify the read/diff/cleanup claim

**No file change.** The reviewer's claim — "read_stdout/read_diff/cleanup are
already near-zero via local file I/O after a recent patch" — is load-bearing
for the spec's scope. Verify before engineering.

Procedure:
1. Run the existing `test_svc_cmd_overlay_high_concurrency_probe` against
   `dask__dask_2023.3.2_2023.4.0` with `EOS_CI_DAEMON_LOG_LEVEL=DEBUG`.
2. Pull the daemon log from `${DAEMON_STATE_DIR}/daemon.log`.
3. Grep for `_timed_stage` entries: `stage=read_stdout`, `stage=read_diff`,
   `stage=cleanup`.
4. Record p50 for each stage at 10× concurrency.

Decision rule:
- If all three stages are < 50ms p50: scope shrinks to "fold `git_snapshot`
  into the unshare invocation only." Modeled 10× p50: ~2.0–2.1s.
- If any stage is > 200ms p50: scope expands to also fold that stage into
  pure-Python file I/O. Modeled 10× p50: ~1.7–1.9s.

The PR description must include the per-stage p50 table from this run and
state which path was taken.

### Task 6.1 — Atomic `result.json` completion marker

**File:** `backend/src/sandbox/code_intelligence/overlay/runtime/runner.py`

Today the runtime writes `<run_dir>/stdout.bin` and `<run_dir>/diff.ndjson`.
Phase 6 adds `<run_dir>/result.json` as the final write of `main()`:

```json
{"snap": "<sha>", "exit_code": 0, "rejected": null,
 "snapshot_timings": {...}, "run_timings": {...}}
```

Write order: stdout.bin → diff.ndjson → result.json (atomic via temp file
+ `os.replace` in the same run_dir).

**Why this is small.** It is *not* a latency win — readback was already
sub-millisecond. It is a **lifecycle marker**: the auditor uses
`result.json` existence as the "script finished cleanly" signal. Without
it, a `kill -9` on the unshare process leaves `diff.ndjson` half-written and
the auditor can't distinguish that from a legitimate empty-diff op. With
it, missing-or-malformed `result.json` deterministically maps to
`OverlayRunError`. The cost is one tiny atomic write; the gain is fewer
silent partial-state failures.

If the existing implementation already has an equivalent marker (Task 6.0
should check), this task collapses to "no-op — reuse existing marker."

### Task 6.2 — Daemon-local branch inside `OverlayAuditor.execute`

**File:** `backend/src/sandbox/code_intelligence/overlay/auditor.py`

Add **one** instance attribute (`self._daemon_local: bool`, defaults False)
set by the constructor. Inside `execute`, before stage 1, branch:

```python
if self._daemon_local and on_progress_line is None:
    return await self._execute_daemon_local(...)  # private helper
# else: existing multi-stage path unchanged
```

`_execute_daemon_local` does:

1. Acquire semaphore (unchanged).
2. Ensure runtime uploaded (unchanged; one-time).
3. **One** `subprocess.run(["unshare", "-Urm", "python3", script, "--snap", "", ...])`,
   capturing stdout. The runtime builds the snapshot in-namespace
   (`overlay/runtime/runner.py:66-71`); no separate `git_snapshot` stage.
4. Read `<run_dir>/result.json` via `pathlib.Path.read_text()`. Missing or
   malformed → `OverlayRunError`.
5. Read `<run_dir>/stdout.bin` via `read_bytes()` and decode utf-8.
6. Read `<run_dir>/diff.ndjson` via `read_text()` (or treat
   `_reject` from result.json directly), parse with the existing
   `parse_diff_ndjson`.
7. OCC commit in-process via `OverlayCommandCommitter` (unchanged).
8. `shutil.rmtree(run_dir, ignore_errors=False)`, log + suppress `OSError`.

Stage timings still recorded into `stage_timings` so result-shape parity
holds; just under different stage names that mirror the daemon-local
flow (e.g., `unshare`, `read_envelope`, `commit`, `cleanup`).

The multi-stage `execute` codepath is preserved verbatim for the
orchestrator-side remote fallback that already exists. (The Phase 5 §7.1
cleanup deletes that fallback; once gone, the multi-stage branch follows.
Out of scope here.)

**Why one branch, not a new public method.** Two callers exist:
daemon-local and remote-fallback. A flag inside `execute` is one decision
point. A separate `execute_single_shot` plus a constructor flag is two,
plus a public surface that downstream code can accidentally call from the
wrong context. The reviewer was right to flag this.

### Task 6.3 — Daemon-side wiring

**File:** `backend/src/sandbox/code_intelligence/overlay/command_executor.py`

`AuditedCommandExecutor` gains `daemon_local: bool = False` in `__init__`
and passes it to the lazily-constructed `OverlayAuditor`. No other API
changes.

**File:** `backend/src/sandbox/code_intelligence/backends/`

`InProcessBackend` (line 201) constructs `AuditedCommandExecutor` with
`daemon_local=True` **only when** the constructor receives the daemon
context (an explicit constructor argument threaded from
`daemon.server.run_daemon`). Do not infer the context from `transport is
None` — that condition is also satisfied by some test fixtures.

### Task 6.4 — Result-shape parity test

**File:** `backend/tests/test_sandbox/test_code_intelligence/test_overlay_daemon_local_parity.py`

Five parametrized cases, each running both `execute` (multi-stage) and
`execute` (daemon-local branch) against the same fixture corpus, asserting
byte-equality on the full 16-field `SimpleNamespace`:

1. **Pure gitinclude OCC commit** — tracked file edit; `git_commit_status="committed"`.
2. **Pure gitignore direct-merge** — write under `.gitignore`'d path; `gitignore_direct_merged_count > 0`; `git_commit_status="noop"`.
3. **Mixed gitinclude + gitignore** — both routes hit; `mixed_gitinclude_gitignore=True`.
4. **Aborted-version** — base content drift between snapshot and commit; `git_commit_status="aborted_version"`.
5. **Policy reject** — overlay script emits `_reject` (e.g., `.git/` write); `git_commit_status="rejected"`.

Both `git_snapshot_timings` and `overlay_run_timings` are normalized to
keys-only for the equality check (absolute timings differ). All other
fields including `gitinclude_changed_paths`, `gitignore_direct_merged_paths`,
`mixed_partial_apply`, and `warnings` must match field-for-field.

The corpus runs against a tmpdir-rooted overlay with mocked exec, the
existing mechanism in `test_sandbox/test_code_intelligence/`. **This test is
necessary but not sufficient** — see Task 6.6 for the unshare/git-ownership
risk that mocked parity cannot exercise.

### Task 6.5 — Strip `wrap_bash_command` from internal hops

**File:** `backend/src/sandbox/code_intelligence/overlay/auditor.py`

In the daemon-local branch (Task 6.2), the only subprocess invocation is
the unshare wrapping the user command. That one keeps `wrap_bash_command`
because the user command needs conda activation.

Audit any internal hop that survives outside the unshare:
- `_ensure_script_uploaded` currently uses `python3 -c …` via
  `wrap_bash_command`. Switch to `subprocess.run([sys.executable, "-c", ...],
  shell=False)`.
- `_do_exec` should not be called at all from the daemon-local branch.
  Static-check via grep.

### Task 6.6 — Phase 6 live E2E

**File:** `backend/tests/test_e2e/test_live_ci_phase6_svc_cmd_fold.py`

Mirror Phase 3.5 §F's `test_svc_cmd_overlay_high_concurrency_probe` at
1×/5×/10× concurrency on the same dask sweevo fixture. Assertions:

- **Headline:** `svc_cmd_10x_latency.p50 < 2.0s` (down from 2.614s
  rebase baseline). Aspirational target `< 1.8s` documented in the
  PR description but not gated.
- **Structural:** `subprocess.run` call count from daemon log per
  `svc.cmd` op = exactly 1 (the unshare invocation). Grep against
  `daemon.log` lines emitted by `AuditedCommandExecutor` /
  `OverlayAuditor`.
- **Smoke:** the unshare/git-ownership / `safe.directory` interaction
  works on the live sandbox. The mocked parity test cannot exercise
  this; the live run is the only gate.

**No 1× p50 gate.** The 1× number is dominated by cold-call tax
(out of scope per §"Out of scope"). Track 1× as informational only.

Includes a `compare_to(phase_3.5_svc_cmd_baseline)` summary printed in
test teardown.

**Run command:** `.venv/bin/pytest backend/tests/test_e2e/test_live_ci_phase6_svc_cmd_fold.py -m live -v -s`

### Task 6.7 — Regression check

- `.venv/bin/pytest backend/tests/test_sandbox/ backend/tests/test_tools/ -q`
  — green.
- Re-run the most recent Phase 3.5 §F probe to capture a pre-fold
  baseline JSON in the same session, so the PR's `compare_to` table is
  apples-to-apples on this sandbox.
- Earlier-phase live E2Es (0–5) re-run only if their hot path
  surface area changed (it shouldn't; verify via diff).

## Definition of done

- [ ] Task 6.0 verification report committed to PR description: per-stage
      p50 from a fresh DEBUG-level run; scope decision (snapshot-only fold
      vs full collapse) recorded.
- [ ] `overlay_run.py` writes `result.json` atomically (or existing
      equivalent confirmed via Task 6.0 — no double-implement).
- [ ] `OverlayAuditor.execute` has a daemon-local branch gated by
      `self._daemon_local`; no new public method; no separate
      `execute_single_shot`.
- [ ] `AuditedCommandExecutor` flag (`daemon_local: bool`) wired only at
      the daemon construction site.
- [ ] Five-case parity test (Task 6.4) green; full 16-field
      `SimpleNamespace` byte-identical between branches.
- [ ] Live E2E (Task 6.6) headline: **`svc_cmd_10x_latency.p50 < 2.0s`**
      against `dask__dask_2023.3.2_2023.4.0`.
- [ ] Structural assertion: per-op `subprocess.run` count = 1 (the unshare
      invocation only).
- [ ] Live smoke confirms the unshare/git-ownership / `safe.directory`
      interaction works.
- [ ] Regression: full unit suite green.
- [ ] PR description includes: side-by-side timing JSON (pre vs post),
      Task 6.0 verification table, scope decision, modeled vs measured
      delta, list of stages that disappeared.

## Risk callouts

| Severity | Risk | Mitigation |
|---|---|---|
| **HIGH** | In-namespace `build_live_snapshot_in_namespace` runs as the unshare-mapped uid (typically root inside `unshare -Urm`); fails if `safe.directory` is set or workspace ownership doesn't match | The runtime already exercises this codepath when `--snap=""` is passed (per `overlay/runtime/runner.py:66-71`), but only off the hot path. Live E2E (Task 6.6) is the gate; the mocked unit-parity test cannot catch this. If `safe.directory` blocks, set `GIT_CONFIG_GLOBAL=/dev/null` + `safe.directory=*` in the snapshot env (the live snapshot script already does similar). |
| **HIGH** | Daemon-local branch produces a `SimpleNamespace` shape drift downstream callers in `backend/src/sandbox/lifecycle/commit.py` rely on | Task 6.4 parity test gates this for the full 16-field shape; Phase 4's existing `test_svc_cmd_shape_parity.py` carries forward as a second wall. |
| **MEDIUM** | `result.json` atomicity contract violated mid-write (kill -9) → daemon reads malformed JSON → `OverlayRunError` | Atomic rename via `os.replace(tmp, result.json)`; daemon falls back to `OverlayRunError` with a recognizable message and the `svc.cmd` surfaces as a normal failure (not a silent hang). Same semantics as today's `read_diff` failing. |
| **MEDIUM** | `shutil.rmtree(run_dir, ignore_errors=False)` raises on real cleanup errors that `rm -rf` would have logged | Catch `OSError` explicitly in the daemon-local branch; log at warning level, do not raise (matches today's `_cleanup_run_dir` semantics). |
| **MEDIUM** | Stripping `wrap_bash_command` from `_ensure_script_uploaded` breaks PATH resolution for `python3` in some sandbox configs | Bootstrap is one-time per daemon; if portability is a concern, keep `wrap_bash_command` here and only strip from per-op hops. The win is in per-op stages, not bootstrap. |
| **LOW** | Modeled save (~0.6–0.9s) doesn't materialize because `subprocess.run` startup tax is smaller than estimated → 10× p50 stays > 2.0s | Task 6.0 verification gives an early signal. If the gate fails, the rollback is to keep the daemon-local branch but loosen the assertion to `< 2.3s` and document the residual cost as the next phase target. |
| **LOW** | `on_progress_line` callers rely on streaming behavior; daemon-local branch falls back to multi-stage when streaming is requested | The branch only activates when `on_progress_line is None`. Streaming callers transparently take the multi-stage path. Documented in Task 6.2. |

## What this does *not* solve

The remaining wall-time floor after Phase 6 lands (modeled, warm 10×):

| Component | Floor |
|---|---:|
| Orchestrator → daemon process.exec-backed daemon command | ~0.5s (process.exec bridge floor) |
| One unshare subprocess invocation | ~0.1–0.3s startup + ~0.5s real work (snapshot + setup_mounts + user_cmd + walk + classify) |
| In-process OCC commit | ~0.005s |
| Pure-Python file reads + rmtree | ~0.005–0.01s |
| **Modeled total per `svc.cmd` warm path** | **~1.1–1.4s** |

To go materially below ~1s — which Phase 6 does **not** attempt — the next
levers are:
1. **Pre-warm the unshare namespace.** Pool one `unshare -Urm bash` per
   daemon; reuse via fifo/signal. Saves ~0.3s namespace setup per op.
2. **Native binary for the in-namespace runtime.** Replace python startup
   with a Go/Rust binary; saves ~0.1s per invocation.
3. **Pipeline OCC commit prep with the user command.** Hash + base-fetch
   while the user command is still running.

These are individually smaller wins than Phase 6's collapse. Worth
considering only if the modeled ~1.2s p50 still leaves `svc.cmd` on the
critical path of agent-latency targets.

## Hand-off

After Phase 6 lands and stabilizes:

1. **Delete the multi-stage `OverlayAuditor.execute` body** once the Phase 5
   §7.1 cleanup removes the orchestrator-side remote fallback. Atomic
   separate commit, easily revertable.
2. **Re-baseline Phase 3.5 §F.** The svc_cmd overlay JSON becomes the new
   `phase_6_svc_cmd_overlay_concurrency_*.json`; carry forward as the perf
   claim of record.
3. **Decide on namespace pre-warming.** If the modeled ~1.2s p50 is still on
   the critical path, propose Phase 7 (namespace pool). If not, declare the
   migration's perf work done; further reductions are feature-time work,
   not migration debt.

---

## Appendix A — Why this is not a CoW redesign

The user-facing question prompted by the perf signal — "should we replace
overlayfs with a userland CoW (CubeSandbox / joeinnes-cow style)?" — was
investigated in the analysis preceding this spec.

Overlayfs's contribution to wall time is ~0.5s of real work on the rebase
baseline (`setup_mounts` 0.10s, `walk_upperdir` 0.001s, `classify` 0.07s,
user command 0.30s, plus the unshare boundary ~0.1s). The remaining ~2.0s
of the 10× p50 is daemon-internal subprocess multiplexing and the
orchestrator→daemon command, not CoW.

A userland CoW would add cost, not remove it: FUSE pays a context switch
per syscall in the user command's hot path; hardlink trees turn the diff
walk from "scan a sparse upperdir" into "scan the entire workspace looking
for changed inodes." The only honest motivation for a userland CoW in this
codebase is **portability** to sandbox providers that ban `unshare -Urm`
or kernel overlay mounts. Daytona allows both.

If portability ever becomes a constraint, the simpler answer than a full
CoW redesign is to drop overlay isolation entirely, run the user command
in the live tree, and let `inotify`/`fanotify` capture writes — the OCC
commit's strict-base contract already provides the conflict-detection
guarantee that makes the overlay's "atomic apply" property non-load-bearing.
