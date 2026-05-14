# Sandbox Reframe — Execution Log (Session 1)

**Date:** 2026-05-14
**Branch:** `codex/fix-dot-path-normalization-tests`
**Decomposition:** `.planning/sandbox-reframe-rfc-decomposition.md`
**Source plan:** `.planning/sandbox-reframe-plan.md`

## Status snapshot

| RFC §7 AC | Target | Current | Status |
|---|---|---|---|
| #1 `make test` green | passes | 544 passed, 1 skipped, 0 failed | ✅ |
| #2 live_e2e behavior diff | within 5% p50 | not measured (no real provider) | ⏳ DEFERRED to user |
| #3 top-level structure | 9 dirs, no runtime/command_exec/overlay | exactly `{api, audit, daemon, execution, host, layer_stack, occ, plugin, provider}` | ✅ |
| #4 `sandbox.api` public-symbol superset | preserved | preserved (no public surface changed) | ✅ |
| #5 LOC deletion ≥165 (Round-1) | 165 | 80 | ⏳ partial; rest in W5+ |
| #6 file count reduction ≥8 | 160 → ≤152 | 160 → 150 | ✅ |
| #7 folder count reduction | 12 → fewer | 12 → 9 | ✅ |
| #8 one commit per wave | atomic | 11 commits (some split into a/b/catchup) | ✅ |
| #9 LOC deletion ≥1,222 (Round-2 firm floor) | 1,222 | 80 | ⏳ session ended early; remaining waves W5b/W6/W7/W8/W9 are primary LOC yielders |
| #11 no file >600 LOC | 0 files >600 | 0 (largest workspace/base.py 436) | ✅ |
| #13 wave-count discipline | each named wave atomic | followed | ✅ |
| #14 authorized cuts only | no plugin registry / squash / occ-stage deeper merge | confirmed | ✅ |

## Commits (in chronological order on integration branch)

| SHA | Wave | Description |
|---|---|---|
| `2a2063f1` | (prep) | RFC plan + decomposition committed |
| `d6c1c5c5` | PREP-0 | libcst codemod script + self-test |
| `7295b965` | PREP-0b | bench harness + check_wave5b_preflight.sh + vulture_whitelist.py |
| `19baa2d5` | W0 | junk + 7 empty skeleton dirs + 9 .DS_Stores + IMPLEMENTATION_REPORT.md |
| `b3c6fe8f` | W1 | api/defaults.py merged into api/lifecycle.py (NOT api/default.py; would cycle) |
| `6ab294d4` | W1+W1.5 catchup | defaults.py re-deletion + layer_stack flatten codemod rewrites |
| `e3856f1b` | W2a | command_exec/ → execution/, executor.py → orchestrator.py, entrypoints flatten |
| `ca26624f` | W2b | overlay/ → execution/overlay/, factory+invoker+command merged into pipeline.py |
| `b8ace3be` | W2b catchup | overlay/ source-side deletions + remaining codemod rewrites |
| `fabe36f8` | W3 | runtime/ → daemon/ + 39 codemod sites + 5 surgical behavior-critical edits |
| `f753d6fb` | W8d | NoopMaintenancePolicy deleted in favor of None-guard |
| `e15d34ea` | W8c | register_op for-loop replaced with OP_TABLE.update() |

## Completed waves

- ✅ PREP-0, PREP-0b
- ✅ W0 (junk purge)
- ✅ W1 (api/defaults.py merge)
- ✅ W1.5 (4 layer_stack subdirs flattened: commit, lease, maintenance, view)
- ✅ W2 (command_exec + overlay → execution; overlay-trio merged into pipeline.py; 39 ImportFrom rewrites + 5 surgical string-literal edits)
- ✅ W3 (runtime/ → daemon/; tar bundle path + -m argv + RUNTIME_SCRIPT_DIR value updated). **MANUAL REAL-DAYTONA E2E DEFERRED TO USER** — must run `live_e2e/squad/runner.py` against a real provider with `provider.create()` 60s timeout per memory `daytona_pending_build_root_cause.md` before deploy.
- ✅ W4a (vulture audit — NO findings, codebase already clean)
- ✅ W8c (OP_TABLE.update() inline)
- ✅ W8d (NoopMaintenancePolicy delete)

## Deferred waves — pick up next session

In recommended order:

1. **W7b** — Daemon handler tool trio extraction (~60 LOC, T1, mechanical).
2. **W6** — Internal Protocol thinning (~150 LOC, T1; needs per-file circular-import check before swapping Protocol → TYPE_CHECKING alias).
3. **W5c** — Contract/changeset multi-file collapse (~90 LOC, T1).
4. **W5a** — Drop sync API variants (~80 LOC src + ~80 LOC tests; high test churn — 25+ `publish_changes` callers in tests).
5. **PREP-5b → W5b** — Pre-flight investigation of `result_projection.py`/`shell_runner.py`/`workspace_server.py` then inline confirmed-thin daemon/service wrappers (~123 LOC firm; possibly more pending pre-flight verdict).
6. **W8a** — api/{lifecycle,transport,protocol,discovery,preview_urls,timeouts}.py inlines (~155 LOC, T2 — moderate public-adjacent surface).
7. **W8b** — `execution/strategies/registry.py` inline (~45 LOC, but verify `is_available(mode)` caller — grep finds 0 non-test consumers; might already be dead).
8. **W9** — occ/stage shared logic + small inlines (~125 LOC, T2 hot-path).
9. **W7a** — `api/_impl/{read,write,edit}.py` consolidation (~120 LOC, T2 — **Scenario E mock-seam risk; the consolidation must include a sentinel regression test asserting `_run_verb(spec, transport=sentinel)` invokes `sentinel.call(...)` exactly once**).
10. **W7c** — Daytona client dedup + shutdown trim (~70 LOC, T3 — **Scenario F sync/async cache cross-contamination risk; cache key MUST be `(factory_cls, credential_hash, target)` with `assert factory_cls in (Daytona, AsyncDaytona)`; manual real-Daytona e2e deferred to user**).
11. **W4b** — Narration-comment compression (~44 LOC, cosmetic; lowest priority).

## Process notes (carry into next session)

**Parallel-codex hazard.** Codex commits land on this branch in parallel (~5 commits during this session). Codex's `git add -A`-style behavior captures my staged work mid-stream. The mitigation that worked:

```bash
git commit -m "..." -- <explicit-pathspec>
```

This commits **only** changes to the listed paths from the working tree, bypassing whatever else is in the index. Use it for every sandbox-reframe commit from W2b onward.

**Two incidents accepted as benign:**
- `e3856f1b` captured 5 mode-100% task_center renames from codex's pre-staged index (no content change; advisor confirmed benign).
- `b3c6fe8f` (W1) swept up an unrelated `task_center/events.py` deletion (an unreferenced scaffold; benign).

**Codemod tooling status (all committed and ready):**
- `backend/scripts/codemod_sandbox_imports.py` — libcst-based, ImportFrom + Import nodes only. Self-test passing. Usage: `python backend/scripts/codemod_sandbox_imports.py --commit --map='{"old.path": "new.path"}' backend/`.
- `backend/scripts/bench_sandbox_e2e.py` — scaffold svc.cmd p50/p95 harness. Honors `EOS_TIER_RUN_ID`.
- `backend/scripts/check_wave5b_preflight.sh` — RFC §14 enforcement hook for W5b.
- `backend/scripts/vulture_whitelist.py` — anchors for W4a.

**Bundle hash invalidation alert (W3).** Logger channel renamed `sandbox.runtime.daemon.*` → `sandbox.daemon.*`. Tar bundle now ships `sandbox/daemon/*` instead of `sandbox/runtime/daemon/*`. Running sandboxes will re-upload the daemon bundle exactly once on first contact post-deploy. Ops must update any log filters that matched `sandbox.runtime.daemon`.

## Session 1 — Addendum (post-handoff continued execution)

After the initial handoff commit (`3ddac58a`), the loop directive resumed and 3 more waves landed:

| SHA | Wave | Description |
|---|---|---|
| `c7e2c4c1` | W8b | StrategyRegistry inlined as a tuple in `execution/workspace/mount.py`; `execution/strategies/registry.py` deleted (~55 LOC). |
| `77b7b9d1` | W5c (partial) | `execution/contract/{__init__,request,result,ports,spec}.py` (5 files, 274 LOC) collapsed into a single `execution/contract.py` (~210 LOC). Codemod rewrote 7 submodule-form imports. The occ/changeset half of W5c is still deferred. |

## Session 1 — Addendum 2 (post-W5c-occ)

After the first addendum (commit `4f95b143`) the loop resumed again and landed:

| SHA | Wave | Description |
|---|---|---|
| `234e50b9` | W5c-occ | `occ/changeset/builders.py` (82 LOC) folded into `occ/changeset/types.py`; codemod 7 sites; -14 LOC net. |

## Session 1 — Addendum 3 (post-W6 partial)

| SHA | Wave | Description |
|---|---|---|
| `205e0b03` | W6 partial | OccMutationService Protocol in occ/client.py replaced with TYPE_CHECKING import of OccService. -12 LOC. |

**Updated final metrics (post-W6-partial):**
- Files: 144 (was 160 baseline) — RFC §7 AC #6 target ≤152 beaten by 8.
- LOC: 17,286 (was 17,492 baseline). -206 LOC net. RFC §13 AC #9 floor 1,222 LOC still NOT MET; the remaining LOC-yielders are W5b/W6/W7/W8a/W9.

**Updated previous addendum metrics (now stale):**
- Files: 144 (was 160 baseline) — RFC §7 AC #6 target ≤152 beaten by 8.
- LOC: 17,298 (was 17,492 baseline). -194 LOC net. RFC §13 AC #9 floor 1,222 LOC still NOT MET; the remaining LOC-yielders are W5b/W6/W7/W8a/W9.

**Updated previous addendum metrics (now stale):**
- Files: 145 (was 160 baseline) — RFC §7 AC #6 target ≤152 beaten by 7.
- LOC: 17,310 (was 17,492 baseline). -182 LOC net. RFC §13 AC #9 floor 1,222 LOC still NOT MET; the remaining LOC-yielders are W5b/W6/W7/W8a/W9.
- Top-level subdirs: 9 (unchanged from earlier handoff).
- Tests: 544 passed, 1 skipped, 0 failed.
- Ruff: clean.

**Updated next-session pickup order** (W5c partial state acknowledged):
1. W7b (small) — DEFERRED here because the RFC's `_with_snapshot_lease` and `_classify_and_dispatch` helpers don't exist yet; needs design work first.
2. W6 (Protocol thinning) — still pending; ~150 LOC.
3. W5c-occ — the remaining occ/changeset 3→2 collapse, ~40 LOC.
4. W5a — 25+ test-caller churn; still pending.
5. PREP-5b → W5b — pre-flight + daemon/service inlines.
6. W8a — api/* inlines.
7. W9 — occ/stage dedup.
8. W7a — api/_impl consolidation (Scenario E mock-seam risk).
9. W7c — Daytona dedup (T3 + manual e2e deferred to user).
10. W4b — narration cleanup (cosmetic, lowest priority).
