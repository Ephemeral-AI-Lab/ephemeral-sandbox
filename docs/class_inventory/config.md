# Module `config` — Class Inventory

> Generated class & field reference. Source of truth is the code under
> `backend/src/config/`. Field/type/default data is extracted directly from
> the AST; one-line purposes come from class docstrings (or, where absent, a
> reviewer summary). This generated inventory is distinct from the hand-curated
> `docs/architecture/` memory layer.

**18 classes across 10 files.**

The config module owns EphemeralOS's typed, runtime-tunable configuration and the precedence chain that loads it (defaults < repo YAML < environment < init/CLI overrides). Its composition root is `CentralConfig` (a pydantic-settings BaseSettings) which aggregates per-domain sections — `SandboxConfig`/`DockerConfig`/`DaytonaConfig`, `ProvidersConfig`/`RetryConfig`/`MinimaxConfig`, `RunnerConfig`/`LiveE2EConfig`/`DaemonAuditPullConfig`/`AuditWarningsConfig`, `DatabaseConfig`, and `EngineConfig`, all extending the `ModuleConfigBase` primitive — and is fed by the custom `EnvConfigSource` (parsing `EOS__`-prefixed nested vars plus retained legacy `EPHEMERALOS_*`/`DAYTONA_*`/`MINIMAX_*` bindings) and `YamlConfigSource` in loader.py. A second group handles legacy/UI projection and path/model resolution: the `Settings` model projected from central config, XDG-style directory helpers in paths.py, and the DB-backed active-model resolver (`get_active_model_*`, `NoActiveModelError`) where `model_registrations` is the sole source of truth for LLM config.

## Contents

- **`config/base.py`** — `ModuleConfigBase`
- **`config/central.py`** — `CentralConfig`
- **`config/loader.py`** — `YamlConfigSource`, `EnvConfigSource`
- **`config/model_config.py`** — `NoActiveModelError`
- **`config/sections/database.py`** — `DatabaseConfig`
- **`config/sections/engine.py`** — `EngineConfig`
- **`config/sections/providers.py`** — `RetryConfig`, `MinimaxConfig`, `ProvidersConfig`
- **`config/sections/runner.py`** — `LiveE2EConfig`, `DaemonAuditPullConfig`, `AuditWarningsConfig`, `RunnerConfig`
- **`config/sections/sandbox.py`** — `DockerConfig`, `DaytonaConfig`, `SandboxConfig`
- **`config/settings.py`** — `Settings`

---

## `config/base.py`

#### `ModuleConfigBase`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L17]

Base class for module-owned config sections.

**Class variables**: `model_config = ConfigDict(extra='forbid', populate_by_name=True)`

<details><summary>Methods (2)</summary>

`section_name`, `with_overrides`

</details>

---

## `config/central.py`

#### `CentralConfig`  ·  _class_  ·  bases: `BaseSettings`  ·  [L28]

Composition root for all runtime-tunable EphemeralOS config.

**Fields**

| name | type | default |
|------|------|---------|
| `database` | `DatabaseConfig` | `Field(default_factory=DatabaseConfig)` |
| `sandbox` | `SandboxConfig` | `Field(default_factory=SandboxConfig)` |
| `providers` | `ProvidersConfig` | `Field(default_factory=ProvidersConfig)` |
| `runner` | `RunnerConfig` | `Field(default_factory=RunnerConfig)` |
| `engine` | `EngineConfig` | `Field(default_factory=EngineConfig)` |

**Class variables**: `model_config`

<details><summary>Methods (2)</summary>

`settings_customise_sources`, `merge_cli_overrides`

</details>

---

## `config/loader.py`

#### `YamlConfigSource`  ·  _pydantic_  ·  bases: `PydanticBaseSettingsSource`  ·  [L130]

Optional YAML config source for ``ephemeralos.yaml``.

<details><summary>Methods (2)</summary>

`__call__`, `get_field_value`

</details>

#### `EnvConfigSource`  ·  _pydantic_  ·  bases: `PydanticBaseSettingsSource`  ·  [L148]

Environment source for ``EOS__`` nested vars and retained legacy bindings.

<details><summary>Methods (2)</summary>

`__call__`, `get_field_value`

</details>

---

## `config/model_config.py`

#### `NoActiveModelError`  ·  _exception_  ·  bases: `RuntimeError`  ·  [L12]

Raised when no active model registration is available.

---

## `config/sections/database.py`

#### `DatabaseConfig`  ·  _class_  ·  bases: `ModuleConfigBase`  ·  [L17]

Database configuration.

**Fields**

| name | type | default |
|------|------|---------|
| `url` | `str` | `DEFAULT_SQLITE_DATABASE_URL` |
| `pool_pre_ping` | `bool` | `True` |
| `pool_size` | `int` | `Field(default=5, ge=1)` |
| `max_overflow` | `int` | `Field(default=10, ge=0)` |
| `echo` | `bool` | `False` |

---

## `config/sections/engine.py`

#### `EngineConfig`  ·  _class_  ·  bases: `ModuleConfigBase`  ·  [L13]

Engine-wide agent-loop tuning.

---

## `config/sections/providers.py`

#### `RetryConfig`  ·  _class_  ·  bases: `ModuleConfigBase`  ·  [L17]

Provider retry policy.

**Fields**

| name | type | default |
|------|------|---------|
| `max_retries` | `int` | `Field(default=3, ge=0)` |
| `base_delay_s` | `float` | `Field(default=1.0, ge=0)` |
| `max_delay_s` | `float` | `Field(default=30.0, ge=0)` |
| `status_codes` | `frozenset[int]` | `frozenset({429, 500, 502, 503, 529})` |

#### `MinimaxConfig`  ·  _class_  ·  bases: `ModuleConfigBase`  ·  [L26]

Minimax provider routing config.

**Fields**

| name | type | default |
|------|------|---------|
| `base_url` | `str` | `''` |
| `model` | `str` | `''` |

#### `ProvidersConfig`  ·  _class_  ·  bases: `ModuleConfigBase`  ·  [L33]

Provider-level runtime configuration.

**Fields**

| name | type | default |
|------|------|---------|
| `retry` | `RetryConfig` | `Field(default_factory=RetryConfig)` |
| `minimax` | `MinimaxConfig` | `Field(default_factory=MinimaxConfig)` |

---

## `config/sections/runner.py`

#### `LiveE2EConfig`  ·  _class_  ·  bases: `ModuleConfigBase`  ·  [L13]

Live/e2e runner gates.

**Fields**

| name | type | default |
|------|------|---------|
| `heavy_enabled` | `bool` | `False` |
| `capacity_enabled` | `bool` | `False` |
| `real_agent_max_duration_s` | `float` | `Field(default=1800.0, gt=0)` |

#### `DaemonAuditPullConfig`  ·  _class_  ·  bases: `ModuleConfigBase`  ·  [L21]

Daemon audit pull runtime toggle (V3 Phase 3 §Default-on rollout).

**Fields**

| name | type | default |
|------|------|---------|
| `enabled` | `bool` | `True` |
| `floor_ms` | `int` | `Field(default=100, gt=0)` |
| `stream_fallback` | `bool` | `True` |

#### `AuditWarningsConfig`  ·  _class_  ·  bases: `ModuleConfigBase`  ·  [L41]

Tunable thresholds for §13 warnings (V3 Phase 3 deferral D6).

**Fields**

| name | type | default |
|------|------|---------|
| `memory_peak_warn_bytes` | `int` | `Field(default=4 * 1024 ** 3, gt=0)` |

#### `RunnerConfig`  ·  _class_  ·  bases: `ModuleConfigBase`  ·  [L52]

TaskCenter runner defaults.

**Fields**

| name | type | default |
|------|------|---------|
| `audit_dir` | `Path` | `Path('.sweevo_runs')` |
| `run_label` | `str` | `'task_center_runner'` |
| `live_e2e` | `LiveE2EConfig` | `Field(default_factory=LiveE2EConfig)` |
| `sandbox_reuse_mode` | `Literal['fresh', 'reuse', 'force_fresh']` | `'fresh'` |
| `sandbox_quota` | `int` | `Field(default=5, ge=0)` |
| `daemon_audit_pull` | `DaemonAuditPullConfig` | `Field(default_factory=DaemonAuditPullConfig)` |
| `audit_warnings` | `AuditWarningsConfig` | `Field(default_factory=AuditWarningsConfig)` |

---

## `config/sections/sandbox.py`

#### `DockerConfig`  ·  _class_  ·  bases: `ModuleConfigBase`  ·  [L17]

Docker-provider settings.

**Fields**

| name | type | default |
|------|------|---------|
| `daemon_tcp` | `bool` | `True` |
| `privileged` | `bool` | `False` |
| `no_privilege` | `bool` | `False` |
| `default_snapshot` | `str` | `''` |

#### `DaytonaConfig`  ·  _class_  ·  bases: `ModuleConfigBase`  ·  [L26]

Daytona-provider settings and env-sourced credentials.

**Fields**

| name | type | default |
|------|------|---------|
| `api_key` | `str` | `''` |
| `api_url` | `str` | `''` |
| `target` | `str` | `''` |
| `tcp_host` | `str` | `''` |
| `tcp_port` | `int \| None` | `Field(default=None, ge=1, le=65535)` |
| `default_image` | `str` | `''` |
| `default_snapshot` | `str` | `''` |

#### `SandboxConfig`  ·  _class_  ·  bases: `ModuleConfigBase`  ·  [L38]

Sandbox provider defaults and provider-specific config.

**Fields**

| name | type | default |
|------|------|---------|
| `default_provider` | `Literal['docker', 'daytona']` | `'docker'` |
| `timeout_s` | `float` | `Field(default=300.0, gt=0)` |
| `runtime_client_timeout_s` | `float` | `Field(default=600.0, gt=0)` |
| `docker` | `DockerConfig` | `Field(default_factory=DockerConfig)` |
| `daytona` | `DaytonaConfig` | `Field(default_factory=DaytonaConfig)` |

---

## `config/settings.py`

#### `Settings`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L26]

Main settings model for EphemeralOS (non-model config).

**Fields**

| name | type | default |
|------|------|---------|
| `system_prompt` | `str \| None` | `None` |
| `database` | `DatabaseSettings` | `Field(default_factory=DatabaseSettings)` |
| `sandbox` | `SandboxSettings` | `Field(default_factory=SandboxSettings)` |
| `theme` | `str` | `'default'` |
| `fast_mode` | `bool` | `False` |
| `effort` | `str` | `'medium'` |
| `passes` | `int` | `1` |
| `verbose` | `bool` | `False` |

<details><summary>Methods (1)</summary>

`merge_cli_overrides`

</details>

