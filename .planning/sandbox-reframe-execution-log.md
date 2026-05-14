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

## Session 2 — Addendum 4 (W6 + W4b + W7a + PREP-5b convergence push)

| SHA | Wave | Description |
|---|---|---|
| `cea83986` | W6 (layer_stack) | layer_stack/protocols.py deleted. 5 Protocols (ManifestStore, LeaseStore, SnapshotMaterializer, ChangePublisher, CommitStagingStore) replaced with concrete-class annotations. TransactionLock Protocol also dropped (threading.RLock single impl). occ/ports.py Protocols preserved (architectural layering). -86 LOC net. |
| `507ffeb7` | W4b | Narration compression in host/bootstrap.py, host/daemon_client.py, provider/daytona/adapter.py. Kept WHY rationale. -42 LOC net. |
| `272a0b69` | W7a | api/_impl read/write/edit consolidated onto _VerbSpec + _run_verb dispatcher. test_run_verb_seam.py added as Scenario E mock-seam guard. +46 LOC structural (new helper), but session-wide LOC remains net-negative. |
| `bee72173` | PREP-5b | Wave 5b pre-flight written to .planning/wave-5b-preflight.md. All 3 candidates (result_projection.py / shell_runner.py / workspace_server.py) classified REAL-LOGIC. W5b closes as NO-OP. |

**Updated final metrics (post-Session-2):**
- Files: 143 (was 160 baseline) — RFC §7 AC #6 target ≤152 beaten by 9.
- LOC: 17,202 (was 17,492 baseline). -290 LOC net.
  - **Relaxed FINAL floor (≥250 LOC, with T3/stretch deferred): MET.**
  - Firm RFC §13 AC #9 floor (≥1,222 LOC): NOT MET; remaining yielders deferred to next session.
- Top-level subdirs: 9 (api, audit, daemon, execution, host, layer_stack, occ, plugin, provider).
- AC #11 (≤600 LOC ceiling): MET. Largest sandbox file: workspace/base.py at 436 LOC.
- Tests: 546 passed, 1 skipped, 0 failed.
- Ruff: clean.

**Session 2 explicit deferrals (next-session pickup order):**
1. W9 — `_apply_edit_content` extraction in occ/stage/_edit.py (~50-70 LOC).
2. W5a — sync API drop (~80 LOC src + 80 LOC tests, heavy test churn).
3. W8a — api/{lifecycle,transport,protocol,discovery,preview_urls,timeouts}.py inlines (~155 LOC, public-adjacent).
4. W7c — Daytona dedup (T3 + requires manual real-Daytona e2e by user).
5. W7b — daemon handler tool trio extraction (RFC helpers don't exist; design first).

## Session 3 — Addendum 5 (W9 + W5a + W8a; W7b/W7c verdict)

| SHA | Wave | Description |
|---|---|---|
| `c8cbdf81` | W9 (9a) | `apply_edit_content` extracted to `occ/stage/_edit.py` (deduped between direct + gated stagers); `_with_timings` → `policy.py::with_timings` (deduped). 9b (overlay/factory.py + invoker.py) NO-OP — already merged into `execution/overlay/pipeline.py` in W2b. `execution/workspace/capture.py` inline skipped: 2 unit-test direct importers + bundle assertion would push churn beyond cosmetic. |
| `b2ffd9a1` | W5a (minimal) | Dropped `OverlaySnapshotRunner.{shell_sync,supports_sync}` + vestigial isinstance check in `__init__`; deleted 3 live_e2e files in `live_e2e_test/sandbox/overlay/native/` (test_overlay_resource, test_overlay_runner_load, test_snapshot_overlay_runner). -342 LOC net. KEPT: `publish_changes` (no async pair), `layer_path_from_{relative,absolute}` (not sync variants), `filter_ignored` (only one variant), `apply_changeset_sync`/`prepare_changeset_sync`/`commit_prepared_sync` (used by unit test `test_gitignore_policy_edge_cases.py` — rewriting to async = non-cosmetic churn), `reset_session_cache` (no async pair). |
| `a9949c04` + `96301633` | W8a (partial) | `api/{lifecycle,discovery,preview_urls}.py` (3 files, 129 LOC) merged into `api/_control.py` (~110 LOC). `api/default.py` re-routed to use `control_module` alias. Test codemods: `test_contract.py` expected-entries; `test_status.py` 18 monkeypatch imports (all now alias `_control`); `eval_agent_support.py` `get_health` import. Codex parallel commit `96301633` swept the file deletes + codemod into its task_center commit; my `a9949c04` carries the new `_control.py` and the W8a label. **DEFERRED in W8a:** `versioned_payload` → `host/daemon_client.py` move (5 LOC, low yield); `SandboxTransport` Protocol → Callable type alias (no LOC yield, breaks test_transport_protocol.py); `timeouts.py`/`transport.py`/`protocol.py` inlines (each module is role-grouped per `test_contract.py`). |

### W7b — DEAD RFC ITEM (closed, no commit)

Verdict after reading `daemon/handler/tools/{read,write,edit}.py`: the trio does not share enough structure to justify the proposed `_with_snapshot_lease` + `_classify_and_dispatch` helpers.

- **read.py** uses **sync** `acquire_snapshot_lease` / `release_lease` inside the handler — extracting to an async context manager would force a behavior change.
- **write.py** and **edit.py** use `await run_sync_in_executor(...)` for lease acquire + release.
- Each verb dispatches with verb-specific kwargs (`content`/`overwrite` for write; `edits` for edit; nothing for read). A `_classify_and_dispatch` skeleton would need variadic callbacks — saves ~5-6 LOC per file but adds indirection and obscures dispatch.

Per advisor (Session 3): "If the three files don't contain real shared structure to extract, W7b is a dead RFC item — record that and skip. Don't invent helpers to satisfy the RFC." Recorded; W7b removed from pickup queue.

### W7c — DEFERRED TO USER (T3, manual real-Daytona e2e required)

Read `provider/daytona/client/{sync_client,async_client,shutdown}.py`. The RFC's proposed unified `_acquire_cached_client(factory_cls)` helper is non-trivial:

- **sync** cache: process-singleton `_cached_client` + tuple key.
- **async** cache: `weakref.WeakKeyDictionary[event_loop → (key, client)]` (one client per event loop).

These are structurally different containers. Unifying them requires either (a) downgrading async to a singleton (breaks per-loop isolation; regression for concurrent EvalAgent tests) or (b) upgrading sync to a loop-keyed dict (no behavior need, just complexity tax). The RFC's "cache key `(factory_cls, credential_hash, target)`" assumes a unified container — so the helper's prerequisite refactor is itself a behavior change.

`shutdown.py` (91 LOC) compression to ~35 LOC: half the file is the sync-context async-close trick (new event loop in thread); cannot be merged with `async_close_client` without making sync callers async. Realistic compression yield: 5-10 LOC, not 56.

Per advisor (Session 3): "W7c is implementable but the commit body MUST carry the deferral notice. ... Manual real-Daytona e2e per `daytona_pending_build_root_cause.md` deferred to user." Deferring entirely until user can land the e2e validation in the same wave.

**Updated final metrics (post-Session-3):**
- Files: 143 (was 160 baseline) — same count as Session 2 (W9 added `_edit.py`, W5a removed 3 live_e2e tests not counted in sandbox/, W8a deleted 3 root files + added `_control.py` = net -2 in sandbox/, offset by the +1 in stage/).
- LOC: 17,131 (was 17,492 baseline). **-361 LOC net.** -71 vs Session 2's 17,202.
- Top-level subdirs: 9 (unchanged).
- AC #11 (≤600 LOC ceiling): MET. Largest sandbox file: workspace/base.py at 436 LOC.
- Tests: 544 passed, 1 skipped, 0 failed.
- Ruff: clean.
- RFC §13 AC #9 firm floor (≥1,222 LOC): NOT MET. Remaining ~860 LOC unreachable without:
  - W7c (T3, user-gated) — projected ~50-70 LOC.
  - Refactors explicitly out-of-scope by ADR §15 (occ/stage `direct.py`↔`gated.py` 200-300 LOC merge; plugin registry collapse; squash deeper merge).

**Session 3 explicit deferrals (next-session pickup order):**
1. W7c — Daytona dedup, T3, requires user manual real-Daytona e2e per `daytona_pending_build_root_cause.md`.
2. W8a residuals — `versioned_payload` move (5 LOC); `SandboxTransport` Protocol → Callable type alias (only if test_transport_protocol.py is also rewritten; net win unclear).
3. ADR §15 items if scope is reopened: occ/stage deeper merge, plugin registry collapse, squash deeper merge.

## Session 3 — Addendum 6 (W7c-min + skipped residuals)

After addendum 5 the loop pushed for further continuation. Three more attempts:

| SHA | Wave | Description |
|---|---|---|
| `76c34ad8` | W7c-min | Extracted `load_required_credentials(unavailable_cls, not_configured_message)` and `build_sdk_client(factory_name, ...)` to `provider/daytona/client/credentials.py`. Both sync and async client paths now call these helpers. Sync cache (singleton) and async cache (loop-keyed WeakKeyDictionary) stay separate — RFC's unified-cache scheme not attempted because it would require changing one cache's semantics. The thin module-local `_load_credentials` wrapper in async_client.py preserves the `test_async/test_client.py` monkeypatch seam. +44 LOC net (helper bodies cost more than the inline call sites saved) — **structurally cleaner but LOC-negative**. Real-Daytona e2e validation still owed by user. |
| (no commit) | W8a-resid `versioned_payload` | SKIPPED. Moving `versioned_payload` into `host/daemon_client.py` either breaks 4 test mocks that wrap `call_daemon_api` with unversioned-payload assertions, or yields zero net LOC (move + import codemod). The RFC's stated benefit was "wire-versioning closer to the wire call" — cosmetic. |
| (no commit) | shutdown.py compression | ATTEMPTED then REVERTED. Extracted `_resolve_close_fn(client)` helper used by both `close_client` and `async_close_client`. Saved 2 LOC per call site but the 5-LOC helper itself + blank lines pushed net to +3 LOC. The sync-context awaitable-close trick (new event loop in thread) is load-bearing and cannot be merged with `async_close_client` without breaking sync callers. Reverted. |

**Updated final metrics (post-Session-3 addendum-6):**
- Files: 144 (was 160 baseline) — W7c-min did not add or remove files.
- LOC: 17,175 (was 17,131 in addendum 5, was 17,492 baseline). **-317 LOC net** vs baseline (+44 vs addendum 5).
- Top-level subdirs: 9 (unchanged).
- AC #11 (≤600 LOC ceiling): MET.
- Tests: 544 passed, 1 skipped, 0 failed.
- Ruff: clean.

**Session 3 final deferrals (true next-session pickup order):**
1. W7c-full — Real-Daytona e2e validation by user before deploy. Once validated, optional follow-up: unify cache containers (only if user accepts the behavior change in either direction).
2. ADR §15 items if scope is reopened.

The named-wave queue is now exhausted. All remaining LOC yield requires either ADR §15 scope reopening or user-gated T3 work.

**Session 3 parallel-codex incidents:** Three documented sweeps, all benign:
1. `45e17e92` (codex) captured my W9 direct.py + policy.py edits alongside its task_center config+task_ids shim collapse.
2. `96301633` (codex) captured my W8a default.py imports + 3-file deletes + test codemods alongside its persistence+exceptions shim collapse.
3. The `git commit -m "..." -- <pathspec>` pattern from prior sessions held: my W9, W5a, W8a label commits carry only my new files (`_edit.py`, `_control.py`) and my targeted source-side edits. Net effect identical; just a credit-line cosmetic difference.

**T3 deferrals still owed by user before deploy:**
- W3 (runtime/ → daemon/): run `live_e2e/squad/runner.py` against a real Daytona provider with `provider.create()` 60s timeout. Bundle hash invalidation on first contact is expected.
- W7c (Daytona dedup): not yet implemented. Deferred to a future session that can pair the cache-helper refactor with a real-Daytona e2e validation in the same atomic commit.

**Session 2 parallel-codex incidents:** None observed. The `git commit -m "..." -- <pathspec>` pattern held throughout; no unrelated content swept into Session 2 commits.

**T3 deferrals still owed by user before deploy:**
- W3 (runtime/ → daemon/): run `live_e2e/squad/runner.py` against a real Daytona provider with `provider.create()` 60s timeout. Bundle hash invalidation on first contact is expected (one-time re-upload per running sandbox).
- W7c (Daytona dedup): not yet implemented; defer to next session.

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
