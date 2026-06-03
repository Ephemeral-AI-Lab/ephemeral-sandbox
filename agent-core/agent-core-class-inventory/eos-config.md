# Crate `eos-config` — Class Inventory

> Generated type & field reference. Source of truth is the code under
> `agent-core/crates/eos-config/src/`. Declarations are enumerated with ripgrep
> and field/variant/trait-item data is read directly from source; one-line
> purposes come from `///` doc comments (or, where absent, a reviewer
> summary). Module-scope types only — test-only (`#[cfg(test)]`) and fn-local
> helper types are excluded. This generated inventory is distinct from any
> hand-curated architecture memory layer.

**13 types across 8 files.**

The `eos-config` crate owns typed, validated, immutable runtime configuration:
it loads [`CentralConfig`] from layered sources (`defaults < YAML < env < init`),
parses raw strings into validated config types at the boundary, resolves on-disk
config/data/log paths, and fails fast on contradictory or unsupported settings
(network database urls, docker `privileged + no_privilege`). The composition
root is `CentralConfig`, assembled from the section structs `DatabaseConfig`,
`SandboxConfig`, `ProvidersConfig`, and the Rust-only `AttemptConfig`; the
validated `DatabaseUrl` newtype rejects network backends at parse time, and
`ConfigError` is the crate's single error enum. `ConfigLoader` (with the
`load_central_config` free fn) does the layered merge, the `EnvMap` alias and
env-tree conversion feed it, and `parse_markdown_frontmatter` is a shared
frontmatter helper. It is a leaf of the workspace dependency DAG — it has no
internal upstream edge (not even `eos-types`) — and is consumed read-only by
every crate that needs tunables; it deliberately performs no I/O beyond reading
config files and the environment and does not resolve the active model, hold
secrets, or open connections.

## Contents

- **`eos-config/src/lib.rs`** — _(no inventoried types; crate root, re-exports + test-only schema-parity module)_
- **`eos-config/src/attempt.rs`** — `AttemptConfig`
- **`eos-config/src/config.rs`** — `CentralConfig`
- **`eos-config/src/database.rs`** — `DatabaseUrl`, `DatabaseConfig`
- **`eos-config/src/env.rs`** — `EnvMap`
- **`eos-config/src/error.rs`** — `ConfigError`
- **`eos-config/src/loader.rs`** — `ConfigLoader`
- **`eos-config/src/providers.rs`** — `RetryConfig`, `MinimaxConfig`, `ProvidersConfig`
- **`eos-config/src/sandbox.rs`** — `SandboxProvider`, `DockerConfig`, `SandboxConfig`

---

## `eos-config/src/attempt.rs`

#### `AttemptConfig`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema`  ·  #[serde(deny_unknown_fields)] · #[non_exhaustive]  ·  [L17]

Per-Attempt run-stage tunables (Rust-only; the per-Attempt fan-out cap consumed by `eos-workflow`).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `max_concurrent_task_runs` | `usize` | `pub` |

**Trait impls**: `Default`

---

## `eos-config/src/config.rs`

#### `CentralConfig`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema`  ·  #[serde(deny_unknown_fields)] · #[non_exhaustive]  ·  [L24]

The validated, immutable composition root for all runtime-tunable config; built by `load_central_config` and read-only after load.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `database` | `DatabaseConfig` | `pub` |
| `sandbox` | `SandboxConfig` | `pub` |
| `providers` | `ProvidersConfig` | `pub` |
| `attempt` | `AttemptConfig` | `pub` |

---

## `eos-config/src/database.rs`

#### `DatabaseUrl`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, JsonSchema`  ·  #[serde(transparent)] · #[schemars(transparent)]  ·  [L20]

A validated local-sqlite database url; network backends are rejected at parse time.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `.0` | `String` |  |

**Trait impls**: `Deserialize`

<details><summary>Methods (2)</summary>

`parse`, `as_str`

</details>

#### `DatabaseConfig`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema`  ·  #[serde(deny_unknown_fields)] · #[non_exhaustive]  ·  [L70]

Sqlite-only database configuration with the agent-core sqlite tunables (`busy_timeout_ms`/`wal`/`foreign_keys`).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `url` | `DatabaseUrl` | `pub` |
| `pool_size` | `u32` | `pub` |
| `busy_timeout_ms` | `u64` | `pub` |
| `wal` | `bool` | `pub` |
| `foreign_keys` | `bool` | `pub` |
| `echo` | `bool` | `pub` |

**Trait impls**: `Default`

---

## `eos-config/src/env.rs`

#### `EnvMap`  ·  _type alias_  ·  = `BTreeMap<String, String>`  ·  [L20]

The injected (or process) environment the loader and path resolvers read.

---

## `eos-config/src/error.rs`

#### `ConfigError`  ·  _enum_  ·  derives: `Debug`  ·  #[derive(thiserror::Error)] · #[non_exhaustive]  ·  [L8]

Errors raised while loading, parsing, or validating `CentralConfig`.

**Variants**:
- `NetworkDatabaseUrl(String)` — a network database url (postgres/mysql scheme or credentialed `//host` authority) was supplied
- `UnsupportedDatabaseUrl(String)` — a url that is neither a `sqlite:` scheme nor a local `.db` path
- `DockerPrivilegeContradiction` — the docker section set both `privileged` and `no_privilege`
- `OutOfRange { field: String, detail: String }` — a numeric field fell outside its allowed range
- `ReadFile(#[source] std::io::Error)` — a config file could not be read from disk
- `ParseYaml(#[source] serde_yaml::Error)` — the config yaml failed to parse or deserialize into `CentralConfig`

**Trait impls**: `Error` (via `#[derive(thiserror::Error)]`), `Display`

---

## `eos-config/src/loader.rs`

#### `ConfigLoader`  ·  _struct_  ·  derives: `Debug`  ·  [L28]

Builder for `CentralConfig` loading; defaults read the process env and discover the central YAML, tests inject explicit env / YAML path / init layer.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `yaml_path` | `Option<PathBuf>` |  |
| `env` | `EnvMap` |  |
| `init` | `Value` |  |

**Trait impls**: `Default`

<details><summary>Methods (7)</summary>

`new`, `yaml_path`, `env`, `init`, `load`, `init_layer`, `read_yaml`

</details>

---

## `eos-config/src/providers.rs`

#### `RetryConfig`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema`  ·  #[serde(deny_unknown_fields)] · #[non_exhaustive]  ·  [L14]

Provider retry policy; the single source of truth for retry defaults consumed by `eos-llm-client`.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `max_retries` | `u32` | `pub` |
| `base_delay_s` | `f64` | `pub` |
| `max_delay_s` | `f64` | `pub` |
| `status_codes` | `BTreeSet<u16>` | `pub` |

**Trait impls**: `Default`

#### `MinimaxConfig`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema`  ·  #[serde(deny_unknown_fields)] · #[non_exhaustive]  ·  [L41]

Minimax provider routing config (base url and model key).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `base_url` | `String` | `pub` |
| `model` | `String` | `pub` |

#### `ProvidersConfig`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema`  ·  #[serde(deny_unknown_fields)] · #[non_exhaustive]  ·  [L52]

Provider-level runtime configuration (retry policy plus minimax routing).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `retry` | `RetryConfig` | `pub` |
| `minimax` | `MinimaxConfig` | `pub` |

---

## `eos-config/src/sandbox.rs`

#### `SandboxProvider`  ·  _enum_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema`  ·  #[serde(rename_all = "snake_case")] · #[non_exhaustive]  ·  [L14]

The sandbox backend seam; Docker is the only supported variant and any other provider string fails to deserialize.

**Variants**: `Docker` (`#[default]`)

#### `DockerConfig`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema`  ·  #[serde(deny_unknown_fields)] · #[non_exhaustive]  ·  [L24]

Docker-provider settings (daemon TCP, privilege flags, default snapshot).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `daemon_tcp` | `bool` | `pub` |
| `privileged` | `bool` | `pub` |
| `no_privilege` | `bool` | `pub` |
| `default_snapshot` | `String` | `pub` |

**Trait impls**: `Default`

#### `SandboxConfig`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema`  ·  #[serde(deny_unknown_fields)] · #[non_exhaustive]  ·  [L50]

Sandbox provider defaults and Docker-specific config (default provider, timeouts, docker settings).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `default_provider` | `SandboxProvider` | `pub` |
| `timeout_s` | `f64` | `pub` |
| `runtime_client_timeout_s` | `f64` | `pub` |
| `docker` | `DockerConfig` | `pub` |

**Trait impls**: `Default`
