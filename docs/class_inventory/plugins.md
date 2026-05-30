# Module `plugins` — Class Inventory

> Generated class & field reference. Source of truth is the code under
> `backend/src/plugins/`. Field/type/default data is extracted directly from
> the AST; one-line purposes come from class docstrings (or, where absent, a
> reviewer summary). This generated inventory is distinct from the hand-curated
> `docs/architecture/` memory layer.

**27 classes across 19 files.**

The `plugins` module is the bundled plugin system that extends the agent toolset from manifest-declared, sandbox-side capabilities. Its `core` layer is the generic framework: `manifest.py` parses and strictly validates `plugin.md` frontmatter (`PluginManifest`/`ToolEntry`, allowed `kind` enum, path-escape checks), `discovery.py` walks `catalog/*/plugin.md` deterministically and rejects duplicates, and `loader.py` imports each tool module, binds its single `BaseTool`, and wraps `execute` with a generic `plugin.*` audit shim (`PluginLoaderError` hierarchy). The only shipped plugin is `catalog/lsp`, whose `runtime` owns a long-lived Pyright language-server subprocess (`PyrightSession` plus the `LspJsonRpcClient`/`JsonRpcError` JSON-RPC framing) keyed by layer-stack root in a `session_manager` cache that reconciles sessions to the active overlay snapshot (including unshare/nsenter namespace remounts); its `tools` are thin Pydantic-typed `@tool` wrappers (hover, find_definitions/references, diagnostics, rename, format, code_actions, query_symbols, apply_*) that route through `call_plugin` to the in-sandbox op registry.

## Contents

- **`plugins/catalog/lsp/runtime/apply_child.py`** — `_Request`
- **`plugins/catalog/lsp/runtime/lsp_jsonrpc.py`** — `LspProtocolError`, `JsonRpcError`, `LspJsonRpcClient`
- **`plugins/catalog/lsp/runtime/namespace_entrypoint.py`** — `_Request`
- **`plugins/catalog/lsp/runtime/namespace_remount.py`** — `_Request`
- **`plugins/catalog/lsp/runtime/pyright_session.py`** — `PyrightSpawnError`, `PyrightOverlayRefreshError`, `PyrightSession`
- **`plugins/catalog/lsp/runtime/session_manager.py`** — `_SessionView`
- **`plugins/catalog/lsp/tools/apply_code_action.py`** — `ApplyCodeActionInput`
- **`plugins/catalog/lsp/tools/apply_workspace_edit.py`** — `ApplyWorkspaceEditInput`
- **`plugins/catalog/lsp/tools/code_actions.py`** — `CodeActionsInput`
- **`plugins/catalog/lsp/tools/diagnostics.py`** — `DiagnosticsInput`
- **`plugins/catalog/lsp/tools/find_definitions.py`** — `FindDefinitionsInput`
- **`plugins/catalog/lsp/tools/find_references.py`** — `FindReferencesInput`
- **`plugins/catalog/lsp/tools/format.py`** — `FormatInput`
- **`plugins/catalog/lsp/tools/hover.py`** — `HoverInput`
- **`plugins/catalog/lsp/tools/query_symbols.py`** — `QuerySymbolsInput`
- **`plugins/catalog/lsp/tools/rename.py`** — `RenameInput`
- **`plugins/core/discovery.py`** — `DuplicatePluginError`
- **`plugins/core/loader.py`** — `PluginLoaderError`, `PluginToolImportError`, `PluginToolBindingError`
- **`plugins/core/manifest.py`** — `PluginManifestError`, `ToolEntry`, `PluginManifest`

---

## `plugins/catalog/lsp/runtime/apply_child.py`

#### `_Request`  ·  _class_  ·  [L70]

Parses and validates the JSON payload for the overlay-namespace workspace-edit apply helper subprocess.

**Instance attributes**: `workspace_root`, `layer_paths`, `upperdir`, `workdir`, `output_ref`, `edit`

<details><summary>Methods (1)</summary>

`__init__`

</details>

---

## `plugins/catalog/lsp/runtime/lsp_jsonrpc.py`

#### `LspProtocolError`  ·  _exception_  ·  bases: `RuntimeError`  ·  [L24]

Raised on framing or protocol-level decode failures.

#### `JsonRpcError`  ·  _exception_  ·  bases: `RuntimeError`  ·  [L28]

Raised when an LSP server returns a JSON-RPC error response.

**Instance attributes**: `code`, `message`, `data`

<details><summary>Methods (1)</summary>

`__init__`

</details>

#### `LspJsonRpcClient`  ·  _class_  ·  [L67]

Async JSON-RPC client over a subprocess's stdin/stdout.

**Instance attributes**: `_writer`, `_reader`, `_next_id`, `_pending`, `_request_timeout_s`, `_notifications`, `_server_request_handler`, `_reader_task`, `_closed`

<details><summary>Methods (9)</summary>

`__init__`, `start`, `add_notification_handler`, `request`, `notify`, `close`, `_read_loop`, `_dispatch`, `_respond_to_server_request`

</details>

---

## `plugins/catalog/lsp/runtime/namespace_entrypoint.py`

#### `_Request`  ·  _class_  ·  [L41]

Parses the JSON payload describing the overlay mount and command to exec inside Pyright's private namespace.

**Instance attributes**: `workspace_root`, `layer_paths`, `upperdir`, `workdir`, `argv`, `env`

<details><summary>Methods (1)</summary>

`__init__`

</details>

---

## `plugins/catalog/lsp/runtime/namespace_remount.py`

#### `_Request`  ·  _class_  ·  [L62]

Parses and validates the JSON payload for remounting an LSP private-namespace overlay with new layers.

**Instance attributes**: `workspace_root`, `layer_paths`, `upperdir`, `workdir`

<details><summary>Methods (1)</summary>

`__init__`

</details>

---

## `plugins/catalog/lsp/runtime/pyright_session.py`

#### `PyrightSpawnError`  ·  _exception_  ·  bases: `RuntimeError`  ·  [L46]

Raised when the Pyright language-server subprocess fails to start.

#### `PyrightOverlayRefreshError`  ·  _exception_  ·  bases: `RuntimeError`  ·  [L50]

Raised when a live Pyright overlay cannot be refreshed in place.

#### `PyrightSession`  ·  _class_  ·  [L54]

Long-lived Pyright session rooted at a leased workspace overlay.

**Instance attributes**: `manifest_key`, `workspace_root`, `_overlay_handle`, `_uses_private_overlay_namespace`, `_overlay_layer_paths`, `_layer_index_cache`, `_proc`, `_client`, `_opened`, `_lock`, `_started`, `_document_versions`, `_document_hashes`, `_diagnostic_cache`, `audit_start_count`, `audit_refresh_count`, `audit_remount_count`, `audit_last_start_s`, `audit_last_remount_s`

<details><summary>Methods (47)</summary>

`__init__`, `refresh_manifest`, `start`, `hover`, `find_definitions`, `find_references`, `diagnostics`, `query_symbols`, `rename`, `format_document`, `code_actions`, `evict`, `_cleanup_failed_start`, `_point_query`, `_normalize_locations`, `_open_document`, `_notify_workspace_refreshed`, `_sync_open_document`, `_send_request`, `_pull_diagnostics`, `_fallback_document_symbols`, `_diagnostic_result`, `_spawn`, `_build_argv`, `_build_overlay_argv`, `_refresh_overlay_handle`, `_install_overlay_handle`, `_remount_private_overlay`, `_build_pyright_argv`, `_release_overlay_handle` _(+17 more)_

</details>

---

## `plugins/catalog/lsp/runtime/session_manager.py`

#### `_SessionView`  ·  _class_  ·  [L145]

Value holder for an acquired LSP-session overlay, carrying manifest key, workspace root, and handle.

**Instance attributes**: `manifest_key`, `workspace_root`, `handle`

<details><summary>Methods (1)</summary>

`__init__`

</details>

---

## `plugins/catalog/lsp/tools/apply_code_action.py`

#### `ApplyCodeActionInput`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L16]

Input schema for the lsp.apply_code_action tool, carrying an LSP CodeAction payload.

**Fields**

| name | type | default |
|------|------|---------|
| `action` | `dict[str, Any]` | `Field(..., description='LSP CodeAction payload.')` |

---

## `plugins/catalog/lsp/tools/apply_workspace_edit.py`

#### `ApplyWorkspaceEditInput`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L16]

Input schema for the lsp.apply_workspace_edit tool, carrying an LSP WorkspaceEdit payload.

**Fields**

| name | type | default |
|------|------|---------|
| `edit` | `dict[str, Any]` | `Field(..., description='LSP WorkspaceEdit payload.')` |

---

## `plugins/catalog/lsp/tools/code_actions.py`

#### `CodeActionsInput`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L17]

Input schema for the lsp.code_actions tool requesting Pyright code actions at a file range.

**Fields**

| name | type | default |
|------|------|---------|
| `file_path` | `str` | `Field(..., description='Repo-relative or absolute file path.')` |
| `line` | `int` | `Field(0, ge=0, description='0-based line number.')` |
| `character` | `int` | `Field(0, ge=0, description='0-based character offset.')` |
| `range` | `dict[str, Any] \| None` | `Field(None, description='Optional LSP range.')` |
| `diagnostics` | `list[dict[str, Any]]` | `Field(default_factory=list)` |
| `only` | `list[str] \| None` | `Field(None, description='Optional code action kinds.')` |

---

## `plugins/catalog/lsp/tools/diagnostics.py`

#### `DiagnosticsInput`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L15]

Input schema for the lsp.diagnostics tool fetching Pyright diagnostics for a Python file.

**Fields**

| name | type | default |
|------|------|---------|
| `file_path` | `str` | `Field(..., description='Repo-relative or absolute file path.')` |
| `wait_for_diagnostics` | `bool` | `Field(False, description='When true, wait for at least one Pyright diagnostic before returning, up to the session diagnostic timeout.')` |

---

## `plugins/catalog/lsp/tools/find_definitions.py`

#### `FindDefinitionsInput`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L15]

Input schema for the lsp.find_definitions tool locating a symbol's definitions at a cursor position.

**Fields**

| name | type | default |
|------|------|---------|
| `file_path` | `str` | `Field(..., description='Repo-relative or absolute file path.')` |
| `line` | `int` | `Field(..., ge=0, description='0-based line number.')` |
| `character` | `int` | `Field(..., ge=0, description='0-based character offset on the line.')` |

---

## `plugins/catalog/lsp/tools/find_references.py`

#### `FindReferencesInput`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L15]

Input schema for the lsp.find_references tool finding references to a symbol at a cursor position.

**Fields**

| name | type | default |
|------|------|---------|
| `file_path` | `str` | `Field(..., description='Repo-relative or absolute file path.')` |
| `line` | `int` | `Field(..., ge=0, description='0-based line number.')` |
| `character` | `int` | `Field(..., ge=0, description='0-based character offset on the line.')` |
| `include_declaration` | `bool` | `Field(default=True, description="Include the symbol's own declaration.")` |

---

## `plugins/catalog/lsp/tools/format.py`

#### `FormatInput`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L17]

Input schema for the lsp.format tool formatting a Python file via Pyright.

**Fields**

| name | type | default |
|------|------|---------|
| `file_path` | `str` | `Field(..., description='Repo-relative or absolute file path.')` |
| `options` | `dict[str, Any]` | `Field(default_factory=lambda: {'tabSize': 4, 'insertSpaces': True}, description='LSP formatting options.')` |

---

## `plugins/catalog/lsp/tools/hover.py`

#### `HoverInput`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L15]

Input schema for the lsp.hover tool retrieving Pyright hover information for a symbol at a cursor position.

**Fields**

| name | type | default |
|------|------|---------|
| `file_path` | `str` | `Field(..., description='Repo-relative or absolute file path.')` |
| `line` | `int` | `Field(..., ge=0, description='0-based line number.')` |
| `character` | `int` | `Field(..., ge=0, description='0-based character offset on the line.')` |

---

## `plugins/catalog/lsp/tools/query_symbols.py`

#### `QuerySymbolsInput`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L15]

Validated input for the lsp.query_symbols tool that searches Python symbols workspace-wide or within one file.

**Fields**

| name | type | default |
|------|------|---------|
| `query` | `str` | `Field(..., description='Symbol name fragment.')` |
| `file_path` | `str \| None` | `Field(default=None, description='Optional file path to restrict the search to one document.')` |

---

## `plugins/catalog/lsp/tools/rename.py`

#### `RenameInput`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L15]

Validated input for the lsp.rename tool that renames a Python symbol at a cursor position.

**Fields**

| name | type | default |
|------|------|---------|
| `file_path` | `str` | `Field(..., description='Repo-relative or absolute file path.')` |
| `line` | `int` | `Field(..., ge=0, description='0-based line number.')` |
| `character` | `int` | `Field(..., ge=0, description='0-based character offset.')` |
| `new_name` | `str` | `Field(..., min_length=1, description='Replacement symbol name.')` |

---

## `plugins/core/discovery.py`

#### `DuplicatePluginError`  ·  _exception_  ·  bases: `PluginManifestError`  ·  [L27]

Raised when two catalog folders declare the same plugin name.

---

## `plugins/core/loader.py`

#### `PluginLoaderError`  ·  _exception_  ·  bases: `RuntimeError`  ·  [L40]

Base class for plugin loader failures.

#### `PluginToolImportError`  ·  _exception_  ·  bases: `PluginLoaderError`  ·  [L44]

Raised when a plugin tool module fails to import.

#### `PluginToolBindingError`  ·  _exception_  ·  bases: `PluginLoaderError`  ·  [L48]

Raised when a plugin tool module's BaseTool surface is wrong.

---

## `plugins/core/manifest.py`

#### `PluginManifestError`  ·  _exception_  ·  bases: `ValueError`  ·  [L31]

Raised when a ``plugin.md`` fails schema validation.

#### `ToolEntry`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L58]

One declared tool in a manifest.

**Fields**

| name | type | default |
|------|------|---------|
| `name` | `str` |  |
| `module` | `Path` |  |

#### `PluginManifest`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L66]

Parsed and validated ``plugin.md``.

**Fields**

| name | type | default |
|------|------|---------|
| `name` | `str` |  |
| `description` | `str` |  |
| `tools` | `tuple[ToolEntry, ...]` |  |
| `setup` | `Path \| None` |  |
| `runtime` | `Path \| None` |  |
| `source_dir` | `Path` |  |
| `body` | `str` |  |
| `kind` | `str \| None` | `None` |

