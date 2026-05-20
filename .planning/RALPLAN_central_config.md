# RALPLAN — Unified Modular Config under a Central YAML

## Goal
Consolidate every runtime-tunable parameter in EphemeralOS into per-module config classes that share a common base interface, and aggregate them under a single `CentralConfig` loaded from one YAML file. No per-module YAML/JSON files; only `.env` (secrets) and one `ephemeralos.yaml`.

---

## RALPLAN-DR Summary

### Principles
1. **Single source of truth** — exactly one YAML file is the authoritative non-secret config; pydantic models validate it; defaults live in code.
2. **Module owns its schema** — each module defines its own `ModuleConfig` (pydantic `BaseModel` subclass of a shared `ModuleConfigBase`); `CentralConfig` only composes, never re-defines fields.
3. **Secrets stay in `.env`** — API keys, DB URLs, tokens never enter the YAML; they remain environment variables and are injected into the validated config at load time.
4. **No silent fallbacks** — unknown YAML keys → fail loud; missing required → fail loud; env-var overrides are an explicit, documented allow-list.
5. **Backwards-incompatible cleanup is in-scope** — `settings.json` is removed; `EPHEMERALOS_*`/`EOS_*` env vars are *only* preserved when they hold secrets or deployment-specific paths; tunables move to YAML.

### Decision Drivers
1. **Discoverability** — today's config is split across `defaults.py`, `settings.py`, `paths.py`, ~30 `EOS_*`/`EPHEMERALOS_*` env vars, and module-local constants. A new contributor cannot enumerate "what is tunable."
2. **Composition without coupling** — modules (sandbox, providers, task_center_runner, plugins, tools) must define their tunables next to their code, but be loadable through one entry point.
3. **Test ergonomics** — fixtures must construct a `CentralConfig` programmatically without touching the filesystem, and override one module's section in isolation.

### Viable Options

**Option A — `pydantic-settings` + nested `BaseSettings` + YAML source (RECOMMENDED).**
- Each module exports `class FooConfig(ModuleConfigBase)`; `CentralConfig(BaseSettings)` has `sandbox: SandboxConfig`, `runner: RunnerConfig`, etc.
- Single `YamlConfigSettingsSource` reads `ephemeralos.yaml`; secrets layered from `.env`/env-vars via an explicit allow-list.
- Pros: industry-standard; nested validation free; one loader; deep-merge of defaults handled by pydantic; supports CLI overrides via `merge_cli_overrides`.
- Cons: pydantic-settings dep (already transitively present via pydantic v2); strictness requires writing one source class.

**Option B — Plain pydantic v2 + hand-rolled YAML loader.**
- Reuse existing `Settings` pattern; add `yaml.safe_load` in `load_settings`; keep `_apply_env_overrides` as the env-var shim.
- Pros: zero new deps; smallest diff against current code.
- Cons: hand-rolled precedence rules drift; no canonical place for module configs to "register"; env-var allow-list grows organically again.

**Option C — Dynaconf / OmegaConf.**
- Pros: powerful interpolation, includes, multi-source.
- Cons: heavier dep; weaker static typing than pydantic; conflicts with codebase's pydantic-everywhere convention; overkill for one YAML.

**Chosen: Option A, refined per Architect synthesis — YAML is OPTIONAL, env-nested-delimiter is PRIMARY.**
- `CentralConfig(BaseSettings)` uses `env_nested_delimiter="__"` so every field is automatically env-overridable as `EOS__SECTION__FIELD` (e.g. `EOS__SANDBOX__DOCKER__PRIVILEGED=1`).
- A `YamlConfigSource` layer loads `ephemeralos.yaml` *only if it exists*. Precedence: `defaults < yaml (if present) < env < cli`.
- Zero-YAML deployments (CI, containers) work unchanged via env vars; laptop devs can opt into YAML.
- The "env allow-list table" becomes **implicit** (any `EOS__*` works) — drift impossible, no hand-maintained mapping.
- B's "small diff" is illusory because the unification work is identical; A gives stronger typing. C's flexibility is unused.

---

## Inventory — What gets moved into `CentralConfig`

Captured during this planning pass (read-only scan of `backend/src` + `.env`).

### `config/` (already centralised — adapt)
| Symbol | File | Notes |
|---|---|---|
| `Settings` (system_prompt, theme, fast_mode, effort, passes, verbose) | `config/settings.py` | Move to `ui:` + `behaviour:` sections |
| `DatabaseSettings` (url, pool_pre_ping, pool_size, max_overflow, echo) | `config/settings.py` | Becomes `DatabaseConfig`; `url` stays env-injected |
| `SandboxSettings` (default_image, default_snapshot) | `config/settings.py` | Folds into `SandboxConfig` |
| `DEFAULT_MAX_RETRIES`, `DEFAULT_BASE_DELAY`, `DEFAULT_MAX_DELAY`, `DEFAULT_RETRY_STATUS_CODES` | `config/defaults.py` | → `providers.retry` section |
| `DEFAULT_DATABASE_POOL_SIZE`, `DEFAULT_DATABASE_MAX_OVERFLOW` | `config/defaults.py` | Already in `DatabaseSettings`; remove duplicates |
| `_DEFAULT_BASE_DIR`, `_CONFIG_FILE_NAME` paths | `config/paths.py` | Keep path resolution; YAML location becomes `$EPHEMERALOS_CONFIG_DIR/ephemeralos.yaml` |

### `sandbox/`
| Param | Current source | New section |
|---|---|---|
| Sandbox provider (`docker` / `daytona` / …) | env `EOS_SANDBOX_PROVIDER` | `sandbox.provider` |
| Docker daemon TCP + privileged flags | env `EOS_DOCKER_DAEMON_TCP`, `EOS_DOCKER_PRIVILEGED`, `EOS_DOCKER_NO_PRIVILEGE` | `sandbox.docker.{daemon_tcp,privileged}` |
| Daytona TCP host/port, API URL, auth token | env `EOS_DAEMON_TCP_HOST`, `EOS_DAEMON_TCP_PORT`, `DAYTONA_API_URL`, `EOS_DAEMON_AUTH_TOKEN`, `DAYTONA_API_KEY` | `sandbox.daytona.{api_url,tcp_host,tcp_port}`; keys remain `.env` |
| Default image / snapshot | env `EPHEMERALOS_SANDBOX_DEFAULT_{IMAGE,SNAPSHOT}` | `sandbox.default_image`, `sandbox.default_snapshot` |
| Provision timeout | env `EPHEMERALOS_SANDBOX_TIMEOUT_SECONDS` | `sandbox.timeout_s` |
| Runtime client timeout | env `EPHEMERALOS_RUNTIME_CLIENT_TIMEOUT` | `sandbox.runtime_client_timeout_s` |

### `providers/`
| Param | Current source | New section |
|---|---|---|
| Retry count, base/max delay, retryable status codes | `config/defaults.py` constants | `providers.retry.{max_retries,base_delay_s,max_delay_s,status_codes}` |
| Minimax base URL / model name (non-secret routing) | env `MINIMAX_BASE_URL`, `MINIMAX_MODEL` | `providers.minimax.{base_url,model}`; key stays `.env` |
| Active model dispatch (`class_path`, kwargs) | DB `model_registrations` | **Out of scope** — DB stays the source of truth; documented in ADR |

### `task_center_runner/`
| Param | Current source | New section |
|---|---|---|
| `audit_dir` default (`.sweevo_runs`) | `core/config.RunConfig` | `runner.audit_dir` |
| `run_label` default | `core/config.RunConfig` | `runner.run_label` |
| Heavy live-e2e gate, real-agent gate, max duration | envs `EPHEMERALOS_RUN_HEAVY_LIVE_E2E`, `EOS_SWEEVO_REAL_AGENT_TESTS`, `EOS_SWEEVO_REAL_AGENT_MAX_DURATION_S` | `runner.live_e2e.{heavy_enabled,real_agent_enabled,real_agent_max_duration_s}` |
| Sandbox reuse / force-fresh | env `EOS_SWEEVO_REUSE_SANDBOX`, `EOS_SWEEVO_FORCE_FRESH_SANDBOX` | `runner.sandbox_reuse_mode` (enum) |
| Sandbox quota | env `EOS_SWEEVO_SANDBOX_QUOTA` | `runner.sandbox_quota` |
| Audit dir override / tmp | env `EOS_SWEEVO_AUDIT_DIR`, `EOS_SWEEVO_AUDIT_TMP` | **stay env-only** — test-fixture knobs, not user config |

### `tools/` + `plugins/`
| Param | Current source | New section |
|---|---|---|
| Plugin search dir | env `EOS_PLUGIN_DIR` | `plugins.plugin_dir` |
| Trusted setup roots | env `EOS_PLUGIN_TRUSTED_SETUP_ROOTS` | `plugins.trusted_setup_roots` (list[str]) |
| Plugin import skip for tests | env `EOS_SKIP_PLUGIN_IMPORTS_FOR_TESTS` | **stay env-only** — test knob |
| Coding-plan-mode disable | env `EOS_DISABLE_CODING_PLAN_MODE` | `task_center.coding_plan_mode_enabled` |

### Stays env-only (documented in ADR)
`DAYTONA_API_KEY`, `MINIMAX_API_KEY`, `EOS_DAEMON_AUTH_TOKEN`, `EPHEMERALOS_DATABASE_URL` (secret), `EPHEMERALOS_CONFIG_DIR`/`DATA_DIR`/`LOGS_DIR` (path roots), `EOS_SWEEVO_AUDIT_DIR/_TMP`, `EOS_SKIP_PLUGIN_IMPORTS_FOR_TESTS`, `EOS_SWEEVO_INSTANCE`, `EOS_LIVE_TESTS`, `EOS_TIER_RUN_ID`.

---

## Architecture

```
backend/src/config/
├── __init__.py          # public exports: load_central_config(), CentralConfig
├── base.py              # ModuleConfigBase (frozen pydantic BaseModel, slots, extra='forbid')
├── central.py           # CentralConfig(BaseSettings) — composes module configs
├── loader.py            # YAML source + nested-env, precedence: defaults < yaml (if present) < env (EOS__*) < cli
├── paths.py             # unchanged — resolves location of ephemeralos.yaml
└── sections/
    ├── database.py      # DatabaseConfig(ModuleConfigBase)
    ├── sandbox.py       # SandboxConfig + nested DockerConfig, DaytonaConfig
    ├── providers.py     # ProvidersConfig + RetryConfig
    ├── runner.py        # RunnerConfig (+ LiveE2EConfig)
    ├── plugins.py       # PluginsConfig
    ├── tools.py         # ToolsConfig (placeholder; extension point)
    ├── task_center.py   # TaskCenterConfig (coding_plan_mode_enabled, …)
    └── ui.py            # UiConfig (theme, fast_mode, effort, passes, verbose)
```

### `ModuleConfigBase`
```python
class ModuleConfigBase(BaseModel):
    # NOT frozen — CLI overrides need model_copy(update=...); strict on unknown fields.
    model_config = ConfigDict(extra="forbid", populate_by_name=True)

    @classmethod
    def section_name(cls) -> str: ...

    def with_overrides(self, **kwargs) -> Self:
        return self.model_copy(update={k: v for k, v in kwargs.items() if v is not None})
```
*Architect fix:* `frozen=True` was removed because it contradicts `merge_cli_overrides` (§Precedence #4). Immutability after construction is enforced socially, not structurally.

### `CentralConfig` (composition only)
```python
class CentralConfig(BaseSettings):
    database: DatabaseConfig = Field(default_factory=DatabaseConfig)
    sandbox:  SandboxConfig  = Field(default_factory=SandboxConfig)
    providers: ProvidersConfig = Field(default_factory=ProvidersConfig)
    runner:   RunnerConfig   = Field(default_factory=RunnerConfig)
    plugins:  PluginsConfig  = Field(default_factory=PluginsConfig)
    task_center: TaskCenterConfig = Field(default_factory=TaskCenterConfig)
    ui:       UiConfig       = Field(default_factory=UiConfig)

    model_config = SettingsConfigDict(extra="forbid", env_nested_delimiter="__")

    @classmethod
    def settings_customise_sources(cls, ...):
        # Native pydantic-settings env source (uses env_nested_delimiter="__").
        # No hand-maintained allow-list.
        return (init, YamlConfigSource(...), env_settings, dotenv_settings)
```

### YAML shape (single file)
```yaml
# ~/.ephemeralos/ephemeralos.yaml   (overridable via $EPHEMERALOS_CONFIG_DIR)
database:
  pool_size: 5
  max_overflow: 10
  echo: false
sandbox:
  provider: docker
  default_image: ""
  timeout_s: 300
  docker:
    privileged: false
  daytona:
    api_url: http://localhost:3000/api
providers:
  retry:
    max_retries: 3
    base_delay_s: 1.0
    max_delay_s: 30.0
    status_codes: [429, 500, 502, 503, 529]
runner:
  audit_dir: .sweevo_runs
  live_e2e:
    heavy_enabled: false
ui:
  theme: default
  fast_mode: false
```

### Precedence (low → high)
1. Field defaults (in pydantic models).
2. YAML file (`ephemeralos.yaml`) — *only if present*.
3. Env vars via `env_nested_delimiter="__"` (any `EOS__SECTION__FIELD`); no hand-maintained allow-list.
4. CLI overrides (`CentralConfig.merge_cli_overrides(**kwargs)` → `model_copy(update=…)`).

### Test ergonomics (Architect-required, Phase 1 acceptance)
- `get_central_config()` reads from a `ContextVar[CentralConfig | None]` with lazy default-load.
- Public test fixture `override_central_config(cfg: CentralConfig)` is a context manager that sets the ContextVar inside its scope and restores on exit. No monkeypatch needed.
- Existing `monkeypatch.setenv("EOS__SANDBOX__DOCKER__PRIVILEGED", "1")` continues to work because the ContextVar default re-reads env on first access per test (cleared between tests via a session-scoped autouse fixture).
- Reference call sites consume `cfg = get_central_config().<section>` at function scope, **never** module-level, so test overrides take effect without import-time caching.

---

## Migration Plan (sequential phases)

### Phase 1 — Skeleton (no behaviour change)
1. Add `config/base.py`, `config/sections/*.py`, `config/central.py`, `config/loader.py`.
2. Each `ModuleConfigBase` subclass mirrors today's defaults exactly.
3. Public API: `load_central_config()` returning a `CentralConfig`; keep `load_settings()` as a thin shim that delegates and projects into the old `Settings` shape.
4. Tests: round-trip a hand-written YAML; assert defaults match `defaults.py` constants; assert `extra='forbid'` rejects unknown keys.
**Verify:** existing test suite green; new `tests/unit_test/test_config/test_central_loader.py` green.

### Phase 2 — Module call-site cutover (one module per commit, low-risk first)
Architect-recommended order: **ui → providers.retry → plugins → task_center → runner → sandbox** (sandbox last because its failures cascade to every benchmark; ui/providers.retry have the smallest blast radius).

For each module:
1. Replace direct env reads / `defaults.py` constants with a function-scoped `cfg = get_central_config().<section>`.
2. Document the field's `EOS__SECTION__FIELD` env binding in the section's docstring. (No allow-list; native nested-env covers it.)
3. Delete the now-unused `EOS_*`/`EPHEMERALOS_*` env reads.
4. Update unit tests to use `override_central_config(CentralConfig(...))` instead of monkeypatching legacy env vars.
**Verify per module:** `make test` green; grep for the retired env var returns 0 references in `backend/src` (test code may still set it via fixtures).

### Phase 3 — Remove `settings.json` and `defaults.py` duplication
1. Delete `config/settings.py:save_settings/load_settings` once all callers migrated.
2. Delete `config/defaults.py` constants that now live on the pydantic models (keep only true cross-cutting constants if any remain — likely empty).
3. Provide a migration helper `scripts/migrate_settings_json_to_yaml.py` (one-shot).
**Verify:** `rg "settings.json"` → only migration helper + ADR; `rg "DEFAULT_MAX_RETRIES"` → only ProvidersConfig + tests.

### Phase 4 — Docs + ADR + `config dump` CLI
1. `docs/config.md` lists every section, every field, every retained env var, every removed env var (with replacement).
2. ADR entry in `docs/adr/` summarising decision drivers + invalidated alternatives. **Must state the invariant: `model_registrations` (DB) remains the sole source of truth for active LLM dispatch — future contributors must not migrate it into the YAML.**
3. `ephemeralos config dump --show-provenance` CLI subcommand prints the effective config + per-field provenance (default | yaml | env | cli). Architect flagged this as the real discoverability fix — promoted from "follow-up" to Phase-4 scope.

### Inventory addenda (Architect-found gaps)
- `EPHEMERALOS_RUN_CAPACITY_LIVE_E2E` (`tests/capacity/test_full_system_capacity_matrix.py:41`) — **test-only**, stays env.
- `EOS_LIVE_TESTS` (`benchmarks/sweevo/test_sweevo_runner.py:42`) — **test-only**, stays env. The "Stays env-only" section already lists it; confirmed intentional.

---

## Acceptance Criteria (testable)
- `load_central_config()` from a fixture YAML returns a `CentralConfig` whose field values exactly match the YAML.
- Unknown top-level key in YAML → `ValidationError` mentioning the unknown key.
- Unknown nested key (e.g., `sandbox.docker.unknown`) → `ValidationError`.
- For every retained env var in the allow-list: setting it overrides the YAML value; unsetting it keeps the YAML value.
- `rg -n "os.environ|os.getenv" backend/src/{sandbox,providers,task_center_runner,plugins,tools}` returns only the documented retained set: secrets (`DAYTONA_API_KEY`, `MINIMAX_API_KEY`, `EOS_DAEMON_AUTH_TOKEN`, `EPHEMERALOS_DATABASE_URL`), path roots (`EPHEMERALOS_CONFIG_DIR`/`DATA_DIR`/`LOGS_DIR`), and test knobs (`EOS_SWEEVO_AUDIT_DIR`/`_TMP`, `EOS_SKIP_PLUGIN_IMPORTS_FOR_TESTS`, `EOS_SWEEVO_INSTANCE`, `EOS_LIVE_TESTS`, `EOS_TIER_RUN_ID`, `EPHEMERALOS_RUN_HEAVY_LIVE_E2E`, `EPHEMERALOS_RUN_CAPACITY_LIVE_E2E`).
- `rg -n "DEFAULT_MAX_RETRIES|DEFAULT_BASE_DELAY|DEFAULT_DATABASE_POOL_SIZE" backend/src` returns 0 outside `config/sections/*` and tests.
- `find backend/src -name "settings.json" -o -name "*.yaml" -path "*/config/*"` returns 0 files (the central YAML lives outside `backend/src`, e.g. at `~/.ephemeralos/ephemeralos.yaml`); test fixtures under `backend/tests/` are excluded from this check.
- `with_overrides()` performs a **shallow** field-level update; nested section overrides must pass a fully-constructed nested model (e.g. `cfg.with_overrides(sandbox=cfg.sandbox.with_overrides(provider="docker"))`). Documented in the docstring.
- Existing test suite (`make test`) passes at every commit boundary.

---

## ADR (placeholder — fill at approval time)
- **Decision:** Adopt Option A (`pydantic-settings` nested `BaseSettings` with a YAML source).
- **Drivers:** discoverability of tunables; composition without coupling; test ergonomics.
- **Alternatives considered:** plain pydantic + hand-rolled YAML loader (B); Dynaconf/OmegaConf (C).
- **Why chosen:** Strong typing, codebase already pydantic-first, explicit precedence, no new heavy dep.
- **Consequences:** breaking change to env-var surface area; YAML becomes a deployment artifact; migration helper required for users with existing `settings.json`.
- **Follow-ups:** consider a `ephemeralos config dump` CLI subcommand that prints the effective config + provenance per field.

---

## Out of Scope
- Hot-reload of config at runtime.
- DB-backed model registration (`config/model_config.py`) — stays as-is.
- Test-only env knobs (`EOS_SWEEVO_AUDIT_TMP`, `EOS_SKIP_PLUGIN_IMPORTS_FOR_TESTS`, etc.) stay env-only.
- Secrets management beyond `.env` (no vault integration).
