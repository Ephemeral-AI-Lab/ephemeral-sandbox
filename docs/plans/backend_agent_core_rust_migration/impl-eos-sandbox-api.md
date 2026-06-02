# impl-eos-sandbox-api — host-facing sandbox protocol: transport seam, daemon op constants, typed request/result wrappers

> Owning crate in the agent-core workspace. Conforms to ./spec-conventions.md.
> Plan section: ../backend_agent_core_rust_migration_PLAN.md §11 (and the
> cross-cutting "Sandbox" bullets around §1135-1142).

## 1. Purpose & Responsibility (SRP)

`eos-sandbox-api` is the **boundary contract** agent-core uses to call the
existing sandbox daemon. Its single responsibility is: define the typed
request/result DTOs for each daemon operation, the daemon op-name constants, the
`SandboxTransport` async trait seam, and the pure `tool_api` helpers that
(1) build a daemon payload from a typed request, (2) call a `SandboxTransport`,
and (3) parse the daemon JSON envelope into a typed result.

This crate must **NOT**:

- Reimplement daemon internals (LayerStack / OCC / overlay / plugin runtime /
  command-session lifecycle) — those stay daemon-side (anchor §2 non-goal).
- Implement the daemon-backed transport. `DaemonSandboxTransport` and
  protocol-version stamping live in `eos-sandbox-host` (anchor §5).
- Emit audit events. Audit wrapping is **not** this crate's job (see §3, §9):
  `eos-sandbox-api` does not depend on `eos-audit`, and the per-tool executor in
  `eos-tools` performs audit wrapping around the pure call.
- Own sandbox provider selection, lifecycle, or provisioning (`eos-sandbox-host`).
- Spawn a Tokio runtime or own background tasks (runtime-agnostic; see §7).

## 2. Dependencies

- **Upstream crates (depends on):**
  - `eos-types` — ID newtypes (`SandboxId`, `AgentRunId`, `ToolUseId`,
    `InvocationId`), `JsonObject`, `CoreError`. See impl-eos-types.md §6.1.
- **Downstream consumers (used by):**
  - `eos-tools` — per-tool executors call `tool_api` helpers and wrap them in
    audit (anchor §5).
  - `eos-sandbox-host` — implements `SandboxTransport` (`DaemonSandboxTransport`)
    and re-exports op constants for the daemon client (anchor §5).
- **External crates** (pinned via `[workspace.dependencies]`, inherited with
  `package.workspace = true` — `proj-workspace-deps`):

  | Crate | Justification | rust-skills |
  |---|---|---|
  | `serde` (derive) | all DTOs are wire types; `Serialize`/`Deserialize` for the daemon JSON envelope | anchor §9 (`api-common-traits`) |
  | `serde_json` | `JsonObject` payload assembly and envelope field access; `serde_json::Value` for the untyped `args`/`error` maps | anchor §3 |
  | `schemars` | `JsonSchema` on request DTOs for Phase 0 schema-snapshot parity vs the Python Pydantic schemas | anchor §11 |
  | `async-trait` | `SandboxTransport` is a `dyn` seam (`Arc<dyn SandboxTransport>` at the composition root); native async-fn-in-trait is not yet `dyn`-safe (anchor §6) | `async-tokio-runtime` |
  | `thiserror` | one library error enum `SandboxApiError` (no `Box<dyn Error>`) | `err-thiserror-lib` |

  No `tokio`, no `futures`: this crate has no runtime-owned state and no streams
  (anchor §7). No `eos-audit` dependency (see §1, §3, §9).

## 3. Scope & Source Mapping

| Python source | Rust target | What moves / what is dropped or relocated |
|---|---|---|
| `sandbox/api/transport.py` (`SandboxTransport` Protocol, `DAEMON_OP_*` constants, `call_sandbox_daemon`) | `transport.rs` (trait) + `ops.rs` (constants) | Trait + op constants move here. **`DaemonSandboxTransport` and `with_daemon_protocol_version`/`_eos_daemon_protocol_version` stamping relocate to `eos-sandbox-host`** (it owns the daemon client — anchor §5). |
| `sandbox/shared/models.py` (`SandboxCaller`, `SandboxRequestBase`, `SandboxResultBase`, `ToolCallRequest`, file/edit/command/search/isolated DTOs, `Intent`, `ConflictInfo`, `GuardedResultBase`, `LifecycleError`) | `models.rs` | All DTOs + `Intent` move here. `RawExecResult` is **dropped** (raw provider exec is a host concern, not routed via daemon op). `SandboxCaller.tool_name` is **removed** (GC-01). |
| `sandbox/api/tool/{read,write,edit,glob,grep}.py`, `sandbox/api/tool/command.py` | `tool_api/{read,write,edit,glob,grep,command}.rs` | The build-payload → call-transport → parse-result core moves here as **pure** functions (no `audit_sink` arg). The deleted public shell API is not ported. The `run_audited_operation` wrapper + `SandboxOperation`→`AuditEvent` translation (`sandbox/api/tool/_operation_audit.py`, `sandbox/audit/translation.py`) **relocate to `eos-tools`** (anchor §5: sandbox-api is absent from the eos-audit consumer list). |
| `sandbox/api/tool/_daemon_response_parsing.py` (`parse_*_result`, `daemon_request_identity_fields`, `strict_int_from_daemon_field`, `parse_*_field`) | `tool_api/parse.rs` | Pure envelope parsers move here verbatim in behavior. Note: `parse.rs` holds **both** the request-identity payload **builder** (`daemon_request_identity_fields`, which constructs the outbound envelope identity, not a response decoder) and the response parsers. |
| `sandbox/api/timeouts.py` (`*_TIMEOUT_S`, command dispatch timeout) | `timeouts.rs` | Constants + `exec_dispatch_timeout` move here. |
| `sandbox/api/daemon_invocations.py` (`cancel`, `heartbeat`, `inflight_count`, `command_session_count`, `isolated_active`) | `tool_api/control.rs` | Control RPCs move here (pure transport calls). The `_CONTROL_TIMEOUT_S=15` constant lives here (not `timeouts.py`); port it as `control.rs` `CONTROL_TIMEOUT_S=15`. `inflight_count`/`command_session_count` return a count integer (`u32`) defaulting to `0` (`int(response.get("count") or 0)`); `isolated_active` returns `bool`. |
| `sandbox/shared/clock.py` (`monotonic_now`, `normalize_timing_map`) | `tool_api/parse.rs` (timing normalization) | Only `normalize_timing_map` shape is needed for parsing `timings`; `monotonic_now` dispatch-timing is recorded by the **caller** (`eos-tools`), not here. |

**In-scope:** request/result DTOs; `Intent`; daemon op constants (typed
`DaemonOp`); `SandboxTransport` trait; pure `tool_api` helpers; envelope
parsers; timeout policy; host-facing isolated-workspace enter/exit schemas.

**Out-of-scope:** the daemon-backed transport impl, protocol-version stamping,
audit wrapping, provider selection/lifecycle, provisioning, daemon internals,
isolated-workspace namespace implementation (anchor §2; plan §11 gap closeout 3).

## 4. File & Module Layout

```
src/
  lib.rs            // pub use re-exports of the public surface (proj-pub-use-reexport)
  models.rs         // SandboxCaller, *RequestBase/*ResultBase, ToolCallRequest, all verb DTOs, Intent, ConflictInfo
  ops.rs            // DaemonOp typed constants (wire-string serialization)
  transport.rs      // SandboxTransport #[async_trait] seam + DaemonEnvelope error decode
  timeouts.rs       // *_TIMEOUT_S consts + exec_dispatch_timeout()
  error.rs          // SandboxApiError (thiserror)
  tool_api/
    mod.rs          // pub(crate) re-exports of helper fns
    parse.rs        // pure envelope parsers + identity-field payload builder + timing normalization
    read.rs         // read_file(transport, sandbox_id, &ReadFileRequest) -> Result<ReadFileResult>
    write.rs        // write_file(...)
    edit.rs         // edit_file(...)  (+ edit-conflict result mapping)
    glob.rs         // glob(...)
    grep.rs         // grep(...)
    command.rs      // exec_command / exec_stdin (+ write_stdin alias) / cancel_command_session / collect_command_completions
    control.rs      // cancel / heartbeat / inflight_count / command_session_count / isolated_active
```

`lib.rs` re-exports DTOs, `Intent`, `DaemonOp`, `SandboxTransport`,
`SandboxApiError`, and the `tool_api` helper fns. Parsers and the
identity-field builder are `pub(crate)` (`proj-pub-crate-internal`).

## 5. Contracts Owned Here

Per anchor §5, this crate owns: `SandboxCaller`, `SandboxRequestBase`/
`SandboxResultBase`, `ToolCallRequest`, daemon op constants, the
`SandboxTransport` trait, and the typed `tool_api`. Full field specs in §6.

### 5.1 `SandboxTransport` (the DIP seam — anchor §6)

```rust
/// One sandbox RPC boundary. Implemented in eos-sandbox-host by the daemon client.
#[async_trait::async_trait]
pub trait SandboxTransport: Send + Sync {
    /// Call one sandbox RPC. The implementor stamps a wire-level protocol
    /// version and reuses any `invocation_id` already present in `payload`
    /// for engine/daemon in-flight correlation.
    async fn call(
        &self,
        sandbox_id: &SandboxId,
        op: DaemonOp,
        payload: JsonObject,
        timeout_s: u32,
    ) -> Result<JsonObject, SandboxApiError>;
}
```

- **Object-safety / async:** `#[async_trait]` because it is used as
  `Arc<dyn SandboxTransport>` at the composition root (heterogeneous: real
  daemon client vs. test double). Not sealed — `eos-sandbox-host` is an external
  implementor by design (do not apply `api-sealed-trait` here).
- The trait is the *only* abstraction this crate introduces beyond DTOs.
  Implementors: `DaemonSandboxTransport` (eos-sandbox-host), in-memory mock
  (tests) — `test-mock-traits`.

### 5.2 `DaemonOp` (typed op constants — anchor §10 / `type-no-stringly`)

A `#[non_exhaustive]` enum whose serialized form is the exact legacy wire string
(`api.v1.read_file`, …). This replaces 18 bare `&str` constants with one typed
surface while preserving protocol compatibility (GC-02). See §6.4.

### Contracts merely USED (referenced, not redefined here)

- `SandboxId`, `AgentRunId`, `ToolUseId`, `InvocationId`, `JsonObject`,
  `CoreError` — owned by `eos-types`; see impl-eos-types.md §6.1.
- `AuditEvent`, `AuditSink` (generic primitives) — owned by `eos-audit`; the
  `SandboxOperation`→`AuditEvent` translation **and** the wrapper that uses them
  both live in `eos-tools` (`SandboxOperation` is a sandbox-domain type and
  `eos-audit` stays sandbox-agnostic — matches §3). **Not** referenced by this
  crate's code (no dependency edge).

## 6. Types, Fields & Schemas

All DTOs derive `Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema`
(`api-common-traits`, anchor §9). Request DTOs additionally derive `Default`
where the Python dataclass had all-defaulted fields beyond the required ones.
Public structs/enums that may grow gain `#[non_exhaustive]` with a constructor
(`api-non-exhaustive`). Source of truth for every row is
`backend/src/sandbox/shared/models.py` unless noted.

### 6.1 `SandboxCaller` (caller identity threaded onto every audit-aware request)

| Field | Rust type | serde/schemars notes | source of truth |
|---|---|---|---|
| `agent_id` | `String` | required; eos-types owns **no** `AgentId` newtype, stays `String` (advisor). Production value is derived from `agent_run_id` (`tools/sandbox/_lib/tool_context.py:17`, eos-tools source: `agent_run_id.strip() or agent_name`), so it frequently equals the `agent_run_id` string — see §6.1 note for the AC-04 fixture | models.py:27 |
| `run_id` | `String` | required-empty (see §8 invariant) | models.py:28 |
| `agent_run_id` | `String` | required-empty raw wire field; typed accessor returns `Option<AgentRunId>` after non-empty validation | models.py:29 |
| `task_id` | `String` | required-empty raw wire field; typed accessor returns `Option<TaskId>` after non-empty validation | models.py:30 |
| `request_id` | `String` | optional-empty raw wire field; omitted when empty in `audit_fields`; typed accessor returns `Option<RequestId>` | models.py:31 |
| `attempt_id` | `String` | optional-empty raw wire field; typed accessor returns `Option<AttemptId>` | models.py:32 |
| `workflow_id` | `String` | optional-empty raw wire field; typed accessor returns `Option<WorkflowId>` | models.py:33 |
| ~~`tool_name`~~ | — | **REMOVED** (GC-01); was almost always empty | models.py:35 |
| `tool_id` | `Option<ToolUseId>` | `None` when empty; production factory populates it (`tools/sandbox/_lib/tool_context.py:26`, eos-tools source) | models.py:36 |

`SandboxCaller` keeps a method mirroring `audit_fields()` for the daemon
envelope's nested `caller` block — but it is a **payload-shape** method (returns a
`JsonObject`), not audit logic. `identity_block()` is **only** the nested
`caller` sub-block. It is distinct from the full envelope identity built by
`daemon_request_identity_fields` (§8.1, `parse.rs`), which wraps a top-level
`agent_id` (duplicating `caller.agent_id` at the envelope root), the nested
`caller` block (= `identity_block()`), and an optional top-level `invocation_id`
(only when present). The production caller's `agent_id` is itself derived from
`agent_run_id` (`tools/sandbox/_lib/tool_context.py:17`, eos-tools source:
`agent_id = agent_run_id.strip() or agent_name`),
so in production `agent_id` and `agent_run_id` frequently hold the **same** string
while remaining distinct fields. This caller-factory derivation lives **downstream**
in the eos-tools source tree, confirming that no `AgentId` derivation logic is ported
into `eos-sandbox-api` (the crate stores the resolved `String` only):

```rust
impl SandboxCaller {
    /// Daemon-facing nested `caller` block (NOT the full envelope identity).
    /// The four required ids (agent_id, run_id, agent_run_id, task_id) are
    /// always present even when empty; optional ids are omitted when empty.
    /// (models.py audit_fields)
    pub(crate) fn identity_block(&self) -> JsonObject { /* ... */ }
}
```

> Note on typed-ID empty strings: eos-types IDs are non-empty by construction
> (impl-eos-types.md §5.2). `SandboxCaller` is therefore a raw daemon wire DTO
> for the required-empty compatibility fields; use typed accessors only when a
> field is non-empty and has passed `TryFrom`/`FromStr` validation.

### 6.2 `SandboxRequestBase` / `SandboxResultBase`

`SandboxRequestBase`: `caller: SandboxCaller`, `description: String` (default
`""`), `invocation_id: Option<InvocationId>` (omitted when absent — matches
`daemon_request_identity_fields`, which only adds `invocation_id` when truthy).
Rust models composition by embedding these as fields on each verb request/result
(no inheritance); a `description_or(fallback: &str)` helper mirrors
`default_description`.

`SandboxResultBase`:

| Field | Rust type | serde/schemars notes |
|---|---|---|
| `success` | `bool` | **decode default `false` (fail-closed)** — a daemon envelope missing `success` decodes to `false`, matching `parse_*_result` (`response.get("success", False)`); this differs from the dataclass *construction* default of `true` (`#[serde(default)]` would be wrong here — use an explicit `false` decode default) |
| `workspace` | `Workspace` | enum `{ Ephemeral, Isolated }` (`type-enum-states`), `#[serde(rename_all="snake_case")]`, default `Ephemeral`. **Never decoded from the daemon envelope** — no `parse_*_result` reads `workspace`; it is always the construction default `Ephemeral` on the parse path (see §8 invariant 9) |
| `timings` | `JsonObject` decoded to `BTreeMap<String, f64>` | from `normalize_timing_map`; deterministic key order |
| `conflict` | `Option<ConflictInfo>` | |
| `conflict_reason` | `Option<String>` | |
| `changed_paths` | `Vec<String>` | tuple/list in Python → `Vec` |
| `error` | `Option<JsonObject>` | untyped daemon error payload |

`GuardedResultBase` extends with `changed_path_kinds: BTreeMap<String,String>`,
`mutation_source: String`, `status: String`. (Rust: embed `SandboxResultBase`
plus these fields per result struct; no class hierarchy.)

### 6.3 Verb request/result DTOs (real field names + types)

| Type | Fields (Rust types) |
|---|---|
| `ReadFileRequest` | base + `path: String` |
| `ReadFileResult` | base + `content: String`, `exists: bool` (**decode default `false`, fail-closed** — `parse_read_file_result` uses `response.get("exists", False)`, distinct from the construction default `true`), `encoding: String` (default `"utf-8"`) |
| `WriteFileRequest` | base + `path: String`, `content: String`, `overwrite: bool` (default `true`) |
| `WriteFileResult` | = `GuardedResultBase` |
| `SearchReplaceEdit` | `old_text: String`, `new_text: String`, `replace_all: bool` (default `false`) |
| `EditFileRequest` | base + `path: String`, `edits: Vec<SearchReplaceEdit>` |
| `EditFileResult` | guarded + `applied_edits: u32` (default `0`) |
| `CommandOutput` | `stdout: String`, `stderr: String` |
| `ExecCommandRequest` | base + `cmd: String`, `yield_time_ms: Option<u32>`, `timeout: Option<u32>`, `max_output_tokens: Option<u32>` |
| `ExecCommandResult` | base + `status: String`, `exit_code: Option<i32>`, `output: CommandOutput`, `command_session_id: Option<String>`, `changed_path_kinds: BTreeMap<String,String>`, `mutation_source: String` |
| `ExecStdinRequest` | base + `command_session_id: String`, `chars: String`, `yield_time_ms: Option<u32>`, `max_output_tokens: Option<u32>`; `CommandSessionWriteRequest` remains a Rust type alias for the model-facing `write_stdin` tool |
| `CommandSessionCancelRequest` | base + `command_session_id: String` |
| `GlobRequest` | base + `pattern: String`, `path: Option<String>` |
| `GlobResult` | base + `filenames: Vec<String>`, `num_files: u32`, `truncated: bool` |
| `GrepRequest` | base + `pattern: String`, `path: Option<String>`, `glob_filter: Option<String>`, `output_mode: String` (default `"files_with_matches"`), `head_limit: Option<u32>`, `offset: u32`, `case_insensitive: bool`, `line_numbers: bool`, `multiline: bool` |
| `GrepResult` | base + `output_mode`, `filenames: Vec<String>`, `content: String`, `num_files: u32`, `num_lines: u32`, `num_matches: u32`, `applied_limit: Option<u32>`, `applied_offset: u32`, `truncated: bool` |
| `ConflictInfo` | `reason: String`, `conflict_file: Option<String>`, `message: String`; ctors `rejected{reason,message}`, `overlap{path,message}` |
| `ToolCallRequest` | `invocation_id: InvocationId`, `agent_id: String`, `verb: String`, `intent: Intent`, `args: JsonObject`, `background: bool`; `to_payload()`/`from_payload()` mirror models.py |

Non-DTO return types (helpers without a typed result struct):
`collect_command_completions` returns `Vec<JsonObject>` (the only verb returning
raw maps) at `exec_dispatch_timeout(None)=90`, not a control timeout; the count
RPCs (`inflight_count`/`command_session_count`) return `u32` defaulting to `0`.

Isolated-workspace **host-facing** schemas (kept; internal namespace impl
excluded — GC-03):

| Type | Fields |
|---|---|
| `LifecycleError` | `kind: String`, `message: String`, `details: BTreeMap<String,String>` |
| `LifecycleResultBase` | `success: bool` (default `true`), `timings: BTreeMap<String,f64>`, `error: Option<LifecycleError>` |
| `EnterIsolatedWorkspaceRequest` | base + `layer_stack_root: String` |
| `EnterIsolatedWorkspaceResult` | lifecycle + `manifest_version: String`, `manifest_root_hash: String` |
| `ExitIsolatedWorkspaceRequest` | base + `grace_s: f64` (default `5.0`) |
| `ExitIsolatedWorkspaceResult` | lifecycle + `evicted_upperdir_bytes: u64`, `lifetime_s: f64`, `phases_ms: BTreeMap<String,f64>` |

### 6.4 `Intent` and `DaemonOp`

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Intent { ReadOnly, WriteAllowed, Lifecycle } // models.py Intent

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum DaemonOp {
    #[serde(rename = "api.v1.read_file")]   ReadFile,
    #[serde(rename = "api.v1.write_file")]  WriteFile,
    #[serde(rename = "api.v1.edit_file")]   EditFile,
    #[serde(rename = "api.v1.exec_command")] ExecCommand,
    #[serde(rename = "api.v1.exec_stdin")] ExecStdin,
    #[serde(rename = "api.v1.command.cancel")]           CommandCancel,
    #[serde(rename = "api.v1.command.collect_completed")] CommandCollectCompleted,
    #[serde(rename = "api.v1.command_session_count")]    CommandSessionCount,
    #[serde(rename = "api.v1.cancel")]      InvocationCancel,
    #[serde(rename = "api.v1.heartbeat")]   InvocationHeartbeat,
    #[serde(rename = "api.v1.inflight_count")]            InflightCount,
    #[serde(rename = "api.isolated_workspace.enter")]     IsolatedWorkspaceEnter,
    #[serde(rename = "api.isolated_workspace.exit")]      IsolatedWorkspaceExit,
    #[serde(rename = "api.isolated_workspace.status")]   IsolatedWorkspaceStatus,
    #[serde(rename = "api.v1.glob")]        Glob,
    #[serde(rename = "api.v1.grep")]        Grep,
    #[serde(rename = "api.audit.pull")]     AuditPull,
    #[serde(rename = "api.audit.snapshot")] AuditSnapshot,
    #[serde(rename = "api.audit.reset_floor")] AuditResetFloor,
}
```

Every wire string above is verbatim from the current daemon transport contract.
User-facing tool naming in `eos-tools`/specs uses `exec_command` /
`write_stdin`; `eos-sandbox-api` maps those helpers to `api.v1.exec_command` and
`api.v1.exec_stdin`. There is no `DaemonOp::Shell` compatibility variant.

## 7. Concurrency & State Ownership

This crate is **effectively stateless** (anchor §7):

- **Runtime-agnostic.** No crate-owned Tokio runtime; helper fns are
  `async fn` taking `&dyn SandboxTransport` / `&self`. They never spawn tasks,
  never own a `JoinSet`/`mpsc`/`watch`/`CancellationToken` (those belong to the
  eos-engine background supervisor, not here).
- **No shared mutable state, no locks.** Nothing is held across `.await`
  (`anti-lock-across-await` is trivially satisfied — there are no locks).
- **Shared transport.** The concrete transport is `Arc<dyn SandboxTransport>`,
  constructed once in `eos-runtime` and cloned cheaply (`own-arc-shared`);
  `&self` calls are `Send + Sync`.
- **DTOs are owned values** moved into payload builders and out of parsers; no
  interior mutability.

## 8. Behavior & Invariants

Semantics that must be preserved (cite Python source):

1. **Required-empty identity fields.** `daemon_request_identity_fields` builds the
   full envelope identity: a **top-level** `agent_id`, a nested `caller` block, and
   (invariant 2) an optional top-level `invocation_id`. `SandboxCaller.audit_fields()`
   (= `identity_block()`) is only the nested `caller` block and always includes
   `agent_id, run_id, agent_run_id, task_id` (even empty), omitting the rest when
   empty (models.py:39-46). `identity_block()` must reproduce the `caller` block
   exactly; the envelope builder must also emit the top-level `agent_id` and
   optional `invocation_id` — both pinned by a daemon-envelope snapshot test (AC-04).
2. **`invocation_id` only when present.** Added to the payload only when
   non-empty (parse.rs `daemon_request_identity_fields`). Modeled as
   `Option<InvocationId>`; skipped via `#[serde(skip_serializing_if)]`.
3. **`success` derivation for commands.** `ExecCommandResult.success` is
   `status not in {"error","timed_out"}` (command.py:152). Must be computed in
   the Rust parser, not read from a daemon `success` field.
4. **Conflict-to-result mapping (recoverable).** `edit_file` translates a
   detected edit conflict exception into a *successful return* of a
   `EditFileResult{ success:false, status, conflict, conflict_reason }` rather
   than an error (edit.py:56-68). In Rust, the edit conflict classifier maps a
   transport error into `Ok(result)` for `edit_file`; all other transport errors
   propagate as `Err(SandboxApiError)`.
5. **No shell compatibility API.** The deleted `shell` API is not ported:
   no `ShellRequest`, no `ShellResult`, no `DaemonOp::Shell`, and no `tool_api::shell`.
   Command execution is exclusively `exec_command`; command-session stdin is
   `api.v1.exec_stdin` (with `write_stdin` kept only as the model-facing helper
   alias used by `eos-tools`).
6. **Timeout policy.** Per-verb file/search constants are `READ_FILE_TIMEOUT_S=60`,
   `WRITE_FILE_TIMEOUT_S=60`, `EDIT_FILE_TIMEOUT_S=20`, `GLOB_TIMEOUT_S=60`,
   `GREP_TIMEOUT_S=60` (timeouts.py). There is **no** standalone exec
   per-verb constant: `exec_command` / `exec_stdin` use
   `exec_dispatch_timeout(t) = t.unwrap_or(EXEC_DEFAULT_COMMAND_TIMEOUT_S=60) + EXEC_DISPATCH_GRACE_S=30`.
   Control RPCs use `CONTROL_TIMEOUT_S=15` (daemon_invocations.py `_CONTROL_TIMEOUT_S`).
   `timeouts.rs` ports `EXEC_DEFAULT_COMMAND_TIMEOUT_S` and `EXEC_DISPATCH_GRACE_S`
   as named consts (not magic 60/30).
7. **Strict int decode.** `strict_int_from_daemon_field` rejects bool-as-int.
   Rust gets this for free via typed `u32`/`i32` `serde` decode, but the parser
   must default missing numeric fields (e.g. `num_files` → 0) to match
   `default=` behavior — pinned by parser unit tests (AC-03).
8. **Timing key normalization.** `timings` keys are normalized to plain strings
   (clock.py `normalize_timing_map`); enum-prefixed keys (`TimingKey.*`) collapse
   to their value. Port this normalization in `parse.rs`. This applies to the
   read-only verbs too: `parse_read_file/glob/grep_result` each populate `timings`
   via `parse_timing_map_field` (`normalize_timing_map`), and the per-verb parser
   tests (AC-03) assert `timings` is decoded for read/glob/grep, not just for the
   guarded/exec verbs.
9. **Result decode is hand-written, not blanket serde.** Results are produced by
   the hand-written `parse_*_result` functions (`parse_read_file/glob/grep_result`,
   `parse_guarded_mutation_result`, exec `_parse_exec_command_result`),
   which do **not** read `workspace` from the daemon envelope — `workspace` is always
   the construction default `Ephemeral` on the parse path. AC-01 schema-parity
   (`schemars` on **request** DTOs) is the wire-shape contract, **not** the
   result-decode contract: any result `Deserialize` derives present for round-tripping
   must **not** be wired into the daemon-response decode path for fields Python ignores
   (notably `workspace`). The Rust port replicates the per-verb parsers rather than
   relying on raw serde decode of the envelope into the result struct.
10. **Path-collection filtering.** `parse_path_tuple_field` (used for
   `changed_paths`, `filenames`, `warnings`) drops empty/whitespace-only entries
   (`if str(path or "").strip()`), and `parse_changed_path_kinds_field` drops pairs
   with a blank key **or** blank value. A naive serde decode of these fields would
   retain empty strings / blank pairs; the Rust `parse_*_result` fns must replicate
   the filter (pinned by AC-03).

Subtle risks called out by the plan: keep public command/session terminology
consistent across the model-facing `write_stdin` helper and the daemon-facing
`api.v1.exec_stdin` op (GC-02); the grep prompt contract (`re`-style regex) is
**not** owned here — it is a tool-spec concern in `eos-tools` (plan §1141) — but
`GrepRequest` fields must round-trip unchanged.

## 9. SOLID & Principles Applied

- **DIP:** `SandboxTransport` is the inversion seam (anchor §6). This crate
  declares the trait; `eos-sandbox-host` implements the daemon-backed concrete;
  `eos-runtime` injects it. `tool_api` helpers depend on `&dyn SandboxTransport`,
  never on a concrete client.
- **SRP:** transport contract + DTOs + pure helpers only. Audit wrapping is
  deliberately **moved up to `eos-tools`** so this crate does not depend on
  `eos-audit` (anchor §5 ownership map). This is the largest divergence from the
  Python layout and is intentional (see §3).
- **ISP:** one small focused trait (`call`); no god-transport. DTOs are
  per-verb, not a union.
- **LSP:** every `SandboxTransport` impl (daemon, mock) is substitutable through
  identical DTO contracts; `DaemonOp` + `Intent` are exhaustive-decoded enums.
- **OCP:** new daemon ops are added as `DaemonOp` variants
  (`#[non_exhaustive]`), not by editing a stringly dispatch.
- **KISS/YAGNI/DRY:** no builder for simple all-field DTOs; `RawExecResult` and
  the unused `tool_name` field are dropped rather than ported; no new abstraction
  beyond the one seam the plan mandates.
- **Non-goals respected:** no daemon internals, no audit dependency, no runtime,
  no provider selection (anchor §2).

## 10. Gap Closeouts (tracked requirements)

- **GC-sandbox-api-01 — `SandboxCaller.tool_name`.** *Resolution: REMOVE it.*
  Evidence: the only consumer is `sandbox/audit/translation.py:147`
  (`tool_name=_none_if_empty(caller.tool_name) or operation`), and the production
  caller factory `sandbox_caller_from_tool_context`
  (`tools/sandbox/_lib/tool_context.py:18-26`, eos-tools source) never
  sets it — so it is empty in production and the audit node already falls back to
  the operation name. Since audit translation relocates to `eos-tools` (§3), the
  audit `tool_name` is derived from the `SandboxOperation` there; no field needed.
- **GC-sandbox-api-02 — daemon op names vs user-facing terminology.**
  *Resolution:* port the current wire strings verbatim into the typed
  `DaemonOp` enum (§6.4). The old `api.v1.shell` op is removed. User-facing
  command/session naming (`exec_command`, `write_stdin`) is used by `eos-tools`
  tool specs; this crate maps those helpers to `DaemonOp::ExecCommand` and
  `DaemonOp::ExecStdin` (`api.v1.exec_stdin`).
- **GC-sandbox-api-03 — isolated-workspace schemas.** *Resolution:* keep the
  host-facing enter/exit request/result DTOs (`Enter/ExitIsolatedWorkspace*`,
  `LifecycleError`, `LifecycleResultBase`) and the `IsolatedWorkspaceStatus` op;
  exclude all internal namespace/LayerStack implementation (daemon-side).

## 11. Acceptance Criteria

TDD: write each test first and confirm it fails for the right reason, then
implement. Maps to anchor §11 "Tests to Port First" row **eos-sandbox-api/host →
daemon envelope tests**.

- **AC-sandbox-api-01 — DTO schema parity.** `schemars` JSON schema for each
  request DTO matches the crate-owned Phase-2 snapshot of the Python Pydantic/dataclass
  schema (field names, optionality, defaults). *Test:* `tests/schema_snapshot.rs`
  (insta snapshot per DTO).
- **AC-sandbox-api-02 — `SandboxCaller` has no `tool_name`; `tool_id` is
  optional.** Constructing/serializing a caller never emits `tool_name`; an unset
  `tool_id` is omitted, a set one round-trips. *Test:*
  `models::tests::caller_omits_tool_name_and_optional_tool_id`.
- **AC-sandbox-api-03 — envelope parsers.** Each `parse_*_result` decodes a
  representative daemon JSON envelope into the typed result, applies defaults for
  missing numeric fields, derives `ExecCommandResult.success` from `status`, and
  decodes `timings` for the read-only verbs too (`parse_read_file/glob/grep_result`
  populate `timings` via `normalize_timing_map`). **A daemon envelope missing
  `success` decodes to `false`, and an envelope missing `exists` (ReadFileResult)
  decodes to `false` (fail-closed), not `true`.** Each path-collection parser test
  feeds an envelope containing a blank/whitespace-only `changed_paths`/`filenames`/
  `warnings` entry and a blank-key-or-value `changed_path_kinds` pair and asserts
  they are filtered out (invariant 10), so the Rust `parse_*_result` fns replicate
  the filter rather than relying on raw serde decode. *Test:* `tool_api::parse::tests::*`
  (one per verb) plus `parse_missing_success_and_exists_are_false` and
  `parse_drops_blank_paths_and_kinds`.
- **AC-sandbox-api-04 — identity-block + envelope invariant.** `identity_block()`
  (nested `caller` block) always contains the four required ids (even empty) and
  omits empty optional ids; the full envelope builder additionally emits the
  top-level `agent_id` and a top-level `invocation_id` iff non-empty. The snapshot
  fixture uses a realistic caller where `agent_id == agent_run_id`
  (`tools/sandbox/_lib/tool_context.py:17`, eos-tools source)
  to catch accidental newtype coupling while keeping the fields distinct. *Test:*
  `models::tests::identity_block_required_empty_and_optional_omitted` plus an
  envelope test asserting top-level `agent_id` + `caller` + optional `invocation_id`.
- **AC-sandbox-api-05 — conflict mapping.** `edit_file` returns
  `Ok(result{success:false,...})` for a classified conflict transport error and
  `Err` for any other error. *Test:* `tool_api::edit::tests::*` using a mock
  `SandboxTransport`.
- **AC-sandbox-api-06 — `DaemonOp` wire strings.** Every variant serializes to
  the exact legacy string from `transport.py`. *Test:*
  `ops::tests::daemon_op_wire_strings` (table-driven equality).
- **AC-sandbox-api-07 — `exec_dispatch_timeout` + timeout constants.**
  `exec_dispatch_timeout(None)==90`, `exec_dispatch_timeout(Some(t))==t+30`;
  per-verb constants equal the Python values. *Test:*
  `timeouts::tests::dispatch_and_constants`.

## 12. Implementation Checklist

Ordered, small, verifiable (`small-incremental-changes`):

1. Scaffold crate; add `serde`, `serde_json`, `schemars`, `async-trait`,
   `thiserror`, `eos-types` (workspace-inherited). Add workspace lints.
2. `error.rs`: `SandboxApiError` (`thiserror`) — `Transport`, `Decode`,
   variants with `#[source]`.
3. `ops.rs` + test AC-06; `timeouts.rs` + test AC-07.
4. `models.rs`: `Intent`, `Workspace`, `SandboxCaller` (no `tool_name`),
   bases, all verb DTOs, isolated DTOs; `identity_block()`. Tests AC-02, AC-04.
5. `transport.rs`: `#[async_trait] SandboxTransport`; in-test mock transport.
6. `tool_api/parse.rs`: identity builder, timing normalization, all
   `parse_*_result`. Test AC-03.
7. `tool_api/{read,write,glob,grep,command,control}.rs`: pure helpers.
8. `tool_api/edit.rs` with conflict classifier. Test AC-05.
9. `tests/schema_snapshot.rs` against crate-owned Phase-2 snapshots. Test AC-01.
10. `lib.rs` re-exports; `cargo fmt --check` + `clippy -D warnings`.

---
**On completion:** update the Progress Tracker in `./overview.md` for row
`eos-sandbox-api` per spec-conventions.md §13. Do not edit other crates' rows.
