# impl-eos-config — typed configuration, env overrides, paths, and validation

> Owning crate in the agent-core workspace. Conforms to ./spec-conventions.md.
> Plan section: ../backend_agent_core_rust_migration_PLAN.md §4 (lines 319-387),
> cross-cutting §"SRP, Naming, and Prompt Gaps to Close" (lines 1115-1121).

## 1. Purpose & Responsibility (SRP)

`eos-config` owns the **typed, validated, immutable runtime configuration** for
agent-core: it loads `CentralConfig` from layered sources (defaults < YAML < env
< init overrides), parses raw strings into validated config types at the
boundary, resolves on-disk config/data/log paths, and fails fast on
contradictory or unsupported settings (network DB URLs, Docker
`privileged + no_privilege`). It is near the root of the dependency DAG and is
consumed read-only by every crate that needs tunables.

What this crate must **NOT** do:

- It does **not** resolve the active model. Model selection comes from the DB
  `model_registrations` table and is owned by `eos-db` (`model_registry.rs`); see
  `impl-eos-db.md`. `model_config.py` does not port here.
- It does **not** own the `Settings` UI/CLI shape (`theme`, `effort`, `passes`,
  `verbose`, `system_prompt`). Those are CLI concerns outside agent-core; `Settings`
  survives only as compatibility evidence (GC-eos-config-01).
- It does **not** hold secrets as durable state, open connections, spawn tasks,
  or perform any I/O beyond reading config files and the process environment.
- It does **not** carry test-runner (`runner.py`) tunables into agent-core.

## 2. Dependencies

- **Upstream crates (depends on):** none in agent-core. `eos-config` defines its
  own `ConfigError` (anchor §8) and validated newtypes; it does **not** import
  `eos-types` (no `TaskId`/`JsonObject`/`CoreError` is needed at the config
  layer). This keeps the DAG root clean.
- **Downstream consumers (used by):** `eos-db`, `eos-llm-client`,
  `eos-sandbox-host`, `eos-skills`, `eos-runtime` (anchor §5).

External crates (pins deferred to `impl-workspace.md` per `proj-workspace-deps`;
all inherited via `[workspace.dependencies]`):

| Crate | Justification | rust-skills |
|---|---|---|
| `serde` (derive) | All config structs are `Deserialize` from YAML/env-merged maps; `Serialize` for parity snapshots. | `type-no-stringly`, `api-common-traits` |
| `schemars` | `JsonSchema` on every config struct for the Phase-0 schema-parity harness vs current Pydantic schema. | anchor §11 |
| `serde_yaml` (or `serde_norway`) | Reads `ephemeralos.yaml`; also drives env scalar coercion (YAML-parse every env value — `EOS__` and legacy-adapter outputs — so `"120"`/`"true"` deserialize into `f64`/`bool`; see §8 item 8). | — |
| `thiserror` | The single `ConfigError` enum (one per crate). | `err-thiserror-lib`, `err-custom-type` |
| `figment` *(candidate)* | Layered source merge (defaults/YAML/env/init) with the documented precedence. If not adopted, hand-roll the merge in `loader.rs`; decision recorded in `impl-workspace.md`. | `api-parse-dont-validate` |

No `dyn` trait objects are needed; this crate exposes concrete types only
(`anti-type-erasure`).

## 3. Scope & Source Mapping

| Python source | Rust target | What moves / what is dropped |
|---|---|---|
| `config/base.py` (`ModuleConfigBase`, `extra="forbid"`) | folded into each struct's derives | `extra="forbid"` → `#[serde(deny_unknown_fields)]` (GC-eos-config-06). `section_name()`/`with_overrides()` reflection helpers are dropped (YAGNI; static field merge instead). |
| `config/central.py` (`CentralConfig`, `load_central_config`, override ContextVar) | `lib.rs` + `loader.rs` (`CentralConfig`, `load_central_config`) | Composition root struct. ContextVar test-override → a test constructor / explicit arg (no global mutable state). |
| `config/loader.py` (`EnvConfigSource`, `YamlConfigSource`, `_LEGACY_ENV_MAP`) | `env.rs` + `loader.rs` | Nested `EOS__` parsing + legacy adapters port (see §8). `_YAML_ONLY_ENV_PATHS` and `EOS_SWEEVO_*` drop with runner. |
| `config/settings.py` (`Settings`, `load_settings`, `_apply_env_overrides`) | **not ported as runtime type** | Compatibility-only; UI fields out of scope (GC-eos-config-01). |
| `config/paths.py` | `paths.rs` | Config/data/log path resolution + `EPHEMERALOS_CONFIG_DIR/DATA_DIR/LOGS_DIR` and `ephemeralos.yaml` discovery. Per-project/feedback/issue/PR helpers are CLI-scoped — drop unless a downstream agent-core caller needs them. |
| `config/model_config.py` | **not here** → `eos-db` `model_registry.rs` | Active-model resolution + `class_path` (migration-only) are DB concerns; see `impl-eos-db.md`. |
| `config/sections/database.py` | `database.rs` | `DatabaseConfig` simplified to SQLite-only; drop `pool_pre_ping`, `max_overflow` (GC-eos-config-03). |
| `config/sections/sandbox.py` | `sandbox.rs` | `SandboxConfig`/`DockerConfig` plus a Docker-only `SandboxProvider`; non-Docker provider config is not ported to Rust. |
| `config/sections/providers.py` | `providers.rs` | `ProvidersConfig`/`RetryConfig`/`MinimaxConfig` port; retry defaults become the source of truth for `eos-llm-client` (GC-eos-config-04). |
| `config/sections/runner.py` | **dropped** | Test-runner flavored; not imported (GC-eos-config-05). |
| `config/sections/engine.py` | **dropped (empty)** | `EngineConfig` has zero fields; omit from Rust `CentralConfig` (YAGNI, GC-eos-config-07). |
| — | `validation.rs` | Contradiction checks: network DB URL reject, Docker `privileged + no_privilege`. |

**In scope:** layered loading, `EOS__` nested env, legacy env adapters, path
resolution, SQLite-only DB config, sandbox/provider config, fail-fast validation.
**Out of scope:** model registry, `Settings` UI, runner config, secrets storage,
any networking or DB access.

## 4. File & Module Layout

```
src/
  lib.rs          # re-exports CentralConfig, section configs, ConfigError,
                  # load_central_config (proj-pub-use-reexport); crate lints.
  config.rs       # CentralConfig { database, sandbox, providers } + builder/Default.
  loader.rs       # load_central_config(): merge defaults<YAML<env<init; precedence.
  env.rs          # EOS__ nested parsing + legacy env adapter table; complex-value parse.
  paths.rs        # config/data/log dir + ephemeralos.yaml discovery; path env vars.
  database.rs     # DatabaseConfig (SQLite-only) + DatabaseUrl newtype.
  sandbox.rs      # SandboxConfig, DockerConfig, Docker-only SandboxProvider enum.
  providers.rs    # ProvidersConfig, RetryConfig, MinimaxConfig.
  validation.rs   # validate(&CentralConfig) -> Result<(), ConfigError> contradictions.
  error.rs        # ConfigError (thiserror).
```

`env.rs`, `validation.rs` internals are `pub(crate)` (`proj-pub-crate-internal`);
only the configs, `ConfigError`, and `load_central_config` are re-exported.

## 5. Contracts Owned Here

`eos-config` owns concrete types, not trait seams — it has **no entry on the
SOLID Seam Map (anchor §6)**. Owned (anchor §5 row "CentralConfig + section
configs, env loading, path resolution, validation"):

- `CentralConfig` and all section configs (`DatabaseConfig`, `SandboxConfig`,
  `DockerConfig`, `ProvidersConfig`, `RetryConfig`, `MinimaxConfig`).
- Validated newtypes: `DatabaseUrl` (SQLite-only), `SandboxProvider` enum.
- `ConfigError`.
- `load_central_config(...) -> Result<CentralConfig, ConfigError>` and the
  internal source-layering.
- Path resolution functions in `paths.rs`.

Contracts merely used: none from other agent-core crates.

## 6. Types, Fields & Schemas

All section/config structs derive `Debug, Clone, PartialEq, Serialize,
Deserialize, JsonSchema` and `#[serde(deny_unknown_fields)]`. The validated
newtypes (`DatabaseUrl`) are exempt: they are `#[serde(transparent)]` with a
hand-written `Deserialize` (shown below) and so carry neither the derived
`Deserialize` nor `deny_unknown_fields`. Each config struct has a `Default`
matching Python defaults exactly (`api-default-impl`). `#[non_exhaustive]` on the
public structs that may grow (`api-non-exhaustive`).

### CentralConfig — source: `central.py`

| Field | Rust type | serde/schemars notes | Source-of-truth |
|---|---|---|---|
| `database` | `DatabaseConfig` | default = `DatabaseConfig::default()` | `central.py:31` |
| `sandbox` | `SandboxConfig` | default | `central.py:32` |
| `providers` | `ProvidersConfig` | default | `central.py:33` |

`runner` and `engine` are intentionally absent (GC-eos-config-05, -07).

### DatabaseConfig — source: `sections/database.py` (Rust shape per plan line 371)

| Field | Rust type | serde/schemars notes | Source-of-truth |
|---|---|---|---|
| `url` | `DatabaseUrl` | newtype; deserializes via parse, rejects network URLs | `database.py:24` (`DEFAULT_SQLITE_DATABASE_URL = "sqlite:///./.ephemeralos/ephemeralos.db"`) |
| `pool_size` | `u32` | default `5`, `>= 1` (validate, §8 item 9) | `database.py:26` |
| `busy_timeout_ms` | `u64` | **new** SQLite control; default e.g. `5000` | plan line 371 |
| `wal` | `bool` | **new**; default `true` | plan line 371 |
| `foreign_keys` | `bool` | **new**; default `true` | plan line 371 |
| `echo` | `bool` | default `false` | `database.py:28` |

Dropped: `pool_pre_ping`, `max_overflow` (GC-eos-config-03 — connection-server
concepts, meaningless for embedded SQLite).

### SandboxConfig / DockerConfig — source: `sections/sandbox.py`

`SandboxConfig`:

| Field | Rust type | notes | Source |
|---|---|---|---|
| `default_provider` | `SandboxProvider` | enum `{ Docker }`, serde `snake_case`; default `Docker`; any other provider string is unsupported in Rust | `sandbox.py:41` |
| `timeout_s` | `f64` | default `300.0`, `> 0` (validate, §8 item 9) | `sandbox.py:42` |
| `runtime_client_timeout_s` | `f64` | default `600.0`, `> 0` (validate, §8 item 9) | `sandbox.py:43` |
| `docker` | `DockerConfig` | default | `sandbox.py:44` |

`DockerConfig`: `daemon_tcp: bool = true`, `privileged: bool = false`,
`no_privilege: bool = false`, `default_snapshot: String = ""`
(`sandbox.py:20-23`).

The Python non-Docker provider config fields are intentionally absent. Rust
agent-core uses Docker as its only sandbox provider.

`SandboxProvider` replaces the Python provider string literal
(`type-no-stringly`) with the Docker-only `Docker` enum variant; deserializing any
other provider string fails at config load.

### ProvidersConfig / RetryConfig / MinimaxConfig — source: `sections/providers.py`

`ProvidersConfig`: `retry: RetryConfig`, `minimax: MinimaxConfig`.

`RetryConfig` (defaults are the **source of truth** for `eos-llm-client`,
GC-eos-config-04):

| Field | Rust type | default | Source |
|---|---|---|---|
| `max_retries` | `u32` | `3`, `>= 0` (enforced by `u32`) | `providers.py:20` |
| `base_delay_s` | `f64` | `1.0`, `>= 0` (validate, §8 item 9) | `providers.py:21` |
| `max_delay_s` | `f64` | `30.0`, `>= 0` (validate, §8 item 9) | `providers.py:22` |
| `status_codes` | `BTreeSet<u16>` | `{429,500,502,503,529}` | `providers.py:23` |

`BTreeSet` (not `HashSet`) for deterministic **serialized output** ordering (JSON
Schema itself has no ordering concept; see AC-10's normalization step).

`MinimaxConfig`: `base_url: String = ""`, `model: String = ""`.

### Representative snippets

DB-URL parse-don't-validate newtype (`api-parse-dont-validate`,
`type-no-stringly`, `err-thiserror-lib`):

```rust
/// A validated local-SQLite database URL. Network backends are rejected.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(transparent)]      // serialize as a plain string, not a 1-tuple
#[schemars(transparent)]   // render as a plain string in the JSON Schema (matches Python `str`)
pub struct DatabaseUrl(String);

impl DatabaseUrl {
    pub fn parse(raw: impl Into<String>) -> Result<Self, ConfigError> {
        let raw = raw.into();
        let lower = raw.to_ascii_lowercase();
        // Reject network DBs (fail fast) — non-goal: no PostgreSQL in agent-core.
        // Scheme-based so a local sqlite path with `@` in a directory name
        // (e.g. `sqlite:///home/user@host/db.db`) is NOT false-rejected; `@` only
        // counts when it appears in a `//host` authority (credentialed network DB).
        let network_scheme = lower.starts_with("postgres://")
            || lower.starts_with("postgresql://")
            || lower.starts_with("mysql://");
        let credentialed_authority = lower
            .split_once("//")
            .is_some_and(|(_, rest)| rest.split('/').next().is_some_and(|a| a.contains('@')));
        if network_scheme || credentialed_authority {
            return Err(ConfigError::NetworkDatabaseUrl(raw));
        }
        if !(lower.starts_with("sqlite:") || raw.ends_with(".db")) {
            return Err(ConfigError::UnsupportedDatabaseUrl(raw));
        }
        Ok(Self(raw))
    }
    pub fn as_str(&self) -> &str { &self.0 }
}

impl<'de> Deserialize<'de> for DatabaseUrl {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::parse(String::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}
```

Docker contradiction check (`validation.rs`):

```rust
pub(crate) fn validate(cfg: &CentralConfig) -> Result<(), ConfigError> {
    let d = &cfg.sandbox.docker;
    if d.privileged && d.no_privilege {
        return Err(ConfigError::DockerPrivilegeContradiction);
    }
    // Pydantic ge/gt parity (only constraints the Rust type does not enforce).
    if cfg.database.pool_size < 1 {
        return Err(ConfigError::OutOfRange {
            field: "database.pool_size".into(),
            detail: "must be >= 1".into(),
        });
    }
    if cfg.sandbox.timeout_s <= 0.0 || cfg.sandbox.runtime_client_timeout_s <= 0.0 {
        return Err(ConfigError::OutOfRange {
            field: "sandbox.*timeout_s".into(),
            detail: "must be > 0".into(),
        });
    }
    let r = &cfg.providers.retry;
    if r.base_delay_s < 0.0 || r.max_delay_s < 0.0 {
        return Err(ConfigError::OutOfRange {
            field: "providers.retry.*delay_s".into(),
            detail: "must be >= 0".into(),
        });
    }
    Ok(())
}
```

`ConfigError` (`err-thiserror-lib`, `err-lowercase-msg`):

```rust
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    #[error("network database urls are not supported in agent-core: {0}")]
    NetworkDatabaseUrl(String),
    #[error("unsupported database url (expected local sqlite): {0}")]
    UnsupportedDatabaseUrl(String),
    #[error("docker config sets both privileged and no_privilege")]
    DockerPrivilegeContradiction,
    #[error("config value '{field}' is out of range: {detail}")]
    OutOfRange { field: String, detail: String },
    #[error("failed to read config file")]
    ReadFile(#[source] std::io::Error),
    #[error("failed to parse config yaml")]
    ParseYaml(#[source] serde_yaml::Error),
    #[error("invalid config value for '{key}'")]
    InvalidValue { key: String, #[source] cause: serde_yaml::Error },
}
```

## 7. Concurrency & State Ownership

- **Runtime-agnostic.** `eos-config` spawns no tasks and creates no Tokio runtime
  (anchor §7). `load_central_config` is synchronous, called once during
  `eos-runtime` bootstrap.
- **Shared immutable state.** The loaded `CentralConfig` is wrapped in `Arc<T>` by
  `eos-runtime` and cloned cheaply into every consumer (`own-arc-shared`). This
  crate exposes `Clone` configs but does not itself hold an `Arc`.
- **No shared mutable state, no locks, no channels.** Config is build-once /
  read-many; there is no in-process mutation after load, so there are no
  `Mutex`/`RwLock` and no lock-across-await concerns.
- The Python ContextVar override (`override_central_config`) is replaced by an
  explicit value passed in tests — no global mutable singleton (avoids hidden
  state; `get_central_config`'s lazy global is not ported).

## 8. Behavior & Invariants

1. **Source precedence (must not invert).** Effective config is
   `defaults < YAML < env < init`, init highest (`central.py`
   `settings_customise_sources` order: init, env, yaml, secrets — pydantic-settings
   treats earlier sources as higher priority, so this tuple yields
   init>env>yaml>secrets). The Rust loader merges in the same direction.
   (Invariant → AC-eos-config-01.)
2. **`EOS__` nested env.** `EOS__SECTION__FIELD[__SUBFIELD]` lowercases each
   segment and sets the nested path (`loader.py:108-111`). Values starting with
   `[` or `{` are YAML-parsed (`_parse_complex_env_value`, `loader.py:94-101`).
3. **Legacy env adapters** (the surviving 9; `EOS_SWEEVO_*` drop with runner —
   GC-eos-config-05). Each maps a legacy var to a nested config path; empty/blank
   values are skipped; the listed transform is applied:

   | Legacy var | Target path | Transform |
   |---|---|---|
   | `EPHEMERALOS_DATABASE_URL` | `database.url` | strip |
   | `EPHEMERALOS_SANDBOX_TIMEOUT_SECONDS` | `sandbox.timeout_s` | strip |
   | `EPHEMERALOS_RUNTIME_CLIENT_TIMEOUT` | `sandbox.runtime_client_timeout_s` | strip |
   | `EOS_SANDBOX_PROVIDER` | `sandbox.default_provider` | strip + **lowercase**; only `docker` is valid |
   | `EOS_DOCKER_DAEMON_TCP` | `sandbox.docker.daemon_tcp` | strip |
   | `EOS_DOCKER_PRIVILEGED` | `sandbox.docker.privileged` | strip |
   | `EOS_DOCKER_NO_PRIVILEGE` | `sandbox.docker.no_privilege` | strip |
   | `MINIMAX_BASE_URL` | `providers.minimax.base_url` | strip |
   | `MINIMAX_MODEL` | `providers.minimax.model` | strip |

   Note: the Python `_LEGACY_ENV_MAP` is broader because it still includes
   non-Docker provider settings. Rust agent-core deliberately ports only the
   Docker-relevant subset.

4. **Default snapshot (special case).** `EPHEMERALOS_SANDBOX_DEFAULT_SNAPSHOT`
   sets `sandbox.docker.default_snapshot`. The Python fan-out to non-Docker config
   is not ported. (Invariant → AC-eos-config-04.)
5. **Provider key alias.** A `sandbox.provider` key (from `EOS__SANDBOX__PROVIDER`
   or YAML) is renamed to `default_provider` if `default_provider` is absent
   (`loader.py:125-127`).
6. **Fail-fast validation.** Network DB URL → reject. This is an **intentional
   divergence** from Python, not parity: Python's `DatabaseConfig.url` is a plain
   `str` with no validation whose docstring states "PostgreSQL remains available
   through an explicit process environment override" (`database.py:20-24`), so the
   Python config layer accepts any url string including postgres. The Rust
   rejection is justified by anchor §2's no-PostgreSQL non-goal, not by Pydantic.
   Docker `privileged && no_privilege` → reject. Separately, `deny_unknown_fields`
   rejects typo'd/unknown YAML keys (this is the only Pydantic `extra="forbid"`
   parity claim here).
7. **Path resolution** preserves `paths.py` order: `EPHEMERALOS_CONFIG_DIR` else
   `~/.ephemeralos/`; central YAML discovery is `EPHEMERALOS_CONFIG_DIR/ephemeralos.yaml`
   else repo-root `ephemeralos.yaml` (if present) else `~/.ephemeralos/ephemeralos.yaml`.
8. **Env scalar coercion (chosen strategy: YAML-parse every env scalar).**
   serde does **not** coerce `str -> numeric/bool`, so to match Pydantic's lax
   coercion every env scalar value — both `EOS__` outputs and legacy-adapter
   outputs — is YAML-parsed (widening `_parse_complex_env_value` from the
   `[`/`{` complex-value case to all scalars) before merging. This is what makes
   `EOS__SANDBOX__TIMEOUT_S=120` deserialize to `f64` and `EOS_DOCKER_PRIVILEGED=true`
   (a legacy-adapter var) deserialize to `bool`. Documented divergence: unlike
   Pydantic's type-directed coercion (which keeps a `str` field's `"123"`/`"true"`
   verbatim), YAML-parse-everything is not type-directed — a `String` field
   receiving `"123"`/`"true"`/`"null"` would be coerced away from string. This is
   accepted: the actual string fields here are URLs, model names, and snapshots,
   none of which take bare numeric/bool/null literals. (Invariant →
   AC-eos-config-03, AC-eos-config-05.)
9. **Numeric range validation (parity with Pydantic `ge`/`gt`).** The §6
   range annotations are enforced in `validate(&CentralConfig)`, not via newtypes
   (newtypes would ripple into the type tables, `Default` impls, and schema). Only
   constraints the Rust type does not already give are checked: `pool_size >= 1`
   (`database.py:26`), `timeout_s > 0` / `runtime_client_timeout_s > 0`
   (`sandbox.py:42-43`), and `base_delay_s >= 0` / `max_delay_s >= 0`
   (`providers.py:21-22`). `max_retries >= 0` is already enforced by `u32`, so no
   runtime check is added. A range failure surfaces as `ConfigError::OutOfRange`.
   (Invariant → AC-eos-config-11.)

Subtle risk (plan): legacy adapters must run **after** nested `EOS__` parsing and
write into the same merged map, so an explicit `EOS__...` and a legacy var resolve
deterministically (legacy wins where both target the same path, matching
`loader.py` ordering: `_data_from_env` applies `EOS__` first, then legacy).

## 9. SOLID & Principles Applied

- **DIP/OCP/ISP/LSP:** not applicable as trait seams — `eos-config` has no Seam
  Map entry (anchor §6). It is concrete leaf config, consumed via `Arc<T>`. DIP is
  satisfied *downstream*: consumers depend on the config value, and the
  composition root (`eos-runtime`) injects it.
- **SRP:** the crate's single responsibility is "produce a validated immutable
  config"; it deliberately excludes model resolution (eos-db), UI settings (CLI),
  and runner tunables.
- **KISS/YAGNI/DRY:** drop the empty `EngineConfig`, the runner section, the
  reflective `section_name`/`with_overrides`, the global ContextVar, and the
  `pool_pre_ping`/`max_overflow` SQLite-irrelevant fields. One `ConfigError`; one
  loader; defaults defined once per `Default` impl.
- **Type safety:** `DatabaseUrl` and the Docker-only `SandboxProvider` are
  validated newtypes/enums (`type-no-stringly`, `api-parse-dont-validate`);
  booleans/numbers are parsed into typed fields, not strings.
- **Non-goals respected:** no PostgreSQL (fail fast on network URLs); no model
  `class_path` import (model resolution is eos-db); no speculative config knobs.

## 10. Gap Closeouts (tracked requirements)

GC-01..05 map one-to-one to PLAN §4's four gap-closeout bullets (Settings
compat-only; SQLite-only + drop `pool_pre_ping`/`max_overflow`; move retry
defaults to config; runner not imported). **GC-06 and GC-07 are
reviewer/spec-derived requirements** (`extra=forbid` parity and empty-section
YAGNI), not literal PLAN §4 gap-closeout bullets; the 4-bullet → 7-GC expansion
is therefore auditable.

- **GC-eos-config-01** — `Settings` is compatibility-only; `CentralConfig` is the
  sole target loader. Resolution: do not port `Settings` as a runtime type; drop
  its UI fields. `CentralConfig` is the only loader entry point.
- **GC-eos-config-02** — SQLite-only DB config. Resolution: `DatabaseConfig` =
  `{url, pool_size, busy_timeout_ms, wal, foreign_keys, echo}`; `url` is a
  `DatabaseUrl` newtype that rejects network URLs at parse time.
- **GC-eos-config-03** — Drop `pool_pre_ping` and `max_overflow`. Resolution:
  absent from the Rust struct (connection-server concepts; meaningless for
  embedded SQLite). Python fields retained only as migration evidence in §3.
- **GC-eos-config-04** — Move provider retry defaults into config. Resolution:
  `RetryConfig` (with its defaults) lives here and is the source of truth;
  `eos-llm-client` consumes `config.providers.retry` and keeps no local retry
  constants.
- **GC-eos-config-05** — Runner config is test-runner flavored. Resolution: do
  **not** import `runner.py`; drop the `runner` field from `CentralConfig` and the
  `EOS_SWEEVO_*` / `_YAML_ONLY_ENV_PATHS` legacy bindings.
- **GC-eos-config-06** — `extra="forbid"` parity. Resolution: every config struct
  carries `#[serde(deny_unknown_fields)]`, surfacing unknown keys as a fail-fast
  `ConfigError`.
- **GC-eos-config-07** — Empty `EngineConfig`. Resolution: omit `engine` from the
  Rust `CentralConfig` (YAGNI); reintroduce only when a real engine knob exists.
- **GC-eos-config-08** — Docker-only sandbox provider for Rust. Resolution: do
  not port non-Docker provider config fields or legacy env bindings; reject any
  non-Docker `sandbox.default_provider` value during config load.

## 11. Acceptance Criteria

TDD: write each test first, confirm it fails for the right reason, then implement.
Anchor §11 maps this crate to **"env-override tests"**.

- **AC-eos-config-01** *(precedence)* — `test_precedence_init_over_env_over_yaml`:
  a field set in YAML, overridden by an env var, overridden by an init arg
  resolves to the init value; YAML-only resolves to YAML; default otherwise.
- **AC-eos-config-02** *(defaults parity)* — `test_default_config_matches_python`:
  `CentralConfig::default()` yields `timeout_s=300.0`,
  `runtime_client_timeout_s=600.0`, retry `{3, 1.0, 30.0, {429,500,502,503,529}}`,
  `pool_size=5`, `default_provider=Docker`, `wal=true`, `foreign_keys=true`.
- **AC-eos-config-03** *(EOS__ nested env + scalar coercion)* —
  `test_eos_nested_env_sets_path`: `EOS__SANDBOX__TIMEOUT_S=120` →
  `sandbox.timeout_s == 120.0` (string `"120"` YAML-coerced to `f64`, §8 item 8);
  `EOS__PROVIDERS__RETRY__STATUS_CODES=[429,503]` YAML-parses the complex value.
- **AC-eos-config-04** *(default snapshot)* —
  `test_default_snapshot_fans_out`: `EPHEMERALOS_SANDBOX_DEFAULT_SNAPSHOT=foo`
  sets `docker.default_snapshot` to `"foo"`.
- **AC-eos-config-05** *(legacy adapters + scalar coercion)* —
  `test_legacy_env_adapters`: each of the 9 surviving vars maps to its target
  path; `EOS_SANDBOX_PROVIDER=DOCKER` lowercases to `Docker`;
  `EOS_DOCKER_PRIVILEGED=true` YAML-coerces the legacy-adapter string to
  `docker.privileged == true` (bool, §8 item 8); blank values are ignored.
- **AC-eos-config-06** *(reject network DB)* —
  `test_network_database_url_rejected`: `DatabaseUrl::parse("postgresql://...")`
  returns `ConfigError::NetworkDatabaseUrl`; `sqlite:///./x.db` parses.
- **AC-eos-config-07** *(docker contradiction)* —
  `test_docker_privilege_contradiction`: `privileged=true, no_privilege=true`
  fails `validate` with `DockerPrivilegeContradiction`.
- **AC-eos-config-08** *(deny unknown fields)* —
  `test_unknown_yaml_key_rejected`: an unrecognized YAML key fails to deserialize
  (Pydantic `extra="forbid"` parity).
- **AC-eos-config-09** *(provider-alias rename)* —
  `test_provider_key_aliases_to_default_provider`: a `sandbox.provider` key sets
  `default_provider` when the latter is absent; a non-Docker provider string is
  rejected during deserialization.
- **AC-eos-config-10** *(schema parity)* — `test_central_config_json_schema`:
  `schema_for!(CentralConfig)` matches the recorded Pydantic-derived schema for
  the surviving sections **after a normalization pass** (Phase-0 parity harness,
  anchor §11). Raw snapshot equality is infeasible because Rust integer types
  carry `format`/min-max the Pydantic schema lacks; normalize both sides before
  comparing: (a) strip integer `format` (`uint16`/`uint32`) and the implied
  min/max bounds so Rust `u16`/`u32` compare as Python unbounded `integer`;
  (b) render the `DatabaseUrl` newtype as a plain `string` (already handled by
  `#[schemars(transparent)]`); (c) normalize `BTreeSet<u16>` to match Python
  `frozenset[int]` (array-of-integer, ignore item ordering).
- **AC-eos-config-11** *(range constraints)* — `test_range_constraints_rejected`:
  `validate` rejects `database.pool_size=0`, `sandbox.timeout_s=0.0`,
  `sandbox.runtime_client_timeout_s=0.0`, and
  `providers.retry.base_delay_s=-1.0` with `ConfigError::OutOfRange` (Pydantic
  `ge`/`gt` parity, §8 item 9); valid in-range values pass.

## 12. Implementation Checklist

1. `error.rs`: `ConfigError` enum (thiserror) — write first, used by all tests.
2. `database.rs`: `DatabaseUrl` newtype + `parse` + `Deserialize`; `DatabaseConfig`
   with SQLite-only fields and `Default`. Tests: AC-06, AC-02 (db subset).
3. `sandbox.rs`: Docker-only `SandboxProvider` enum, `SandboxConfig`/`Docker` +
   defaults. Test: AC-02 (sandbox subset).
4. `providers.rs`: `ProvidersConfig`/`RetryConfig`/`MinimaxConfig` + defaults
   (`BTreeSet<u16>`). Test: AC-02 (retry).
5. `config.rs`: `CentralConfig { database, sandbox, providers }` + `Default` +
   optional builder (`api-builder-pattern`) for init overrides.
6. `env.rs`: `EOS__` nested parser + all-scalar YAML coercion (§8 item 8) + the
   9-entry legacy adapter table + Docker default snapshot + provider alias. Tests:
   AC-03, AC-04, AC-05, AC-09.
7. `loader.rs`: `load_central_config` layering `defaults < YAML < env < init`.
   Test: AC-01.
8. `validation.rs`: `validate` (network URL already enforced by parse; docker
   contradiction; numeric range checks per §8 item 9). Tests: AC-07, AC-11.
9. `paths.rs`: config/data/log + central YAML discovery, honoring path env vars.
10. `lib.rs`: re-exports + crate lints; add `deny_unknown_fields` audit.
    Tests: AC-08, AC-10 (schema snapshot with the §AC-10 normalization pass).

---
**On completion:** update the Progress Tracker in `./overview.md` for row
`eos-config` per spec-conventions.md §13. Do not edit other crates' rows.
