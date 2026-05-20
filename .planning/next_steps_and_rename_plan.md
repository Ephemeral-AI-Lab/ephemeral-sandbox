# Next-Steps Roadmap + `plan_mode` → `coding_plan_mode` Rename (with `api_mode` as the explicit default)

**Owner:** Yifan
**Date:** 2026-05-20
**Status:** **APPROVED v5** (2026-05-20) — Ralplan consensus through v4 (Planner v1 → Architect → v2 → Critic ITERATE → v3 → Architect re-review → v4 → Critic re-review APPROVE). v5 is a user-requested shape revision: `is_plan_mode: bool` → `llm_client_mode: Literal["api_mode", "coding_plan_mode"]` (class attribute on `AuthStrategy` Protocol; `LLM_CLIENT_MODE_API` / `LLM_CLIENT_MODE_CODING_PLAN` module constants in `providers/auth_strategy.py`). Same semantics, extensible to Hermes patterns B/C without future refactor. No re-review required — pure substitution at all call sites with no logical change.

---

## Goals

1. **Sequencing.** Given Phase 0 + Phase 0.3 + Phase 0.7 are GO, and Phase 1 MVP (A1, A2, A3-macOS, A5, A9, A12, A13) has landed and is live-verified end-to-end against real Anthropic OAuth, draft the next-step prioritized work for landing the remaining acceptance criteria + Phase 2 + Phase 3.
2. **Naming.** Rename the surface area `plan_mode` → `coding_plan_mode` everywhere it appears (vars, env vars, file names, doc references, audit field). Make the default explicitly named `api_mode` (today it is the "empty `class_path`" fallback path — this rename gives it a first-class name).

## RALPLAN-DR summary

### Principles (3-5)
1. **No backwards-compat shims for a feature that has not yet shipped.** Phase 1 MVP is unmerged. Renaming now costs one careful sweep and zero downstream pain; renaming after merge costs everyone who reads the code thereafter.
2. **Production names match the codebase namespace.** The Python package is already `providers.clients.coding_plan.*`. The env var, audit field, test file names, and docs are stale (`plan_mode`/`PLAN_MODE`). Bring them all into one consistent shape.
3. **The "default" path deserves an explicit name.** `api_mode` is what users opt into by NOT setting `class_path`. Today that is implicit. Naming it eliminates a question users have to answer ("which mode am I in?") by reading source.
4. **Sequence by safety floor, not by feature density.** A8 (token-leak regression) + A11 (audit `coding_plan_mode_active`) + A10 (CLI notice) are the safety floor that lets the Phase 1 MVP be safely merged. They go first, before Phase 2 (Codex client) or Phase 3 (capability parity).
5. **Avoid churn on the throwaway spike scripts.** The three `scripts/spike_*.py` files are deleted when Phase 1 lands per plan §6.5. Do not rename inside them.

### Decision Drivers (top 3)
1. **Merge safety.** What is the smallest set of additions that lets Phase 1 MVP be merged with the audit + leak + kill-switch story complete? → A11 + A10 + A8 + the rename.
2. **Discoverability.** When a new contributor reads `EOS_DISABLE_PLAN_MODE` they will reasonably ask "plan mode for what?" The Hermes / OpenHands / cursor "plan mode" name collision is real and confusing. `coding_plan_mode` resolves it.
3. **Reversibility.** Renaming an unshipped feature is cheap. Adding new acceptance criteria (Phase 2 / Phase 3) on top of stale names compounds the rename cost. Do it now or commit to never doing it.

### Viable Options

#### Option A — Safety bundle FIRST, then rename, then Phase 2
Sequence: (1) A11 + A10 + A8 + ai-slop-cleaner, (2) rename sweep, (3) Phase 2 (Codex client), (4) Phase 3 (capability parity).

**Pros**
- Phase 1 MVP becomes mergeable after step (1). Each subsequent step is a self-contained PR.
- Rename in step (2) touches a smaller surface (Phase 2 is not yet written, so we are not renaming new code we just wrote).
- Phase 2 in step (3) is written with the right names from day one.

**Cons**
- Phase 1 production code (env var, audit field) ships under the old name in step (1) and gets renamed in step (2). One extra commit churn.

#### Option B — Rename FIRST, then safety bundle, then Phase 2
Sequence: (1) rename sweep, (2) A11 + A10 + A8, (3) Phase 2, (4) Phase 3.

**Pros**
- Audit field, env var, CLI notice are written with the correct name on first try. Zero churn.
- Cleaner git history.

**Cons**
- Defers the safety floor by one PR cycle. Token-leak regression should land as fast as possible because the gap is real (today: no test asserts the OAuth token literal does not appear in audit JSON).
- Rename PR is mostly mechanical, which means a reviewer cannot meaningfully gate it on anything substantive — it bypasses the architect/critic loop.

#### Option C — Safety bundle + rename in ONE bundled PR, then Phase 2
Sequence: (1) A11 + A10 + A8 + rename together, (2) Phase 2, (3) Phase 3.

**Pros**
- Zero churn (vs A) AND no deferred safety (vs B).
- One PR review captures both the safety surface and the naming convention.
- Phase 2 starts on a stable, named foundation.

**Cons**
- Larger single PR (~7-9 file touches for A11+A10+A8 plus ~10-15 mechanical rename hits). Mixed-intent diff is harder to review than two focused diffs.
- If reviewer rejects the rename naming, the safety work is blocked behind a bikeshed.

#### Recommendation: **Option A** (safety FIRST, then rename, then Phase 2).

Architect (v2 review) flagged that the churn is materially larger than the v1 draft estimated — closer to **~20-25 hits across 9-10 files** when you include S1's just-written code, the existing landed dispatch in `provider.py`, and existing tests. The reviewability argument still wins (substantive A8/A11 review separated from naming bikeshed), but the cost is honestly material — not negligible. If the rename touches expand further during S1 execution, revisit Option C.

Architect also surfaced **Option D** (do A7 first because it closes a live bug in landed code; A11 + A10 are user affordances not safety; A8 part 3 alone is the irreversible-risk floor). Counter-argument: A7's "live bug" is a mid-stream-401 path that today's manual usage rarely hits (one-shot smoke calls succeed); the irreversible risk is leak-into-version-control, which is fixed only by A8 (specifically part 2 + part 3). User affordances A10 + A11 ride along with the safety PR because they share `engine.py` / `factory.py` touch points and are an O(few-lines) marginal cost — keeping them in S1 amortizes one engine.py edit cycle. A7 is small and orthogonal enough that S3 ordering is fine. **Adopt Option A** with this acknowledged tradeoff.

---

## Concrete next-steps acceptance criteria

### Sprint S1 — Safety floor (~1 day, lands Phase 1 MVP mergeable)

**S1.1 — A11: `plan_mode_active` audit field**

- `backend/src/task_center_runner/audit/recorder.py:AuditRecorder.__init__` gains `plan_mode_active: bool = False` parameter, stored on `self._plan_mode_active`.
- `_write_run_json` adds `"plan_mode_active": self._plan_mode_active` to its run.json payload (8th field).
- Wire site (per Critic Major #1): scenario_name resolution lives at `backend/src/task_center_runner/core/engine.py:114-116`; the `AuditRecorder(...)` constructor call begins at line 118. **Insert at line 117** (the blank line between resolution and construction):
  ```python
  class_path = (try_get_active_model_kwargs() or {}).get("class_path", "") or ""
  plan_mode_active = class_path.startswith("providers.clients.coding_plan.")
  ```
- Pass `plan_mode_active=plan_mode_active` to `AuditRecorder(...)`.
- Use `try_get_active_model_kwargs` (non-raising) so mock-runner / uninit-store paths default to `False` without error.
- **Test:** `backend/tests/unit_test/test_task_center_runner/test_audit_recorder_plan_mode.py` — THREE cases per architect amendment #7. Per Critic ambiguity-risk pin: use `is` identity comparison, NOT `==`, to catch type-drift (e.g., truthy str slipping through). (a) construct recorder with `plan_mode_active=True`, run mock fixture, assert `payload["plan_mode_active"] is True`; (b) default `False`, assert `payload["plan_mode_active"] is False`; (c) no-model-registered path: mock `try_get_active_model_kwargs()` to return None, run engine.py:117 expression, assert the `(... or {})` fallback yields `plan_mode_active is False` and the recorder is built without error. (Cites `backend/src/config/model_config.py:46-50`.)

**S1.2 — A10: CLI `[plan-mode]` notice at dispatch (per architect amendment #3, pinned to `provider.py`)**

- Wire location (per Critic Major #2): **inside `provider.py:make_api_client`** — fire the print **AFTER successful `_resolve_class_path(class_path)` but BEFORE `cls(db_kwargs=db_kwargs)` construction**. Order rationale: a failed class resolution (importerror, attribute missing, not-a-class) raises `NoActiveModelError` from `_resolve_class_path`, and we should NOT have already emitted `[plan-mode] anthropic` in that case — the operator would see a misleading notice followed by an error. Print after resolution succeeds but before construction so a credential failure (mocked-keychain absence, keychain entry malformed) still emits the notice (it tells the operator "we are intentionally attempting plan mode") and the subsequent error is contextualized. Print one line: `[plan-mode] <provider>` where `<provider>` is the segment of `class_path` immediately following `providers.clients.coding_plan.` (e.g., `providers.clients.coding_plan.anthropic:AnthropicPlanClient` → `anthropic`). Master plan §A10 phrases this as "the resolved client module path starts with `providers.clients.coding_plan.`" — `provider.py` has the string directly, so this is the natural site (vs `factory.py:191` which would have to introspect `type(api_client).__module__`).
- Granularity: fires **once per agent spawn**, including every subagent (since `factory.py:191` is the sole spawn path per `_resolve_api_client_and_model_id` shared setup).
- **Test:** `backend/tests/unit_test/test_providers/test_plan_mode_notice.py` (provider-adjacent, not engine-adjacent — matches the wire site) — three cases: (a) capture stdout, dispatch via `class_path="providers.clients.coding_plan.anthropic:AnthropicPlanClient"` (mocked keychain), assert `[plan-mode] anthropic\n` in stdout; (b) API-mode path (empty `class_path`), assert NO `[plan-mode]` in stdout; (c) two consecutive dispatch calls assert the line fires once per call (proves per-subagent granularity).

**S1.3 — A8: Token-leak regression (3 parts; both Anthropic AND Codex per architect amendment #2)**

- **Part 1 (static graph):** already exists at `backend/tests/unit_test/test_task_center_runner/test_audit_recorder_no_plan_mode_imports.py` ✓ (Round-1 S10).
- **Part 2 (runtime payload audit; all files unconditionally per Critic ambiguity-risk pin):** `backend/tests/unit_test/test_task_center_runner/test_audit_recorder_no_token_leak.py` — register a plan-mode row (mock keychain with a known fake token literal), run a recorded no-op fixture, recursively walk the run dir, assert NO file at ANY path under the run dir contains the OAuth token literal. **Walk ALL files unconditionally — do NOT filter by extension.** Read each file as bytes (binary-safe), scan for the fake token byte sequence. Skip only directories. Fail with the offending file path. **Run BOTH cases:** Anthropic-plan-mode row (fake `sk-ant-oat01-FAKE_TOKEN_LITERAL`) AND Codex-plan-mode row (fake `ya29.FAKE_CODEX_ACCESS_TOKEN`-shaped literal + fake JWT id_token `eyJ.FAKE.JWT`). Two cases × full file-tree walk × one negative assertion per case = the test.
- **Part 3 (subprocess env audit; both vendors per architect amendment #2; intercept pin per Critic missing-item):** `backend/tests/unit_test/test_sandbox/test_subprocess_no_token_env.py` — under a plan-mode run, intercept subprocess spawns at the IMPORT SITES inside `backend/src/sandbox/execution/subprocess_runner.py`: `monkeypatch.setattr("sandbox.execution.subprocess_runner.subprocess.Popen", spy)` AND `monkeypatch.setattr("sandbox.execution.subprocess_runner.asyncio.create_subprocess_exec", async_spy)` (verify by grep before writing that those are the actual import paths in subprocess_runner.py; adjust if subprocess_runner uses different aliases). Spies capture the `env` kwarg. Assert `env` does NOT contain:
  - **Anthropic side (when in Anthropic plan mode):** keys `CLAUDE_CODE_OAUTH_TOKEN`, `ANTHROPIC_API_KEY`; values matching regex `sk-ant-(oat|ort)01-[A-Za-z0-9_-]+`.
  - **Codex side (when in Codex plan mode; per Critic missing-item, split id_token regex from access_token literal):**
    - Key `OPENAI_API_KEY` absent.
    - No value equals the loaded `tokens.access_token` literal from the mock `~/.codex/auth.json` fixture (literal-value match; access_tokens may be opaque bearer strings, NOT necessarily JWT-shaped).
    - No value equals the loaded `tokens.id_token` literal (full three-segment JWT string from the mock fixture).
    - No value matches the generic JWT regex `eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+` (catches any other id_token-shaped string).
    - **Two independent checks, not one.** Access_token = literal; id_token = literal AND regex.
  - Two parametrized cases (Anthropic, Codex), one negative assertion bundle each.

**S1.4 — A17 Anthropic-side observability log line (per architect amendment #1; master plan §A17 requires this in Phase 1, not as a follow-up)**

- In `AnthropicClient.stream_message`'s error path (`anthropic_native.py:122-124`), when running under an OAuth strategy AND an unrecoverable error fires, emit one structured log line via `log.error("plan_mode_error", extra={"provider": "anthropic", "error_type": <category>, "request_id": <request_id_or_none>})`.
- **Required local refactor (per Architect re-review blocker #1).** Today's code is a single-line `raise self._translate_error(exc) from exc` at line 122. S1.4 splits this into three lines so the log call has a typed translated exception in scope:
  ```python
  except Exception as exc:
      if emitted_any or attempt >= MAX_RETRIES or not self._is_retryable(exc):
          translated = self._translate_error(exc)
          if self._auth_strategy.llm_client_mode == LLM_CLIENT_MODE_CODING_PLAN:
              log.error("plan_mode_error", extra={"provider": "anthropic",
                                                  "error_type": _categorize(translated),
                                                  "request_id": getattr(exc, "request_id", None)})
          raise translated from exc
  ```
  Approach 1 from architect re-review trade-off table (minimum diff, no new helper method, preserves Critic Major #3 typed-exception categorization).
- **A17 ALSO fires on the `except EphemeralOSApiError` re-raise branch (per Architect re-review blocker #2).** `anthropic_native.py:120-121` re-raises `EphemeralOSApiError` directly without translation. Same emission pattern, except the exception is already typed: `log.error("plan_mode_error", extra={"provider": "anthropic", "error_type": _categorize(exc), "request_id": getattr(exc, "request_id", None)})` — fires before `raise`. Without this, a typed error that bypasses `_translate_error` silently skips A17 observability.
- **Error categorization (per Critic Major #3): categorize from the POST-translation typed exception**, not from raw `exc.status_code`. Mapping (single source of truth, mirrored in S4 Codex side):
  - `AuthenticationFailure` with `.status_code == 401` → `"auth_401"`
  - `AuthenticationFailure` with `.status_code == 403` → `"auth_403"`
  - `RateLimitFailure` → `"rate_limit_429"`
  - `RequestFailure` with `.status_code in {500,502,503,529}` → `"server_5xx"`
  - `RequestFailure` whose message contains `content_filter` or `policy` (case-insensitive) → `"content_filter_rejection"`
  - otherwise → `"unknown"`
- Detection of mode (per Critic Major #4, shape pinned per Architect re-review nit #3, **enum-shaped per user-requested v5 revision**): **add `llm_client_mode: Literal["api_mode", "coding_plan_mode"]` as a CLASS ATTRIBUTE (not `@property`) on the `AuthStrategy` Protocol** in `providers/auth_strategy.py`. Concrete classes set `llm_client_mode = "api_mode"` (default on `_ApiKeyStrategy`) or `llm_client_mode = "coding_plan_mode"` (on `_ClaudeOAuthStrategy`; same value will be set on Phase-2 Codex strategy in S4). Class-attribute + Literal shape was chosen over `@property`/`bool` because:
  - (a) **extensibility**: Hermes patterns B (long-lived JSON-RPC subprocess) and C (per-turn ACP subprocess) are explicitly deferred per master plan §Phase 4. If/when they land, the union extends to `Literal["api_mode", "coding_plan_mode", "subprocess_mode_b", "subprocess_mode_c"]` without changing call-site shape. A bool would need a flip-day refactor.
  - (b) it matches the rename theme — S2 renames `plan_mode_*` → `coding_plan_mode_*` everywhere; this attribute aligns with that vocabulary and with the explicit `api_mode` default name.
  - (c) test fixtures parameterize trivially: `strat = MagicMock(spec=AuthStrategy, llm_client_mode="coding_plan_mode")`.
  - (d) it avoids method/property collision in test doubles.
- Detection at the call site: `if self._auth_strategy.llm_client_mode == "coding_plan_mode":`. This keeps the private class name (`_ClaudeOAuthStrategy`) inside its own module — no cross-module isinstance on a private name. The string-literal comparison is one extra `==` over `is_plan_mode` boolean, accepted cost.
- **Module-level constants** (avoid string-literal typos at comparison sites): in `providers/auth_strategy.py`, add `LLM_CLIENT_MODE_API = "api_mode"` and `LLM_CLIENT_MODE_CODING_PLAN = "coding_plan_mode"`. Concrete strategies use these; call-site detection becomes `if self._auth_strategy.llm_client_mode == LLM_CLIENT_MODE_CODING_PLAN:`.
- **Test:** `backend/tests/unit_test/test_providers/test_anthropic_plan_mode_error_log.py` — mock SDK to raise a 401; instantiate `AnthropicPlanClient` (mocked keychain); call `stream_message`; assert `caplog` captured one `ERROR` record with message `plan_mode_error` and `extra.provider == "anthropic"` and `extra.error_type == "auth_401"`. Symmetric test for 429 (`rate_limit_429`).
- **NOT in scope for S1:** A17 Codex-side wiring (lives in S4 with the Codex client).

**S1.5 — Regression gate (per Critic minor #2: specific assertions, not "make test")** — All existing unit tests pass. NEW tests added in S1 and required to be green:
- `backend/tests/unit_test/test_task_center_runner/test_audit_recorder_plan_mode.py` (3 cases per S1.1)
- `backend/tests/unit_test/test_providers/test_plan_mode_notice.py` (3 cases per S1.2)
- `backend/tests/unit_test/test_task_center_runner/test_audit_recorder_no_token_leak.py` (2 cases per S1.3 part 2)
- `backend/tests/unit_test/test_sandbox/test_subprocess_no_token_env.py` (2 cases per S1.3 part 3)
- `backend/tests/unit_test/test_providers/test_anthropic_plan_mode_error_log.py` (2 cases per S1.4)

Run: `.venv/bin/pytest backend/tests/unit_test/ -q --ignore=backend/tests/unit_test/test_benchmarks --ignore=backend/tests/unit_test/test_live_e2e --ignore=backend/tests/unit_test/test_live_e2e_tools` — assert exit 0 with prior pass count + 12 new passes (3+3+2+2+2). Pre-existing 9 sandbox/layer_stack failures from parallel codex session are out of S1 scope (per `feedback_parallel_user_commits.md` memory).

### Sprint S2 — Rename sweep (~2 hours, mechanical)

**S2.1 — Code & env var renames (corrected hit count per architect amendment #5: ~20-25 hits across 9-10 files)**

| Old name | New name | Files (enumerated) |
|---|---|---|
| `EOS_DISABLE_PLAN_MODE` | `EOS_DISABLE_CODING_PLAN_MODE` | `providers/provider.py:28,44,49` (docstring + condition + error message); `tests/unit_test/test_providers/test_class_path_dispatch.py:58,70`; `docs/plan_mode.md`; `.env.example` if present |
| `plan_mode_active` (audit field) | `coding_plan_mode_active` | `recorder.py` (S1.1 just-landed); `engine.py:117` (S1.1 just-landed); `test_audit_recorder_plan_mode.py` (S1.1 just-landed); `test_real_agent_plan_mode.py:15,129,167,172`; `docs/plan_mode.md` (per Critic Major #5: 6 prose mentions at lines 60, 79, 105, 117, 132, 133 — careful rewording; 4 operator-contract literals at lines 166, 169, 175, 179 — `plan_mode_active`×2 + `plan_mode_error`×2; 4 path references at lines 195, 200, 206, 207 — straightforward replace. Total 14 hits in this one doc file.) |
| `[plan-mode]` (CLI string) | `[coding-plan-mode]` | `provider.py` print (S1.2 just-landed); `test_plan_mode_notice.py` (S1.2 just-landed) |
| `plan_mode_error` (log line literal — **operator-contract change per architect amendment #6**) | `coding_plan_mode_error` | `anthropic_native.py` (S1.4 just-landed); `codex.py` (will be S4); S1.4 + S4 test files |

**Operator-contract acknowledgement (architect amendment #6):** `plan_mode_error` is a structured log-line literal — operators may grep for it or build log dashboards keyed on the token. Renaming changes that contract. **Acceptance:** in pre-production dev state today (no live operator monitoring), the cleaner name is worth the contract change. If anyone subsequently builds a log dashboard keyed on `plan_mode_error` before S2 lands, S2 must coordinate the rename with that dashboard owner. Documented here so future-you does not silently rename a live operator contract.

Search command to find all hits (excluding throwaway spikes + this plan file):
```
grep -rnE "plan_mode|PLAN_MODE|\[plan-mode\]" backend/ docs/ --exclude-dir=__pycache__ \
  | grep -v "scripts/spike_" \
  | grep -v "next_steps_and_rename_plan" \
  | grep -v "coding_plan_mode_plan"
```

(`coding_plan_mode_plan.md` is the master plan body and is handled in S2.3 separately.)

**S2.2 — File renames**

| Old path | New path |
|---|---|
| `docs/plan_mode.md` | `docs/coding_plan_mode.md` |
| `backend/src/task_center_runner/tests/sweevo/test_real_agent_plan_mode.py` | `..._test_real_agent_coding_plan_mode.py` |
| `backend/tests/unit_test/test_task_center_runner/test_audit_recorder_no_plan_mode_imports.py` | `..._no_coding_plan_mode_imports.py` |
| `.planning/coding_plan_mode_plan.md` | (already named correctly, no rename) |

**S2.3 — Plan body updates**

- `.planning/coding_plan_mode_plan.md` body uses `plan_mode_active` (A11), `plan-mode` (A10), `EOS_DISABLE_PLAN_MODE` (A12) — find/replace to the `coding_plan_mode` form. Add a v9.3 iteration log entry documenting the rename.

**S2.4 — Default mode = `api_mode` (explicit naming; v6 reorg deferred per architect amendment #4)**

Today: `make_api_client` resolves empty/missing `class_path` to a hardcoded `AnthropicClient(api_key=..., base_url=...)` path. This is the implicit default. We are NOT going to add an `api_mode` enum or a parallel dispatch table — that would be premature abstraction (CLAUDE.md §2). Instead:

- **Per Critic missing-item: `API_MODE_CLASS_PATH` must not be dead code.** Two viable shapes considered: (a) keep the constant as a module-level identifier referenced ONLY by a comment + docstring — dead-code smell; (b) actively use it as the dispatch target for the empty-`class_path` branch via `_resolve_class_path(API_MODE_CLASS_PATH)(api_key=..., base_url=...)` — adds resolution overhead per call AND changes construction signature (the `AnthropicClient.__init__` accepts `api_key` + `base_url`, not `db_kwargs=`, so this would need a wrapping). **Choose (a)** but downgrade the "constant" to a module-level **docstring comment** so it stays discoverable to grep without pretending to be wired:
  ```python
  # The implicit api_mode dispatch resolves to:
  #   API_MODE_CLASS_PATH = "providers.clients.anthropic_native:AnthropicClient"
  # The empty-class_path branch below instantiates this class directly with
  # (api_key, base_url) rather than via _resolve_class_path, because the
  # API-key constructor signature differs from the (db_kwargs=) pattern
  # used by plan-mode classes. Renaming this comment when v6 reorg lands
  # is captured under Follow-ups.
  ```
  No live identifier, no dead code, name is grep-discoverable.
- The empty-`class_path` branch continues to construct `AnthropicClient(api_key=..., base_url=base_url)` directly (no behavior change).
- `docs/coding_plan_mode.md` opens with: "EphemeralOS runs in `api_mode` by default — an `AnthropicClient` configured with an explicit API key from the active `model_registrations` row. Coding plan modes are opt-in by setting `class_path` to a `providers.clients.coding_plan.*` value."

This gives the default a named identity without adding speculative machinery.

### Sprint S3 — A7: refresh-on-401 retry once (~1 hour, lifts critic-acknowledged gap)

- `AnthropicClient.stream_message`: on a 401 error from the first attempt, call `self._auth_strategy.refresh()`. If `True`, rebuild the SDK client with the new `get_auth_kwargs()` and retry ONCE even if `emitted_any` is True (per master plan §A7; replayed deltas are accepted cost).
- **If `refresh()` returns False** (per Critic minor #4): raise the original 401 with NO retry — the strategy is signaling it cannot self-heal, and a blind retry would just hit the same error.
- **Test:** `backend/tests/unit_test/test_providers/test_anthropic_refresh_replay.py` — two cases:
  - (a) refresh-returns-True: mock SDK to emit two text deltas then raise a 401 on the first attempt; instrument a strategy whose `refresh()` returns True and mutates state; assert: refresh called, retry attempted, full stream re-emitted.
  - (b) refresh-returns-False: same 401, strategy `refresh()` returns False; assert: refresh called once, NO retry attempted, original 401 surfaces as `AuthenticationFailure`.

### Sprint S4 — Phase 2: `CodexResponsesClient` (~2-3 hours)

- New file `backend/src/providers/clients/coding_plan/codex.py`. Class `CodexResponsesClient` implementing `SupportsStreamingMessages`.
- Reads `~/.codex/auth.json` (`tokens.access_token`, `tokens.id_token`).
- `chatgpt_account_id` extraction via Auth0-namespaced claim (per v9.2 correction): `payload["https://api.openai.com/auth"]["chatgpt_account_id"]`, fallback to top-level for forward-compat.
- Model: read from `~/.codex/config.toml` (per v9.2 correction). Default `gpt-5.5` if config absent. **Confirmed via live Phase 0 spike** (`.planning/codex_event_mapping.md` — `model: gpt-5.5` returned by `response.created` event); NOT a typo for `gpt-5`/`gpt-5-codex` (both rejected with HTTP 400 on ChatGPT-account auth).
- Tool envelope: FLAT (per v9.2 correction). Do NOT include `max_output_tokens`.
- **5-header Cloudflare allowlist (per A4, enumerated here per Critic ambiguity-risk pin):**
  - `Authorization: Bearer <tokens.access_token>`
  - `ChatGPT-Account-Id: <Auth0-namespaced JWT claim from tokens.id_token>`
  - `originator: codex_cli_rs`
  - `User-Agent: codex_cli_rs/0.125`
  - `OpenAI-Beta: responses=experimental`
  - (Plus implicit `Content-Type: application/json` — not allowlist-checked but required.)
- Translate Codex Responses SSE events → `ApiStreamEvent` union (per v9.2 mapping table in `.planning/codex_event_mapping.md`).
- `CodexCredentialIncompleteError` per plan §A15.
- **A17 Codex-side wiring (mirrored to S1.4 with the same translated-exception refactor; per Architect re-review blocker #1 applied symmetrically):** in `CodexResponsesClient.stream_message`'s error path, follow the same `translated = ...; log.error(...); raise translated from exc` shape as S1.4. Fires on BOTH the `_translate_error`-path and any `except EphemeralOSApiError` re-raise path (per Architect re-review blocker #2). `log.error("plan_mode_error" / "coding_plan_mode_error" per S2 rename, extra={"provider": "codex", "error_type": <category>, "request_id": <response.id_or_none>})`. **Categorize from the post-translation typed exception** (consistent with S1.4), using the same mapping table as S1.4 plus Codex-specific additions:
  - `AuthenticationFailure` 401 → `"auth_401"`, 403 → `"auth_403"`
  - `RateLimitFailure` → `"rate_limit_429"`
  - `RequestFailure` 5xx → `"server_5xx"`
  - Response header `cf-mitigated` non-empty → `"cf_mitigated_challenge"`
  - Body matches `"not supported when using Codex with a ChatGPT account"` → `"model_rejected"`
  - Body matches schema-rejection hints (`schema`, `parameters`, `additionalProperties`) → `"schema_rejected"`
  - otherwise → `"unknown"`
- **Tests:** `test_codex_jwt_decode.py` (Auth0-namespaced + top-level fallback + missing claim), `test_codex_request_headers.py` (5 headers exact), `test_codex_event_translation.py` (7 SSE event types → variants), `test_codex_plan_mode_error_log.py` (mock 401 + 403 + cf_mitigated → assert log emission with right category).
- **Live smoke (Phase 2 manual-smoke gate):** one real round-trip through `CodexResponsesClient` via `make_api_client(class_path="providers.clients.coding_plan.codex:CodexResponsesClient")`. Assert end-to-end like the AnthropicPlanClient smoke this round delivered.

> **Round-2 status (2026-05-20):** S4 code + tests landed (15/15 codex unit tests pass). Live smoke is **DEFERRED** to a credential-gated follow-up round — operator runs `~/.codex/auth.json` against the production seam and confirms a real round-trip succeeds + 5 headers pass Cloudflare allowlist. Same shape as Phase 0.3 Anthropic smoke gate (Round-1 S2).

### Sprint S5 — Phase 3: Capability parity benchmark (~1 day)

Per plan §Phase 3 + §Verification Plan tolerance gate:
- Run sweevo `complex_project_build` twice on the same task: (a) API mode, (b) plan mode (Anthropic OR Codex).
- Gate: identical tool-call sequence AND identical set of modified files AND sweevo assertion pass in both runs.
- Document outcome in `.planning/capability_parity_benchmark.md`.

---

## ADR

- **Decision:** Sequence next work as Option A — safety floor (A8 + A10 + A11) BEFORE rename, rename BEFORE Phase 2. Make `api_mode` explicitly named via a module constant + a doc opener, without adding a parallel dispatch table.
- **Drivers:** (1) merge safety — Phase 1 MVP needs A11/A10/A8 to ship safely; (2) discoverability — `plan_mode` collides with vendor "plan mode" naming; (3) reversibility — renaming unshipped code is cheap, renaming shipped code is not.
- **Alternatives considered:** Option B (rename first then safety) rejected because it defers the safety floor by one PR cycle. Option C (bundled PR) rejected because it mixes substantive review (does the audit cover the right cases?) with bikeshed-bait naming review.
- **Why chosen:** Each sprint is independently reviewable. A8 + A11 land first because token leaks are irreversible. Rename sweep is mechanical and lands second because it is low-risk to land on top of a clean safety baseline. Phase 2 starts on a stable, named foundation.
- **Consequences:**
  - One commit churn between S1 (writes `plan_mode_active`/`EOS_DISABLE_PLAN_MODE`) and S2 (renames them). Acceptable in exchange for keeping the safety PR reviewable independently.
  - `api_mode` becomes a first-class name in docs + a single constant in code, but does NOT introduce an enum/registry — that would be speculative.
  - Phase 2 (S4) writes `CodexResponsesClient` with the renamed audit field + env var from day one.
- **Follow-ups:**
  - **v6 file reorg trigger (per Critic missing-item, hard trigger per Architect re-review nit #4).** `providers/clients/anthropic_native.py` → `providers/clients/api/anthropic_native.py` per master plan §A2 v6 amendment. **Hard trigger:** execute as a standalone sprint **immediately after S5 (Phase 3 capability parity)** lands — no slip clause. Rationale: by S5 completion, both `AnthropicClient` (API mode) and the `coding_plan/` siblings are stable; renaming files at that point is purely mechanical (4 import-redirect sites + 2 `patch(...)` mock string updates enumerated in master plan §A2 v6) and bumps the `API_MODE_CLASS_PATH` docstring constant. If S5 itself never lands, this follow-up is moot.
  - **A6 per-agent override.** Out of scope today; revisit if multi-mode-per-run demand arises.
  - **Live operator dashboard coordination.** If anyone builds a log dashboard keyed on `plan_mode_error` between now and S2 landing, S2 must coordinate the rename with that owner (see S2.1 operator-contract acknowledgement).
  - **Rollback story (per Critic missing-item).** Each sprint produces an independently-revertable commit (or commit cluster on a feature branch). If S1 lands and a bug surfaces a week later: `git revert` the S1 commit cluster — leaves the codebase in Phase 1 MVP state (today). If S2 lands and the rename breaks something: `git revert` the S2 commit — pure mechanical undo, no behavior change to restore. If S4 (Codex client) lands and the production client misbehaves: `EOS_DISABLE_PLAN_MODE=1` (post-S2: `EOS_DISABLE_CODING_PLAN_MODE=1`) kills the plan-mode path at the dispatch layer without a code revert — runtime kill switch is the first line of defense. Code revert is the second.

---

## Sprint sizing summary

| Sprint | Scope | Estimate | Gate |
|---|---|---|---|
| S1 | A11 + A10 + A8 (3 parts, both vendors) + A17 Anthropic-side (safety floor) | ~1-1.5 days | All tests green; token-leak walk negative for both Anthropic + Codex fixtures |
| S2 | Rename sweep (~20-25 hits, 9-10 files) + `api_mode` explicit naming + plan body v9.3 entry | ~3 hours | grep clean for `plan_mode|PLAN_MODE|\[plan-mode\]` outside spike scripts + master plan iteration log; v6 reorg deferred per follow-up |
| S3 | A7 refresh-on-401 retry-once | ~1 hour | New retry-replay test passes |
| S4 | Phase 2 Codex client + A17 Codex-side wiring + live smoke | ~3-4 hours | One live round-trip succeeds; 5 headers verified; A17 log line emits on simulated 401/cf_mitigated |
| S5 | Phase 3 capability parity benchmark | ~1 day | Tolerance gate passes on `complex_project_build` |

Total: ~3.5-4.5 days. Sprints are independently mergeable.
