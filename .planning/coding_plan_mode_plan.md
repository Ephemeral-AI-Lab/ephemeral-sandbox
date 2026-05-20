# Coding Plan Mode for EphemeralOS

**Status:** APPROVED v8.1 + v9 amendment (progressive de-risking); Phase 0.3 = GO, Phase 0 = GO, Phase 0.7 = SHIP-AS-IS (all 2026-05-20) — v6 baseline approved at iteration 5; v7 amendment (three-repo cross-verification) ran ralplan consensus loop: Planner v7 → Architect (PROCEED-WITH-AMENDMENTS, 5 items) + Critic (ITERATE, +2 items) → Planner v8 (7 fixes) → Architect re-review (PROCEED-WITH-AMENDMENTS, 2 stale ADR phrases) → v8.1 textual cleanup → Critic re-review **APPROVE**.
**Owner:** Yifan
**Date:** 2026-05-20
**Iteration log:** v1 → Architect (6 items) → v2 → Critic (10 items) → v3 → Architect re-review (4 FLAGs) → v4 → Critic partial re-review (1 wire-ordering FLAG) → v5 → Critic final (APPROVE-WITH-NOTES, `try_get_active_model_kwargs` swap applied) → APPROVED.

---

## 1. Background

EphemeralOS drives agents today via direct API calls to Anthropic (`anthropic.AsyncAnthropic.messages.stream(...)`). Every token is metered per call.

"Coding plan mode" lets the user instead drive agents off their flat-rate vendor subscription (Claude Max + overage credits, ChatGPT Plus/Pro via the Codex backend, etc.). For long multi-agent runs this can flip economics significantly. This plan: how do we add plan mode without disturbing API mode or our sandbox/tool architecture, modeled on what Nous Research's Hermes Agent has already shipped?

## 2. Hermes findings (the "study" deliverable)

Hermes does NOT have one coding-plan mechanism — it has **three distinct patterns**, picked per vendor:

| Pattern | Used for | Subprocess? | Tool loop | Wire |
|---|---|---|---|---|
| **A. OAuth token reuse → direct HTTPS** | Anthropic (Claude Max + overage credits); OpenAI Codex default mode | No | Hermes owns | Anthropic Messages API / Codex Responses API |
| **B. Long-lived JSON-RPC subprocess** | Codex `app-server` mode (opt-in) | Yes (1 per agent) | Codex owns | Newline-delimited JSON-RPC 2.0 stdio |
| **C. Per-turn ACP subprocess** | GitHub Copilot | Yes (1 per turn) | Copilot owns, Hermes regex-extracts | ACP over stdin/stdout |

Critical, load-bearing facts:

- **Hermes does NOT drive the `claude` CLI as a subprocess.** "Claude Code plan mode" in Hermes = reuse Claude Code's OAuth credentials and POST directly to `api.anthropic.com`. The CLI binary is never invoked.
- **The abstraction is `ProviderProfile` + `api_mode` discriminator** routing into per-mode transports.
- **Patterns B and C surrender the tool loop.** When Codex `app-server` owns the turn, Hermes' in-process tools become unreachable mid-turn unless re-exposed via an MCP callback. We do not accept this trade.
- **Subscription-mode vendor quirks are real and per-vendor.** Claude Max plan-mode only consumes overage credits (Pro doesn't work at all). Codex app-server needs CLI ≥0.125. Copilot has no permission flow.
- **Hermes refreshes OAuth tokens.** Without it, mid-stream 401s after token expiry would be unrecoverable.

Full file-level findings: `/Users/yifanxu/.claude/jobs/ba6dd6fd/hermes_research.md`.

## 3. EphemeralOS substitution surface

- **Single LLM call site:** `backend/src/providers/clients/anthropic_native.py:138` (to be moved to `backend/src/providers/clients/api/anthropic_native.py` per v6 namespace reorg) — `self._client.messages.stream(**params)`.
- **Already-pluggable protocol:** `backend/src/providers/types.py:98-102` — `SupportsStreamingMessages` accepts any object with `stream_message(request) -> AsyncIterator[ApiStreamEvent]`. **This is our seam.**
- **Already-existing-but-unused dispatch discriminator:** `backend/src/db/models/model_registration.py:23` declares `class_path: Mapped[str]`. `backend/src/db/stores/model_store.py:75,131,241` persists it. But `backend/src/providers/provider.py:24` hardcodes `from providers.clients.anthropic_native import AnthropicClient` and ignores `class_path`. **Verified at planning time by grep.** → No new schema column; activate the dead one.
- **Flexible JSON metadata column also already exists:** `model_store.py:108` — `kwargs_json` is `Text`-typed JSON, used for any per-class config (auth strategy choice, override fields, notice text).
- **Framework owns the tool loop:** `backend/src/engine/query/loop.py:294-404` — load-bearing for layerstack/OCC ([[project_ephemeralos_layerstack_occ_design]]).
- **`db_kwargs` post-client-creation only reads `max_tokens`** (`backend/src/engine/agent/factory.py:340`). The audit recorder (`backend/src/task_center_runner/audit/recorder.py:40-49`) only persists record dataclasses, never `model_registrations`. OAuth-mode-that-bypasses-DB is **strictly safer** than api-key mode. [Architect-confirmed.]

Full map: `/Users/yifanxu/.claude/jobs/ba6dd6fd/ephemeralos_map.md`.

## 3.5 Vendor credential storage (verified at planning time)

Probed on macOS dev machine 2026-05-20. Resolves Critic OQ#6.

**Anthropic (Claude Code on macOS):**
- Credentials live in **macOS Keychain**, NOT in `~/.claude/credentials.json` (that path does not exist).
- Keychain entry: service `Claude Code-credentials`, account `$USER`. Retrieve via `security find-generic-password -s "Claude Code-credentials" -a "$USER" -w`.
- Value is a JSON blob:
  ```
  {"claudeAiOauth": {
    "accessToken": "sk-ant-oat01-...",
    "refreshToken": "sk-ant-ort01-...",
    "expiresAt": <ms-since-epoch>,
    "scopes": [...],
    "subscriptionType": "max" | "pro" | ...,
    "rateLimitTier": "..."
  }}
  ```
- **Linux storage path is open** — likely Secret Service via `libsecret`, possibly a fallback file. Phase 1 entry gate: also probe on a Linux dev host; document the path before merging.
- `refreshToken` IS present → A3's `refresh()` is live, not dead code.

**OpenAI Codex:**
- Credentials at `~/.codex/auth.json` (cross-platform). Mode 0600.
- Shape: `{auth_mode, OPENAI_API_KEY, tokens: {id_token, access_token, refresh_token, account_id}, last_refresh}`.
- `tokens.refresh_token` IS present → A4's refresh path is live.

**Security implication:** access to either credential is sufficient to impersonate the user against the vendor. Token-leak test in A8 must cover *both* the JSON-payload egress paths AND any subprocess env exposure.

## 4. RALPLAN-DR

### Principles

1. **Preserve framework-owned tool loop.** EphemeralOS's value is layerstack + OCC + our tool ecosystem + audit. Surrendering the tool loop surrenders that value.
2. **Provider selection is data, not branches.** Substitution happens at the `SupportsStreamingMessages` seam. The factory has at most *one* branch (empty vs. non-empty `class_path`). All per-class config lives inside the chosen class.
3. **API mode stays the default, unchanged.** Plan mode is opt-in. Existing tests and benchmarks must run untouched.
4. **One pattern per vendor.** Mirror Hermes' three-pattern menu; don't force one mechanism on every vendor. Ship the lowest-cost / highest-value pattern first.
5. **Auth artifacts stay where the vendor put them.** Read keychain / `~/.codex/auth.json` in-place. Don't copy tokens into our DB.
6. **Activate dead discriminators before adding new ones.** `class_path` already sits on the row.

### Decision Drivers (top 3)

1. **Vendor coverage × integration depth tradeoff.** OAuth-direct (pattern A) covers Claude Max + Codex cheaply but excludes Copilot/Gemini-CLI. Subprocess patterns (B/C) cover those at the cost of tool-loop ownership.
2. **Tool-loop ownership = sandbox-correctness story.** Patterns B/C make the vendor own the loop, which means the vendor's sandbox runs, not ours. Incompatible with our layerstack/OCC bet.
3. **First-PR scope.** Ship A only; structure the seam so B/C can slot in later via additional `class_path` values without touching existing rows.

### Viable Options

#### Option 1 — OAuth-direct only (Hermes pattern A), strategy-injected on existing client

- Wire `make_api_client()` to dispatch on the **already-existing** `class_path` field via `importlib.import_module`. Empty `class_path` → today's behavior (backwards-safe).
- Refactor `AnthropicClient.__init__` to accept an `auth_strategy` callable returning `{auth_token | api_key, default_headers}` plus an optional `refresh()` hook. Today's behavior becomes `make_api_key_strategy(api_key)`. Plan-mode = `make_claude_oauth_strategy()` reading from macOS keychain.
- `CodexResponsesClient` is a separate class (different wire format → no shared base) reading `~/.codex/auth.json`.
- Sub-mode metadata lives in the existing `kwargs_json` blob.

**Pros**
- No schema migration. Activates dead `class_path` column.
- No duplicated stream parser. Anthropic plan + API share one class with two strategies. Codex is honestly different.
- Full preservation of tool loop, sandbox, tool ecosystem.
- Covers the two highest-value subscription paths.

**Cons**
- Does NOT cover Copilot, Gemini-CLI, Cursor.
- Claude Max plan-mode charges overage credits, not base allowance — needs explicit warning.
- Vendor endpoint instability — `chatgpt.com/backend-api/codex` is not a public API.
- Token-refresh mid-stream means we may have to replay deltas — small semantic cost.

#### Option 2 — OAuth-direct + per-turn ACP subprocess (A + C)
*[unchanged from v2]*

#### Option 3 — Full Hermes-equivalent (A + B + C) with `ProviderProfile` abstraction
*[unchanged from v2 — Critic confirmed steelmanning is fair]*

### Recommendation: Option 1, with `class_path` activation + auth-strategy injection

### Pre-mortem (3 scenarios)

1. **Surprise overage-bill scenario.**
   - *Mitigation:* notice-text in `kwargs_json`, `[coding-plan-mode] <provider>` CLI line, `coding_plan_mode_active=true` audit field. Enforced by A10 + new A11. Documented in `docs/coding_plan_mode.md`.

2. **Token leak via shared sandbox / audit log scenario.**
   - *Mitigation:* tokens live only in host-process client memory. Structural property: `db_kwargs` only reaches `make_api_client` + `max_tokens` (`engine/agent/factory.py:340`); the audit recorder only persists record dataclasses (`recorder.py:40-49`). Enforced by A8's static-graph regression. **Strengthened:** A8 also asserts no plan-mode subprocess env passes the token string. (Tokens accidentally leaking into a subprocess env are the genuine remaining risk, since macOS keychain access is per-process; the keychain itself doesn't leak.)

3. **Vendor ToS violation / account-ban scenario.**
   - *Mitigation (strengthened):* (a) **`EOS_DISABLE_CODING_PLAN_MODE=1` kill-switch env var** — if set, `make_api_client()` rejects any non-empty `class_path` resolving into `providers.clients.coding_plan.*` and raises a clear error. Documented in `docs/coding_plan_mode.md` and `.env.example`. (b) Mark `experimental` in docs. (c) User responsible for vendor ToS.

### Acceptance Criteria

**A1.** No new schema columns. `class_path` (existing, unused) becomes the dispatch discriminator. Empty `class_path` → today's `AnthropicClient(api_key=..., base_url=...)`. Verified by `git diff db/migrations/` being empty + a regression test that loads a pre-existing seed row.

**A2.** `AnthropicClient.__init__` accepts a new **required** `auth_strategy: AuthStrategy` parameter and an optional `default_headers: dict[str, str] | None = None`. **Required, not None-default**, because all three current call sites are under our control. Required because making it optional would create two code paths for one behavior (CLAUDE.md §2 violation). **Three constructor call sites updated in the same patch** (verified at planning time by `grep "AnthropicClient(" backend/`):
   1. `backend/src/providers/provider.py:38` — production factory; updated to construct `make_api_key_strategy(api_key)` and pass it.
   2. `backend/tests/unit_test/test_providers/test_provider_routing.py:14` — test site; same update.
   3. `backend/tests/unit_test/test_providers/test_anthropic_client.py:108` — test site; same update.

   **v6 reorg additional sites** (file moves to `providers/clients/api/anthropic_native.py`; module-path references must also be updated — verified by `grep "providers\.clients\.anthropic_native\|providers/clients/anthropic_native" backend/`):
   - `backend/src/providers/clients/__init__.py:3` — `from providers.clients.anthropic_native import AnthropicClient` → `from providers.clients.api.anthropic_native import AnthropicClient` (re-export kept so external callers using `providers.clients.AnthropicClient` continue to work; preserves Principle 3 "API mode stays the default, unchanged" for downstream consumers).
   - `backend/src/providers/provider.py:24` — import inside `make_api_client` fallback path; same redirect.
   - `backend/tests/unit_test/test_providers/test_provider_routing.py:8` — test import; redirect.
   - `backend/tests/unit_test/test_providers/test_anthropic_client.py:11` — test import; redirect.
   - `backend/tests/unit_test/test_providers/test_anthropic_client.py:423` and `:457` — `patch("providers.clients.anthropic_native.asyncio.sleep", ...)` mock strings; both update to `providers.clients.api.anthropic_native.asyncio.sleep`.
   - Total mechanical update sites for v6 reorg: 6 (4 imports + 2 patch strings). Total per-patch update sites combined with A2 strategy injection: 3 constructor calls + 6 path references = 9 sites in one PR. Mechanical, IDE-assisted, low-risk.

   (Architect re-review caught the two test sites the Critic and Planner missed. v6 reorg adds the path-rename sites.)

   `AuthStrategy` is a `Protocol` with exactly two methods: `get_auth_kwargs() -> dict[str, str]` (returns headers / api_key / auth_token to pass to the SDK) and `refresh() -> bool` (returns True if the strategy mutated its state with a new credential). Two implementations Day 1 (`make_api_key_strategy`, `make_claude_oauth_strategy`); Protocol over callable chosen for discoverability of the two-method contract.

**A3.** `make_claude_oauth_strategy()`:
   - On macOS: shells out to `security find-generic-password -s "Claude Code-credentials" -a "$USER" -w`, parses JSON, extracts `claudeAiOauth.accessToken` and `refreshToken`.
   - On Linux: documented as Phase 1 entry-gate — probe `libsecret` / Secret Service / fallback file *before* writing code. **Linux support deferred to Phase 1.5 if non-trivial.**
   - `refresh()` exchanges `refreshToken` for a new `accessToken` against Anthropic's OAuth token endpoint. Caches new credentials back to keychain (matching Claude Code's own behavior, so re-running `claude` afterwards still works).

**A4.** `CodexResponsesClient` reads `~/.codex/auth.json` (`tokens.access_token`, `tokens.refresh_token`). POSTs to `chatgpt.com/backend-api/codex`. Translates Codex Responses-API stream → `ApiStreamEvent` union. **Spike (Phase 0) confirms event translation does not require extending `ApiStreamEvent`** — see Phase 0 gate below.

**A5.** `make_api_client()` dispatch — pinned contract:
   > Reads `class_path` from `db_kwargs`. **Empty/missing → fallback:** today's hardcoded `AnthropicClient(...)` path, with import redirected to the new location: `from providers.clients.api.anthropic_native import AnthropicClient` (per v6 reorg), and with the new `auth_strategy=make_api_key_strategy(api_key)` argument. **Non-empty → activated:** `module_str, _, attr_str = class_path.partition(":")`; `cls = getattr(importlib.import_module(module_str), attr_str)`; instantiate as `cls(db_kwargs=db_kwargs)`. **Class only — no factory-function branch** (no Day 1 use case; rejected as premature optionality per CLAUDE.md §2; revisit if/when a factory is ever needed). **Each coding-plan client class is responsible for inspecting `db_kwargs.get('kwargs', {}).get('auth')` and constructing its own `auth_strategy` internally.** Factory has exactly one branch (empty vs. non-empty); the kwargs schema lives in the client. Unknown / malformed / unimportable / non-class `class_path` raises `NoActiveModelError(f"unknown class_path {class_path!r}: {cause}")`. Unit test covers (i) empty path, (ii) valid class path (e.g. `providers.clients.coding_plan.anthropic:AnthropicPlanClient`), (iii) malformed path (no `:`), (iv) unimportable module, (v) attribute-not-found, (vi) attribute not a class.

**A6.** Agent profile frontmatter `model:` field continues to work exactly as today (overrides `db_kwargs["model"]`). **Scope clarified:** per-agent override of `class_path` is OUT OF SCOPE for this plan. Plan-mode is selected by which `model_registrations` row is active; if a user wants different agents on different modes, they set up multiple rows and use existing model-id routing. Adding per-agent `class_path` override would require new work in `engine/agent/factory.py:180-183` that today only routes `resolved_model`, not the client constructor. Deferred to a follow-up if demanded. (Closes Critic item #10.)

**A7.** Token-refresh semantics: when `auth_strategy.refresh()` returns `True` mid-stream after a 401, the request retries **once** even if `emitted_any=True` — at the cost of replaying deltas already yielded. `stream_message`'s docstring states this explicitly. Default `api_key_strategy.refresh()` returns `False`, preserving current behavior. **Closes the `anthropic_native.py:96` short-circuit gap.** Explicit unit test in Verification Plan below.

**A8.** Token-leak regression test, three parts:
   - **Static graph:** assert `task_center_runner/audit/recorder.py` has no import of `db.stores.model_store` or `providers.clients.coding_plan.*`.
   - **Runtime payload audit:** run a no-op agent in plan mode against a recorded fixture; assert the OAuth token literal does not appear in any audit JSON file under the run dir.
   - **Subprocess env audit:** assert that any subprocess spawned during a plan-mode run (`sandbox/execution/subprocess_runner.py:56-90`) does NOT include `CLAUDE_CODE_OAUTH_TOKEN`, `ANTHROPIC_API_KEY` (when in plan mode), or any string starting with `sk-ant-oat01-` / `sk-ant-ort01-` in its `env`. (Closes Critic item #2 strengthening.)

**A9.** Regression-safety: `make test` passes with no changes to existing API-mode tests, sweevo benchmarks, or mock agent fixtures.

**A10.** CLI prints `[coding-plan-mode] <provider>` notice at agent spawn iff the resolved client module path starts with `providers.clients.coding_plan.`. **Pinned: namespace rule only, no escape hatch.** (Closes Critic item #2 amendment.)

**A11.** *(new)* Audit recorder enforcement: `run.json` written by `task_center_runner/audit/recorder.py::_write_run_json` (line 505) gains a `"coding_plan_mode_active": bool` field. **Wire (pinned, ordering-correct):**
   - `AuditRecorder.__init__` (`recorder.py:154`) gains a new optional parameter `coding_plan_mode_active: bool = False`, stored on `self._coding_plan_mode_active`. Default `False` preserves all existing call sites unchanged.
   - **Resolution happens at run-start in `task_center_runner/core/engine.py:118`** — NOT at spawn — because the recorder is built at engine.py:118 before any agent is spawned (`factory.py:191`'s `make_api_client` call comes strictly later). The earlier critic loop assumed wrong ordering; v4 has been corrected.
   - Add at engine.py:117 (between scenario_name resolution and recorder construction): `class_path = (try_get_active_model_kwargs() or {}).get("class_path", "")` and `coding_plan_mode_active = class_path.startswith("providers.clients.coding_plan.")`. Pass `coding_plan_mode_active=coding_plan_mode_active` to `AuditRecorder(...)`. **Use the non-raising `try_get_active_model_kwargs()` variant** (not `get_active_model_kwargs()`) so that mock-runner / uninitialised-store code paths that today succeed past engine.py:118 continue to work; if no active model is registered, `coding_plan_mode_active=False` is the correct default. (Closes Critic-v5 single non-blocking note.)
   - `engine.py` already imports from `task_center_runner.audit.recorder`; it gains one new import from `config.model_config` for `try_get_active_model_kwargs`. This does NOT violate A8's static-graph check, which targets `recorder.py` specifically. `recorder.py` itself remains free of `providers/` and `model_store` imports — verified by grep at planning time.
   - `_write_run_json` adds `"coding_plan_mode_active": self._coding_plan_mode_active` to its 7-field payload (which becomes 8 fields) at line 507.
   - **Why resolve at engine, not at spawn:** the active model is per-run-fixed (one `model_registrations` row, marked `is_active=true`, resolved once at run-start). It cannot change mid-run. A setter on the recorder (`set_coding_plan_mode_active`) was considered and rejected: it would introduce mutable post-construction state for no flexibility gain.
   - **Multi-agent / per-agent override note (interaction with A6):** the run-level `coding_plan_mode_active` reflects the active registration's class_path, NOT any per-agent profile override. This is intentional: A6 explicitly scopes per-agent `class_path` override out. If/when that lands, A11's wire becomes per-agent (recorder gains a setter, or audit per-agent record gains its own field) — explicit follow-up.
   - Test: register a plan-mode row (`class_path="providers.clients.coding_plan.anthropic:AnthropicPlanClient"`), run a fixture agent, assert `run.json` contains `"coding_plan_mode_active": true`. Register an API-mode row (empty `class_path`), run, assert `false`. (Closes Critic item #3 + Architect-v3 FLAG + Critic-v4 A11 wire-ordering FLAG.)

**A12.** *(new)* Kill-switch env var: `EOS_DISABLE_CODING_PLAN_MODE=1` causes `make_api_client()` to reject any `class_path` resolving into `providers.clients.coding_plan.*` with a clear error. Documented in `docs/coding_plan_mode.md` and `.env.example`. Test: with env var set, attempting to spawn a plan-mode agent raises the expected error. (Closes Critic item #9.)

### Verification Plan

- **Phase 0 spike output gate (pinned):** Spike produces `/Users/yifanxu/machine_learning/LoVC/EphemeralOS/.planning/codex_event_mapping.md` — a markdown table mapping every observed Codex Responses-API event type to one `ApiStreamEvent` variant, with explicit notes for tool-use chunk boundaries (the risk point: `anthropic_native.py:168-186` where Anthropic uses `content_block_stop` semantics). **GO** iff every row maps onto an existing variant with no lossy coalescing. **EXTEND-UNION** if a new variant is needed → Phase 1 starts with the union extension first. **RECONSIDER** if tool-use boundary semantics fundamentally differ. (Closes Critic item #4.)
- **Unit tests:**
  - `test_anthropic_oauth_strategy.py`: keychain read + parse, refresh-token exchange, refresh-failure handling. **Mocks the `security` subprocess at the `subprocess.run` boundary** — unsigned CI processes cannot access the macOS keychain reliably and would silently false-fail. Live `security` binary is exercised only in manual smoke (§Manual smoke). (Closes Architect-v3 FLAG on §3.5 CI feasibility.)
  - `test_codex_responses_client.py`: event translation correctness for each `ApiStreamEvent` variant.
  - `test_anthropic_client_refresh_replay.py`: **explicit A7 test slot** — feed a recorded stream that emits 2 deltas then 401; assert refresh() runs, request retries once, second attempt yields the full event stream including the replayed deltas. (Closes Critic item #7.)
  - `test_make_api_client_dispatch.py`: six dispatch cases listed in A5.
  - `test_kill_switch.py`: A12 enforcement.
- **Integration (offline replay):** one recorded plan-mode stream per provider, anonymized, replayed in CI. Live record gated by `EOS_LIVE_PLAN_MODE=1`.
- **Phase 3 capability-parity (tolerance pinned):** Run sweevo `complex_project_build` in (a) API mode and (b) plan mode for the same task. **GATE:** tool-call sequence identical AND set of modified files identical AND sweevo's existing test-suite assertion passes in both runs. Final patch may differ in non-functional whitespace; we do NOT gate on patch hash equality (LLM nondeterminism). (Closes Critic item #5.)
- **Security:** A8 static-graph + runtime + subprocess-env tests.
- **Backwards compat:** `test_sweevo_docker_smoke.py`, `test_complex_project_build_fixtures.py`, `test_docker_adapter.py` all run unchanged.
- **Manual smoke:** Yifan registers plan-mode rows; runs one task per provider; confirms `coding_plan_mode_active=true` and no token leakage.

### Phases / Sprints

| Phase | Scope | Estimate | Entry / exit gate |
|---|---|---|---|
| **0. Codex stream-translation spike** | A4 spike. Output: `codex_event_mapping.md` (only). Decision: GO / EXTEND-UNION / RECONSIDER. | 1 day | **Exit:** mapping table written; decision recorded. |
| **0.3. Anthropic OAuth end-to-end smoke spike** | Throwaway script (`scripts/spike_anthropic_oauth.py`) exercises the real OAuth endpoint with our exact payload shape (identity preamble + a real recipe system prompt + 3 synthetic user messages + 3-5 real tool schemas from `backend/src/tools/`). Output: `.planning/anthropic_oauth_smoke.md`. Decision: GO / RECONSIDER. See §6.5 below. | 0.5 day, parallel with Phase 0.5 / 0.7 | **Exit:** smoke report written; GO decision recorded (required before Phase 1 starts). |
| **0.5. Linux credential probe (if applicable)** | Probe Anthropic OAuth credential storage on Linux dev host. | 0.5 day, parallel with Phase 0 | **Exit:** Linux path documented or formally deferred to Phase 1.5. |
| **0.7. Codex schema-validity probe** | Dry-call every `backend/src/tools/` schema against Codex Responses API via a thin direct-HTTPS harness (or stub Codex client). Output: `codex_schema_validity_report.md`. Decision: SHIP-AS-IS / SHIP-WITH-SANITIZER. | 1 day, parallel with Phase 0.5 | **Exit:** report written; A16 marked active or N/A. |
| **1. Anthropic plan + factory + class_path activation** | A1, A2, A3 (macOS only), A5, A6, A7, A8, A9, A10, A11, A12. Behind `EOS_PLAN_MODE_ENABLED=1` flag. **Entry gate:** Phase 0.3 returned GO. | 1-2 days | **Exit:** one end-to-end task on Claude Max via plan-mode row; token-leak suite green; API-mode regression suite green. |
| **2. Codex plan client** | A4. Reuses factory + auth-strategy machinery where applicable. | 1 day | **Exit:** one end-to-end task on ChatGPT Plus. |
| **3. Capability-parity benchmark** | Sweevo apples-to-apples with pinned tolerance gate. | 1 day | **Exit:** gate passes on `complex_project_build`. |
| **4. (Deferred) Subprocess patterns** | B / C for vendors gated behind their own CLI. | TBD on demand | **Trigger:** user-reported vendor coverage gap. |

### Open Questions — resolved this iteration

- ~~Claude Code credential file format~~ → **Resolved:** macOS keychain, entry `Claude Code-credentials`. Linux TBD in Phase 0.5.
- ~~Anthropic refresh-token presence~~ → **Resolved:** present (`refreshToken` field).
- ~~Codex auth path / refresh~~ → **Resolved:** `~/.codex/auth.json`, `tokens.refresh_token` present.
- ~~Phase 0 GO/NO-GO criterion~~ → **Resolved:** delta mapping table in `codex_event_mapping.md`.
- ~~Phase 3 tolerance~~ → **Resolved:** identical tool-call sequence + modified-file set + sweevo assertion pass.

### Open Questions — still open

- Anthropic plan-mode required headers (`anthropic-beta` value, UA string). Resolve by reading the current Hermes `agent/anthropic_adapter.py` at implementation time, OR by inspecting Claude Code's own outbound HTTP via mitmproxy on Yifan's box.
- ChatGPT-backend rate limits under N=4 parallel agents. Discover empirically in Phase 2 manual smoke; if rate-limited, add a per-agent semaphore in the client.
- `class_path` import-string format: pinned to `module.path:ClassName` (importlib + entry-point convention).

### ADR

- **Decision:** Adopt Option 1 — OAuth-direct plan mode for Anthropic and OpenAI Codex, dispatched via the already-existing-but-unused `class_path` column. One refactored `AnthropicClient` with auth-strategy injection covers Anthropic API + Anthropic plan modes. One orthogonal `CodexResponsesClient` covers Codex plan mode.
- **Drivers:** (1) vendor coverage × integration depth → Option 1 covers the two highest-value subscriptions at the lowest code cost; (2) tool-loop ownership preserves layerstack/OCC story; (3) first-PR scope kept small via existing seam.
- **Alternatives considered:** Option 2 (A+C, Copilot ACP added) — rejected for premature scope on vendors we cannot yet rank. Option 3 (A+B+C with `ProviderProfile` registry) — rejected because Codex `app-server` surrenders both tool loop and sandbox, breaking layerstack/OCC; and abstraction is premature at N=2.
- **Why chosen:** Activates dead `class_path` instead of adding a new column. Single Anthropic client with strategy injection instead of duplicate sibling. Honest N=2 (auth varies for Anthropic, wire genuinely differs for Codex). Closes the OAuth refresh fault model `anthropic_native.py:96` exposes.
- **Consequences:**
  - One refactored constructor + one strategy Protocol + two strategy implementations + one new client class + one factory branch + three new acceptance tests + one new audit field. No schema migration. No agent-loop changes.
  - **v6 reorg:** `providers/clients/` gains two subfolders — `api/` (housing the existing `anthropic_native.py`) and `coding_plan/` (housing `anthropic.py` thin-wrapper + `codex.py` new client). Symmetric naming invites a third sibling (e.g. `subprocess_plan/`) iff Hermes patterns B/C ever land. Public `providers.clients.AnthropicClient` re-export preserved for backwards compatibility.
  - We do NOT cover Copilot, Gemini-CLI, Cursor with this plan. If demand emerges, Phase 4 picks up Hermes patterns B and/or C.
  - Plan mode is marked `experimental`; user is responsible for vendor ToS.
- **Follow-ups:** (1) Linux Anthropic credential support (Phase 0.5 or Phase 1.5). (2) Per-agent `class_path` override if multi-mode-per-run demand arises (out of current scope). (3) Phase 4 subprocess patterns on demand.

---

## 5. Iteration log

- **v1:** Initial plan with new `auth_mode` DB column + sibling `AnthropicPlanClient` class.
- **Architect (PROCEED-WITH-AMENDMENTS, 6 items):** Caught (i) `class_path` already exists, drop the new column; (ii) one Anthropic client with strategy injection, not a sibling; (iii) token-refresh fault-model gap; (iv) Codex spike moved to pre-Phase-1; (v) static-graph token-leak test instead of regex; (vi) pre-mortem #2 affirmed as positive property.
- **v2:** Applied all six architect amendments. Diff size strictly smaller than v1.
- **Critic (ITERATE, 10 items):** A5 dispatch contract ambiguity, A6 overpromise on per-agent override, A10 escape-hatch wording, Phase 0/3 gate vagueness, A3 credentials file format unverified, missing A7 test slot, A2 None-default rationale, pre-mortem #3 weak mitigation, plus new A11 (audit field) needed.
- **v3:** Closed all 10 critic items. Probed real credential files (macOS keychain for Anthropic, `~/.codex/auth.json` for OpenAI — both have refresh tokens). Pinned A5 dispatch contract, scoped A6 down, pinned A10 namespace rule, added A11 (audit field), A12 (kill-switch). Wrote final ADR.
- **Architect re-review (PROCEED-WITH-AMENDMENTS, 4 FLAGs):** A2 missed 2 test call-sites; A5 added premature class-OR-factory branch; A11 unimplementable as written (no wire to recorder); keychain test infra needs CI mocking.
- **v4:** Surgically closed all 4 architect FLAGs. A2 now lists all 3 call sites. A5 drops factory branch (classes only). A11 pinned constructor param + (then-believed) spawn-side resolution. Verification Plan documents keychain subprocess mock.
- **Critic re-review (partial, 1 FLAG):** A11's spawn-side resolution is wrong — the recorder is built at `engine.py:118` BEFORE any spawn at `factory.py:191`. Wire ordering is reversed.
- **v5:** Corrected A11 wire to resolve `coding_plan_mode_active` at engine.py:117 (one extra `try_get_active_model_kwargs()` call between scenario resolution and recorder construction). Setter approach explicitly considered and rejected. A8 static-graph property on `recorder.py` preserved (the new import lives in `engine.py`, not `recorder.py`). Per-agent override interaction with A6 documented as explicit follow-up. **APPROVED by Critic-final.**
- **v6 (this):** User-requested namespace reorg AFTER consensus approval. Renamed `providers/clients/plan/` → `providers/clients/coding_plan/` and introduced symmetric `providers/clients/api/` housing `anthropic_native.py`. Adds 6 mechanical path-update sites (4 imports + 2 `patch(...)` mock strings) on top of A2's 3 constructor-call updates. Architecturally non-substantive — pure file organization; all acceptance criteria, ADR, and risk analysis unchanged. Re-export from `providers/clients/__init__.py` preserves backwards-compatible public namespace for downstream consumers.

---

## 6. v7 amendment — three-repo cross-verification findings

**Status:** PROPOSED v7 amendment — appended after v6 approval. Net-new acceptance criteria A13–A16. Existing A1–A12 unchanged.

### Background

On 2026-05-20, a three-way cross-repo study verified our v6 plan against three independent, production implementations of the same Anthropic-OAuth-direct + Codex-Responses-direct pattern: **Nous Research's hermes-agent** (Python; `agent/anthropic_adapter.py`, `agent/codex_responses_adapter.py`, `tools/schema_sanitizer.py`), **earendil-works/pi** (TypeScript; `packages/ai/src/providers/anthropic.ts:840-940`), and **openclaw** (TypeScript; `src/agents/anthropic-transport-stream.ts:~1072`). Convergent behavior across three independent codebases is treated as vendor-enforced contract, not local style. Four constraints surfaced that v6 does not address. This amendment closes them.

### Finding 1 — Anthropic OAuth requires hard-coded system block #0

**(a) Constraint.** Every Anthropic OAuth request must include, as the **first** `system` content block (`system[0].text`), this exact string:

> `You are Claude Code, Anthropic's official CLI for Claude.`

The caller's actual system prompt becomes `system[1]`. Omitting block #0 produces intermittent HTTP 500s from Anthropic's infrastructure. Anthropic's OAuth-tier content filters also reject messages that name competitor models / products by name; the identity block is part of how the filter is keyed.

**(b) Evidence.**
- hermes: `agent/anthropic_adapter.py:282` (inline comment explicitly stating the 500 + content-filter rationale).
- pi: `packages/ai/src/providers/anthropic.ts:840-940` (system-block builder prepends the identity string for OAuth auth mode).
- openclaw: `src/agents/anthropic-transport-stream.ts:~1072` (same prepend, same string, OAuth-only).

Three independent implementations, same exact string, same position, same conditionality (OAuth only — not API-key). This is a vendor contract, not a stylistic choice.

**(c) Decision.** **Option 1a — transport-layer transparent prepend inside `AnthropicClient` when the active `AuthStrategy` is an OAuth strategy.** The caller's `system` (recipe-produced) is unchanged; the client wraps it.

**Rationale over alternatives.** Recipe-layer injection (Option 1b) would require every recipe under `backend/src/task_center/context_engine/recipes/*` to know whether the active strategy is OAuth — that's a leak of transport concern into the prompt layer, violating Principle 2 ("provider selection is data, not branches"). A middleware hook on `AuthStrategy` (Option 1c) smears a single concern across two protocols (`AuthStrategy` for credentials, plus a system-block hook). The identity block is *vendor impersonation* and belongs with the strategy that does the impersonating. The Claude-Code OAuth strategy already exists to impersonate Claude Code; making it own the identity block is cohesive (Principle 5, amended).

**(d) Acceptance criteria amendment — net-new A13.**

**A13.** `AnthropicClient.__init__` gains a new optional kwarg `system_prefix: str | None = None`, stored on `self._system_prefix`. `AnthropicClient.stream_message()` prepends a `{"type": "text", "text": self._system_prefix}` block as `system[0]` before sending iff `self._system_prefix is not None`; the caller's `system` blocks become `system[1..N]`. The `AuthStrategy` Protocol itself stays unchanged — strictly two methods (`get_auth_kwargs` + `refresh`). Server-spec strings do NOT leak into the credential abstraction.

  **Source of the prefix.** The factory `make_claude_oauth_strategy()` in `provider.py` constructs the OAuth strategy AND the prefix string `"You are Claude Code, Anthropic's official CLI for Claude."` side-by-side, then passes BOTH to `AnthropicClient(auth_strategy=..., system_prefix=...)`. The API-key path passes nothing (`system_prefix=None` by default), preserving Principle 3 exactly. The strategy object never sees or owns the prefix.

  **Wire site.** Insertion happens inside `AnthropicClient.stream_message` (`backend/src/providers/clients/api/anthropic_native.py`) immediately before the `self._client.messages.stream(**params)` call at line 138. The recipe layer (`backend/src/task_center/context_engine/recipes/*`) is untouched — A13 is invisible to it.

  **Idempotency guard.** If `params["system"]` already begins with a block whose `text` equals `self._system_prefix` verbatim, do NOT prepend a duplicate. (Defensive: lets the same recipe round-trip if someone ever calls `stream_message` with pre-massaged input, e.g. replay tests.)

  **Sub-bullet update on A2.** `AuthStrategy` Protocol remains exactly two methods (`get_auth_kwargs() -> dict[str, str]`, `refresh() -> bool`) — NO third method. The vendor-impersonation identity string is a server-spec detail that flows through the client constructor (`system_prefix` kwarg), wired up by the factory adjacent to the strategy. This keeps the credential abstraction clean and avoids smearing transport concerns across the Protocol.

**(e) Verification.** New unit test `test_anthropic_oauth_system_prefix.py`:
  1. Construct `AnthropicClient(auth_strategy=api_key_strategy, system_prefix=None)`; call `stream_message` with `system=[{"type":"text","text":"You are a helpful Python tutor"}]`; assert the request sent to the SDK has `system[0].text == "You are a helpful Python tutor"` (no prepend).
  2. Construct `AnthropicClient(auth_strategy=claude_oauth_strategy, system_prefix="You are Claude Code, Anthropic's official CLI for Claude.")`; same call; assert `system[0].text == "You are Claude Code, Anthropic's official CLI for Claude."` AND `system[1].text == "You are a helpful Python tutor"`.
  3. Idempotency: with the OAuth-configured client, feed `system=[{"type":"text","text":"You are Claude Code, Anthropic's official CLI for Claude."}, {"type":"text","text":"<recipe>"}]`; assert `len(system) == 2` (no duplicate prepend).
  4. Factory test: call `make_claude_oauth_strategy()` (or whatever the factory entry point becomes); assert the constructed `AnthropicClient` has `self._system_prefix` set to the exact literal and `self._auth_strategy` is the OAuth strategy.

---

### Finding 2 — Tool-name collisions with Claude Code's built-in tools

**(a) Constraint.** Anthropic's OAuth-tier server appears to expect a specific shape for tool blocks. Tool names that collide with Claude Code's built-in tool names — `Read`, `Edit`, `Bash`, `Glob`, `Grep`, `Write`, `WebFetch`, `WebSearch`, `TodoWrite`, `Task`, `MultiEdit`, `NotebookEdit`, `BashOutput`, `KillShell`, `ExitPlanMode` — must either match the canonical casing exactly or be renamed off the collision. Three implementations diverged on strategy but agree that *uncanonicalized collisions cause issues*:
- hermes: blanket-prefixes all tool names with `mcp_`.
- pi & openclaw: rewrite *only colliding* tool names to canonical Claude-Code casing.

**(b) Evidence.**
- hermes pattern: tool registration in `agent/anthropic_adapter.py` (blanket prefix; documented near the schema sanitizer).
- pi: `packages/ai/src/providers/anthropic.ts:840-940` region — collision rewriter against fixed allowlist.
- openclaw: `src/agents/anthropic-transport-stream.ts:~1072` — same fixed allowlist pattern.

**(c) Codebase context.** Our tools live under `backend/src/tools/`. All current tool names are snake_case (`read_file`, `edit_file`, `shell`, `glob`, `grep`, `write_file`, `run_subagent`, etc.) — none collide with the Claude-Code PascalCase reserved set today. The risk is purely future drift: someone adding a new tool named `Read` or `Bash` that silently triggers Anthropic-side weirdness.

**(d) Decision.** **Option 2d — single CI test, no runtime code.** A constructor-time assertion + custom exception class + override flag was originally proposed (Option 2c), but reviewers concurred: shipping defensive runtime machinery for a collision that cannot exist today (all our tool names are snake_case) is a CLAUDE.md §2 violation (no speculative code). Future-proofing instead lives in a single CI guard.

**Rationale over alternatives.** Blanket prefixing (Option 2a, hermes) churns audit trails. Rename-on-collision (Option 2b, pi/openclaw) silently masks bugs. Runtime guard with exception + override flag (Option 2c) is dead-on-arrival code — the override would never fire, the exception would never raise. The CI test (2d) achieves the same future-proofing at zero runtime cost.

**(e) Acceptance criteria amendment — net-new A14.**

**A14.** A single CI unit test at `backend/tests/unit_test/test_tools/test_no_claude_code_collision.py` enumerates every registered tool name in `backend/src/tools/` and asserts that no name appears in the frozenset of Claude Code reserved names: `{"Read", "Edit", "Bash", "Glob", "Grep", "Write", "WebFetch", "WebSearch", "TodoWrite", "Task", "MultiEdit", "NotebookEdit", "BashOutput", "KillShell", "ExitPlanMode"}`. The frozenset is defined **inline in the test file**, NOT exported as a module constant — it has no other consumers, and inlining keeps coding_plan client code free of dead defensive machinery.

  **Rationale.** Today: zero collisions (all snake_case). Tomorrow: if someone introduces a PascalCase tool named e.g. `Read`, CI fails before merge. No runtime code, no exception class, no override flag, no `__init__`-time check. Pure future-proofing via test.

  **What was rejected (and why) — explicit non-ship list:**
   - `PlanModeToolCollisionError` exception class: NOT shipped. Would never raise.
   - `CLAUDE_CODE_RESERVED_TOOL_NAMES` module constant in `providers/clients/coding_plan/anthropic.py`: NOT shipped. The frozenset lives only in the test file.
   - `kwargs_json.allow_tool_name_collisions` override flag: NOT shipped. Would have no use case.
   - Runtime constructor check inside `AnthropicPlanClient.__init__`: NOT shipped. Same reason.

**(e) Verification.** Single unit test `backend/tests/unit_test/test_tools/test_no_claude_code_collision.py`:
  1. Enumerate every tool-name declaration under `backend/src/tools/` (use the same enumeration the framework uses to register tools — discover at test time via the tool registry, NOT by re-implementing the scan).
  2. Assert the set intersection with the inline frozenset is empty.
  3. Failure message names the colliding tool(s) and points the author at this CI rule.

---

### Finding 3 — Codex Cloudflare-allowlist headers

**(a) Constraint.** Codex Responses API endpoint `https://chatgpt.com/backend-api/codex/responses` is fronted by Cloudflare with an allowlist on `originator` + `User-Agent`. Non-allowlisted clients receive `HTTP 403` with header `cf-mitigated: challenge` regardless of token validity. Required headers per hermes inline documentation:

| Header | Value |
|---|---|
| `Authorization` | `Bearer <tokens.access_token>` |
| `ChatGPT-Account-Id` | `<jwt(id_token).chatgpt_account_id>` (JWT-decode the `id_token`; not stored separately in `~/.codex/auth.json`) |
| `originator` | `codex_cli_rs` |
| `User-Agent` | `codex_cli_rs/<version>` (matches originator) |
| `OpenAI-Beta` | `responses=experimental` |

**(b) Evidence.**
- hermes: `agent/codex_responses_adapter.py` (header builder; explicit cf-mitigated comment near the header construction).
- pi: TypeScript Codex adapter (same header set, same `originator=pi` choice — divergent from hermes).
- openclaw: same shape as pi.

v6's A4 currently reads: *"POSTs to `chatgpt.com/backend-api/codex`"* — endpoint correct, headers unspecified. That gap closes here.

**(c) Decision.** **Option 3a — ship as `originator=codex_cli_rs` with matching `User-Agent`.**

**Rationale over alternatives.** `codex_cli_rs` is the most-documented allowlist value (hermes verified live). `originator=pi` works for pi (evidence: they're shipping) but uses one team's allowlist slot we don't own — fragile to vendor allowlist revisions. A new `originator=ephemeralos` (Option 3c) would require Cloudflare allowlisting we cannot guarantee. Principle 5 (amended): we already accept vendor-impersonation cost for Anthropic identity block; symmetric here.

**ToS note.** Impersonating `codex_cli_rs` may violate OpenAI ToS. Pre-mortem #3 (vendor-ban scenario) already covers this; A12 kill-switch + `experimental` doc flag stand. Recorded as accepted risk in the ADR consequences below.

**(d) Acceptance criteria amendment — A4 sub-bullets + net-new A15.**

**A4 (amended sub-bullets — DOES NOT renumber).** `CodexResponsesClient.__init__` constructs the following header set for every request:
  - `Authorization: Bearer <tokens.access_token>` (existing).
  - `ChatGPT-Account-Id: <chatgpt_account_id>` — extracted by decoding `tokens.id_token` (JWT, three base64url segments) and reading the `chatgpt_account_id` claim from the payload. **Pure decode, no signature verification** — we're identifying the account we already authenticated, not validating OpenAI's signature.
  - `originator: codex_cli_rs`.
  - `User-Agent: codex_cli_rs/0.125` (pinned version string matching hermes minimum app-server version; revisit if Cloudflare tightens). Stored as `CODEX_ORIGINATOR_VERSION` module constant for single-source-of-truth.
  - `OpenAI-Beta: responses=experimental`.

**A15.** `CodexResponsesClient.__init__` validates that `~/.codex/auth.json` contains both `tokens.access_token` and `tokens.id_token`, and that the JWT-decoded `id_token` payload contains a `chatgpt_account_id` claim. Missing `id_token` → raise `CodexCredentialIncompleteError("id_token absent from ~/.codex/auth.json — re-run `codex login`")`. Missing claim → same exception class, message names the claim. The `chatgpt_account_id` extraction is unit-tested with a fabricated JWT (three base64url segments, middle = `{"chatgpt_account_id":"abc-123"}`); test asserts the extracted value matches.

  **Convergence evidence.** Convergence on the `chatgpt_account_id` JWT-claim path is confirmed across hermes + pi (hermes: `agent/auxiliary_client.py:444-480` extracts `payload["chatgpt_account_id"]`; pi: `providers/openai-codex-responses.ts:1300-1310` extracts the same claim). openclaw uses the `ChatGPT-Account-Id` header but the JWT-extraction site was not directly observed in the cross-repo study (`unknown from repo via WebFetch` per the openclaw subagent report); treated as one-degree-less-confirmed. Phase 0 manual smoke is the final empirical gate before merge.

  **Mid-stream refresh handling.** If `tokens.id_token` is rotated mid-run (e.g., via `auth_strategy.refresh()` per A7's retry-once flow), `CodexResponsesClient` re-extracts `chatgpt_account_id` from the new `id_token` and updates the cached `ChatGPT-Account-Id` header before the retry request. If extraction fails on refresh (malformed JWT, missing claim), the client raises `CodexCredentialIncompleteError` on the next `stream_message` call, mirroring `__init__` behavior. No silent reuse of the stale account-id; no swallowing of decode errors.

**(e) Verification.** New unit tests:
  - `test_codex_jwt_decode.py`: feed fabricated JWT; assert correct `chatgpt_account_id` extraction. Feed malformed JWT (two segments); assert raise. Feed missing-claim payload; assert raise.
  - `test_codex_request_headers.py`: mock HTTPX `AsyncClient`; instantiate `CodexResponsesClient`; call `stream_message`; assert the outbound request has exactly the five headers above with the expected values.
  - **Phase 2 manual smoke** (Yifan's box): send one request without `originator`/UA; verify Cloudflare returns `cf-mitigated: challenge`. Then send with the full header set; verify 200. Confirms the constraint is live, not just folklore. **Gate Phase 2 exit on this empirical check.**

---

### Finding 4 — Codex stricter JSON-schema validation on nested tool params

**(a) Constraint.** Codex Responses API's backend rejects certain JSON Schema constructs that Anthropic Messages API accepts. Specifically (from hermes `tools/schema_sanitizer.py`):
  - Nested `object` types without explicit `properties` map.
  - `additionalProperties` types Codex doesn't recognize.
  - `$ref` / `$defs` indirection (Codex flattens; Anthropic resolves).
  - `oneOf` / `anyOf` in nested positions.
  - Specific `format` strings on string types.

The sanitizer normalizes schemas pre-request. Without it, Codex returns `HTTP 400` with cryptic schema-validation messages.

**(b) Evidence.**
- hermes: dedicated module `tools/schema_sanitizer.py`. Exists *because* the Codex backend is stricter than Anthropic; the file's docstring states this.
- pi & openclaw: lighter-weight schema normalization in their Codex adapter paths (less aggressive than hermes; presumably their tool set is smaller).

**(c) Codebase context.** Our tool schemas live next to each tool (e.g. `backend/src/tools/*/schema.py` patterns from prior tools refactor — confirmed by recent git log: `tools: upgrade docstrings + restructure each tool into a package`). Exact rejection set is empirical; we don't know which of our schemas will trip Codex until we test.

**(d) Decision.** **Option 4a — extend Phase 0 spike to produce a Codex schema-validity report. Add sanitizer only if rejections are observed.**

**Rationale over alternatives.** Pre-emptively porting hermes' `schema_sanitizer.py` (Option 4b) is non-trivial code shipping ahead of evidence — violates CLAUDE.md §2 (no speculative code). Trusting and fixing on first failure (Option 4c) means production failures during real runs — wastes user time. Evidence-gated sanitizer (4a) is cheapest: Phase 0 already exists for Codex stream translation, extending its output by one report is incremental.

**(d) Acceptance criteria amendment — A4 + Phase 0 gate amendment + net-new A16.**

**Phase 0 stays single-output.** Phase 0 deliverable is ONLY `.planning/codex_event_mapping.md`. Gate decisions are GO / EXTEND-UNION / RECONSIDER — see §Verification Plan. The schema-validity question moves to its own phase (Phase 0.7) so that Phase 0 can ship in 1 day on stream translation alone, and the schema dry-call work — which materially requires a working Codex client or a thin direct-HTTPS test harness — has its own scoped phase.

**Phase 0.7 (new) — Codex schema-validity probe.** 1 day, parallel with Phase 0.5 (Linux credential probe). Deliverable: `.planning/codex_schema_validity_report.md` — runs every tool schema from `backend/src/tools/` through a Codex Responses API `tools=[...]` dry-call (minimal request, empty user message, capture the validation response). Records: schema source file, full request payload, full response, classification (ACCEPTED / REJECTED-with-reason). Tooling: either a minimal direct-HTTPS test harness (requests + the five Cloudflare-allowlist headers from A4) or a stub `CodexResponsesClient` if Phase 1's Codex client is already buildable enough for dry-calls. Gate decision matrix:
  - **SHIP-AS-IS** if 0 rejections. No sanitizer needed; A16 marked N/A and dropped from Phase 2.
  - **SHIP-WITH-SANITIZER** if ≥1 rejection. A16 active: port the minimum subset of hermes-sanitizer transformations needed by evidence.
  - (RECONSIDER folds into SHIP-WITH-SANITIZER with an extra "unknown transformation needed" note — does not block Phase 2; it informs sanitizer scope.)

**A16.** *(conditional — active iff Phase 0.7 returns SHIP-WITH-SANITIZER)* `CodexResponsesClient.__init__` runs every tool schema through `providers/clients/coding_plan/codex_schema_sanitizer.py::sanitize_tool_schema()` before stashing in `self._tools`. The sanitizer implements **only the transformations required by Phase 0.7 evidence** — no speculative hermes-port. Each sanitizer transformation has a unit test driven from the Phase-0.7 report; the report is the authoritative spec.

**(e) Verification.**
  - Phase 0 spike report (above).
  - Unit tests under `test_codex_schema_sanitizer.py`, one per transformation, each citing the Phase-0 evidence row that motivates it.
  - **Phase 2 entry check:** sanity-call one real tool via Codex with sanitized schema; assert 200 response (no schema-validation 400).

---

---

### Finding 5 (v8 net-new) — Plan-mode 4xx/5xx observability

**(a) Constraint.** When Anthropic or OpenAI rotates an allowlist, changes the required identity string, or tightens rate limits, plan-mode requests will start returning 4xx/5xx. Without a structured drift signal, breakage looks like our bug — the user sees opaque failures with no provider attribution. Pre-mortem #4 already calls this out as a likely failure mode; we need *signal*, not pre-emptive gating.

**(b) Decision.** **Option 5a — structured log line at the 4xx/5xx exception boundary inside each client's `stream_message`.** Monitor, don't gate.

**Rationale over alternatives.** Adding a new audit-record field (Option 5b) would gate a feature on schema work for what is fundamentally a debugging affordance. The existing per-event audit structure (`task_center_runner/audit/recorder.py`) does not need extending — a `logging.getLogger(__name__)` line at the right boundary gives operators what they need (provider attribution, status code, error class, retry decision) without touching audit dataclasses.

**(c) Acceptance criteria amendment — net-new A17.**

**A17.** `AnthropicClient.stream_message` and `CodexResponsesClient.stream_message` emit a structured log line via the module-level `log = logging.getLogger(__name__)` (already present in `backend/src/providers/clients/api/anthropic_native.py:36`; mirrored in the new Codex client) tagged with the literal string `coding_plan_mode_error` plus structured fields:
  - `provider`: `"anthropic"` | `"codex"`.
  - `status_code`: int (e.g. 429, 500, 403).
  - `error_class`: short symbolic name (e.g. `"rate_limit"`, `"server_error"`, `"cloudflare_challenge"`, `"unauthorized"`).
  - `retry_attempted`: bool — whether A7's refresh-and-retry path fired.

  **Wire site (Anthropic).** Inside `AnthropicClient.stream_message`, at the existing `except anthropic.APIStatusError` boundary (`anthropic_native.py:205`). Emit the log line BEFORE re-raising or retrying. Gated on `self._system_prefix is not None` (i.e. only when plan mode is active — API-mode 4xx/5xx is already logged elsewhere and is not the drift signal we care about here).

  **Wire site (Codex).** Inside `CodexResponsesClient.stream_message`, at the equivalent HTTPX `HTTPStatusError` (or `httpx.Response.raise_for_status()` failure) boundary. Same field set. No gate needed — the Codex client only runs in plan mode by construction.

  **Audit recorder untouched.** `task_center_runner/audit/recorder.py` is NOT modified. Per-event audit dataclasses do not gain a new field. Rationale: log lines are sufficient for drift detection; promoting them to audit-record state is premature.

  **Rationale (CLAUDE.md §2 check).** This adds one log line at two existing exception boundaries — minimum code that solves the problem. No new abstractions, no new exception classes.

**(d) Verification.** New unit test `test_coding_plan_mode_error_log.py`:
  1. Feed a recorded 429 response to `AnthropicClient` with `system_prefix=<oauth literal>`; assert one `coding_plan_mode_error` log line emitted with `provider="anthropic"`, `status_code=429`, `error_class="rate_limit"`, `retry_attempted=<bool from A7 path>`.
  2. Same for a recorded 500.
  3. Feed a recorded 403 with `cf-mitigated: challenge` header to `CodexResponsesClient`; assert log line with `provider="codex"`, `status_code=403`, `error_class="cloudflare_challenge"`.
  4. API-mode (no `system_prefix`) regression: feed a 429 to `AnthropicClient` with `system_prefix=None`; assert NO `coding_plan_mode_error` log line (the regular API-error log path still fires; that is unchanged behavior).

---

### RALPLAN-DR delta

**Principles amended / added.**
- P2 amended: "provider selection is data, not branches" — extend: vendor-protocol quirks (identity block, originator headers, schema sanitization) live inside the client/strategy, never leak to recipe layer or caller config.
- P5 amended: "auth artifacts stay where the vendor put them" — extend: vendor *identity* artifacts (system block #0 string, originator string, UA, account-id JWT claim) belong with the factory/client surface that owns the vendor-impersonation wiring (not on the credential abstraction itself; see v8 A13 correction).
- *(P7 and P8 were proposed in v7 and dropped in v8. Rationale: A14 collapsed to a single CI test makes P7's "defensive validation at registration time" non-load-bearing, and A16 already justifies itself via Phase 0.7 evidence without needing a meta-principle to authorize evidence-gating. Avoiding bootstrap-justified principles.)*

**Decision Drivers (additive).** Vendor server-side enforcement is opaque — three independent implementations converging on the same hard-coded strings is the strongest signal we'll get of vendor contract. Treat convergence as evidence.

**Pre-mortem (new scenario #4).** **Cloudflare allowlist shift / Anthropic identity-block string change.** If OpenAI rotates the `originator` allowlist or Anthropic changes the required identity string, Day-1 plan mode breaks silently (cf-mitigated challenges, intermittent 500s) and looks like our bug. *Mitigation:* (a) `CODEX_ORIGINATOR_VERSION` and the Anthropic identity literal are single-source-of-truth module constants, easy to bump; (b) Phase 2 manual smoke includes a "headers-stripped" negative test to confirm the constraint is still live and detectable; (c) `[coding-plan-mode]` CLI notice in A10 already surfaces the mode so users can correlate breakage; (d) `EOS_DISABLE_CODING_PLAN_MODE=1` kill-switch (A12) lets users fall back to API mode without code change.

**Open Questions — newly opened by v7.**
- Exact `User-Agent` version string to ship — pinned to `codex_cli_rs/0.125` provisionally; Phase 2 manual smoke validates. Open: does Cloudflare check the version segment, or only the originator? If only originator, version is documentation-only.
- Linux `id_token` parity — does Codex on Linux populate `id_token` with the same `chatgpt_account_id` claim? Probe in Phase 0.5 alongside Anthropic Linux credential probe.
- Anthropic identity-block string drift — is the exact wording stable? Snapshot date is 2026-05-20 across three repos; revisit on every vendor-side breakage.
- Per-recipe override of the identity prefix (e.g. some recipe wants `system[0]` to be something else) — explicitly OUT OF SCOPE; if a recipe needs to override A13's prepend, it must use API mode, not plan mode. Closes layering ambiguity.

**Open Questions — resolved by v7.**
- ~~v6 OQ: Anthropic plan-mode required headers (`anthropic-beta` value, UA string)~~ → partially resolved: A13 nails the identity block (the harder constraint); UA/`anthropic-beta` defer to Phase 1 mitmproxy inspection unchanged from v6.

### ADR addendum

**Consequences (extended from v6).**
- Add one optional constructor kwarg (`system_prefix`) to `AnthropicClient`, wired by `make_claude_oauth_strategy()` factory (v8 — replaces v7's Protocol-method approach); one client-side runtime check (JWT-claim presence on Codex); four new request headers on Codex side; one conditional sanitizer module. **Tool-name collision is enforced by CI test only**, not runtime code (v8).
- Net new acceptance criteria: A13 (identity block), A14 (tool collision guard), A15 (Codex JWT claim), A16 (Codex schema sanitizer, conditional).
- Phase 0 spike output extended: schema validity report added next to event mapping table. Phase 0 still 1 day estimated; running tool schemas through a dry-call is automatable in a script.
- **Accepted impersonation cost:** we ship `originator: codex_cli_rs` and `system[0]: "You are Claude Code…"`. Both are vendor-impersonation. Plan mode remains `experimental`; user is responsible for vendor ToS (unchanged from v6).
- **Backwards-compatibility preserved:** A13 is gated by the constructor `system_prefix` kwarg defaulting to `None` (factory passes the literal only when constructing the OAuth-mode client); API mode is bit-identical to today. A14 is a CI test only — runtime is unchanged in both modes.
- **Ban exit ramp.** If the user account is banned by Anthropic or OpenAI under their ToS (the risk Pre-mortem #3 covers), restore-to-API-mode is trivial: remove the plan-mode `class_path` value from the active `model_registrations` row and the system falls back to today's API-mode behavior. A8 ensures no OAuth tokens were ever persisted in our DB, so no purge step is needed — credentials live only in the vendor's storage (macOS Keychain / `~/.codex/auth.json`).

**Drivers (additive).** Three-repo convergence as primary evidence for vendor-contract constraints.

**Alternatives considered (additive).**
- Identity block: recipe-layer injection rejected (leak of transport concern into prompt layer).
- Tool collisions: blanket-prefix rejected (audit churn); silent rename rejected (CLAUDE.md §1 violation).
- Codex originator: `pi` rejected (squatting another team's slot); `ephemeralos` rejected (no Cloudflare allowlist).
- Codex sanitizer: pre-emptive port rejected (speculative code, CLAUDE.md §2).

**Why chosen (additive).** Each chosen option places vendor-quirk knowledge inside the smallest unit that already owns the corresponding concern (strategy for impersonation, client for transport, spike for evidence). No new abstractions; A13–A16 ride existing seams.

**Follow-ups (additive).**
- If a third plan-mode vendor lands (Phase 4 subprocess patterns), revisit whether registration-time validation should be promoted to a named principle (v7's P7 was dropped in v8 as bootstrap-justification; revisit if a real generalization need emerges).
- Watch for Cloudflare allowlist rotation; Phase 2 manual smoke is the canary.

---

## 7. Iteration log — v7 entry

- **v7 (this amendment):** Cross-verification against hermes-agent, earendil-works/pi, and openclaw (2026-05-20). Closes four gaps in v6: (1) Anthropic OAuth identity-block #0 must be prepended (A13); (2) tool-name collisions with Claude Code built-ins must fail loud at registration (A14); (3) Codex Responses requires Cloudflare-allowlist headers including JWT-decoded `ChatGPT-Account-Id` (A4 sub-bullets + A15); (4) Codex stricter schema validation — Phase 0 spike extended to produce a schema-validity report, sanitizer added only on evidence (A16 conditional). Principles P7 + P8 added. Pre-mortem scenario #4 added. ADR consequences extended. v6 sections 1–5 untouched; all existing acceptance criteria A1–A12 unchanged.
- **Architect re-review of v7 (PROCEED-WITH-AMENDMENTS, 5 items):** A13 leaks server-spec onto `AuthStrategy` Protocol; A14 ships runtime machinery that cannot trigger today (CLAUDE.md §2); A15 typo ("Anthropic's signature" → "OpenAI's signature") and missing convergence statement; Phase 0 over-scoped by folding schema validity into a 1-day spike; path references use stale `backend/src/engine/tools/`.
- **Critic re-review of v7 (ITERATE, 7 items — 5 concurring with Architect + 2 net-new):** concurs on the five above; flags (Major #5) missing plan-mode 4xx/5xx observability — drift detection needs structured signal; flags (Skeptic note) ADR consequences lack explicit ban-exit-ramp documentation.
- **v8 (this revision):** Surgical edits to v7. (1) A13 moved off `AuthStrategy` Protocol; identity prefix now flows via `AnthropicClient(system_prefix=...)` constructor kwarg, wired by `make_claude_oauth_strategy()` factory side-by-side with the strategy. Protocol stays exactly 2 methods (`get_auth_kwargs` + `refresh`). Idempotency guard preserved. (2) A14 collapsed to a single CI test at `backend/tests/unit_test/test_tools/test_no_claude_code_collision.py`; exception class, module constant, override flag, and runtime check all dropped. (3) A15 typo fixed; convergence evidence statement added (hermes + pi confirmed at named line ranges; openclaw treated as one-degree-less-confirmed; Phase 0 manual smoke is the final gate); mid-stream `id_token` refresh handling specified — `CodexResponsesClient` re-extracts `chatgpt_account_id` on refresh, raises `CodexCredentialIncompleteError` on failure. (4) Phase 0 split: Phase 0 stays 1 day for stream translation only (`codex_event_mapping.md`, GO / EXTEND-UNION / RECONSIDER); new Phase 0.7 (1 day, parallel with 0.5) owns schema validity (`codex_schema_validity_report.md`, SHIP-AS-IS / SHIP-WITH-SANITIZER); A16 gating rewritten as "active iff Phase 0.7 returns SHIP-WITH-SANITIZER". (5) P7 and P8 dropped from RALPLAN-DR principles section — A14 collapse removes P7's load, A16's evidence-gating self-justifies without needing P8 as cover; comment preserved noting the drop for future readers. (6) Search-replaced `backend/src/engine/tools/` → `backend/src/tools/` across the v7 amendment. (7) New A17 — structured `coding_plan_mode_error` log line at the `APIStatusError` / `HTTPStatusError` boundaries in both clients (`provider`, `status_code`, `error_class`, `retry_attempted` fields); audit recorder untouched; new test `test_coding_plan_mode_error_log.py` covers 429 / 500 / 403-cf-challenge. (8) ADR consequences extended with explicit ban-exit-ramp sentence — remove plan-mode `class_path` from active row, fall back to today's API mode, no token-purge needed because A8 prevents persistence. Status header bumped v6 → v8.

---

- **v8.1 (this revision):** Architect re-review of v8 caught two stale phrases in the ADR addendum that referenced v7's design (`get_required_system_prefix` Protocol method at :485, `revisit P7` follow-up at :503). Both updated in place to match v8's actual design. No acceptance-criteria or scope changes. Sub-revision rather than v9 because it is purely textual cleanup.

---

## 6.5 v9 amendment — progressive de-risking (Phase 0.3 + A18)

**Status:** APPROVED v8.1 baseline + v9 additive amendment. v9 introduces a new entry-gate spike (Phase 0.3) and a new live e2e acceptance criterion (A18). No existing acceptance criteria modified.

### Rationale

v8.1 derisks Codex stream translation (Phase 0) and Codex schema validity (Phase 0.7) before Phase 1 commits to the larger refactor. There is no equivalent early-evidence gate for the **Anthropic OAuth side** of plan mode — Phase 1 bundles 10+ acceptance criteria (A1–A12) over 1-2 days, and if Anthropic's OAuth contract rejects our specific payload shape (custom recipe system prompt at `system[1]`, our snake_case custom tool schemas, the 3-message spawn shape) at the END of Phase 1, the refactor is wasted. v9 closes that asymmetry by adding a parallel 0.5-day spike.

Separately, v8.1's verification plan covers unit tests + recorded-fixture integration + manual smoke. There is no automated live e2e test that exercises plan mode through a real sandbox, with real custom tools, against a real OAuth endpoint. A18 adds that layer alongside the existing `EOS_SWEEVO_REAL_AGENT_TESTS` pattern.

Both additions are progressive-derisking — they surface contract / wire breakage earlier (Phase 0.3) and later (A18, CI-gated in Phase 3) with low marginal cost.

### Phase 0.3 — Anthropic OAuth end-to-end smoke spike

**Duration:** 0.5 day, parallel with Phase 0.5 / Phase 0.7.

**Deliverables.**
- Throwaway spike script at `scripts/spike_anthropic_oauth.py` (NOT under `backend/src/`; deleted before Phase 1 PR lands).
- Report at `.planning/anthropic_oauth_smoke.md` containing: request headers (token redacted), full response (first 5 KB if streamed), GO / RECONSIDER decision.

**What the script does.**
1. Reads OAuth credentials from macOS keychain via `security find-generic-password -s "Claude Code-credentials" -a "$USER" -w`. Parses the JSON blob; extracts `claudeAiOauth.accessToken`.
2. Constructs `POST https://api.anthropic.com/v1/messages` with:
   - `Authorization: Bearer <accessToken>`
   - `anthropic-beta: claude-code-20250219,oauth-2025-04-20`
   - `User-Agent: claude-cli/<latest>` (provisional — Phase 1 mitmproxy pins exact value; the smoke spike just needs SOMETHING accepted)
   - `x-app: cli`
   - `anthropic-version: 2023-06-01`
   - `system`: two blocks. Block #0 = the Claude-Code identity literal (`"You are Claude Code, Anthropic's official CLI for Claude."`, per A13). Block #1 = a REAL recipe system prompt loaded from `backend/src/task_center/context_engine/recipes/` (script reads the recipe registry to identify and pick a representative recipe).
   - `messages`: 3 synthetic user messages mirroring `backend/src/engine/agent/factory.py`'s spawn-prompt shape (script reads `factory.py` to confirm the 3-message pattern; if the exact shape resists extraction in 0.5 day, fall back to placeholder strings clearly annotated as "representative, not extracted from factory").
   - `tools`: 3-5 REAL tool schemas serialized from `backend/src/tools/` (expected representative set: `read_file`, `edit_file`, `shell`, `glob`, `grep` — script reads the tool registry to identify the exact entries).
3. Streams the response and asserts: (a) HTTP 200, (b) at least one `content_block` of type `text` OR `tool_use`, (c) no content-filter rejection in error fields.

**Gate.** **GO required before Phase 1 starts** (added to the Phase 1 entry gate in §Phases table above).

- **GO:** All three assertions pass. Phase 1 starts as planned.
- **RECONSIDER:** Any assertion fails. Triggers scope re-examination: sanitize recipe system prompt for competitor-name mentions, rewrite tool schemas, swap the beta header set, investigate `cf-mitigated`-style challenges, etc. Phase 1 does not start until the failure mode is understood and either resolved in the spike or a new acceptance criterion is added to absorb the work.

### A18 — Live e2e smoke tests for plan mode

**Net-new acceptance criterion (additive; nothing existing changes).**

**A18.** New live e2e test file at `backend/src/task_center_runner/tests/sweevo/test_real_agent_coding_plan_mode.py`, sibling of the existing `backend/src/task_center_runner/tests/sweevo/test_real_agent.py`. Mirrors the existing real-agent gating pattern: `pytest.mark.skipif(os.getenv("EOS_SWEEVO_REAL_AGENT_TESTS") != "1", reason="Real-agent live e2e gated by EOS_SWEEVO_REAL_AGENT_TESTS=1")`. Additional `pytest.mark.skipif` skips when plan-mode credentials are absent (no `Claude Code-credentials` keychain entry on macOS / no `~/.codex/auth.json`), so unprivileged CI still passes. Depends on function-scoped `workspace` fixture (same as `test_real_agent.py`).

**Test cases (three minimum).**

1. **Anthropic plan-mode + custom tools, end-to-end.** Register a plan-mode `model_registrations` row with `class_path="providers.clients.api.anthropic_native:AnthropicClient"` (or whichever class A5's dispatch contract pins for the plan-mode side — test reads v8 A5 to confirm the exact dispatch + `kwargs_json` shape and mirrors it). `kwargs_json` sets `{"auth": "claude_oauth", "system_prefix": "You are Claude Code, Anthropic's official CLI for Claude."}` (per A13). Run a canonical SWE-EVO instance via `run_sweevo_real_agent`. Assert:
   - `coding_plan_mode_active == true` in the recorded `run.json` (per A11).
   - At least one `tool_use` event uses a custom EphemeralOS tool (e.g. `read_file` or `shell`).
   - The recorded sandbox-touched-files set matches the expected SWE-EVO diff envelope.
   - No `coding_plan_mode_error` log lines at 4xx/5xx (per A17).

2. **Codex plan-mode + custom tools, end-to-end.** Equivalent for Codex via `class_path="providers.clients.coding_plan.codex:CodexResponsesClient"`. Same four assertions.

3. **API-mode regression.** Existing `test_real_agent.py` flow runs unchanged with no `class_path` set: `coding_plan_mode_active == false`, no `coding_plan_mode_error` noise. (If this is fully redundant with `test_real_agent.py` post-Phase-1, replace the body with a cross-reference comment rather than re-asserting.)

**Why this matters (and what A18 catches that earlier layers don't).** A8 unit tests use static analysis + recorded fixtures. A11 unit tests use mocked stores. A17 unit tests feed recorded HTTP responses. None of those exercise: (i) keychain / `~/.codex/auth.json` reads on a real dev box, (ii) the Anthropic identity preamble actually unblocking the request server-side, (iii) Codex's Cloudflare allowlist + JWT-extracted account-id actually passing live, (iv) our snake_case custom tools surviving the OAuth wire and getting invoked by the model, (v) sandbox + plan mode + layerstack/OCC composing end-to-end. A18 is the only layer that does.

**Verification phase.** A18 is part of **Phase 3** (capability-parity benchmark). Phase 3 already runs sweevo `complex_project_build`; A18 runs alongside under the same `EOS_SWEEVO_REAL_AGENT_TESTS=1` gate.

### RALPLAN-DR delta (v9)

- **Principles.** None added. Progressive de-risking is implicit in v6's phase-gate structure; v9 just tightens coverage symmetry between Codex and Anthropic.
- **Pre-mortem #4 (Anthropic identity-block string change / Cloudflare allowlist shift).** Mitigation now empirically gated by **Phase 0.3** (drift surfaces before Phase 1 starts) AND by **A18** (drift surfaces in CI / manual live e2e). Existing mitigations (A10 notice, A12 kill-switch, `CODEX_ORIGINATOR_VERSION` constant) unchanged.
- **Acceptance criteria added.** A18. No others.

---

## 7. Iteration log — v9 entry

- **v9 (this amendment, progressive de-risking):** User-requested additive amendment after v8.1 APPROVED. Closes two asymmetries: (a) v8.1 derisks the Codex side of plan mode with Phase 0 (stream translation) and Phase 0.7 (schema validity) but has no equivalent early-evidence gate for the Anthropic OAuth side — Phase 1 bundles 10+ acceptance criteria and a contract rejection at end-of-Phase-1 wastes 1-2 days; (b) v8.1's verification covers unit tests + recorded fixtures + manual smoke but has no automated live e2e that proves plan mode + real OAuth + real sandbox + our custom tools compose end-to-end. Additions: (1) **Phase 0.3** — 0.5-day throwaway smoke spike (`scripts/spike_anthropic_oauth.py` + `.planning/anthropic_oauth_smoke.md`) that POSTs to `api.anthropic.com/v1/messages` with our actual payload shape (identity preamble + real recipe system prompt + 3-message spawn shape + 3-5 real custom tool schemas read from `backend/src/tools/`); GO required before Phase 1 starts; (2) **A18** — live e2e tests at `backend/src/task_center_runner/tests/sweevo/test_real_agent_coding_plan_mode.py` mirroring the existing `EOS_SWEEVO_REAL_AGENT_TESTS=1` pattern; three cases (Anthropic plan-mode, Codex plan-mode, API-mode regression) asserting `coding_plan_mode_active`, custom-tool invocation, sandbox-diff envelope, and no `coding_plan_mode_error` noise; runs in Phase 3. Pre-mortem #4 mitigation strengthened by gating drift detection on Phase 0.3 + A18. No existing acceptance criteria modified; no principles added. Status header bumped to `APPROVED v8.1 + v9 amendment (progressive de-risking)`.

- **v9.0-correction (textual, 2026-05-20):** Phase 0.3 / Codex schema-probe spike implementations surfaced a path typo in v9 §6.5. The plan repeatedly referenced `backend/src/engine/context_engine/recipes/` (three occurrences); actual repo path is `backend/src/task_center/context_engine/recipes/`. All three corrected via search-replace. No acceptance-criteria changes; spike scripts (`scripts/spike_anthropic_oauth.py`, `scripts/spike_codex_stream.py`, `scripts/spike_codex_schema_probe.py`) handle both paths gracefully via try/except so no implementation rework needed. Documentation-only follow-up; no review pass required.

- **v9.1 (Phase 0.3 execution, 2026-05-20):** Phase 0.3 spike `scripts/spike_anthropic_oauth.py --live` executed against operator's macOS Keychain `Claude Code-credentials` (subscription `max`, tier `default_claude_max_20x`). First attempt returned HTTP 400 `tools.0.custom.output_schema: Extra inputs are not permitted` — root cause: `BaseTool.to_api_schema()` emits a Pydantic-derived `output_schema` field that the OAuth Messages-API rejects. Spike fixed to strip tool payloads down to `{name, description, input_schema}` before sending. Re-run returned HTTP 200 with model `claude-sonnet-4-5-20250929`, two well-formed `tool_use` content blocks invoking custom `shell` tool, `stop_reason=tool_use`, no `cf-mitigated` challenge, no content-filter rejection. **Verdict: GO.** Phase 1 cleared to start, with one new mechanical requirement: `AnthropicClient.stream_message()` under the OAuth strategy must filter outgoing tool schemas to the Anthropic-Messages-API allowlist (`name`, `description`, `input_schema`). This is a one-line transformation; it does NOT require recipe-layer changes (Principle 2 preserved) and does NOT add a new acceptance criterion (covered under A13's wire-site discipline). Full run log in `.planning/anthropic_oauth_smoke.md`.

- **v9.2 (Phase 0 + Phase 0.7 execution corrections, 2026-05-20):** Live Codex spikes (`spike_codex_stream.py --live` + `spike_codex_schema_probe.py --live`) surfaced three corrections to v8.1/v9 claims. The live runs are authoritative; the plan body is now stale where it conflicts. Verdicts: **Phase 0 = GO** (event union sufficient, see `.planning/codex_event_mapping.md`); **Phase 0.7 = SHIP-AS-IS** (all 23 EphemeralOS tools PASS, A16 = N/A, see `.planning/codex_schema_validity_report.md`).

  **Correction 1 — A15 JWT claim path (Auth0-namespaced, not top-level).** Plan A15 cites hermes + pi convergence on `payload["chatgpt_account_id"]`. Empirical id_token from `codex login` on this machine has the claim at `payload["https://api.openai.com/auth"]["chatgpt_account_id"]` (Auth0 namespaced-claim convention). Phase 2 `CodexResponsesClient` MUST check the namespace first, fall back to top-level for forward-compat: `payload.get("https://api.openai.com/auth", {}).get("chatgpt_account_id") or payload.get("chatgpt_account_id")`. Treat the convergence evidence as one-degree-less-confirmed (hermes/pi may have been observed in different account configurations). No test changes — `test_codex_jwt_decode.py`'s fabricated JWT must include the Auth0 namespace.

  **Correction 2 — A4 model gating (ChatGPT-account auth rejects `gpt-5-codex` and `gpt-5`).** ChatGPT-account auth on `chatgpt.com/backend-api/codex/responses` returns HTTP 400 `"<model> is not supported when using Codex with a ChatGPT account"` for both `gpt-5-codex` and `gpt-5`. The accepted model is whatever the user's local `~/.codex/config.toml` declares (`gpt-5.5` on this machine). Phase 2 MUST read the model from `~/.codex/config.toml` `model = "..."` line; do NOT hard-code `gpt-5-codex` as A4's implicit baseline suggested. Default to `gpt-5.5` only if config absent. (Note: this is per-machine config, not a vendor constant.)

  **Correction 3 — Codex Responses tool envelope is FLAT.** Codex Responses API rejects the Chat-Completions nested `{"type":"function","function":{"name":...}}` shape with HTTP 400 `"Missing required parameter: 'tools[0].name'"`. The accepted envelope is FLAT: `{"type":"function","name":..., "description":..., "parameters":...}` at the top level. Phase 2 `CodexResponsesClient` MUST emit the flat envelope. This was an unstated assumption in v8.1 §A4; pin it explicitly here.

  **Correction 4 — `max_output_tokens` rejected by ChatGPT-account auth.** Codex Responses API returns HTTP 400 `"Unsupported parameter: max_output_tokens"` for ChatGPT-account requests. Phase 2 MUST omit `max_output_tokens` from the request body under ChatGPT-account auth. (Codex CLI does not set it.)

  **A16 status:** marked **N/A**. All 23 EphemeralOS tools round-trip through Codex Responses unchanged. Hermes' `schema_sanitizer.py` is not needed for our tool set. If future tools introduce `$ref`/`anyOf` constructs, re-run `scripts/spike_codex_schema_probe.py`; the SHIP-WITH-SANITIZER branch then activates A16 as originally specified.

  **A14 status:** **DONE** (S5 ralph round-1). 24 tests pass; zero collisions with Claude Code reserved set across all 23 tools.

  **Phase 1 entry-gate:** with Phase 0.3 GO + Phase 0 GO + Phase 0.7 SHIP-AS-IS recorded, the progressive-de-risking contract is satisfied. Phase 1 (Anthropic refactor: A1, A2, A3-macOS, A5, A6, A7, A8, A9, A10, A11, A12, A13) cleared to start.

  **Phase 1 schema-strip note:** Phase 0.3 live spike revealed that `BaseTool.to_api_schema()` emits an extra `output_schema` field (Pydantic-derived) that Anthropic's OAuth Messages-API rejects with HTTP 400. Production `AnthropicClient.stream_message` MUST strip outgoing tool schemas to `{name, description, input_schema}` before sending. Per advisor, this transformation is always-on (the field is just dropped — harmless under API-key auth where Anthropic also doesn't expect it; required under OAuth where it is rejected). No new acceptance criterion needed; this is the wire-site discipline noted under A13.

- **v9.3 (this entry, 2026-05-20, ralph round 2 sprint S2):** Rename sweep landed — `plan_mode` → `coding_plan_mode` across env var, audit field, CLI notice literal, and log line. Module constants `LLM_CLIENT_MODE_API = "api_mode"` / `LLM_CLIENT_MODE_CODING_PLAN = "coding_plan_mode"` introduced in `backend/src/providers/auth_strategy.py`; `AuthStrategy` Protocol gains class attribute `llm_client_mode: Literal["api_mode", "coding_plan_mode"]` (per user-requested v5 shape; class attribute, not `@property`, so future Hermes patterns B/C extend the Literal union without changing call-site shape). Files renamed: `docs/plan_mode.md` → `docs/coding_plan_mode.md`, `test_real_agent_plan_mode.py` → `test_real_agent_coding_plan_mode.py`, `test_audit_recorder_no_plan_mode_imports.py` → `test_audit_recorder_no_coding_plan_mode_imports.py`. `api_mode` named as the explicit default via docstring comment in `provider.py` (empty-`class_path` branch) plus opening sentence in `docs/coding_plan_mode.md`. Token-substitution applied to this master plan body (live spec sections + iteration log identifier mentions). Operator-contract change acknowledged per `next_steps_and_rename_plan.md` ADR amendment #6: `coding_plan_mode_error` is the structured log-line literal operators should grep for.

*End of plan, v9 + v9.0-correction + v9.1 + v9.2 + v9.3.*
