# impl-eos-plugin-catalog — plugin manifest discovery, catalog state, and plugin tool-spec sources

> Owning crate in the agent-core workspace. Conforms to ./spec-conventions.md.
> Plan section: ../backend_agent_core_rust_migration_PLAN.md §15 (`eos-plugin-catalog`),
> with cross-cutting guidance at PLAN lines 1144-1148 ("Plugins and skills").

## 1. Purpose & Responsibility (SRP)

`eos-plugin-catalog` owns the **static plugin catalog**: it parses and validates
each plugin's `plugin.md` frontmatter into a `PluginManifest`, discovers all
manifests under a single configured catalog root (with duplicate-name detection),
holds the result as an immutable `PluginCatalog` app-state value, and supplies the
**catalog-native model-facing tool-spec sources** (`PluginToolSpec`) plus a
provider-neutral **audit wrapper** for plugin tool invocations. Its single job is
*turn the on-disk plugin catalog into validated, in-memory metadata that the
runtime can bind into real tools.*

This crate must **NOT**: import or execute Python (or any) plugin tool modules
(no `importlib`, no `BaseTool` binding — that machinery in `loader.py` is dropped,
GC-plugin-catalog-01); own `eos_llm_client::ToolSpec`, `ToolName`, `ToolIntent`,
`ToolExecutor`, or `ToolRegistry` (those are owned by `eos-llm-client`/`eos-tools`
and assembled in `eos-runtime` — this crate adds **no** dependency on either, see
§2/§5); run plugin `setup`/`runtime` scripts or hold any Pyright/LSP session state
(that is a sandbox/plugin-runtime concern; this crate only validates the paths and
*describes* the LSP tool boundary, §8); or perform filesystem traversal outside the
configured catalog root.

## 2. Dependencies

- **Upstream crates (depends on):**
  - `eos-types` — `CoreError` participation, `Clock` trait (the audit wrapper
    times calls via `Clock`, not `std::time::Instant`), and `JsonObject` for the
    audit event payload. (See impl-eos-types.md; anchor §5.)
  - `eos-audit` — `AuditEvent`, the **`AuditSink` trait**, and event-builder
    surface the plugin audit wrapper emits through. The plugin-specific section
    payload is owned **here** (it is plugin-domain shaped); the transport/sink is
    referenced (see impl-eos-audit.md, anchor §5).
  - `eos-sandbox-api` — the sandbox **`Intent`** enum (`READ_ONLY` /
    `WRITE_ALLOWED`) that each plugin tool spec carries, matching what the Python
    LSP tools declare today. (See impl-eos-sandbox-api.md.)
  - `eos-config` — resolves the catalog root path; the root is a config-provided
    `&Path`, never derived from `__file__`/`cwd` (GC-plugin-catalog-02). (See
    impl-eos-config.md.)
- **Downstream consumers (used by):**
  - `eos-runtime` — the **sole** declared consumer (overview §4). It constructs
    the `PluginCatalog` once at the composition root, wraps it in `Arc`, binds each
    `PluginToolSpec` into a real `eos_llm_client::ToolSpec` + a `ToolExecutor` that
    routes through `SandboxTransport`, registers those into the `ToolRegistry`, and
    threads the audit wrapper. **All of that binding lives in `eos-runtime`**, not
    here — which is exactly why this crate needs no edge to `eos-tools`/
    `eos-llm-client` and the phase layering (overview §5) is unchanged. (See
    impl-eos-runtime.md, GC-plugin-catalog-04.)

- **External crates** (pinned via workspace dependency inheritance,
  `proj-workspace-deps`; declared `{ workspace = true }` in this crate's
  `Cargo.toml`):

  | Crate | Why | rust-skills rule |
  |---|---|---|
  | `thiserror` | the one library error enum `PluginCatalogError`; no `Box<dyn Error>` in public signatures | `err-thiserror-lib`, `err-custom-type` |
  | `serde` (derive) | `Serialize` on manifests/`PluginToolSpec`/audit section (emit into audit + crate-owned Phase-3 snapshots); `Deserialize` only on the wire-input DTOs (`RawManifest`, LSP input structs), not on the invariant types (§6) | `api-common-traits`, `api-parse-dont-validate` |
  | `serde_yaml` | parse the `---`-delimited YAML frontmatter of `plugin.md` (mirrors Python `yaml.safe_load`) | `api-parse-dont-validate` |
  | `schemars` (`JsonSchema`) | LSP tool input structs participate in the crate-owned Phase-3 schema-snapshot parity harness (anchor §11) | anchor §3 (Pydantic → serde + JsonSchema) |

  No `tokio` runtime, no `async-trait`, no `regex`, no `walkdir`/glob: discovery is
  a single synchronous `read_dir` of the catalog root performed **once** at the
  composition root (anchor §7: lower crates are runtime-agnostic); the name pattern
  is a manual ASCII char-class check (KISS, mirrors skills which also avoids
  `regex`); the audit wrapper is a plain `async fn` combinator over a caller-passed
  future and needs no `#[async_trait]` because it is a generic function, not a
  `dyn` method. The frontmatter split is crate-local (see §3 DRY note).

## 3. Scope & Source Mapping

| Python source | Rust target | What moves / what is dropped |
|---|---|---|
| `plugins/core/manifest.py` (`PluginManifest`, `ToolEntry`, `ALLOWED_PLUGIN_KINDS`, `parse_plugin_manifest`, `PluginManifestError`) | `src/manifest.rs` | Moves all validation: frontmatter split, name pattern, `name == dir`, tool-prefix, duplicate-tool, path-under-dir resolution, `kind` enum check. Validation is **two-stage**: deserialize a `pub(crate) RawManifest` DTO (the only `Deserialize` target) from the frontmatter `serde_yaml::Value`, then validate-into the invariant-bearing `PluginManifest` with `plugin_dir` context (mirrors Python's dynamic dict inspection in `_parse_tools`/`_require_str`, which yields the granular error variants a typed deserialize cannot). `ALLOWED_PLUGIN_KINDS: frozenset[str]` → `PluginKind` enum (`type-no-stringly`). `module: Path` (validated-under-dir) → `PluginResolvedPath` newtype. `setup`/`runtime: Path \| None` → `Option<PluginResolvedPath>` (validated, **not** executed). |
| `plugins/core/discovery.py` (`discover_plugins`, `DuplicatePluginError`, `DEFAULT_CATALOG_DIR`, `default_catalog_dir`) | `src/discovery.rs` | Moves: sorted deterministic walk, skip-folders-without-`plugin.md`, duplicate-name → hard error. **DROPS** `DEFAULT_CATALOG_DIR`/`default_catalog_dir` (the `Path(__file__).parent.parent/"catalog"` derivation); root comes from `eos-config` (GC-plugin-catalog-02). PLAN's separate `catalog.rs` (app-state plugin registry, PLAN line 1010) is intentionally **merged into `discovery.rs`** (the `PluginCatalog` value is colocated with `discover_under`, KISS); no standalone `catalog.rs`. |
| `plugins/core/loader.py` (`register_plugin_tools`, `_load_*`, `_import_from_path`, `_collect_base_tools`, `_LOAD_CACHE`, `PluginToolImportError`, `PluginToolBindingError`) | **mostly DROPPED** | No Python-module import/bind in Rust (GC-plugin-catalog-01). `PluginToolImportError`/`PluginToolBindingError`/`_collect_base_tools`/`_import_from_path`/`_LOAD_CACHE` **vanish**. The **only** surviving concept is the `_install_plugin_audit_shim` wrapper → `src/audit.rs` as a generic combinator (GC-plugin-catalog-03). |
| `plugins/catalog/lsp/plugin.md` + `tools/*.py` (input models, names, intents) | `src/tool_specs.rs` | Moves the **declared** LSP tool surface as catalog-native `PluginToolSpec`s: name const, `const DESCRIPTION`, `#[derive(JsonSchema)]` input struct, `Intent`. Does **not** move `call_plugin`/`call_plugin_write` dispatch (that is the runtime's executor) nor `runtime/server.py` (Pyright session — stays out of agent-core, GC-plugin-catalog-05). |
| `sandbox/daemon/audit_schema.py` (`PluginSection`, `build_plugin_event`) | `src/audit.rs` (`PluginAuditSection`, event builder) | Re-implements the plugin-specific event section + `plugin.*` event builder; emits through the referenced `AuditSink`. |

**In scope:** `PluginManifest`, `ToolEntry`, `PluginKind`, `PluginResolvedPath`,
`PluginName`/`PluginToolName`, `PluginCatalog` + discovery/duplicate detection,
`PluginToolSpec` sources (incl. the 10 LSP specs), the plugin audit wrapper, and
`PluginCatalogError`.
**Out of scope:** Python-module import/bind, plugin tool *execution* and
`call_plugin` dispatch, `setup`/`runtime` script execution, Pyright/LSP session
internals, the real `ToolSpec`/`ToolExecutor`/`ToolRegistry` binding (eos-runtime).

DRY note (mirrors impl-eos-skills.md §3): the `---`-delimited YAML frontmatter
split is needed by both `eos-skills` and this crate. It is duplicated crate-locally
in `frontmatter.rs` for now; if/when a third consumer appears it hoists into
`eos-config`. This is a deliberate not-yet-extraction, not an oversight.

## 4. File & Module Layout

```
eos-plugin-catalog/
  Cargo.toml
  src/
    lib.rs          # crate docs + `pub use` re-exports (proj-pub-use-reexport)
    manifest.rs     # PluginManifest, ToolEntry, PluginKind, parse_plugin_manifest
    names.rs        # PluginName, PluginToolName, PluginResolvedPath validated newtypes
    discovery.rs    # PluginCatalog: discover_under(root) + duplicate detection
    tool_specs.rs   # PluginToolSpec + the 10 LSP tool-spec sources + input structs
    audit.rs        # PluginAuditSection, plugin.* event builder, audit_plugin_call
    frontmatter.rs  # pub(crate) `---`-delimited YAML frontmatter split
    error.rs        # PluginCatalogError (thiserror)
```

`lib.rs` re-exports the public surface: `PluginManifest`, `ToolEntry`,
`PluginKind`, `PluginName`, `PluginToolName`, `PluginResolvedPath`, `PluginCatalog`,
`PluginToolSpec`, `PluginAuditSection`, `audit_plugin_call`, `plugin_tool_specs`,
and `PluginCatalogError`. `frontmatter.rs` is `pub(crate)`
(`proj-pub-crate-internal`); manifest validation helpers are `pub(crate)`.

## 5. Contracts Owned Here

Per anchor §5 this crate owns **`PluginManifest`, `ToolEntry`, `PluginCatalog`,
plugin tool specs, and the plugin audit wrapper**, and is the implementor of the
**`PluginCatalog` seam** in anchor §6 ("manifest-discovered catalog"). As with
`eos-skills`, the seam is the concrete catalog type plus its single
`discover_under` constructor; extension happens by adding plugin folders (OCP),
not by adding a speculative `PluginProvider` trait (would violate anchor §1 /
YAGNI).

Signature sketches (full field tables in §6):

```rust
pub struct PluginCatalog { plugins: BTreeMap<PluginName, PluginManifest> }

impl PluginCatalog {
    /// Discover + validate every `<root>/<name>/plugin.md` (deterministic,
    /// name-sorted). The seam's only filesystem constructor.
    pub fn discover_under(catalog_root: &Path) -> Result<Self, PluginCatalogError>;
    pub fn get(&self, name: &PluginName) -> Option<&PluginManifest>;
    pub fn manifests(&self) -> impl Iterator<Item = &PluginManifest>; // PluginName order
}

/// Catalog-native, model-facing tool-spec *source* (NOT eos_llm_client::ToolSpec).
/// eos-runtime binds this into a real ToolSpec + ToolExecutor.
#[non_exhaustive]
pub struct PluginToolSpec {
    pub name: PluginToolName,        // e.g. "lsp.hover"
    pub description: &'static str,   // const DESCRIPTION near the spec
    pub input_schema: schemars::schema::RootSchema,
    pub intent: eos_sandbox_api::Intent,
}

/// All built-in plugin tool specs (today: the 10 LSP specs).
pub fn plugin_tool_specs() -> Vec<PluginToolSpec>;

/// Provider-neutral audit combinator: time + emit plugin.* around any call.
pub async fn audit_plugin_call<T, E, Fut>(
    sink: &dyn eos_audit::AuditSink,
    clock: &dyn eos_types::Clock,
    section: PluginAuditSection,
    call: Fut,
) -> Result<T, E>
where Fut: std::future::Future<Output = Result<T, E>>;
```

**Object-safety / async note:** `PluginCatalog` and `PluginToolSpec` are concrete
structs (no `dyn`); the composition root holds the catalog as
`Arc<PluginCatalog>` (`own-arc-shared`). `audit_plugin_call` is a generic free
`async fn` (no `#[async_trait]`); it borrows `&dyn AuditSink`/`&dyn Clock` (those
traits' object-safety is the owners' concern, anchor §6). This honors anchor §6's
preference for concrete/`impl Trait` over `Box<dyn Trait>` (`anti-type-erasure`).

**Contracts merely USED (references only, not redefined here):**
- `eos_llm_client::ToolSpec`, `ToolName`, `ToolIntent`, `ToolExecutor`,
  `ToolRegistry` — owned by `eos-llm-client`/`eos-tools`; this crate emits a
  *source* (`PluginToolSpec`) that `eos-runtime` binds (see impl-eos-runtime.md,
  GC-plugin-catalog-04).
- `AuditEvent` / `AuditSink` trait — owned by `eos-audit` (see impl-eos-audit.md).
- `Intent` — owned by `eos-sandbox-api` (see impl-eos-sandbox-api.md).
- `Clock` / `CoreError` / `JsonObject` — owned by `eos-types` (anchor §5).
- Catalog-root path resolution / `CentralConfig` — owned by `eos-config`.

## 6. Types, Fields & Schemas

### `PluginManifest` (source: `plugins/core/manifest.py` lines 65-77)

| Field | Rust type | serde / schemars notes | Source-of-truth |
|---|---|---|---|
| `name` | `PluginName` | transparent newtype over `String` | `name: str` |
| `description` | `String` | plain, non-empty | `description: str` |
| `tools` | `Vec<ToolEntry>` | non-empty (validated) | `tools: tuple[ToolEntry, ...]` |
| `setup` | `Option<PluginResolvedPath>` | validated-under-dir, file-exists; not executed | `setup: Path \| None` |
| `runtime` | `Option<PluginResolvedPath>` | validated-under-dir, file-exists; not executed | `runtime: Path \| None` |
| `source_dir` | `PathBuf` | absolute, canonicalized plugin dir | `source_dir: Path` |
| `body` | `String` | trimmed markdown after frontmatter (informational) | `body: str` |
| `kind` | `Option<PluginKind>` | `#[serde(default)]`; `None` = unset | `kind: str \| None = None` |

Derives `Debug, Clone, PartialEq, Eq, Serialize, JsonSchema` (**no**
`Deserialize` — it is an invariant-bearing type produced only by the validating
parser, not a wire-input DTO; see the two-stage parse below);
`#[non_exhaustive]` (`api-non-exhaustive`). The Python type is
`@dataclass(frozen=True)` → an immutable value type (no `&mut` accessors).

**Two-stage parse (parse-raw → validate).** `PluginManifest` is *not* a direct
`Deserialize` target: two of its fields (`source_dir`, and the resolved
`PluginResolvedPath`s for `tools[].module`/`setup`/`runtime`) are only knowable with
`plugin_dir` context, never from frontmatter alone, and a typed
`serde_yaml::from_value` would collapse the granular `PluginCatalogError`
variants (`MissingField`/`NotMapping`/`EmptyTools`) into one
opaque serde error. So `manifest.rs` defines a `pub(crate) RawManifest` DTO
(plain `Option<String>` / `Option<Vec<RawToolEntry>>`, the only `Deserialize`
target here): parse frontmatter to `serde_yaml::Value`, guard
top-level-is-`Mapping` (→ `NotMapping`), deserialize into `RawManifest`, then
**validate-into** `PluginManifest` with `plugin_dir` context, emitting the
granular variants (name/dir, tool prefix/dup, path resolution + exists, kind).
`RawManifest` lives in `manifest.rs`, not `frontmatter.rs` (that file is the
DRY-shared split with `eos-skills`; a plugin-shaped DTO would couple it).

### `ToolEntry` (source: lines 57-62)

| Field | Rust type | Notes | Source-of-truth |
|---|---|---|---|
| `name` | `PluginToolName` | validated to start with `<plugin_name>.` | `name: str` |
| `module` | `PluginResolvedPath` | resolves under `source_dir`, file exists | `module: Path` |

### `PluginKind` (was `ALLOWED_PLUGIN_KINDS: frozenset[str]`, lines 45-54)

| Variant | serde rename |
|---|---|
| `LanguageServer` | `language_server` |
| `Formatter` | `formatter` |
| `Indexer` | `indexer` |
| `BuildDaemon` | `build_daemon` |
| `McpBridge` | `mcp_bridge` |
| `Custom` | `custom` |

`#[derive(... )]` + `#[serde(rename_all = "snake_case")]`; `#[non_exhaustive]`
(plan reserves room for new kinds). A `kind` that is set-but-not-a-non-empty-string
→ `KindNotString` (matches Python lines 154-157) and an unrecognized value →
`UnknownKind` (matches Python lines 159-164); both are **hard parse errors** —
`type-no-stringly`/`type-enum-states`.
The audit fallback to `Custom` when `kind` is unset is preserved in §8/audit.

### `PluginName`, `PluginToolName`, `PluginResolvedPath` (validated newtypes)

```rust
/// A plugin folder + manifest name. Must match `^[a-z][a-z0-9_]*$` and equal the
/// plugin directory name (manifest.py lines 35, 106-115).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct PluginName(String);

impl PluginName {
    /// Parse-don't-validate: manual ASCII char-class check (no `regex` dep).
    pub fn parse(s: impl Into<String>) -> Result<Self, PluginCatalogError> { /* ... */ }
    pub fn as_str(&self) -> &str { &self.0 }
}

/// A path declared in `plugin.md`, resolved and proven to live UNDER the plugin
/// dir (no `..` escape). The security invariant from manifest.py `_resolve_under`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct PluginResolvedPath(PathBuf); // absolute, canonicalized, `starts_with(source_dir)`
```

`PluginToolName` is a transparent `String` newtype carrying the validated
`<plugin_name>.<suffix>` shape (manifest.py lines 179-194). All three newtypes
(`PluginName`, `PluginToolName`, `PluginResolvedPath`) derive **`Serialize` +
`JsonSchema` only — never `Deserialize`** (`type-newtype-validated`,
`api-parse-dont-validate`): a derived `Deserialize` would be an unvalidated
public constructor able to mint a `PluginResolvedPath` holding `../evil` or a
`PluginName` violating `^[a-z][a-z0-9_]*$` without ever running
`parse`/`resolve_under`. By withholding it, the `parse`/`resolve_under`
validators are the **sole** constructors, so — once parsed — a `PluginResolvedPath`
is structurally guaranteed not to escape `source_dir`; that withholding is what
actually makes this guarantee (and GC-plugin-catalog-06) true. `Serialize` +
`JsonSchema` still satisfy the §2 audit-emit and Phase-3 snapshot uses. (Wire
*input* DTOs — the LSP input structs below and the `pub(crate) RawManifest` —
*do* derive `Deserialize`; the runtime legitimately parses into those.) `Ord` on
`PluginName` gives `BTreeMap` deterministic ordering for free (replaces Python
`sorted(..., key=lambda m: m.name)`).

### `PluginToolSpec` + LSP input structs (source: `catalog/lsp/tools/*.py`)

`tool_specs.rs` declares one `#[derive(JsonSchema)]` input struct, one name const,
one `const DESCRIPTION: &str`, and one `Intent` per LSP tool (anchor §10: exactly
one colocated spec source per tool; doc comments are *not* model-facing text). The
10 tools, grounded in the manifest + tool modules (note: the manifest declares
`lsp.format`, **not** `format_document` — source wins, GC-plugin-catalog-07):

| `PluginToolName` | `Intent` | Input struct fields (Rust types) |
|---|---|---|
| `lsp.hover` | `ReadOnly` | `file_path: String, line: u32, character: u32` |
| `lsp.find_definitions` | `ReadOnly` | `file_path: String, line: u32, character: u32` |
| `lsp.find_references` | `ReadOnly` | `file_path: String, line: u32, character: u32, include_declaration: bool` (default `true`) |
| `lsp.diagnostics` | `ReadOnly` | `file_path: String, wait_for_diagnostics: bool` (default `false`) |
| `lsp.query_symbols` | `ReadOnly` | `query: String, file_path: Option<String>` |
| `lsp.rename` | `WriteAllowed` | `file_path: String, line: u32, character: u32, new_name: String` (min len 1) |
| `lsp.format` | `WriteAllowed` | `file_path: String, options: JsonObject` (`#[serde(default)]`, default `{"tabSize":4,"insertSpaces":true}` — non-optional with default) |
| `lsp.code_actions` | `ReadOnly` | `file_path: String, line: u32` (`#[serde(default)]` `0`), `character: u32` (`#[serde(default)]` `0`), `range: Option<JsonObject>, diagnostics: Vec<JsonObject>` (`#[serde(default)]`, empty), `only: Option<Vec<String>>` |
| `lsp.apply_code_action` | `WriteAllowed` | `action: JsonObject` |
| `lsp.apply_workspace_edit` | `WriteAllowed` | `edit: JsonObject` |

`line`/`character` are `u32` (Python `Field(ge=0)` → non-negative; `u32` makes the
`ge=0` constraint structural, `api-parse-dont-validate`). Opaque LSP payloads
(`range`, `diagnostics[]`, `action`, `edit`, `options`) stay `JsonObject` — the
agent-core boundary does not model the full LSP `WorkspaceEdit`/`CodeAction`
schema (that lives in the daemon-side Pyright runtime, GC-plugin-catalog-05).

### `PluginAuditSection` (source: `audit_schema.py` `PluginSection`, lines 174-200)

| Field | Rust type | Notes |
|---|---|---|
| `plugin_id` | `PluginName` | required (Python `plugin_id`, drop-none `required`) |
| `plugin_kind` | `PluginKind` | required; `Custom` when manifest `kind` unset (loader.py line 117) |
| `plugin_tool_name` | `Option<PluginToolName>` | omitted when `None` |
| `duration_ms` | `Option<f64>` | wall-clock elapsed, ms — two `Clock::now()` reads around the await (deliberate deviation from Python `monotonic_now()`, loader.py 127/141/157); see Clock note below |
| `status` | `Option<PluginCallStatus>` | enum `Ok`/`Error` with `#[serde(rename_all = "lowercase")]` → serializes `"ok"`/`"error"` (was free `str` `"ok"`/`"error"`) |
| `error_kind` | `Option<String>` | error type name on failure |

Only the subset the loader shim actually populates is modeled (the wider
`PluginSection` fields — `request_bytes`, `workspace_handle_id`, etc. — are daemon
telemetry, not emitted by the host wrapper, so YAGNI-dropped). Serialization drops
`None` fields to match `_drop_none`. Marked `#[non_exhaustive]`.

`duration_ms` is **wall-clock elapsed**: `audit_plugin_call` subtracts two
`Clock::now()` reads taken before and after the awaited call. The anchor
`eos-types::Clock` exposes only `fn now(&self) -> UtcDateTime` (a wall-clock
instant, impl-eos-types.md §5.1) and has no monotonic-instant read, so wall-clock
elapsed is the measurement the seam can actually provide. This is a **deliberate
deviation** from the Python shim's `monotonic_now()` (NTP/clock-skew sensitive in
the rare adjusted-clock case); it keeps this crate self-contained with no
cross-crate blocker on the `Clock` owner, and makes the duration deterministically
checkable from a `TestClock` (AC-plugin-catalog-08).

### `PluginCatalogError` (this crate's one error enum, `err-thiserror-lib`)

```rust
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PluginCatalogError {
    #[error("catalog root is not a directory: {0}")]
    RootNotDir(PathBuf),
    #[error("plugin.md missing under {0}")]
    ManifestMissing(PathBuf),
    #[error("plugin.md missing `---`-delimited frontmatter block in {0}")]
    MissingFrontmatter(PathBuf),
    #[error("plugin.md frontmatter is not valid yaml in {path}")]
    Frontmatter { path: PathBuf, #[source] cause: serde_yaml::Error },
    #[error("plugin.md frontmatter is not a yaml mapping in {0}")]
    NotMapping(PathBuf),
    #[error("plugin.md {field} must be a non-empty string in {path}")]
    MissingField { path: PathBuf, field: String },
    #[error("plugin.md tools must be a non-empty list in {0}")]
    EmptyTools(PathBuf),
    #[error("invalid plugin name {0:?}")]
    InvalidName(String),
    #[error("plugin name {name:?} does not match directory {dir:?}")]
    NameDirMismatch { name: String, dir: String },
    #[error("plugin tool name {name:?} must start with {prefix:?}")]
    ToolPrefix { name: String, prefix: String },
    #[error("duplicate tool name {0:?}")]
    DuplicateTool(String),
    #[error("path escapes plugin dir: {0:?}")]
    PathEscape(String),
    #[error("declared path does not exist: {0}")]
    PathMissing(PathBuf),
    #[error("plugin kind must be a non-empty string when set, in {0}")]
    KindNotString(PathBuf),
    #[error("plugin kind {0:?} is not recognized")]
    UnknownKind(String),
    #[error("duplicate plugin name {name:?} in {first} and {second}")]
    DuplicatePlugin { name: String, first: PathBuf, second: PathBuf },
    #[error("failed to read {path}")]
    Io { path: PathBuf, #[source] cause: std::io::Error },
}
```

Messages lowercase, no trailing punctuation (`err-lowercase-msg`); `#[source]`
chains underlying causes (`err-source-chain`). `#[from]` is intentionally omitted
where one upstream type maps to multiple path-carrying variants (explicit
`.map_err`, per `err-from-impl` one-to-one guidance). `DuplicatePlugin` is the
Rust analogue of Python's `DuplicatePluginError` (discovery.py 55-58).

## 7. Concurrency & State Ownership

- **Runtime:** none owned here. Discovery is synchronous `std::fs` I/O run
  **once** at the composition root (`eos-runtime`), before the catalog is frozen
  (anchor §7: lower crates runtime-agnostic; `async-tokio-fs` does not apply — no
  executor to block). The Python process-global `_LOAD_CACHE` (loader.py line 52)
  becomes a single `Arc<PluginCatalog>` built once in the DI graph — no global
  mutable singleton, no interior mutability (mirrors how `eos-skills` dropped its
  `@lru_cache`).
- **Shared immutable state:** after `discover_under`, the `PluginCatalog` and the
  `Vec<PluginToolSpec>` are immutable and shared as `Arc<…>` (`own-arc-shared`),
  cloned cheaply into the runtime's tool layer. No locks anywhere in this crate, so
  `async-no-lock-await`/`anti-lock-across-await` are satisfied vacuously.
- **Audit wrapper async discipline:** `audit_plugin_call` is the only async
  surface. It clones/owns the `PluginAuditSection` fields and reads the `Clock`
  start time **before** `.await`ing the wrapped call, then re-reads the clock and
  emits the completion/error event after (`async-clone-before-await`). It holds no
  lock across the await; emitting through `&dyn AuditSink` is the sink's
  concurrency concern, not this crate's.
- **CPU-bound work:** none; manifests are small markdown/YAML. No `spawn_blocking`.

State-ownership summary: the catalog map and tool-spec list are **owned** by their
structs, **shared** as `Arc` post-construction, and **never** placed behind a lock.

## 8. Behavior & Invariants

Semantics to preserve (cite source files):

1. **Frontmatter contract** (manifest.py 36-39, 87-103): `plugin.md` must begin
   with a `---`-delimited YAML mapping; non-mapping or invalid YAML → error.
2. **Name validation** (manifest.py 35, 105-115): `name` matches
   `^[a-z][a-z0-9_]*$` **and** equals the plugin directory name; otherwise error.
3. **Tools** (manifest.py 167-215): `tools` is a **non-empty** list; each tool
   `name` must start with `"<plugin_name>."`; duplicate tool names within a
   manifest are a hard error; each `module` resolves under the plugin dir and must
   exist on disk.
4. **Path-under-dir** (manifest.py 282-297): every declared path (`tool.module`,
   `setup`, `runtime`) is resolved and must `relative_to(plugin_dir)` — no `..`
   escape. `setup` defaults to `setup.sh` iff it exists when the field is unset
   (manifest.py 218-239). Paths are **validated only, never executed**
   (GC-plugin-catalog-05).
5. **Kind** (manifest.py 140-164): optional; when set must be one of the six
   `PluginKind` values; unknown is a hard error; unset is `None`.
6. **Discovery determinism** (discovery.py 41-73): walk the catalog root in
   name-sorted order; skip dot-dirs, `__pycache__`, and folders without
   `plugin.md` (silently, no error); duplicate **plugin** names across folders →
   `DuplicatePlugin`. The `BTreeMap<PluginName, _>` makes ordering an invariant of
   the structure (replaces explicit `sorted`).
7. **Catalog root** (GC-plugin-catalog-02): the root is the config-resolved
   catalog dir, not `Path(__file__).parent.parent/"catalog"`. A non-existent root
   yields an **empty** catalog (Python `discover_plugins` returns `[]`); a root
   that exists but is a file → `RootNotDir` (fail fast, stricter than Python — a
   non-dir root is a config error, not a "no plugins" state; mirrors skills §8).
8. **Audit wrapper** (loader.py 106-173): around each plugin tool call, emit
   `plugin.tool_invoked` before, then `plugin.tool_completed` (status `Ok`,
   `duration_ms`) on success or `plugin.error` (status `Error`, `error_kind`,
   `duration_ms`) on failure, re-raising the error. `plugin_kind` falls back to
   `Custom` when the manifest declared no `kind`. Events are generic by
   construction: `plugin_kind` is a **value**, never a key (V3 Principle 2).

**LSP boundary (PLAN lines 1002-1004, 1019-1022):** this crate *lists* the 10 LSP
tool schemas (§6) as model-facing spec sources so the runtime can expose them, but
owns **no** Pyright session, `call_plugin` dispatch, or `runtime/server.py`
internals. The `lsp.format` vs `format_document` directive drift is resolved in
favor of the source (`lsp.format`), GC-plugin-catalog-07.

## 9. SOLID & Principles Applied

- **SRP:** parse + discover + describe the catalog; nothing about importing,
  executing, or LSP sessions (those cross boundaries — see §1).
- **OCP:** behavior extends by adding plugin folders or `PluginKind` variants,
  never by editing a dispatch `match`. `PluginCatalog` is the OCP registry seam
  (anchor §6).
- **DIP:** the audit wrapper depends on the **`AuditSink`** and **`Clock`** trait
  abstractions (owned upstream), not on a concrete sink/clock; the composition
  root injects concretes. `PluginToolSpec` depends only on the neutral `Intent`
  abstraction, not on any tool/provider — so the runtime can bind it to a real
  `ToolSpec`/`ToolExecutor` without this crate knowing those types (anchor §5a
  reasoning, applied without the edge).
- **ISP:** the public surface is tiny — `discover_under`/`get`/`manifests`,
  `plugin_tool_specs`, `audit_plugin_call`; no god-object.
- **LSP:** `PluginKind`/`PluginCallStatus` enums and validated newtypes make
  invalid states unrepresentable; substitutability holds for the audit `AuditSink`.
- **KISS/YAGNI/DRY:** **no** `PluginProvider` trait (single implementor — anchor
  §1); **no** Python-module import machinery (dropped); **no** runtime reload/watch
  or async discovery; **no** `regex`; only the `PluginSection` subset the wrapper
  actually emits is modeled. Frontmatter parsing is duplicated locally only because
  it has two consumers today; it hoists to `eos-config` on a third (§3 DRY note).
- **Non-goals respected (anchor §2):** no tool visibility enum (a plugin tool is
  visible iff its bound `ToolSpec` is in the request's `Vec<ToolSpec>` — assembled
  in runtime); no deferred/lazy model-facing tool loading (specs are concrete and
  built eagerly); no dynamic `class_path` import; no orchestration concerns.

## 10. Gap Closeouts (tracked requirements)

- **GC-plugin-catalog-01 — no Python plugin-module import (PLAN lines 1016-1018,
  first bullet).** *Resolution:* the Rust crate **never** imports or binds plugin
  tool modules. `loader.py`'s `importlib`/`BaseTool` machinery
  (`_import_from_path`, `_collect_base_tools`, `PluginToolImportError`,
  `PluginToolBindingError`, `_LOAD_CACHE`) is dropped entirely. Model-facing specs
  come from Rust-native `PluginToolSpec` sources in `tool_specs.rs`; the runtime's
  `ToolExecutor` calls the sandbox plugin RPC at execution time. Proven by
  AC-plugin-catalog-07 (no `importlib`/dynamic-load surface exists).
- **GC-plugin-catalog-02 — single explicit catalog root, no `__file__`.**
  *Resolution:* `discover_under(catalog_root: &Path)` takes the root resolved by
  `eos-config`; `DEFAULT_CATALOG_DIR`/`default_catalog_dir` are dropped. No
  process-`cwd`/`__file__` derivation. Mirrors `eos-skills` GC-skills-01/03. Proven
  by AC-plugin-catalog-06.
- **GC-plugin-catalog-03 — audit wrapper is the only loader survivor (PLAN
  §audit).** *Resolution:* `_install_plugin_audit_shim` → `audit_plugin_call`, a
  generic `async fn` combinator over `&dyn AuditSink` + `&dyn Clock`; it preserves
  the three-event sequence and the `Custom` fallback. Proven by
  AC-plugin-catalog-08.
- **GC-plugin-catalog-04 — specs are sources, bound in runtime (the topology
  constraint).** *Resolution:* this crate emits `PluginToolSpec` (carrying name,
  description, `JsonSchema`, sandbox `Intent`) and adds **no** dependency on
  `eos-llm-client`/`eos-tools`. `eos-runtime` binds each into a real
  `eos_llm_client::ToolSpec` + `ToolExecutor` (`SandboxTransport`) and registers it
  in the `ToolRegistry`. No new dependency edge, no phase-plan change (overview
  §4/§5). Proven by AC-plugin-catalog-09 (spec source compiles against only the
  declared deps).
- **GC-plugin-catalog-05 — validated-not-executed paths; LSP runtime stays out
  (PLAN lines 1019-1022, second + third bullets).** *Resolution:* `setup`/
  `runtime`/`module` are validated as `PluginResolvedPath` (under-dir + exists) but
  never executed here; Pyright sessions / `runtime/server.py` remain a sandbox
  plugin-runtime concern. The crate only *describes* the LSP tool boundary (§6).
  Proven by AC-plugin-catalog-03, AC-plugin-catalog-10.
- **GC-plugin-catalog-06 — path-escape is structurally impossible.**
  *Resolution:* `PluginResolvedPath::resolve_under(plugin_dir, raw)` canonicalizes and
  `starts_with`-checks; a `..`-escaping path returns `PathEscape`. Proven by
  AC-plugin-catalog-04.
- **GC-plugin-catalog-07 — `lsp.format` naming (directive/source drift).**
  *Resolution:* the manifest + tool module declare `lsp.format`; source wins, so
  the Rust spec name const is `lsp.format`. The PLAN/directive wording
  (`format_document`) is noted as drift, not adopted. Proven by
  AC-plugin-catalog-10.

## 11. Acceptance Criteria

TDD: write each test first, confirm it fails for the right reason, then implement.
Maps to anchor §11 "Tests to Port First" row `eos-plugin-catalog` →
"manifest validation".

- **AC-plugin-catalog-01 — manifest happy-path parity.** A fixture
  `<root>/lsp/plugin.md` matching the real LSP manifest parses to a
  `PluginManifest` with `name = "lsp"`, the 10 `ToolEntry`s (names prefixed
  `lsp.`), `kind = Some(LanguageServer)`, resolved `setup`/`runtime`, and a trimmed
  `body`. *Proving test:* `manifest::tests::parses_lsp_manifest`.
- **AC-plugin-catalog-02 — manifest rejection paths.** Frontmatter where
  `name != dir`, a tool name lacking the `<name>.` prefix, or a repeated tool name
  each yields the matching `PluginCatalogError`; additionally, valid YAML that is
  not a mapping → `NotMapping`, an empty `tools` list → `EmptyTools`, and a missing
  required string (e.g. `name`/`description`) → `MissingField`. *Proving test:*
  `manifest::tests::rejects_bad_names_prefixes_and_duplicates`.
- **AC-plugin-catalog-03 — missing declared path rejected.** A `module`/`setup`/
  `runtime` path that resolves under-dir but does not exist → `PathMissing`.
  *Proving test:* `manifest::tests::missing_declared_path_errors`.
- **AC-plugin-catalog-04 — path-escape rejected.** A tool `module` of `../evil.py`
  (or absolute outside) → `PathEscape`; `PluginResolvedPath::resolve_under` never
  returns a path outside `plugin_dir`. *Proving test:*
  `names::tests::rejects_path_escape` (proves GC-plugin-catalog-06).
- **AC-plugin-catalog-05 — kind validation.** Unset `kind` → `None`; a recognized
  value → the right `PluginKind`; an unknown value → `UnknownKind`. *Proving test:*
  `manifest::tests::kind_enum_validation`.
- **AC-plugin-catalog-06 — discovery determinism + roots.** A two-plugin root
  discovers both in `PluginName` order; folders without `plugin.md`/dot-dirs/
  `__pycache__` are skipped; a duplicate plugin name across folders →
  `DuplicatePlugin`; a non-existent root → empty catalog; a file-as-root →
  `RootNotDir`. *Proving test:* `discovery::tests::discovers_sorted_dedup_and_roots`
  (proves GC-plugin-catalog-02).
- **AC-plugin-catalog-07 — no dynamic-import surface.** A source-level check (and
  the absence of any `importlib`/module-load API in the public surface) confirms
  the crate exposes only metadata + specs, never a Python-module loader. *Proving
  test:* `tool_specs::tests::no_module_import_surface` (proves
  GC-plugin-catalog-01).
- **AC-plugin-catalog-08 — audit wrapper event sequence.** `audit_plugin_call`
  over a success future emits `plugin.tool_invoked` then `plugin.tool_completed`
  (status `Ok`, `duration_ms` equal to the wall-clock elapsed between the two
  `TestClock` readings the wrapper takes around the call); over a failing future emits
  `plugin.tool_invoked` then `plugin.error` (status `Error`, `error_kind`) and
  re-returns the `Err`; an unset manifest `kind` records `Custom`. *Proving test:*
  `audit::tests::wrapper_emits_invoked_completed_error` (proves
  GC-plugin-catalog-03; uses an in-memory `AuditSink` + test `Clock`,
  `test-mock-traits`).
- **AC-plugin-catalog-09 — specs build against declared deps only.**
  `plugin_tool_specs()` returns 10 `PluginToolSpec`s with the §6 names/intents and
  valid `JsonSchema`s, compiling with **no** `eos-tools`/`eos-llm-client`
  dependency. *Proving test:* `tool_specs::tests::ten_lsp_specs_with_intents`
  (proves GC-plugin-catalog-04).
- **AC-plugin-catalog-10 — LSP input-schema snapshot parity.**
  `schema_for!(HoverInput)` … `schema_for!(ApplyWorkspaceEditInput)` match the
  committed Phase-3 snapshots derived from the current Pydantic schemas on a
  **normalized comparison** — the field-name/optionality set, defaults, and the
  `lsp.format` name — **not** raw JSON-Schema byte-equality. The deliberate
  `u32` (for `line`/`character`, was Pydantic `int(ge=0)`) and `JsonObject` (for
  opaque LSP payloads, was `dict`/`list`) choices intentionally change the
  emitted schema, so a raw byte-equal snapshot would mismatch by design.
  *Proving test:* `tool_specs::tests::lsp_input_schema_snapshots` (anchor §11
  Phase-3 parity harness; proves GC-plugin-catalog-07).

## 12. Implementation Checklist

Ordered, small, verifiable steps (`small-incremental-changes`):

1. Scaffold crate per anchor §14 (workspace member, inherited deps, workspace
   lints). `cargo build` green with empty `lib.rs`.
2. `error.rs`: define `PluginCatalogError` (thiserror, `#[non_exhaustive]`).
3. `names.rs`: `PluginName`/`PluginToolName` parse (char-class, prefix),
   `PluginResolvedPath::resolve_under` (canonicalize + `starts_with`). Write
   AC-plugin-catalog-04 first.
4. `frontmatter.rs` (`pub(crate)`): port the `---` YAML frontmatter split.
5. `manifest.rs`: `PluginKind` enum, `ToolEntry`, `PluginManifest`, the
   `pub(crate) RawManifest`/`RawToolEntry` `Deserialize` DTOs, and
   `parse_plugin_manifest` as a two-stage parse — deserialize `RawManifest` from
   the frontmatter `serde_yaml::Value` (guard top-level-is-`Mapping` →
   `NotMapping`), then validate-into `PluginManifest` with `plugin_dir` context
   (name/dir, tools non-empty + prefix + dup, path resolution + exists, kind,
   setup default), emitting the granular `PluginCatalogError` variants. Write
   AC-plugin-catalog-01/02/03/05 first.
6. `discovery.rs`: `PluginCatalog::discover_under` over `BTreeMap`; skip rules;
   `DuplicatePlugin`; empty-vs-non-dir root. Write AC-plugin-catalog-06 first.
7. `tool_specs.rs`: `PluginToolSpec`, the 10 LSP input structs + name consts +
   `const DESCRIPTION` + `Intent`, `plugin_tool_specs()`. Write
   AC-plugin-catalog-07/09 first; commit Phase-3 schema snapshots (AC-10).
8. `audit.rs`: `PluginAuditSection`, `PluginCallStatus`, `plugin.*` event builder,
   `audit_plugin_call`. Write AC-plugin-catalog-08 first.
9. `lib.rs`: `pub use` re-exports; `cargo clippy -D warnings` + `cargo fmt
   --check` clean.

---
**On completion:** update the Progress Tracker in `./overview.md` for row
`eos-plugin-catalog` per spec-conventions.md §13 (status + date + short note +
commit/PR ref). Do not edit other crates' rows.
