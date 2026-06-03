# Crate `eos-sandbox-api` — Class Inventory

> Generated type & field reference. Source of truth is the code under
> `agent-core/crates/eos-sandbox-api/src/`. Declarations are enumerated with ripgrep
> and field/variant/trait-item data is read directly from source; one-line
> purposes come from `///` doc comments (or, where absent, a reviewer
> summary). Module-scope types only — test-only (`#[cfg(test)]`) and fn-local
> helper types are excluded. This generated inventory is distinct from any
> hand-curated architecture memory layer.

**34 types across 5 files.**

The `eos-sandbox-api` crate owns the host-facing sandbox protocol boundary: the typed contract agent-core uses to call the existing sandbox daemon. Its responsibility is purely declarative and translational — it defines the request/result DTOs and the [`Intent`] for each daemon operation (`models.rs`), the typed daemon-op constants `DaemonOp` whose serialized form is the exact daemon wire string (`ops.rs`), the single library error enum `SandboxApiError` (`error.rs`), the per-verb timeout policy (`timeouts.rs`), and the `SandboxTransport` async-trait DIP seam (`transport.rs`). The pure `tool_api` helpers each build a daemon payload from a typed request, call a `&dyn SandboxTransport`, and hand-parse the JSON envelope into a typed result struct via the decoders/coercers/conflict-classifier in `tool_api/parse.rs` (whose only module-scope helper struct is `GuardedCommon`). The crate depends on `eos-types` (for id newtypes, `JsonObject`, `SandboxId`) plus `serde`/`schemars`/`thiserror`/`async-trait`; it deliberately does not implement the daemon-backed transport (that is `eos-sandbox-host`'s `DaemonSandboxTransport`), select a provider, own a runtime, or emit audit events (audit wrapping lives in `eos-tools`). `eos-runtime` injects an `Arc<dyn SandboxTransport>` at the composition root, and `eos-tools` consumes the typed helpers.

## Contents

- **`eos-sandbox-api/src/error.rs`** — `SandboxApiError`
- **`eos-sandbox-api/src/models.rs`** — `Intent`, `Workspace`, `SandboxCaller`, `SandboxRequestBase`, `SandboxResultBase`, `ConflictInfo`, `ReadFileRequest`, `ReadFileResult`, `WriteFileRequest`, `WriteFileResult`, `SearchReplaceEdit`, `EditFileRequest`, `EditFileResult`, `CommandOutput`, `ExecCommandRequest`, `ExecCommandResult`, `ExecStdinRequest`, `CommandSessionWriteRequest`, `CommandSessionCancelRequest`, `GlobRequest`, `GlobResult`, `GrepRequest`, `GrepResult`, `LifecycleError`, `LifecycleResultBase`, `EnterIsolatedWorkspaceRequest`, `EnterIsolatedWorkspaceResult`, `ExitIsolatedWorkspaceRequest`, `ExitIsolatedWorkspaceResult`, `ToolCallRequest`
- **`eos-sandbox-api/src/ops.rs`** — `DaemonOp`
- **`eos-sandbox-api/src/tool_api/parse.rs`** — `GuardedCommon`
- **`eos-sandbox-api/src/transport.rs`** — `SandboxTransport`

---

## `eos-sandbox-api/src/error.rs`

#### `SandboxApiError`  ·  _enum_  ·  derives: `Debug, Clone, thiserror::Error`  ·  #[non_exhaustive]  ·  [L22]

The single library error enum raised when calling the sandbox daemon through a `SandboxTransport`.

**Variants**:
- `Transport { code: Option<String>, message: String }` — a sandbox RPC failed at the transport; `code` is the daemon-resolved structured error code, `message` the user-facing text.
- `Decode { message: String }` — a daemon JSON envelope failed to decode into the expected typed result.

<details><summary>Methods (4)</summary>

`transport`, `decode`, `message`, `code`

</details>

---

## `eos-sandbox-api/src/models.rs`

#### `Intent`  ·  _enum_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema`  ·  [L27]

High-level execution intent for a foreground sandbox tool call (serde `snake_case`).

**Variants**: `ReadOnly`, `WriteAllowed`, `Lifecycle`

<details><summary>Methods (1)</summary>

`as_wire`

</details>

#### `Workspace`  ·  _enum_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema`  ·  [L54]

Which workspace a result was produced against; never decoded from a daemon envelope (always the `Ephemeral` default).

**Variants**: `Ephemeral` (`#[default]`), `Isolated`

#### `SandboxCaller`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema`  ·  [L70]

Caller identity threaded onto every audit-aware request; four required ids always present, the rest optional.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `agent_id` | `String` | `pub` |
| `run_id` | `String` | `pub` · `#[serde(default)]` |
| `agent_run_id` | `String` | `pub` · `#[serde(default)]` |
| `task_id` | `String` | `pub` · `#[serde(default)]` |
| `request_id` | `String` | `pub` · `#[serde(default)]` |
| `attempt_id` | `String` | `pub` · `#[serde(default)]` |
| `workflow_id` | `String` | `pub` · `#[serde(default)]` |
| `tool_id` | `Option<ToolUseId>` | `pub` · `#[serde(default, skip_serializing_if = "Option::is_none")]` |

<details><summary>Methods (6)</summary>

`identity_block`, `agent_run`, `task`, `request`, `attempt`, `workflow`

</details>

#### `SandboxRequestBase`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema`  ·  [L175]

Base request shape for audit-aware public sandbox operations; embedded as a flattened field on each verb request.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `caller` | `SandboxCaller` | `pub` |
| `description` | `String` | `pub` · `#[serde(default)]` |
| `invocation_id` | `Option<InvocationId>` | `pub` · `#[serde(default, skip_serializing_if = "Option::is_none")]` |

<details><summary>Methods (1)</summary>

`description_or`

</details>

#### `SandboxResultBase`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema`  ·  [L205]

Base result shape for public sandbox operations; embedded as a flattened field on each verb result.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `success` | `bool` | `pub` |
| `workspace` | `Workspace` | `pub` · `#[serde(default)]` |
| `timings` | `BTreeMap<String, f64>` | `pub` · `#[serde(default)]` |
| `conflict` | `Option<ConflictInfo>` | `pub` · `#[serde(default, skip_serializing_if = "Option::is_none")]` |
| `conflict_reason` | `Option<String>` | `pub` · `#[serde(default, skip_serializing_if = "Option::is_none")]` |
| `changed_paths` | `Vec<String>` | `pub` · `#[serde(default)]` |
| `error` | `Option<JsonObject>` | `pub` · `#[serde(default, skip_serializing_if = "Option::is_none")]` |

#### `ConflictInfo`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema`  ·  [L230]

Structured guarded-operation conflict details.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `reason` | `String` | `pub` |
| `conflict_file` | `Option<String>` | `pub` · `#[serde(default, skip_serializing_if = "Option::is_none")]` |
| `message` | `String` | `pub` · `#[serde(default)]` |

<details><summary>Methods (2)</summary>

`rejected`, `overlap`

</details>

#### `ReadFileRequest`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema`  ·  [L265]

Read one UTF-8 text file.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `base` | `SandboxRequestBase` | `pub` · `#[serde(flatten)]` |
| `path` | `String` | `pub` |

#### `ReadFileResult`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema`  ·  [L275]

Result of `ReadFileRequest`.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `base` | `SandboxResultBase` | `pub` · `#[serde(flatten)]` |
| `content` | `String` | `pub` |
| `exists` | `bool` | `pub` · `#[serde(default)]` |
| `encoding` | `String` | `pub` |

#### `WriteFileRequest`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema`  ·  [L290]

Write one UTF-8 file through OCC.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `base` | `SandboxRequestBase` | `pub` · `#[serde(flatten)]` |
| `path` | `String` | `pub` |
| `content` | `String` | `pub` |
| `overwrite` | `bool` | `pub` · `#[serde(default = "default_true")]` |

#### `WriteFileResult`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema`  ·  [L305]

Result of `WriteFileRequest` (a guarded mutation).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `base` | `SandboxResultBase` | `pub` · `#[serde(flatten)]` |
| `changed_path_kinds` | `BTreeMap<String, String>` | `pub` · `#[serde(default)]` |
| `mutation_source` | `String` | `pub` · `#[serde(default)]` |
| `status` | `String` | `pub` · `#[serde(default)]` |

#### `SearchReplaceEdit`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema`  ·  [L322]

One exact-match replacement applied as part of an `EditFileRequest`.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `old_text` | `String` | `pub` |
| `new_text` | `String` | `pub` |
| `replace_all` | `bool` | `pub` · `#[serde(default)]` |

#### `EditFileRequest`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema`  ·  [L334]

Apply search/replace edits through OCC.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `base` | `SandboxRequestBase` | `pub` · `#[serde(flatten)]` |
| `path` | `String` | `pub` |
| `edits` | `Vec<SearchReplaceEdit>` | `pub` |

#### `EditFileResult`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema`  ·  [L346]

Result of `EditFileRequest` (a guarded mutation).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `base` | `SandboxResultBase` | `pub` · `#[serde(flatten)]` |
| `changed_path_kinds` | `BTreeMap<String, String>` | `pub` · `#[serde(default)]` |
| `mutation_source` | `String` | `pub` · `#[serde(default)]` |
| `status` | `String` | `pub` · `#[serde(default)]` |
| `applied_edits` | `u32` | `pub` · `#[serde(default)]` |

#### `CommandOutput`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema`  ·  [L366]

Stdout/stderr captured from a command session.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `stdout` | `String` | `pub` · `#[serde(default)]` |
| `stderr` | `String` | `pub` · `#[serde(default)]` |

#### `ExecCommandRequest`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema`  ·  [L377]

Run or start a managed command session.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `base` | `SandboxRequestBase` | `pub` · `#[serde(flatten)]` |
| `cmd` | `String` | `pub` |
| `yield_time_ms` | `Option<u32>` | `pub` · `#[serde(default, skip_serializing_if = "Option::is_none")]` |
| `timeout` | `Option<u32>` | `pub` · `#[serde(default, skip_serializing_if = "Option::is_none")]` |
| `max_output_tokens` | `Option<u32>` | `pub` · `#[serde(default, skip_serializing_if = "Option::is_none")]` |

#### `ExecCommandResult`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema`  ·  [L396]

Result of `ExecCommandRequest` / command-session writes.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `base` | `SandboxResultBase` | `pub` · `#[serde(flatten)]` |
| `status` | `String` | `pub` |
| `exit_code` | `Option<i32>` | `pub` · `#[serde(default, skip_serializing_if = "Option::is_none")]` |
| `output` | `CommandOutput` | `pub` |
| `command_session_id` | `Option<String>` | `pub` · `#[serde(default, skip_serializing_if = "Option::is_none")]` |
| `changed_path_kinds` | `BTreeMap<String, String>` | `pub` · `#[serde(default)]` |
| `mutation_source` | `String` | `pub` · `#[serde(default)]` |

#### `ExecStdinRequest`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema`  ·  [L420]

Write characters to an open command session through `api.v1.exec_stdin`.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `base` | `SandboxRequestBase` | `pub` · `#[serde(flatten)]` |
| `command_session_id` | `String` | `pub` |
| `chars` | `String` | `pub` |
| `yield_time_ms` | `Option<u32>` | `pub` · `#[serde(default, skip_serializing_if = "Option::is_none")]` |
| `max_output_tokens` | `Option<u32>` | `pub` · `#[serde(default, skip_serializing_if = "Option::is_none")]` |

#### `CommandSessionWriteRequest`  ·  _type alias_  ·  = `ExecStdinRequest`  ·  [L437]

Model-facing `write_stdin` request alias for `ExecStdinRequest`.

#### `CommandSessionCancelRequest`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema`  ·  [L441]

Cancel an open command session.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `base` | `SandboxRequestBase` | `pub` · `#[serde(flatten)]` |
| `command_session_id` | `String` | `pub` |

#### `GlobRequest`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema`  ·  [L451]

Enumerate workspace paths matching a glob pattern.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `base` | `SandboxRequestBase` | `pub` · `#[serde(flatten)]` |
| `pattern` | `String` | `pub` |
| `path` | `Option<String>` | `pub` · `#[serde(default, skip_serializing_if = "Option::is_none")]` |

#### `GlobResult`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema`  ·  [L464]

Result of `GlobRequest`.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `base` | `SandboxResultBase` | `pub` · `#[serde(flatten)]` |
| `filenames` | `Vec<String>` | `pub` · `#[serde(default)]` |
| `num_files` | `u32` | `pub` · `#[serde(default)]` |
| `truncated` | `bool` | `pub` · `#[serde(default)]` |

#### `GrepRequest`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema`  ·  [L481]

Regex-scan workspace file contents.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `base` | `SandboxRequestBase` | `pub` · `#[serde(flatten)]` |
| `pattern` | `String` | `pub` |
| `path` | `Option<String>` | `pub` · `#[serde(default, skip_serializing_if = "Option::is_none")]` |
| `glob_filter` | `Option<String>` | `pub` · `#[serde(default, skip_serializing_if = "Option::is_none")]` |
| `output_mode` | `String` | `pub` · `#[serde(default = "default_output_mode")]` |
| `head_limit` | `Option<u32>` | `pub` · `#[serde(default, skip_serializing_if = "Option::is_none")]` |
| `offset` | `u32` | `pub` · `#[serde(default)]` |
| `case_insensitive` | `bool` | `pub` · `#[serde(default)]` |
| `line_numbers` | `bool` | `pub` · `#[serde(default)]` |
| `multiline` | `bool` | `pub` · `#[serde(default)]` |

#### `GrepResult`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema`  ·  [L515]

Result of `GrepRequest`.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `base` | `SandboxResultBase` | `pub` · `#[serde(flatten)]` |
| `output_mode` | `String` | `pub` · `#[serde(default = "default_output_mode")]` |
| `filenames` | `Vec<String>` | `pub` · `#[serde(default)]` |
| `content` | `String` | `pub` · `#[serde(default)]` |
| `num_files` | `u32` | `pub` · `#[serde(default)]` |
| `num_lines` | `u32` | `pub` · `#[serde(default)]` |
| `num_matches` | `u32` | `pub` · `#[serde(default)]` |
| `applied_limit` | `Option<u32>` | `pub` · `#[serde(default, skip_serializing_if = "Option::is_none")]` |
| `applied_offset` | `u32` | `pub` · `#[serde(default)]` |
| `truncated` | `bool` | `pub` · `#[serde(default)]` |

#### `LifecycleError`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema`  ·  [L550]

Categorical isolated-workspace lifecycle error.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `kind` | `String` | `pub` |
| `message` | `String` | `pub` · `#[serde(default)]` |
| `details` | `BTreeMap<String, String>` | `pub` · `#[serde(default)]` |

#### `LifecycleResultBase`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema`  ·  [L564]

Base result for isolated-workspace lifecycle operations (distinct from OCC conflicts).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `success` | `bool` | `pub` · `#[serde(default = "default_true")]` |
| `timings` | `BTreeMap<String, f64>` | `pub` · `#[serde(default)]` |
| `error` | `Option<LifecycleError>` | `pub` · `#[serde(default, skip_serializing_if = "Option::is_none")]` |

#### `EnterIsolatedWorkspaceRequest`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema`  ·  [L578]

Enter an isolated workspace.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `base` | `SandboxRequestBase` | `pub` · `#[serde(flatten)]` |
| `layer_stack_root` | `String` | `pub` |

#### `EnterIsolatedWorkspaceResult`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema`  ·  [L588]

Result of `EnterIsolatedWorkspaceRequest`.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `base` | `LifecycleResultBase` | `pub` · `#[serde(flatten)]` |
| `manifest_version` | `String` | `pub` · `#[serde(default)]` |
| `manifest_root_hash` | `String` | `pub` · `#[serde(default)]` |

#### `ExitIsolatedWorkspaceRequest`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema`  ·  [L602]

Exit an isolated workspace.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `base` | `SandboxRequestBase` | `pub` · `#[serde(flatten)]` |
| `grace_s` | `f64` | `pub` · `#[serde(default = "default_grace_s")]` |

#### `ExitIsolatedWorkspaceResult`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema`  ·  [L613]

Result of `ExitIsolatedWorkspaceRequest`.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `base` | `LifecycleResultBase` | `pub` · `#[serde(flatten)]` |
| `evicted_upperdir_bytes` | `u64` | `pub` · `#[serde(default)]` |
| `lifetime_s` | `f64` | `pub` · `#[serde(default)]` |
| `phases_ms` | `BTreeMap<String, f64>` | `pub` · `#[serde(default)]` |

#### `ToolCallRequest`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema`  ·  [L634]

One tool invocation routed through a workspace pipeline; `invocation_id` is the typed `InvocationId` parsed fallibly at the boundary.

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `invocation_id` | `InvocationId` | `pub` |
| `agent_id` | `String` | `pub` |
| `verb` | `String` | `pub` |
| `intent` | `Intent` | `pub` |
| `args` | `JsonObject` | `pub` |
| `background` | `bool` | `pub` · `#[serde(default)]` |

<details><summary>Methods (2)</summary>

`to_payload`, `from_payload`

</details>

---

## `eos-sandbox-api/src/ops.rs`

#### `DaemonOp`  ·  _enum_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize`  ·  #[non_exhaustive]  ·  [L17]

One sandbox daemon operation; serializes (via per-variant `#[serde(rename)]`) to its verbatim daemon wire string.

**Variants**: `ReadFile`, `WriteFile`, `EditFile`, `ExecCommand`, `ExecStdin`, `CommandCancel`, `CommandCollectCompleted`, `CommandSessionCount`, `InvocationCancel`, `InvocationHeartbeat`, `InflightCount`, `IsolatedWorkspaceEnter`, `IsolatedWorkspaceExit`, `IsolatedWorkspaceStatus`, `Glob`, `Grep`, `AuditPull`, `AuditSnapshot`, `AuditResetFloor` (each carries a `#[serde(rename = "api…")]` wire string)

<details><summary>Methods (1)</summary>

`as_wire`

</details>

---

## `eos-sandbox-api/src/tool_api/parse.rs`

#### `GuardedCommon`  ·  _struct_  ·  private  ·  [L351]

The common guarded-mutation fields shared by the write/edit/shell result parsers (a parse-time intermediate, not a wire type).

**Fields**

| name | type | vis / attrs |
|------|------|-------------|
| `base` | `SandboxResultBase` |  |
| `changed_path_kinds` | `BTreeMap<String, String>` |  |
| `mutation_source` | `String` |  |
| `status` | `String` |  |

---

## `eos-sandbox-api/src/transport.rs`

#### `SandboxTransport`  ·  _trait_  ·  bases: `Send + Sync`  ·  async  ·  [L22]

The single sandbox RPC boundary (DIP seam); implemented downstream by `eos-sandbox-host` and injected as `Arc<dyn SandboxTransport>`.

**Trait items**:
- `async fn call(&self, sandbox_id: &SandboxId, op: DaemonOp, payload: JsonObject, timeout_s: u32) -> Result<JsonObject, SandboxApiError>;`
