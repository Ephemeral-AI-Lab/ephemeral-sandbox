# Final Phase — Live e2e Sandboxes + Canonical SWE-EVO × 3 Modes (S6 + S7 + S8 + S9)

**Owner:** Yifan
**Date:** 2026-05-20
**Status:** DRAFT v3 (RALPLAN — critic ITERATE applied; awaiting re-architect + re-critic)
**Predecessors:**
- `.planning/coding_plan_mode_plan.md` APPROVED v9.3 (master plan)
- `.planning/next_steps_and_rename_plan.md` APPROVED v5 (S1-S4 code landed in ralph round-2)

---

## v3 changelog (critic ITERATE applied)

Critic surfaced 2 BLOCKING + 2 MAJOR + 3 MINOR + 4 missing-items.

**BLOCKING-1 — `coding_plan_mode_error` grep is unsound.** `_emit_coding_plan_mode_error` writes via `log.error(...)` to a stderr-rooted logger. `AuditRecorder` does NOT add a FileHandler under `report.run_dir`; nothing writes the log lines to disk. v2's "grep run_dir for the literal" assertion was trivially passing.
**v3 fix:** S7 tests use `caplog` to capture log records of the `providers.clients.anthropic_native` and `providers.clients.coding_plan.codex` loggers, then assert `len([r for r in caplog.records if r.message == "coding_plan_mode_error"]) == 0`. No on-disk log file needed.

**BLOCKING-2 — `stores.model_store.create(...)` doesn't exist.** `TaskCenterStoreBundle` has no `model_store` attribute. `ModelStore.register(key=, label=, class_path=, kwargs=, activate=True)` is the actual API (`backend/src/db/stores/model_store.py:98`). Runtime resolution uses `runtime.app_factory:model_store` singleton (`app_factory.py:66`), initialized via `model_store.initialize(session_factory)` (line 102).
**v3 fix:** S7 introduces a parametric fixture `_register_plan_mode_row(class_path, kwargs_extra=None)` that (a) imports `runtime.app_factory.model_store`, (b) calls `model_store.initialize(bundle.session_factory)` if not already ready, (c) calls `model_store.register(key="test/plan-mode", label="Test Plan-Mode Row", class_path=class_path, kwargs=kwargs_extra or {}, activate=True)`, (d) yields, (e) on teardown deactivates / deletes the row to avoid bleed across tests. Reference real registration pattern at `backend/src/db/stores/model_store.py:98-131` + `runtime/app_factory.py:101-110`.

**MAJOR-3 — S6 control flow pinned to mirror S3 exactly.** v3 specifies "mirror `backend/src/providers/clients/anthropic_native.py:141-179` exactly" — same `attempted_refresh` flag + `for attempt in range(2)` wrapping both `async with` blocks. Codex's existing yield-then-translate pattern requires `emitted_any` parity (replay safety) — if a 401 fires AFTER text deltas were already yielded, refresh-replay replays them. Spec'd explicitly.

**MAJOR-4 — S8 PASS-WITH-NOTES no longer auto-promotes.** v3 verdict matrix: PASS (auto-promote S9), PASS-WITH-NOTES (requires explicit operator sign-off in `capability_parity_benchmark.md` before S9 fires), FAIL (no promote; defer; investigate). Thresholds (2x runtime, 50% F2P delta) are kept as soft tripwires for the PASS-WITH-NOTES classification; explicitly documented as "no data baseline yet; revisit after first three runs land".

**MINOR-1 — S7 step 5 reframed.** Tool-name-in-registry assertion is "routing-regression detection" (catches a future bug where Codex routes through a non-sandbox tool registry), NOT "capability proof" (F2P>0 already proves capability). Kept for the routing signal.

**MINOR-2 — S10 collapsed** into the per-sprint gates. No separate sprint.

**MINOR-3 — JSONL schema citation pinned.** v3 references `backend/src/message/agent_message_recorder.py` (not `recorder.py`) for the on-disk JSONL event schema.

**Missing-1 — Pre-mortem #5 added** for canonical instance flakiness + re-run budget.
**Missing-2 — S8 directory naming explicit.** Each S7 test invocation produces its own `report.run_dir` under `audit_dir` keyed by `instance_id` + timestamp; three S8 runs → three distinct directories.
**Missing-3 — Codex `_refresh_credentials` semantics** pinned to "returns True iff EITHER `_access_token` OR `_chatgpt_account_id` changed".
**Missing-4 — Architect-noted v6 file reorg ordering** retained.

---

## Goal

Close out the coding-plan-mode iteration with **meaningful live e2e evidence** via the existing `run_sweevo_real_agent` harness (canonical SWE-EVO instance × 3 modes):

1. Real Claude Max OAuth round-trip through `AnthropicPlanClient`.
2. Real `~/.codex/auth.json` round-trip through `CodexResponsesClient`.
3. Three-mode parity gate on the same canonical instance.
4. Status promotion: "experimental" → "stable-beta" iff S8 passes.

---

## RALPLAN-DR Summary

### Principles (5)

1. **E2E means E2E.** Real OAuth, real Daytona sandbox, real LLM, real SWE-EVO instance. Scripted ScenarioBase probes (e.g., `ComplexProjectBuildSmoke`) do NOT satisfy the gate — they hardcode `ToolCallSpec` and skip the LLM.
2. **Reproducibility is not the goal.** Vendor responses are nondeterministic. Gate on **outcome shape** (`task_center_status`, `fail_to_pass_total`, A11 audit field, log absence), not on tool-call sequence equality.
3. **Two-vendor symmetry.** Anthropic and Codex share the same harness (`run_sweevo_real_agent`) and the same assertion set. Only the `model_registrations` row's `class_path` differs.
4. **Credential-gated, never CI-default.** Live e2e skips unless `EOS_SWEEVO_REAL_AGENT_TESTS=1` AND vendor credentials are present.
5. **Customized tools means OUR tools.** SWE-EVO drives the agent through `tools.sandbox._lib.registry.make_sandbox_tools`. `fail_to_pass_total > 0` mechanically proves at least one sandbox-tool invocation succeeded — the canonical instance cannot resolve without them.

### Decision Drivers (3)

1. **Confidence** — real-vendor evidence on the canonical instance before promoting plan-mode.
2. **Cost** — bound to 1 canonical instance × 3 modes = 3 live runs (plus up to 1 re-run per mode for flakes; see Pre-mortem #5).
3. **Drift detection** — A18 live tests are the canary for vendor-side rotations.

### Viable Options

**Option A (this plan)** — Symmetric live e2e + canonical SWE-EVO × 3 modes + outcome-shape gate.

**Option B (architect steelman)** — Live smoke only; skip the three-mode comparison. Faster, but master plan §Phase 3 contract leaves "capability parity" unsatisfied. Adopt as fallback iff S8 returns FAIL repeatedly.

**Option C** — Multi-scenario sweep. Rejected as scope creep.

Recommendation: **Option A** with Option B as the documented fallback for v9.4.

---

### Pre-mortem (Deliberate Mode — 5 scenarios)

1. **Cloudflare allowlist rotation breaks Codex Day-1.**
   *Mitigation:* `CODEX_ORIGINATOR` + `CODEX_UA_VERSION` are single-source constants in `codex.py:57-59`. A18 live tests are the drift canary.

2. **Codex token rotates mid-run; no refresh-on-401 Codex-side.**
   *Mitigation:* S6 closes this. `_refresh_credentials()` returns True iff EITHER `_access_token` OR `_chatgpt_account_id` changed (since headers depend on both — `build_headers()` at `codex.py:208-216` reads both).

3. **Daytona sandbox cred-loading collision.** Plan-mode client tries to read `~/.codex/auth.json` or query macOS Keychain from inside the sandbox container.
   *Mitigation:* `CodexResponsesClient.__init__` and `_ClaudeOAuthStrategy._read_keychain` run on the HOST during `make_api_client` dispatch — BEFORE Daytona provisioning. Token lives in host process memory thereafter. A8 Part 3 subprocess-env test already enforces no env-leakage to sandbox children. S7 additionally asserts `coding_plan_mode_active is True` in run.json — proves host-side resolution fired.

4. **Operator wires S7 against scripted scenario by mistake.** v1 mistake; explicitly guarded against.
   *Mitigation:* (a) S7 test bodies use `run_sweevo_real_agent` with `SWEEvoInstance` (NOT `run_sweevo_scenario` with `ScenarioBase`). (b) `fail_to_pass_total > 0` on `done` is unachievable by the scripted probe (scripted scenarios return hardcoded ToolCallSpec; F2P is computed by `SweevoLifecycle.after_run`, requires real agent output).

5. **Canonical SWE-EVO instance vendor-side flakiness.** A vendor model update changes resolution success rate; S8 returns FAIL not because plan-mode is broken but because the instance is now harder/easier than before.
   *Mitigation:* **Re-run budget**: each mode gets up to 1 retry on `task_center_status in {"failed", "cancelled"}` before declaring S8 FAIL. Retries reuse the same `EOS_TIER_RUN_ID` to keep artifacts stable per project memory `eos_tier_run_id_artifact_stability.md`. If both attempts fail, the mode counts as FAIL. If retries are needed in 2+ modes, document the divergence in `capability_parity_benchmark.md` as a vendor-stability observation (separate from the plan-mode signal).

---

### Expanded Test Plan (Deliberate Mode)

**Unit (credential-independent, CI-default):**
- Existing 94 tests stay green (regression-safe).
- **New** `backend/tests/unit_test/test_providers/test_codex_refresh_replay.py` — 2 cases per S3 pattern.

**Integration:** None new. `run_sweevo_real_agent` IS the integration layer.

**E2E (gated by `EOS_SWEEVO_REAL_AGENT_TESTS=1`):**

| Test | Per-test skip gate | Assertion set |
|---|---|---|
| `test_anthropic_coding_plan_mode_e2e` | not Darwin OR no Keychain entry OR no plan-mode infra | Register row → `run_sweevo_real_agent` → assert outcome shape (status + F2P) + A11 True + sandbox-tool invocation in message.jsonl + caplog has zero `coding_plan_mode_error` records |
| `test_codex_coding_plan_mode_e2e` | no `~/.codex/auth.json` OR no plan-mode infra | Same shape, Codex class_path |
| `test_api_mode_regression` | none | API mode → A11 False + zero `coding_plan_mode_error` |

**Observability:**
- `.planning/capability_parity_benchmark.md` records per-run row in a three-row table.
- Verdict + (if PASS-WITH-NOTES) operator sign-off line.

---

## Concrete Acceptance Criteria

### Sprint S6 — Codex A7 refresh-on-401 symmetry (~1 hour, credential-independent)

- **Pinned: mirror `backend/src/providers/clients/anthropic_native.py:141-179` exactly** for control flow shape. Differences are only at the Codex-specific edges (auth.json reload vs Keychain reload; httpx vs anthropic SDK).
- `backend/src/providers/clients/coding_plan/codex.py`:
  - Add `_refresh_credentials(self) -> bool` instance method:
    - Wraps `_load_codex_auth(self._auth_path)` + `jwt_extract_chatgpt_account_id(...)`. Each call may raise `CodexCredentialIncompleteError` — catch and return `False` (no retry).
    - Compute `changed = new_access != self._access_token or new_account_id != self._chatgpt_account_id`.
    - If `changed`: update both fields in place, return `True`. Else return `False`.
    - Returning True iff EITHER changes is load-bearing per `build_headers()` (`codex.py:208-216`) reading both.
  - Restructure `stream_message` to enable retry:
    - Outer: `attempted_refresh = False`
    - Outer: `for attempt in range(2):` — bounds the retry to at most one refresh+replay (mirrors S3's `for attempt in range(2)` shape since S3 doesn't use MAX_RETRIES on the refresh path).
    - Inside: existing `async with httpx.AsyncClient` + `async with http.stream` block. On 401 from response.status_code: if `not attempted_refresh`, set flag, call `self._refresh_credentials()`. True → `break` the inner blocks and let the outer `for` retry. False → translate + emit + raise (no retry).
  - **Replay safety note**: Codex `stream_message` yields events MID-iteration via `async for line in response.aiter_lines()`. A 401 mid-stream is unusual (Codex returns 401 at the HEAD; the spike confirmed status-code is set in the initial response). If a 401 arrives after deltas were emitted, the retry will re-emit them — accepted cost per plan §A7 (Anthropic side does the same).
- **NEW unit test** `backend/tests/unit_test/test_providers/test_codex_refresh_replay.py`:
  - Test (a) refresh-True: monkeypatch `_load_codex_auth` to return `("OLD", "OLD-id-token")` first call, `("NEW", "NEW-id-token")` second call (with valid JWT decode). Monkeypatch httpx to return 401 first stream, 200 + canned SSE second stream. Assert: `_refresh_credentials` called once, retry happened, full event sequence yielded.
  - Test (b) refresh-False: monkeypatch `_load_codex_auth` to return the SAME `("OLD", "OLD")` on both reads. Stream returns 401. Assert: `_refresh_credentials` called once and returned False (token unchanged), NO retry, `AuthenticationFailure` raised.
- **Gate (this sprint):**
  - `.venv/bin/pytest backend/tests/unit_test/test_providers/test_codex_refresh_replay.py -v` → 2 passes.
  - `.venv/bin/pytest backend/tests/unit_test/test_providers/ -q` → no regression (existing tests still pass).
  - `.venv/bin/ruff check backend/src/providers/clients/coding_plan/codex.py backend/tests/unit_test/test_providers/test_codex_refresh_replay.py` → clean.

### Sprint S7 — A18 live e2e implementation against `run_sweevo_real_agent` (~3 hours code; credential-INDEPENDENT build, credential-gated execution)

- File: `backend/src/task_center_runner/tests/sweevo/test_real_agent_coding_plan_mode.py`.
- **New fixture** (in the test file itself, or in `backend/src/task_center_runner/tests/sweevo/conftest.py` if discoverable):
  ```python
  @pytest.fixture
  def _register_plan_mode_row(stores: TaskCenterStoreBundle):
      from runtime.app_factory import model_store

      # `initialize()` (base.py:26-27) unconditionally rebinds
      # `_session_factory` regardless of prior `is_ready` state — it's
      # idempotent. Each test's row writes land on the per-test DB
      # without any private-attribute reset (critic v3 final pass: the
      # earlier `_initialized = False` patch was a silent no-op against
      # a fictitious attribute; removed).
      model_store.initialize(stores.session_factory)

      registered_keys: list[str] = []

      def _register(class_path: str, kwargs_extra: dict | None = None) -> str:
          key = f"test/plan-mode-{uuid.uuid4().hex[:8]}"
          model_store.register(
              key=key,
              label="Test Plan-Mode Row",
              class_path=class_path,
              kwargs=kwargs_extra or {},
              activate=True,
          )
          registered_keys.append(key)
          return key

      yield _register

      # ModelStore.delete already handles active-row promotion; no
      # separate deactivate call needed (deactivate does not exist —
      # only `_deactivate_all` is internal).
      for key in registered_keys:
          try:
              model_store.delete(key)
          except Exception:
              pass
  ```
  Reference: `backend/src/db/stores/model_store.py:98-176` for the actual `register(key, label, class_path, kwargs, activate)` + `delete(key)` signatures.
- Replace each `pytest.skip(...)` body with a real implementation following this template:
  1. **Setup** — accept fixtures: `sweevo_instance`, `workspace`, `audit_dir`, `stores`, `_register_plan_mode_row`, `caplog`.
  2. **Register model row** — call `_register_plan_mode_row(class_path=<vendor class_path>, kwargs_extra={"model": "<model_id>"})`. Concrete:
     - Anthropic: `class_path="providers.clients.coding_plan.anthropic:AnthropicPlanClient"`, kwargs `{"model": "claude-sonnet-4-5"}`.
     - Codex: `class_path="providers.clients.coding_plan.codex:CodexResponsesClient"`, kwargs `{}` (model auto-resolves from `~/.codex/config.toml`).
     - API-mode regression: NO `_register_plan_mode_row` call; existing API-mode registration from default fixtures stays active.
  3. **Caplog setup** — `caplog.set_level(logging.ERROR, logger="providers.clients.anthropic_native")` AND `caplog.set_level(logging.ERROR, logger="providers.clients.coding_plan.codex")`.
  4. **Run** — `report = await run_sweevo_real_agent(instance=sweevo_instance, sandbox_id=str(workspace["sandbox_id"]), audit_dir=audit_dir, stores=stores, max_duration_s=float(os.getenv("EOS_SWEEVO_REAL_AGENT_MAX_DURATION_S", "1800")))`.
  5. **Outcome assertions** (verbatim from `test_real_agent.py:42-49`):
     - `assert report.task_center_run_id`
     - `assert report.run_dir.is_dir()`
     - `assert (report.run_dir / "run.json").is_file()`
     - `assert (report.run_dir / "sweevo_result.json").is_file()`
     - `assert report.task_center_status in {"done", "failed", "cancelled"}`
     - `if report.task_center_status == "done" and not report.aborted_by_timeout: assert report.sweevo_result.fail_to_pass_total > 0`
  6. **A11 assertion** — `run_json = json.loads((report.run_dir / "run.json").read_text()); assert run_json["coding_plan_mode_active"] is <True for plan-mode, False for api-mode>` (identity check, not equality).
  7. **Sandbox-tool routing-regression assertion** (per critic minor-1 reframe — not capability proof, just routing detection):
     - Walk all `message.jsonl` files under `report.run_dir` recursively.
     - Schema reference: `backend/src/message/agent_message_recorder.py` defines the on-disk event format. Each line is JSON; events with assistant tool_use content have a `content` array with dicts shaped `{"type": "tool_use", "id": ..., "name": ..., "input": ...}`.
     - Collect every `tool_use` block's `name`; load `tools.sandbox._lib.registry.make_sandbox_tools()` tool names; assert at least one collected name is in the sandbox tool set.
  8. **Log assertion (BLOCKING-1 fix)** — `coding_plan_mode_error_records = [r for r in caplog.records if r.message == "coding_plan_mode_error"]; assert len(coding_plan_mode_error_records) == 0`.
- Per-test skip gates (`_SKIP_NO_ANTHROPIC_CREDS`, `_SKIP_NO_CODEX_CREDS`, `_SKIP_NO_PLAN_INFRA`) stay in place.
- **Gate (this sprint, credential-independent half):**
  - `.venv/bin/python -m py_compile backend/src/task_center_runner/tests/sweevo/test_real_agent_coding_plan_mode.py` → exits 0.
  - `.venv/bin/ruff check backend/src/task_center_runner/tests/sweevo/test_real_agent_coding_plan_mode.py` → clean.
  - `.venv/bin/pytest backend/src/task_center_runner/tests/sweevo/test_real_agent_coding_plan_mode.py -v` → 3 tests collected; all 3 SKIPPED without `EOS_SWEEVO_REAL_AGENT_TESTS=1`.
  - Existing 94 unit tests still pass.

### Sprint S8 — Canonical SWE-EVO × 3 modes capability parity (~1 day operator; credential-gated)

- Operator sets `EOS_SWEEVO_REAL_AGENT_TESTS=1` and runs the three S7 tests against the same canonical instance from the conftest `sweevo_instance` fixture.
- Each test produces its own `report.run_dir` under `audit_dir` keyed by `instance_id + timestamp` — three distinct directories. Mechanically distinguishable by inspecting `run.json["coding_plan_mode_active"]` and `run.json["scenario_name"]`.
- Capture per mode (mechanically):
  - `report.task_center_status`
  - `report.sweevo_result.fail_to_pass_total`
  - `report.sweevo_result.pass_to_pass_total`
  - `run.json["coding_plan_mode_active"]`
  - `coding_plan_mode_error` caplog count (should be 0)
  - `report.duration_s`
  - Tool-call shape — ordered list of `tool_name` (no arg hashing; deferred per architect feedback)
  - `report.run_dir`, `report.sandbox_id`
- **Re-run budget (per Pre-mortem #5):** each mode gets up to 1 retry on `task_center_status in {"failed", "cancelled"}`. Reuse same `EOS_TIER_RUN_ID`. After 2 attempts, the mode counts as FAIL.
- **Gate verdict matrix:**

  | Condition | Verdict | S9 promotion |
  |---|---|---|
  | All 3 modes (after re-run budget) reach `task_center_status == "done"` AND `fail_to_pass_total > 0` AND `coding_plan_mode_active` correct per mode AND zero `coding_plan_mode_error` in all 3 | **PASS** | Auto-promote |
  | All 3 modes pass the above AND no re-runs needed AND runtime + F2P deltas across modes within soft tripwires (runtime ratio < 2x AND F2P delta < 50%) | **PASS (strong)** | Auto-promote |
  | All 3 modes satisfy outcome assertions BUT soft tripwires exceeded (runtime > 2x OR F2P delta > 50%) OR retries needed in any mode | **PASS-WITH-NOTES** | **Requires explicit operator sign-off line in `capability_parity_benchmark.md`** before S9 fires |
  | Any mode reaches `"failed"`/`"cancelled"` after retry OR `fail_to_pass_total == 0` on `done` OR `coding_plan_mode_active` wrong OR `coding_plan_mode_error` count > 0 | **FAIL** | No promote; defer; investigate |

  *Soft-tripwire baseline note:* 2x runtime + 50% F2P delta are unjustified placeholders (no data baseline). They're tripwires for the PASS-WITH-NOTES classification only — they DO NOT block PASS by themselves. After the first three runs land, the benchmark report should propose new baseline thresholds informed by observed data.

- Report: `.planning/capability_parity_benchmark.md`. Sections: Setup, Run Commands, Three Mode Rows (status / F2P / P2P / A11 / errors / runtime / tool-call shape), Soft Tripwire Analysis, Verdict, Operator Sign-Off (if PASS-WITH-NOTES), Follow-up Actions.

### Sprint S9 — Status promotion (~30 min, doc-only, gated on S8)

**"Operator" definition (architect v3 review):** the human running the S8 sweep, named in the sign-off checkbox (e.g., `- [x] Operator sign-off: 2026-MM-DD Yifan — divergence acceptable because: ...`). In a multi-operator environment, the sign-off line names the specific human accepting the divergence.

- On S8 **PASS** / **PASS (strong)** — auto-promote:
  - `docs/coding_plan_mode.md`: `**Status: experimental.**` → `**Status: stable-beta.**` + one-line link to benchmark report.
  - `.planning/coding_plan_mode_plan.md` status header: append `; Phase 3 = PASS (2026-MM-DD)`.
  - Iteration log: append v9.4 entry with run timestamps + report link.
- On S8 **PASS-WITH-NOTES** — promote ONLY after explicit operator sign-off line in benchmark report (e.g., a markdown checkbox `- [x] Operator sign-off: {{date}} {{name}} — divergence acceptable because: {{rationale}}`). Without sign-off, status stays "experimental".
- On S8 **FAIL** — do NOT promote. Open follow-up ticket. Iteration log: v9.4-FAIL entry.

### Sprint gates summary (regression gate folded into S6 + S7 per critic minor-2)

- S6 gate: new unit test passes + no regression in 94 existing tests + ruff clean.
- S7 gate: py_compile + ruff clean + 3 tests collected (SKIP without env var) + 94 existing tests still pass.
- S8 gate: benchmark report verdict.
- S9 gate: docs updated; iteration log entry; status header bumped.

---

## ADR

- **Decision:** Execute Option A — symmetric live e2e via `run_sweevo_real_agent` against the canonical SWE-EVO instance, three modes, outcome-shape gate, with re-run budget for vendor flakes. S6 closes Codex A7 symmetry before live runs. S9 promotes status iff S8 PASS or PASS-WITH-NOTES + operator sign-off.
- **Drivers:** Confidence, cost (3 runs × at-most-2 attempts), drift detection.
- **Alternatives considered:**
  - **Option B (live smoke only)** — architect steelman. Adopt as fallback iff S8 FAILs and divergence investigation determines parity claim is structurally infeasible against current vendor offerings.
  - **Option C (multi-scenario sweep)** — scope creep.
- **Why chosen:** Outcome-shape gate is achievable on first try; matches master plan §Phase 3 contract; symmetric vendor coverage; tightly bounded operator commitment with documented re-run budget.
- **Consequences:**
  - ~½-day operator time for S8 (3 runs + up to 3 retries = at most 6 invocations).
  - Status promotion auto on PASS; gated on operator sign-off for PASS-WITH-NOTES.
  - FAIL triggers divergence investigation; plan-mode stays experimental.
  - Codex side gains A7 symmetry — token-rotation resilience uniform across vendors.
- **Follow-ups:**
  - **v6 file reorg** (`anthropic_native.py` → `providers/clients/api/`) — fires after S9 lands.
  - **A6 per-agent override** — out of scope.
  - **Linux Keychain support** — separate spike.
  - **Capability divergence investigation skill** — codify if S8 FAILs.
  - **Tool-call argument canonicalization** — demoted to follow-up observation; revisit when vendor outputs converge.
  - **PASS-WITH-NOTES baseline thresholds** — after first three runs land, propose data-informed thresholds for runtime and F2P delta.
  - **Cloudflare allowlist bump procedure** — document single-source-of-truth constants location.

---

## Sprint Sizing Summary

| Sprint | Scope | Estimate | Credential-gated? | Gate |
|---|---|---|---|---|
| S6 | Codex A7 refresh-on-401 symmetry (mirrors S3 file:line) + test | ~1 hour | No | 2 new test passes; 94 existing tests still pass; ruff clean |
| S7 | A18 live e2e impl (3 tests via run_sweevo_real_agent + plan-mode registration fixture + caplog for log assertion) | ~3 hours | Build no; execution yes | py_compile + ruff clean; 3 tests collected; SKIP without env var; 94 existing tests still pass |
| S8 | Canonical SWE-EVO × 3 modes (3 runs, up to 3 retries) | ~1 day operator | YES | `.planning/capability_parity_benchmark.md` records PASS / PASS-WITH-NOTES / FAIL with re-run notes |
| S9 | Status promotion (docs + plan headers) | ~30 min | No (S8-gated) | Auto on PASS; operator sign-off required for PASS-WITH-NOTES; no-op on FAIL |

**Total: ~1.5 days code + ~1 day operator runs.**

S6 + S7 are credential-INDEPENDENT and runnable in a ralph session immediately. S8 is the credential gate. S9 is doc-only.

---

*End of plan, v3 (critic ITERATE applied).*
