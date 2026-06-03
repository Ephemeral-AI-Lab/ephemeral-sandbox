# Crate `eos-plugin-catalog` — Class Inventory

> Generated type & field reference. Source of truth is the code under
> `agent-core/crates/eos-plugin-catalog/src/`. Declarations are enumerated with ripgrep
> and field/variant/trait-item data is read directly from source; one-line
> purposes come from `///` doc comments (or, where absent, a reviewer
> summary). Module-scope types only — test-only (`#[cfg(test)]`) and fn-local
> helper types are excluded. This generated inventory is distinct from any
> hand-curated architecture memory layer.

**20 types across 5 files.**

The `eos-plugin-catalog` crate owns the static plugin catalog: it turns the
on-disk plugin tree into validated, in-memory metadata the runtime can bind into
real tools. Each plugin's `plugin.md` frontmatter is parsed two-stage — a
tolerant `RawManifest` DTO validated into the invariant-bearing
`PluginManifest` (with its `PluginKind`, `ToolEntry`, and the validated newtypes
`PluginName` / `PluginToolName` / `PluginResolvedPath`, where `resolve_under`
enforces the no-`..`-escape security invariant) — and all manifests under one
configured catalog root are discovered into the immutable `PluginCatalog`. It
also supplies the catalog-native, model-facing `PluginToolSpec` sources (the ten
LSP input structs and their schemas) and the provider-neutral plugin audit
wrapper (`audit_plugin_call` / `plugin_section`). Every failure flows through the
single `PluginCatalogError` enum. The crate deliberately neither imports nor
executes plugin modules and holds no Pyright/LSP session: it depends on
`eos-types` (`Clock`, `JsonObject`), `eos-audit` (the re-exported `PluginSection`
and the `plugin.*` event family), and `eos-sandbox-api` (`Intent`); downstream,
`eos-runtime` consumes `PluginToolSpec` and binds each into a real
`eos_llm_client::ToolSpec` + `ToolExecutor`.

## Contents

- **`eos-plugin-catalog/src/discovery.rs`** — `PluginCatalog`
- **`eos-plugin-catalog/src/error.rs`** — `PluginCatalogError`
- **`eos-plugin-catalog/src/manifest.rs`** — `PluginKind`, `ToolEntry`, `PluginManifest`, `RawManifest`
- **`eos-plugin-catalog/src/names.rs`** — `PluginName`, `PluginToolName`, `PluginResolvedPath`
- **`eos-plugin-catalog/src/tool_specs.rs`** — `PluginToolSpec`, `HoverInput`, `FindDefinitionsInput`, `FindReferencesInput`, `DiagnosticsInput`, `QuerySymbolsInput`, `RenameInput`, `FormatInput`, `CodeActionsInput`, `ApplyCodeActionInput`, `ApplyWorkspaceEditInput`

---

## `eos-plugin-catalog/src/discovery.rs`

#### `PluginCatalog`  ·  _struct_  ·  derives: `Debug, Clone, Default`  ·  [L22]

The immutable, discovered plugin registry: every validated `<root>/<name>/plugin.md`, keyed (and thus ordered) by `PluginName`.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `plugins` | `BTreeMap<PluginName, PluginManifest>` |  |

<details><summary>Methods (5)</summary>

`discover_under`, `get`, `manifests`, `len`, `is_empty`

</details>

---

## `eos-plugin-catalog/src/error.rs`

#### `PluginCatalogError`  ·  _enum_  ·  derives: `Debug, thiserror::Error`  ·  #[non_exhaustive]  ·  [L16]

This crate's single library error enum — every failure mode of manifest parsing, path resolution, and catalog discovery.

**Variants**:
- `RootNotDir(PathBuf)` — the configured catalog root exists but is not a directory
- `ManifestMissing(PathBuf)` — no `plugin.md` was found under the plugin directory
- `MissingFrontmatter(PathBuf)` — `plugin.md` lacks a `---`-delimited frontmatter block
- `Frontmatter { path: PathBuf, cause: serde_yaml::Error }` — frontmatter is not valid YAML (`#[source]` on `cause`)
- `NotMapping(PathBuf)` — frontmatter parsed but is not a YAML mapping
- `MissingField { path: PathBuf, field: String }` — a required string field is missing, empty, or not a string
- `EmptyTools(PathBuf)` — `tools` is absent, not a list, or empty
- `InvalidName(String)` — a plugin name does not match `^[a-z][a-z0-9_]*$`
- `NameDirMismatch { name: String, dir: String }` — manifest `name` does not equal the plugin directory name
- `ToolPrefix { name: String, prefix: String }` — a tool name does not start with the `<plugin_name>.` prefix
- `DuplicateTool(String)` — two tools within one manifest declare the same name
- `PathEscape(String)` — a declared path resolves outside the plugin directory
- `PathMissing(PathBuf)` — a declared path resolves under the plugin dir but does not exist
- `KindNotString(PathBuf)` — `kind` is present but not a non-empty string
- `UnknownKind(String)` — `kind` is a string but not a recognized `PluginKind`
- `DuplicatePlugin { name: String, first: PathBuf, second: PathBuf }` — two catalog folders declare the same plugin name
- `Io { path: PathBuf, cause: std::io::Error }` — a filesystem read failed (`#[source]` on `cause`)

---

## `eos-plugin-catalog/src/manifest.rs`

#### `PluginKind`  ·  _enum_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema`  ·  #[serde(rename_all = "snake_case")]  ·  #[non_exhaustive]  ·  [L28]

The declared `kind` of a plugin (was `ALLOWED_PLUGIN_KINDS`); the plan reserves room for new kinds.

**Variants**: `LanguageServer`, `Formatter`, `Indexer`, `BuildDaemon`, `McpBridge`, `Custom`

<details><summary>Methods (2)</summary>

`parse`, `as_wire`

</details>

#### `ToolEntry`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, JsonSchema`  ·  #[non_exhaustive]  ·  [L76]

One declared tool in a manifest.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `name` | `PluginToolName` | `pub` |
| `module` | `PluginResolvedPath` | `pub` |

#### `PluginManifest`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, JsonSchema`  ·  #[non_exhaustive]  ·  [L89]

A parsed and validated `plugin.md` — an immutable value type produced only by `parse_plugin_manifest`; derives no `Deserialize`.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `name` | `PluginName` | `pub` |
| `description` | `String` | `pub` |
| `tools` | `Vec<ToolEntry>` | `pub` |
| `setup` | `Option<PluginResolvedPath>` | `pub` |
| `runtime` | `Option<PluginResolvedPath>` | `pub` |
| `source_dir` | `PathBuf` | `pub` |
| `body` | `String` | `pub` |
| `kind` | `Option<PluginKind>` | `pub` |

#### `RawManifest`  ·  _struct_  ·  derives: `Debug, Deserialize`  ·  pub(crate)  ·  [L114]

Tolerant wire-input DTO and the only `Deserialize` target in this crate; each field is `Option<serde_yaml::Value>` so a wrong-typed field surfaces as a granular error.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `name` | `Option<serde_yaml::Value>` | `#[serde(default)]` |
| `description` | `Option<serde_yaml::Value>` | `#[serde(default)]` |
| `tools` | `Option<serde_yaml::Value>` | `#[serde(default)]` |
| `setup` | `Option<serde_yaml::Value>` | `#[serde(default)]` |
| `runtime` | `Option<serde_yaml::Value>` | `#[serde(default)]` |
| `kind` | `Option<serde_yaml::Value>` | `#[serde(default)]` |

---

## `eos-plugin-catalog/src/names.rs`

#### `PluginName`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema`  ·  #[serde(transparent)]  ·  #[schemars(transparent)]  ·  [L25]

A validated plugin folder + manifest name matching `^[a-z][a-z0-9_]*$`; a parse-don't-validate newtype that never derives `Deserialize`.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `.0` | `String` |  |

<details><summary>Methods (2)</summary>

`parse`, `as_str`

</details>

#### `PluginToolName`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema`  ·  #[serde(transparent)]  ·  #[schemars(transparent)]  ·  [L58]

A validated `<plugin_name>.<suffix>` tool name; carries the result already validated by the manifest parser.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `.0` | `String` |  |

<details><summary>Methods (2)</summary>

`new`, `as_str`

</details>

#### `PluginResolvedPath`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Serialize, JsonSchema`  ·  [L80]

A path declared in `plugin.md`, resolved and proven to live under the plugin directory (no `..` escape) — the GC-plugin-catalog-06 security invariant.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `.0` | `PathBuf` |  |

<details><summary>Methods (3)</summary>

`resolve_under`, `as_path`, `into_path_buf`

</details>

---

## `eos-plugin-catalog/src/tool_specs.rs`

#### `PluginToolSpec`  ·  _struct_  ·  derives: `Debug, Clone`  ·  #[non_exhaustive]  ·  [L30]

A catalog-native, model-facing tool-spec source (not an `eos_llm_client::ToolSpec`); `eos-runtime` binds it into a real `ToolSpec` + `ToolExecutor`.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `name` | `PluginToolName` | `pub` |
| `description` | `&'static str` | `pub` |
| `input_schema` | `RootSchema` | `pub` |
| `intent` | `Intent` | `pub` |

#### `HoverInput`  ·  _struct_  ·  derives: `Debug, Deserialize, JsonSchema`  ·  private  ·  [L173]

Crate-private schema source for `lsp.hover`: file path plus a 0-based cursor.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `file_path` | `String` |  |
| `line` | `u32` |  |
| `character` | `u32` |  |

#### `FindDefinitionsInput`  ·  _struct_  ·  derives: `Debug, Deserialize, JsonSchema`  ·  private  ·  [L184]

Crate-private schema source for `lsp.find_definitions`: file path plus a 0-based cursor.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `file_path` | `String` |  |
| `line` | `u32` |  |
| `character` | `u32` |  |

#### `FindReferencesInput`  ·  _struct_  ·  derives: `Debug, Deserialize, JsonSchema`  ·  private  ·  [L195]

Crate-private schema source for `lsp.find_references`: cursor plus an optional declaration-inclusion flag.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `file_path` | `String` |  |
| `line` | `u32` |  |
| `character` | `u32` |  |
| `include_declaration` | `bool` | `#[serde(default = "default_true")]` |

#### `DiagnosticsInput`  ·  _struct_  ·  derives: `Debug, Deserialize, JsonSchema`  ·  private  ·  [L209]

Crate-private schema source for `lsp.diagnostics`: file path plus an optional wait-for-diagnostics flag.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `file_path` | `String` |  |
| `wait_for_diagnostics` | `bool` | `#[serde(default)]` |

#### `QuerySymbolsInput`  ·  _struct_  ·  derives: `Debug, Deserialize, JsonSchema`  ·  private  ·  [L219]

Crate-private schema source for `lsp.query_symbols`: a name fragment with an optional per-file scope.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `query` | `String` |  |
| `file_path` | `Option<String>` | `#[serde(default)]` |

#### `RenameInput`  ·  _struct_  ·  derives: `Debug, Deserialize, JsonSchema`  ·  private  ·  [L229]

Crate-private schema source for `lsp.rename`: cursor plus the replacement symbol name.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `file_path` | `String` |  |
| `line` | `u32` |  |
| `character` | `u32` |  |
| `new_name` | `String` |  |

#### `FormatInput`  ·  _struct_  ·  derives: `Debug, Deserialize, JsonSchema`  ·  private  ·  [L242]

Crate-private schema source for `lsp.format`: file path plus LSP formatting options.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `file_path` | `String` |  |
| `options` | `JsonObject` | `#[serde(default = "default_format_options")]` |

#### `CodeActionsInput`  ·  _struct_  ·  derives: `Debug, Deserialize, JsonSchema`  ·  private  ·  [L252]

Crate-private schema source for `lsp.code_actions`: file path plus optional range, diagnostics, and kind filters.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `file_path` | `String` |  |
| `line` | `u32` | `#[serde(default)]` |
| `character` | `u32` | `#[serde(default)]` |
| `range` | `Option<JsonObject>` | `#[serde(default)]` |
| `diagnostics` | `Vec<JsonObject>` | `#[serde(default)]` |
| `only` | `Option<Vec<String>>` | `#[serde(default)]` |

#### `ApplyCodeActionInput`  ·  _struct_  ·  derives: `Debug, Deserialize, JsonSchema`  ·  private  ·  [L274]

Crate-private schema source for `lsp.apply_code_action`: an opaque LSP `CodeAction` payload.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `action` | `JsonObject` |  |

#### `ApplyWorkspaceEditInput`  ·  _struct_  ·  derives: `Debug, Deserialize, JsonSchema`  ·  private  ·  [L281]

Crate-private schema source for `lsp.apply_workspace_edit`: an opaque LSP `WorkspaceEdit` payload.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `edit` | `JsonObject` |  |
