# Coding Plan Mode

EphemeralOS runs in `api_mode` by default — an `AnthropicClient` configured
with an explicit API key from the active `model_registrations` row. Coding
plan modes are opt-in by setting `class_path` to a
`providers.clients.coding_plan.*` value.

Coding plan mode lets EphemeralOS drive agents off a vendor flat-rate subscription
(Anthropic Claude Max + overage credits, OpenAI ChatGPT Plus/Pro via the
Codex backend) instead of the metered Anthropic / OpenAI APIs. For long
multi-agent runs this can flip economics significantly.

> **Status: experimental.** Coding plan mode runs against vendor endpoints that
> are not formally public APIs and may change without notice. Per
> Anthropic / OpenAI Terms of Service, the user is responsible for usage
> compliance. See [Vendor ToS](#vendor-tos-disclaimer) below.

Implementation contract: `.planning/coding_plan_mode_plan.md`
(currently APPROVED v8.1 + v9 amendment).

---

## Overview

Coding plan mode is dispatched via the existing-but-previously-unused
`class_path` column on the `model_registrations` row. Each row picks one
of two clients:

* `providers.clients.api.anthropic_native:AnthropicClient` (Anthropic API
  mode — today's default) — `class_path` empty.
* `providers.clients.api.anthropic_native:AnthropicClient` with
  `kwargs_json.auth = "claude_oauth"` — Anthropic coding plan mode via OAuth.
* `providers.clients.coding_plan.codex:CodexResponsesClient` —
  Codex / ChatGPT coding plan mode via the ChatGPT backend.

The agent's tool loop, sandbox (layerstack + OCC), and audit recorder
remain framework-owned in all three cases. Coding plan mode does NOT surrender
the loop to a vendor CLI — see the Hermes Pattern A discussion in the
plan for the architectural rationale.

---

## How to enable

### Anthropic Claude Max coding plan mode

1. Have an active Claude Max subscription. (Claude Pro does not work —
   see [Overage-credit warning](#overage-credit-warning).)
2. Log into Claude Code (`claude` CLI) so its OAuth credentials populate
   the macOS Keychain entry `Claude Code-credentials`. Linux storage
   path is TBD pending Phase 0.5 probe.
3. Register a coding-plan-mode row:

   ```sql
   INSERT INTO model_registrations (model_id, class_path, kwargs_json, is_active)
   VALUES (
     'plan/claude-max',
     'providers.clients.api.anthropic_native:AnthropicClient',
     '{"auth": "claude_oauth", "model": "claude-sonnet-4-5"}',
     true
   );
   ```

4. Run any EphemeralOS scenario. The CLI prints
   `[coding-plan-mode] anthropic` at agent spawn (A10).

### OpenAI Codex / ChatGPT coding plan mode

1. Have an active ChatGPT Plus / Pro / Team subscription that includes
   Codex access.
2. Log into the Codex CLI so credentials populate `~/.codex/auth.json`.
3. Register a coding-plan-mode row:

   ```sql
   INSERT INTO model_registrations (model_id, class_path, kwargs_json, is_active)
   VALUES (
     'plan/codex',
     'providers.clients.coding_plan.codex:CodexResponsesClient',
     '{"model": "gpt-5-codex"}',
     true
   );
   ```

4. CLI prints `[coding-plan-mode] codex` at agent spawn.

---

## Credential discovery

| Vendor | Storage location | Required claims |
|--------|------------------|------------------|
| Anthropic (macOS) | Keychain service `Claude Code-credentials`, account `$USER` | `claudeAiOauth.accessToken`, `claudeAiOauth.refreshToken` |
| Anthropic (Linux) | TBD — Phase 0.5 probe in progress | TBD |
| OpenAI Codex (all platforms) | `~/.codex/auth.json` mode 0600 | `tokens.access_token`, `tokens.id_token`, `tokens.refresh_token` |

JWT-decoded `chatgpt_account_id` from `tokens.id_token` populates the
`ChatGPT-Account-Id` HTTP header on every Codex request (A15).
No signature verification is performed — we are identifying the account
we already authenticated against, not validating OpenAI's signature.

EphemeralOS never persists OAuth tokens to its own database. Tokens
live exclusively in vendor-owned storage (macOS Keychain /
`~/.codex/auth.json`) and in-process client memory. See A8 for the
static + runtime + subprocess-env tests that enforce this property.

---

## Kill switch

Setting the environment variable `EOS_DISABLE_CODING_PLAN_MODE=1` causes
`make_api_client()` to reject any `class_path` resolving into
`providers.clients.coding_plan.*` with a clear error (A12). Use this
to:

* Disable coding plan mode org-wide via deployment config.
* Force a fallback to API mode if a coding-plan-mode vendor surface is
  reported degraded.
* Comply with downstream auditing requirements that mandate metered
  vendor billing.

```bash
EOS_DISABLE_CODING_PLAN_MODE=1 ./your-eos-launch-command
# → if any active model_registrations row has coding-plan-mode class_path,
#   spawn raises NoActiveModelError with clear message.
```

---

## Overage-credit warning

**Claude Max coding-plan-mode consumes ONLY overage credits, not the base
allowance.** This is an Anthropic-side quota policy, not an EphemeralOS
choice — Hermes Agent documented the same behavior in their providers
guide. **Claude Pro does not work at all** with this auth flow.

If your Max subscription's overage credit pool is empty, coding-plan-mode
requests will return 4xx — the `coding_plan_mode_error` audit log (A17) will
flag this, and the CLI's `[coding-plan-mode]` notice (A10) advises checking
the Anthropic billing dashboard.

**Codex / ChatGPT Plus / Pro** consume the normal subscription bucket;
no separate overage pool — but rate limits are real (see A17 / Phase 2
manual smoke).

---

## Vendor ToS disclaimer

Coding plan mode involves vendor impersonation at the HTTP layer:

* Anthropic OAuth requires a hard-coded system block #0:
  `"You are Claude Code, Anthropic's official CLI for Claude."` (A13).
* Codex / ChatGPT requires Cloudflare-allowlist headers (`originator:
  codex_cli_rs`, matching `User-Agent`, `ChatGPT-Account-Id`).

These are accepted impersonations (the three-repo cross-verification —
hermes / pi / openclaw — all ship the same approach). They do, however,
mean that your account may be reviewed under Anthropic / OpenAI ToS at
the vendor's discretion. **The user, not EphemeralOS, owns the ToS
relationship.**

If the user account is banned, restore to API mode trivially: remove
the coding-plan-mode `class_path` from the active `model_registrations` row.
A8 ensures no OAuth tokens were ever persisted in our DB, so no purge
step is needed.

---

## Audit & observability

### `coding_plan_mode_active` field in `run.json`

Every `run.json` produced by `AuditRecorder` carries
`"coding_plan_mode_active": bool`, resolved once at run-start in
`task_center_runner/core/engine.py` (A11). `true` iff the active
`model_registrations.class_path` starts with
`providers.clients.coding_plan.` OR is an Anthropic client constructed
with `auth: "claude_oauth"` kwargs.

### `coding_plan_mode_error` log lines

Both `AnthropicClient.stream_message` and
`CodexResponsesClient.stream_message` emit a structured log line tagged
`coding_plan_mode_error` at every 4xx / 5xx boundary (A17). Fields:

* `provider`: `"anthropic"` | `"codex"`
* `status_code`: HTTP status
* `error_class`: short error tag (`SCHEMA_REJECT`, `CF_CHALLENGE`, etc.)
* `retry_attempted`: bool

Use this signal for drift detection — if Anthropic changes the
mandatory identity-block string (Pre-mortem #4) or Cloudflare rotates
its allowlist, the error rate spike surfaces here.

---

## Live e2e tests

Coding-plan-mode end-to-end tests live in
`backend/src/task_center_runner/tests/sweevo/test_real_agent_coding_plan_mode.py`
and follow the existing `EOS_SWEEVO_REAL_AGENT_TESTS=1` pattern:

```bash
EOS_SWEEVO_REAL_AGENT_TESTS=1 .venv/bin/pytest \
    backend/src/task_center_runner/tests/sweevo/test_real_agent_coding_plan_mode.py \
    -v
```

Three test cases:

* `test_anthropic_coding_plan_mode_e2e` — needs Claude Max OAuth + coding-plan-mode infra
* `test_codex_coding_plan_mode_e2e` — needs Codex creds + coding-plan-mode infra
* `test_api_mode_regression` — existing API path, sanity check

Each test auto-skips if its credentials or coding-plan-mode infrastructure are
absent. Phase 1+3 land the infrastructure that flips the gates.

---

## Smoke spikes (Phase 0 / 0.3 / 0.7 — used to derisk before Phase 1)

Three throwaway scripts under `scripts/`:

* `scripts/spike_anthropic_oauth.py` — Phase 0.3 Anthropic OAuth smoke.
  Output: `.planning/anthropic_oauth_smoke.md`.
* `scripts/spike_codex_stream.py` — Phase 0 Codex stream-translation smoke.
  Output: `.planning/codex_event_mapping.md`.
* `scripts/spike_codex_schema_probe.py` — Phase 0.7 schema-validity probe.
  Output: `.planning/codex_schema_validity_report.md`.

Each supports `--dry-run` (no network, no credentials) and `--live`
(requires the corresponding credential). The spikes are deleted once
Phase 1 / Phase 2 lands the production clients.

---

## Plan reference

The full plan, including acceptance criteria A1–A18, principles, ADR,
phases, and iteration log, lives at
`.planning/coding_plan_mode_plan.md`. Status as of this writing:
APPROVED v8.1 + v9 amendment (progressive de-risking).
