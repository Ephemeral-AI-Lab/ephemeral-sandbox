---
title: MCP, CLI, and daemon HTTP phase plan
tags:
  - ephemeral-os
  - mcp
  - cli
  - http
  - implementation-plan
  - progress-tracking
status: active
updated: 2026-07-10
aliases:
  - MCP CLI HTTP migration tracker
---

# MCP, CLI, and daemon HTTP phase plan

This is the execution and progress-tracking plan for the MCP/CLI/daemon HTTP
boundary migration. It converts [[implementation-spec]] into independently
verifiable phases. The detailed target contracts are [[mcp]], [[cli]], and
[[http]]; the compact cross-boundary catalog is [[operation-contract]].

> [!important] How to use this tracker
> A phase is complete only when every task and every acceptance criterion in
> that phase has evidence. “Compiles locally” is necessary but insufficient.
> Do not start the dependent destructive/removal portion of a later phase
> until its predecessor’s gate is satisfied. Record test commands, commit/PR,
> and exceptions in the phase evidence block when work begins.

## Current progress

| Phase | Status | Dependency | Outcome |
| --- | --- | --- | --- |
| 0. Contract baseline | complete | none | public surface, architecture, and migration constraints documented |
| 1. Catalog and visibility boundary | complete | 0 | one canonical public catalog with correct names/visibility |
| 2. Consolidate the CLI package | complete | 1 | one package, three separately grantable binaries |
| 3. Add the MCP adapter | complete | 1, 2 | one set-configured stdio server with three registrations |
| 4. Replace export HTTP streaming | complete | 1 | `export_changes` uses authenticated RPC chunk paging only |
| 5. Move console operation callers | complete | 2, 4 | console uses gateway RPC for operations and narrow daemon proxies |
| 6. Enforce daemon HTTP allowlist | not started | 4, 5 | only health, forward, and file list remain direct daemon HTTP |
| 7. Release verification and cutover | not started | 1–6 | end-to-end proof, documentation, and release-ready boundary |

Phases 0 through 5 are complete. Phase 6 has not started.

## Fixed decisions and non-negotiable invariants

Every phase must preserve these decisions. A change requires an explicit
architecture decision and an update to all four contracts, not an incidental
implementation shortcut.

| Invariant | Required state after migration |
| --- | --- |
| Operation source of truth | the three existing operation-catalog crates remain the canonical CLI/MCP definitions; no duplicate MCP operation registry |
| Public sets | exactly `management`, `runtime`, `observability`; no fourth all-operations set/server/binary |
| MCP deployment | one `sandbox-mcp` binary, launched with a fixed `--set`; three separate host registrations/grants |
| CLI deployment | one `sandbox-cli` package with three feature-gated binaries: `sandbox-manager-cli`, `sandbox-runtime-cli`, `sandbox-observability-cli` |
| Workspace lifecycle | `create_workspace_session` and `destroy_workspace_session` remain daemon-internal, not public CLI/MCP operations |
| File listing | `file_list` remains direct `POST /files/list` daemon HTTP only, not a CLI command or MCP tool |
| Public squash name | `squash_layerstacks`; internal daemon operation stays singular `squash_layerstack` |
| Export semantics | public name stays `export_changes`; it exports a published-layer delta, not a full workspace |
| Daemon HTTP | exact allowlist: `GET /health`, `/forward/shared/...`, `/forward/isolated=...`, and `POST /files/list`; all other operation paths are `404` |
| Export transport | manager composes `export_layerstack` plus `read_export_chunk` authenticated RPC; no daemon `/export/*` stream remains |
| Console | regular operations use authenticated console `/api/rpc`; daemon HTTP is used only for health, forward, and list proxying |

## Phase 0 — Contract baseline

**Status:** complete

**Purpose:** Freeze the intended public shape before moving code.

### Completed deliverables

- [x] [[operation-contract]] states the cross-boundary operation catalog.
- [x] [[mcp]] defines the three MCP registrations, tools, schemas, routing,
  target implementation structure, and test expectations.
- [x] [[cli]] defines all binaries, operations, arguments, output semantics,
  package structure, and migration locations.
- [x] [[http]] defines the direct daemon HTTP allowlist, forwarding semantics,
  `POST /files/list`, removal matrix, and console impact.
- [x] [[implementation-spec]] contains the detailed file/LOC budget and
  migration rationale.
- [x] Public `squash_layerstacks`, internal workspace lifecycle, HTTP-only
  `file_list`, and published-delta `export_changes` decisions are recorded.

### Acceptance criteria

- [x] There is one unambiguous answer for the public MCP, CLI, and HTTP
  surface across all linked documents.
- [x] All three public sets and their authority boundaries are named.
- [x] `export_workspace` is explicitly rejected as a misleading name for the
  existing delta-only export implementation.
- [x] The route exception for `POST /files/list` is explicit and consistently
  excluded from MCP/CLI.

### Evidence

- Documentation only; no production code has changed in this phase.

## Phase 1 — Catalog and visibility boundary

**Status:** complete

**Depends on:** Phase 0

**Purpose:** Make the existing catalog source accurately describe the final
public operation sets before changing clients or HTTP callers.

### Scope and tasks

- [x] Rename the public manager operation specification from
  `checkpoint_squash` to `squash_layerstacks` in
  `crates/sandbox-manager-operations/src/lib.rs`.
- [x] Update manager dispatch registration in
  `crates/sandbox-manager/src/operation/cli_definition/management_operations.rs`
  to use the renamed public specification while retaining the internal
  `squash_layerstack` daemon request.
- [x] Remove `create_workspace_session`, `destroy_workspace_session`, and
  their `workspace_session` family from the public
  `sandbox-runtime-operations` catalog and public exports.
- [x] Retain daemon workspace lifecycle dispatch in
  `crates/sandbox-runtime/operation/src/cli_definition/workspace_session_operations.rs`,
  but make both entries non-public with `cli: None`.
- [x] Keep `FILE_LIST_SPEC`/the runtime `FILE_LIST` dispatch entry non-public;
  it remains callable only by daemon HTTP `POST /files/list`.
- [x] Move the canonical `snapshot` operation specification from
  `sandbox-manager-operations` into
  `sandbox-observability-operations`; manager imports it only to dispatch
  aggregate snapshot work.
- [x] Update observability catalog command usage/examples to the future
  `sandbox-observability-cli` program name.
- [x] Rename `crates/sandbox-runtime/operation/src/cli_definition/` to
  `operation_adapter/` and update module paths/tests. This is a naming cleanup
  only: daemon-side request parsing/dispatch stays in the runtime engine.
- [x] Add catalog-membership tests that encode the final exact public sets.

### Files expected to change

```text
crates/sandbox-manager-operations/src/lib.rs
crates/sandbox-manager/src/operation/cli_definition/management_operations.rs
crates/sandbox-runtime-operations/src/{lib.rs,workspace_session.rs}
crates/sandbox-observability-operations/src/cli_definition/{mod.rs,snapshot.rs,*.rs}
crates/sandbox-runtime/operation/src/{operation.rs,operation_adapter/**}
relevant catalog/operation tests
```

### Acceptance criteria

- [x] The public management catalog lists exactly
  `create_sandbox`, `destroy_sandbox`, `list_sandboxes`, `inspect_sandbox`,
  `squash_layerstacks`, and `export_changes`; it does not list
  `checkpoint_squash`.
- [x] The public runtime catalog lists exactly `exec_command`,
  `write_command_stdin`, `read_command_lines`, `file_read`, `file_write`,
  `file_edit`, and `file_blame`.
- [x] `file_list`, `create_workspace_session`, and
  `destroy_workspace_session` are absent from all public runtime catalog/help
  projections yet remain daemon-dispatchable where required.
- [x] The public observability catalog lists exactly `snapshot`, `trace`,
  `events`, `cgroup`, and `layerstack`; `snapshot` is not a management tool.
- [x] An automatic `exec_command` session still has
  `publish_then_destroy` lifecycle behaviour; no regression in command
  execution/session finalization tests.
- [x] `cargo test -p sandbox-manager-operations -p sandbox-runtime-operations -p sandbox-observability-operations` passes.
- [x] Focused manager/runtime operation tests pass, including public squash
  forwarding and file-list daemon dispatch.

### Evidence to record

- Commits: `4bda5df70` (catalog/dispatch implementation) and `0217248e5`
  (internal lifecycle E2E routing and caller/documentation cleanup).
  PR: not applicable; this repository's `CLAUDE.md` requires direct work on
  `main`.
- Exact catalog proof:
  `cargo test -p sandbox-manager-operations -p sandbox-runtime-operations -p sandbox-observability-operations`
  passed (7 catalog tests: manager 2, runtime 2, observability 3), including
  exact membership, pairwise disjointness, the public squash rename, canonical
  observability snapshot, and hidden lifecycle/file-list serialization checks.
- Manager proof:
  `cargo test -p sandbox-manager --test manager_core --test manager_router --test manager_export`
  passed 54 tests, including public `squash_layerstacks` forwarding to the
  singular daemon request and aggregate snapshot routing.
- Runtime proof:
  `cargo test -p sandbox-runtime --test service_graph --test workspace_session --test file_operations --test layerstack_publish --test layerstack_squash --test layerstack_export`
  passed 90 tests, including non-public lifecycle dispatch, HTTP-only
  `file_list` dispatch, and automatic publish-then-destroy finalization.
- Public projection proof:
  `cargo test -p sandbox-manager-cli --test smoke -p sandbox-runtime-cli --test smoke`
  passed 24 tests; old squash, lifecycle, and `file_list` commands are rejected.
  `cargo test -p sandbox-console --test console` passed 26 tests against the
  corrected catalog projections.
- E2E harness proof: an inline fake TCP gateway test of
  `core.cli.internal_runtime` passed with exact operation, sandbox scope,
  arguments, authentication token, request id, and newline framing assertions
  (`fake gateway internal lifecycle routing: PASS`).
  `python3 -m compileall -q cli-operation-e2e-live-test` also passed.
- Boundary/quality proof: `cargo fmt --all -- --check` and
  `git diff --check` passed. Targeted runtime-source and current CLI-guide
  searches found no old `cli_definition` module path, duplicate runtime
  catalog export, or public lifecycle invocation; `checkpoint_squash` remains
  only in intentional negative tests.
- Known deviations/waivers: none. No live Docker E2E was required for this
  catalog boundary; the mandatory gateway rebuild remains gated to Phase 7.

## Phase 2 — Consolidate the CLI package

**Status:** complete

**Depends on:** Phase 1

**Purpose:** Replace three CLI packages with one shared package while retaining
three executable and authority boundaries.

### Scope and tasks

- [x] Add `crates/sandbox-cli` as a workspace package and replace workspace
  references to `sandbox-cli-core`, `sandbox-manager-cli`, and
  `sandbox-runtime-cli`.
- [x] Move shared gateway/configuration, output, help, and request-building
  code into `crates/sandbox-cli/src/core/`.
- [x] Move manager client flow to `src/manager.rs` and runtime client flow to
  `src/runtime.rs`; retain their current global flag/scoping behaviour.
- [x] Extract current manager observability flow into
  `src/observability.rs`; it must not remain a manager subcommand.
- [x] Add three thin binary entrypoints in `src/bin/`, each calling only its
  own adapter module’s `run_cli`.
- [x] Add optional Cargo features `manager`, `runtime`, and `observability`.
  Each binary has only its matching `required-features` dependency set.
- [x] Preserve `sandbox-cli::core` as the only shared import for console/MCP;
  do not force them to enable a CLI-set feature.
- [x] Move/split manager/runtime smoke tests and add observability CLI smoke
  coverage.
- [x] Delete old CLI package directories only after consumers compile against
  `sandbox-cli`.

### Files expected to change

```text
Cargo.toml
crates/sandbox-cli/Cargo.toml
crates/sandbox-cli/src/{lib.rs,core/**,manager.rs,runtime.rs,observability.rs,bin/**}
crates/sandbox-cli/tests/{manager.rs,runtime.rs,observability.rs}
crates/sandbox-console/{Cargo.toml,src/**,tests/**}
deleted: crates/sandbox-cli-core/
deleted: crates/sandbox-manager-cli/
deleted: crates/sandbox-runtime-cli/
```

### Acceptance criteria

- [x] `cargo build -p sandbox-cli --features manager` builds only the manager
  executable path; equivalent builds work for `runtime` and `observability`.
- [x] `sandbox-manager-cli help` lists only management operations and has no
  `observability` subcommand.
- [x] `sandbox-runtime-cli --sandbox-id ID help` lists exactly the Phase 1
  runtime catalog and rejects a missing/empty sandbox id before gateway I/O.
- [x] `sandbox-observability-cli help` lists exactly the five observability
  operations; `snapshot` permits an omitted sandbox id while other views do
  not.
- [x] All three binaries preserve JSON-line output/error/exit-code behaviour:
  success `0` stdout, operation failure `1` stderr, usage/config failure `2`
  stderr.
- [x] The console compiles using `sandbox-cli::core` without enabling any
  CLI-set feature.
- [x] Old CLI crates are absent from workspace members and reverse dependency
  checks; there is no duplicate client/request-builder implementation.
- [x] `cargo test -p sandbox-cli --all-features` and affected console tests
  pass.

### Evidence to record

- Commits: `73e5fa612` (package consolidation, consumer migration, wrappers,
  and relocated smoke coverage), `2fa2943fc` (runtime adapter documentation),
  and `7aab1e035` (canonical defaults, complete help/protocol/progress proof,
  and feature-isolated launcher builds). PR: not applicable; this repository's
  `CLAUDE.md` requires direct work on `main`.
- Feature-boundary proof: three clean
  `cargo build -p sandbox-cli --features <manager|runtime|observability>
  --message-format=json` runs each emitted exactly one matching executable:
  `sandbox-manager-cli`, `sandbox-runtime-cli`, and
  `sandbox-observability-cli`. `cargo tree` assertions showed no operation
  catalog for the core-only build and exactly the matching catalog for each
  feature. `cargo metadata` assertions passed for `default = []`, all three
  exact feature dependency lists, each binary's single matching
  `required-features` value, and the console's empty `sandbox-cli` feature
  list. `cargo test -p sandbox-cli --no-default-features` also passed.
- Binary help snapshots: `bin/sandbox-manager-cli help` returned exactly
  `create_sandbox`, `destroy_sandbox`, `list_sandboxes`, `inspect_sandbox`,
  `squash_layerstacks`, and `export_changes`; it has no observability
  subcommand. `bin/sandbox-runtime-cli --sandbox-id ID help` returned exactly
  `exec_command`, `write_command_stdin`, `read_command_lines`, `file_read`,
  `file_write`, `file_edit`, and `file_blame`. `bin/sandbox-observability-cli
  help` returned exactly `snapshot`, `trace`, `events`, `cgroup`, and
  `layerstack`. All three commands exited `0`.
- CLI behavior proof: `cargo test -p sandbox-cli --all-features` passed all 34
  integration tests (manager 14, runtime 10, observability 10). The tests
  derive requiredness, defaults, and examples from the catalogs; reject
  cross-set/internal operations; prove system, sandbox, aggregate snapshot,
  and scoped observability routing against fake gateways; and cover success
  stdout/exit `0`, unchanged operation-error stderr/exit `1`, malformed
  protocol stderr/exit `1`, and usage/config stderr/exit `2`. They also prove
  progress streaming and the compatible create-operation `--progress` form,
  with final JSON kept on stdout.
- Consumer/catalog proof:
  `cargo test -p sandbox-manager-operations -p sandbox-runtime-operations -p
  sandbox-observability-operations` passed all 7 catalog tests, and
  `cargo test -p sandbox-console` passed all 26 console tests. The console
  source and metadata use only `sandbox-cli::core`, with no CLI-set feature.
  `python3 -m compileall -q cli-operation-e2e-live-test` passed for the
  migrated three-binary E2E caller routing.
- Old-package dependency audit: filesystem and full Cargo-metadata assertions
  found no `sandbox-cli-core`, `sandbox-manager-cli`, or
  `sandbox-runtime-cli` package/directory/dependency. Repository symbol
  searches found the sole `GatewayClient` and catalog request builder at
  `crates/sandbox-cli/src/core/{client.rs,request_builder.rs}`.
- Launcher/quality proof: `sh -n` passed for all three wrappers and
  `bin/start-sandbox-docker-gateway`; the launcher's three isolated feature
  builds and `bin/start-sandbox-docker-gateway --help` exited `0`.
  `cargo clippy -p sandbox-cli --all-features --all-targets -- -D warnings`,
  `cargo fmt --all -- --check`, and `git diff --check` passed.
- Known deviations/waivers: none. The mandatory Docker gateway binary rebuild
  remains reserved for Phase 7. Scope note: `73e5fa612` was committed
  concurrently by its author with separately authored configuration E2E
  coverage named in that commit; it was preserved rather than rewritten.

## Phase 3 — Add the MCP adapter

**Status:** complete

**Depends on:** Phases 1 and 2

**Purpose:** Expose the catalog-defined public sets through one fixed-set MCP
stdio server without duplicating operation semantics.

### Scope and tasks

- [x] Add `crates/sandbox-mcp` to the workspace with a maintained Rust MCP
  stdio-server library.
- [x] Implement `--set management|runtime|observability`; reject absent,
  unknown, or caller-supplied per-request set selection.
- [x] Implement only MCP `initialize`, `notifications/initialized`, `ping`,
  `tools/list`, and `tools/call`.
- [x] Select exactly one existing catalog for each process; do not create an
  MCP-specific business-operation list.
- [x] Generate tool descriptions and JSON schemas from `ArgSpec`; add required
  runtime `sandbox_id` and optional observability `snapshot` sandbox selector
  according to [[mcp]].
- [x] Add value-object request construction in `sandbox-cli::core` so MCP and
  CLI share defaults, validation, operation lookup, and scope construction.
- [x] Route management/system, runtime/sandbox, aggregate snapshot, and
  sandbox-scoped observability exactly as specified; keep internal `view`
  hidden.
- [x] Preserve gateway failure `kind`, `message`, and `details` in structured
  MCP tool errors.
- [x] Add fake-gateway stdio contract tests for all three server registrations.

### Files expected to change

```text
Cargo.toml
crates/sandbox-mcp/Cargo.toml
crates/sandbox-mcp/src/{main.rs,lib.rs,config.rs,catalog.rs,schema.rs,server.rs,tools.rs}
crates/sandbox-mcp/tests/server.rs
crates/sandbox-cli/src/core/request_builder.rs
```

### Acceptance criteria

- [x] `sandbox-mcp --set management`, `--set runtime`, and
  `--set observability` each start a valid stdio MCP server; an invalid set
  fails before it reads tool calls.
- [x] `tools/list` outputs the exact Phase 1 operation names for the selected
  set and no names from another set.
- [x] Runtime MCP schemas require `sandbox_id`; observability schemas require
  it except for aggregate `snapshot`; no schema contains request id, gateway
  token, scope, daemon endpoint, `view`, or export token.
- [x] MCP tools omit `file_list`, `create_workspace_session`, and
  `destroy_workspace_session`.
- [x] A fake-gateway `tools/call` proves correct wire request operation and
  scope for management, runtime, aggregate snapshot, and one scoped
  observability view.
- [x] Invalid values are rejected before gateway dispatch and return the
  standard structured error envelope.
- [x] Gateway operation failures preserve original error `kind`, `message`,
  and `details` in MCP tool-error content.
- [x] `cargo test -p sandbox-mcp` passes.

### Evidence to record

- Commits: `7bee540ca` (workspace package, shared value request builder,
  catalog-derived schemas, fixed-set server, and initial stdio coverage) and
  `e15839cda` (structured-content, hidden-schema, and startup-exit hardening).
  PR: not applicable; this repository's `CLAUDE.md` requires direct work on
  `main`.
- MCP compatibility proof: `rmcp = 0.11.0` is pinned as the newest maintained
  release that compiles on the workspace's Rust 1.85 MSRV. A real
  `cargo +1.85.0 check -p sandbox-mcp` passed. Releases from `0.12.0` onward
  require newer Rust or contain syntax unavailable on 1.85. The server
  explicitly advertises MCP `2025-06-18`, the version that defines structured
  tool content, and advertises only the `tools` capability.
- Stdio contract proof: `cargo test -p sandbox-mcp` passed all 7 integration
  tests. Real child processes completed initialize/initialized and ping;
  accepted each fixed set; returned exact schemas and set-local tools; rejected
  absent, unknown, and combined sets with exit `2` while stdin remained open;
  returned `-32601` for completion, prompt, and resource methods; and kept tool
  `content` empty with one object in `structuredContent`.
- `tools/list` fixtures: management returned exactly `create_sandbox`,
  `destroy_sandbox`, `list_sandboxes`, `inspect_sandbox`,
  `squash_layerstacks`, `export_changes`; runtime returned exactly
  `exec_command`, `write_command_stdin`, `read_command_lines`, `file_read`,
  `file_write`, `file_edit`, `file_blame`; observability returned exactly
  `snapshot`, `trace`, `events`, `cgroup`, `layerstack`. Every schema was
  compared with its selected catalog for description, properties,
  requiredness, native defaults, and types, with `additionalProperties: false`
  and a recursive hidden-field assertion. The public observability
  `cgroup.scope` argument is the documented semantic selector, not protocol
  routing scope.
- `tools/call` routing fixtures: management `create_sandbox` became
  `op=create_sandbox`, system scope, with catalog default `count=1`; runtime
  `exec_command` became sandbox scope with caller `sandbox_id` removed from
  args; aggregate `snapshot` remained `op=snapshot`, system scope; scoped
  `trace` became internal `op=get_observability`, sandbox scope, with hidden
  `view=trace`; and `file_edit` retained a native JSON edit array. Each request
  contained a generated UUID and gateway authentication outside the tool
  schema.
- Validation/error proof: missing and wrong-typed selectors, wrong scalar
  types, unknown/hidden arguments, cross-set tools, `file_list`, workspace
  lifecycle, and internal observability names all returned a structured
  `invalid_request` envelope without a fake-gateway connection. Gateway
  operation errors were returned as the unchanged object with original `kind`,
  `message`, and `details`; connection, malformed JSON, and non-object response
  cases returned structured `connection_error` or `protocol_error` envelopes.
- Shared-core/catalog regressions: `cargo test -p sandbox-cli --all-features`
  passed all 42 integration tests, including 8 value-builder tests; `cargo test
  -p sandbox-protocol -p sandbox-manager-operations -p
  sandbox-runtime-operations -p sandbox-observability-operations` passed all
  26 focused tests. This includes the catalog-owned native `JsonArray` type for
  `file_edit.edits`; item-shape validation remains in the canonical runtime
  operation adapter as required by [[mcp]].
- Boundary/quality proof: a Cargo metadata/tree assertion passed for one
  `sandbox-mcp` binary, exact catalog/core dependencies, and no manager,
  runtime, daemon, or observability engine dependency. `cargo +1.85.0 clippy
  -p sandbox-mcp -p sandbox-cli -p sandbox-protocol -p
  sandbox-manager-operations -p sandbox-runtime-operations -p
  sandbox-observability-operations --all-targets --all-features -- -D
  warnings`, `cargo fmt --all -- --check`, and `git diff --check` passed. An
  independent contract/security review reported no remaining actionable
  findings.
- Known deviations/waivers: none. The mandatory Docker gateway binary rebuild
  remains reserved for Phase 7. Scope note: `7bee540ca` was created
  concurrently and also captured separately authored configuration-plan and
  squash-report changes already present in the shared worktree. Repository
  preservation rules prohibit rewriting that concurrent history; the changes
  were retained and are not Phase 3 evidence.

## Phase 4 — Replace export HTTP streaming with gateway RPC chunks

**Status:** complete

**Depends on:** Phase 1

**Purpose:** Remove the manager’s dependency on daemon HTTP export streaming
before removing the endpoint itself.

### Scope and tasks

- [x] Make `export_changes` always start the internal daemon export operation
  and read all bytes via authenticated gateway `read_export_chunk` paging.
- [x] Retain byte limits, expected-total/completeness checks, cleanup, and
  atomic destination application semantics.
- [x] Remove manager HTTP export client logic: daemon HTTP URL construction,
  stream token/header selection, response-head parsing, bounded HTTP socket
  reader, and HTTP stream error path.
- [x] Keep public operation name/output contract unchanged: `export_changes`
  is a published delta, not a full workspace export.
- [x] Add export tests for normal paging, final chunk, missing/truncated chunk,
  archive result, directory result, and atomic failure behaviour.

### Files expected to change

```text
crates/sandbox-manager/src/operation/management/service/impls/export_changes.rs
crates/sandbox-manager/src/export_apply.rs
crates/sandbox-manager/tests/manager_export.rs
related manager operation/export tests
```

### Acceptance criteria

- [x] `export_changes` succeeds with no usable `daemon_http` export endpoint
  and without any HTTP export request.
- [x] Directory export still applies newest-wins/whiteout/opaque published
  delta semantics to the supplied destination.
- [x] Archive export still emits the documented delta result and byte/file
  metadata.
- [x] A missing, malformed, or truncated RPC chunk fails before destination
  replacement/application can leave partial visible output.
- [x] No code in `sandbox-manager` references `/export/`, export stream token
  headers, or daemon HTTP export client helpers.
- [x] Focused manager export tests pass.

### Evidence to record

- Commit/PR: implementation commit `0644fd64b` on the repository's direct
  `main` workflow; no PR was created.
- Commands/results:
  - `cargo test -p sandbox-manager --test manager_export` — 30 passed.
  - `cargo +1.85.0 test -p sandbox-manager --test manager_export` — 30
    passed on the repository MSRV.
  - `cargo test -p sandbox-manager` — all package test targets passed.
  - `cargo test -p sandbox-runtime --test layerstack_export
    export_spools_and_pages_to_eof -- --exact` — passed; the assertion proves
    exact byte reassembly, final-page spool unlink, post-EOF rejection, and
    lease release.
  - `cargo +1.85.0 clippy -p sandbox-manager --all-targets -- -D warnings`,
    `cargo fmt -p sandbox-manager -- --check`, and `git diff --check` — passed.
- Normal/chunk-failure evidence: the fake daemon proves exact multi-page
  offsets and final EOF, successful export with a dead `daemon_http` endpoint
  and ignored legacy stream token, directory newest-wins/whiteout/opaque
  application, raw and decompressed archive results, and documented metadata.
  Table-driven failures cover missing fields, invalid base64, offset/length/
  total mismatch, missing or malformed EOF, empty non-final pages, daemon
  chunk faults, early EOF, overrun, absent EOF, missing/malformed/oversized
  `spool_bytes`, and complete invalid archives. Chunk/start failures occur
  before destination preparation; invalid archives preserve seeded directory
  and archive outputs and leave no temporary sibling.
- HTTP-client removal proof: both
  `rg -n '(/export/|EXPORT_STREAM_|x-eos-export-token|SpoolStreamReader|open_spool_stream|stream_delivery|read_stream_head|parse_stream_head)' crates/sandbox-manager/src`
  and the narrower `daemon_http|SandboxHttpEndpoint|TcpStream|EXPORT_STREAM_`
  search in `export_changes.rs` returned no matches. Positive search finds
  only `export_layerstack` and `read_export_chunk` composition in the manager
  implementation and tests.
- Cleanup/atomicity note: normal final-page reads delete the runtime registry
  entry and spool; failures before EOF retain the sealed spool until the
  existing daemon boot reap, as required by the binding contract. Directory
  mode validates the complete archive before mutation; archive mode retains
  sibling-temp plus atomic-rename replacement.
- Independent contract/security review: pass with no blocking findings.
- Known deviations/waivers: none. Daemon HTTP export route/protocol deletion
  remains intentionally gated on Phase 6, and the Docker gateway rebuild
  remains reserved for Phase 7. Concurrent configuration, runtime, daemon,
  and Obsidian workspace edits were preserved and excluded from this commit.

## Phase 5 — Move console operation callers to gateway RPC

**Status:** complete

**Depends on:** Phases 2 and 4

**Purpose:** Ensure console callers do not keep daemon HTTP operation routes
alive after clients have a canonical gateway path.

### Scope and tasks

- [x] Change console imports from `sandbox-cli-core` to `sandbox-cli::core`
  without enabling a CLI-set feature.
- [x] Replace generic `/api/sandboxes/:id/files/:op` proxying with exact,
  read-only `/api/sandboxes/:id/files/list` proxying.
- [x] Remove `/api/sandboxes/:id/observability/:view` daemon HTTP proxying.
- [x] Change frontend/API callers for file read/write/edit/blame and all
  observability views to console authenticated `/api/rpc` request envelopes.
- [x] Retain console daemon HTTP health and forwarding proxy behaviour.
- [x] Update console catalog/tests to use the canonical three catalogs and
  public operation names.

### Files expected to change

```text
crates/sandbox-console/{Cargo.toml,src/lib.rs,src/router.rs,src/daemon_api.rs}
crates/sandbox-console/src/{catalog.rs,rpc.rs,health.rs,proxy.rs}
console frontend/API caller assets (located during implementation)
crates/sandbox-console/tests/console/{catalog.rs,daemon_api.rs,health.rs,proxy.rs,rpc.rs}
```

### Acceptance criteria

- [x] Console `/api/rpc` successfully carries a representative runtime file
  call and a representative observability call through the gateway.
- [x] The console exposes only `files/list` as its daemon file-operation
  proxy; direct console proxy routes for read/write/edit/blame/observability
  are absent or return `404`.
- [x] Console health and preview forwarding still resolve `daemon_http` and
  preserve existing request/response semantics.
- [x] Browser-facing code never receives the gateway authentication token.
- [x] `cargo test -p sandbox-console` passes, including exact route assertions.

### Evidence to record

- Commit/PR: implementation commit `f2cc10651` on the repository's direct
  `main` workflow; no PR.
- Commands: `cargo test -p sandbox-console` passed 29 tests; the same 29 tests
  passed under the MSRV with `cargo +1.85.0 test -p sandbox-console`.
  `cargo +1.85.0 clippy -p sandbox-console --all-targets -- -D warnings`,
  `cargo fmt --package sandbox-console -- --check`, and `git diff --check`
  passed. `npm run build` under `web/console` passed TypeScript and the Vite
  production build (the installed Node 22.7 version and bundle-size warnings
  were non-fatal). A dependency-tree check showed only the default,
  feature-free `sandbox-cli` core dependency for the console.
- Console route matrix: fake-daemon integration proved exact
  `POST /api/sandboxes/eos-1/files/list` forwards to `POST /files/list` with
  its body unchanged; `GET` on that exact path returns `405`. Table-driven
  assertions proved file read/write/edit/blame, every observability view,
  list suffixes, and nested sandbox paths return `404` without any gateway
  endpoint lookup. The unchanged health and shared/isolated preview tests
  continued to cover endpoint resolution, request/response forwarding,
  errors, caching, and upgrade tunnelling.
- `/api/rpc` integration evidence: fake-gateway tests captured exact
  authenticated `file_read` and scoped `get_observability` requests, including
  sandbox scope and the private observability `view`, and returned their
  results through the console. Aggregate snapshot remains `snapshot` with
  system scope. A spoofed browser auth token and `_stream_logs` field were
  discarded/overridden; only the server-configured token reached the gateway,
  and neither request token appeared in browser response headers or body.
- Frontend/catalog proof: `npm run build` compiled the migrated adapters;
  positive source searches found RPC-only file read/write/blame and scoped
  observability plus the sole `postJson` list call. Negative source and built-
  asset searches found no legacy operation URLs or gateway-token names. No
  frontend `file_edit` caller exists; the current editor saves through the
  migrated `file_write` call, while the former edit proxy is covered by the
  `404` matrix. A direct Node check accepted a valid canonical `json_array`
  argument and rejected scalar/malformed values. Catalog integration asserts
  exact `management`, `runtime`, and `observability` names and memberships,
  including `file_edit.edits` as `json_array`.
- Independent contract/security review: pass with no blocking findings.
- Known deviations/waivers: none. The Phase 6 daemon HTTP removal work and
  Phase 7 Docker gateway rebuild remain intentionally unstarted. Concurrent
  config, benchmark, daemon, gateway, manager, and experiment edits were
  preserved and excluded from the implementation commit.

## Phase 6 — Enforce the daemon HTTP allowlist

**Status:** not started

**Depends on:** Phases 4 and 5

**Purpose:** Delete the now-obsolete direct daemon operation routes and leave
only the documented liveness, proxying, and list surface.

### Scope and tasks

- [ ] Reduce `crates/sandbox-daemon/src/http/api.rs` to bounded JSON parsing
  and internal `file_list` dispatch only.
- [ ] Change `http/router.rs` to an exact allowlist: `GET /health`, exact
  `/files/list` handling, and `/forward/...`; all other paths are `404`.
- [ ] Delete `crates/sandbox-daemon/src/http/export.rs` and remove its module
  wiring.
- [ ] Remove unused export stream constants/types from `sandbox-protocol` only
  after a whole-workspace caller search shows no internal references remain.
- [ ] Preserve the standalone HTTP listener and `daemon_http` record metadata
  because health, forward, and list still need them.
- [ ] Preserve forwarding parsing/proxy semantics, including isolated live
  workspace resolution, timeout/status mapping, headers, and upgrades.
- [ ] Update daemon HTTP/console route documentation to declare the breaking
  removal.

### Files expected to change

```text
crates/sandbox-daemon/src/http/{mod.rs,router.rs,api.rs,health.rs,response.rs,server.rs,forward/**}
deleted: crates/sandbox-daemon/src/http/export.rs
crates/sandbox-daemon/tests/**
crates/sandbox-protocol/src/{lib.rs,export_stream.rs}
docs/daemon-http/README.md
README.md
```

### Acceptance criteria

- [ ] `GET /health` returns exact fixed `200` JSON and does not require an
  initialized runtime state.
- [ ] Shared and isolated `/forward/...` routes preserve body/path/query and
  have documented `400`, `403`, `404`, `502`, and `504` error mapping.
- [ ] `POST /files/list` works for root, published snapshot, and a live
  workspace session; bad JSON/body-size/method handling preserves its
  documented `400`/`405` behaviour.
- [ ] `POST /files/read`, `/files/write`, `/files/edit`, `/files/blame`,
  `/observability/snapshot`, `/export/x`, and every other removed operation
  route return `404`.
- [ ] No daemon HTTP `export` module, route prefix, token header, or spool
  stream claim endpoint remains.
- [ ] `cargo test -p sandbox-daemon` and HTTP-focused integration tests pass.

### Evidence to record

```text
Commit/PR:
Commands:
HTTP allowlist/404 test results:
Search proving export-route removal:
Known deviations/waivers:
```

## Phase 7 — Release verification and cutover

**Status:** not started

**Depends on:** Phases 1 through 6

**Purpose:** Prove that the new boundaries work together and that no stale
compatibility path silently preserves the old surface.

### Scope and tasks

- [ ] Update root README, daemon HTTP documentation, CLI guidance, and MCP
  registration example to match the landed behavior.
- [ ] Capture CLI help snapshots for all three binaries and MCP `tools/list`
  snapshots for all three fixed sets.
- [ ] Verify one tool/operation per set against a real or controlled gateway,
  including management, runtime, aggregate observability snapshot, and scoped
  observability view.
- [ ] Run full daemon HTTP allowlist evidence against a real sandbox where
  feasible, including shared/isolated forwarding and list behaviour.
- [ ] Run export end-to-end evidence covering chunk paging and a failure case.
- [ ] Rebuild the Docker sandbox gateway binary using the required command.
- [ ] Run focused crate tests followed by workspace test evidence.
- [ ] Remove temporary compatibility wrappers unless a release policy
  explicitly approves one documented, time-bounded manager-observability
  delegation wrapper.

### Required verification commands

Run the exact subset appropriate to changed crates first, then the workspace
suite. The final gateway rebuild is mandatory for Docker gateway release
evidence.

```sh
cargo test -p sandbox-manager-operations \
  -p sandbox-runtime-operations \
  -p sandbox-observability-operations
cargo test -p sandbox-cli --all-features
cargo test -p sandbox-mcp
cargo test -p sandbox-manager
cargo test -p sandbox-console
cargo test -p sandbox-daemon
bin/start-sandbox-docker-gateway --rebuild-binary
cargo test --workspace
```

If a command is intentionally inapplicable, record why and substitute the
closest focused proof in the evidence block; do not silently omit it.

### Release acceptance criteria

- [ ] CLI help and MCP `tools/list` show exactly their authorized set; no
  principal can enumerate a different set through that surface.
- [ ] Public management uses `squash_layerstacks`; `checkpoint_squash` is not
  accepted as a current public operation.
- [ ] Neither CLI nor MCP exposes workspace create/destroy lifecycle or
  `file_list`.
- [ ] Direct daemon HTTP succeeds only for health, forward, and
  `POST /files/list`; removed operation routes are proven `404`.
- [ ] `export_changes` works via authenticated chunk RPC and its docs/results
  correctly describe a published-layer delta rather than a full workspace.
- [ ] The console does not create an alternate direct daemon operation API.
- [ ] The Docker gateway binary was rebuilt with
  `bin/start-sandbox-docker-gateway --rebuild-binary` after the final source
  change.
- [ ] Focused and workspace test evidence is attached; no unapproved waiver
  remains.

### Evidence to record

```text
Release commit/PR:
CLI help snapshots:
MCP tools/list snapshots:
Daemon HTTP allowlist evidence:
Export paging evidence:
Docker gateway rebuild output:
Focused test commands/results:
Workspace test command/result:
Approved waivers and expiry date:
```

## Progress update protocol

When work lands, update only the relevant phase in this file:

1. Change its status in **Current progress** (`not started` → `in progress` →
   `complete`).
2. Check a task only when its code and direct test are both present.
3. Check an acceptance criterion only when its evidence has been recorded.
4. Put command output summaries, commit/PR, and any approved deviation in the
   phase’s evidence block.
5. Update `updated` frontmatter date and add a short entry below.

### Change log

| Date | Phase | Update | Evidence |
| --- | --- | --- | --- |
| 2026-07-10 | 5 | Completed the console operation cutover to authenticated gateway RPC, exact list-only daemon proxy, canonical public catalogs, and server-only credential boundary. | commit `f2cc10651`; 29 console tests on default and Rust 1.85 toolchains, frontend production build, JSON-array parser check, lint/format/search/dependency checks, and independent review |
| 2026-07-10 | 5 | Started the console RPC migration after confirming the Phase 2 and Phase 4 gates and re-reading the binding console, CLI, operation, and daemon HTTP contracts. | implementation and direct acceptance proof pending |
| 2026-07-10 | 4 | Completed authenticated RPC-only export paging with strict start/page completeness checks, pre-mutation failure handling, and removal of the manager HTTP export client. | commit `0644fd64b`; 30 focused manager export tests on default and Rust 1.85 toolchains, full manager suite, runtime EOF cleanup proof, lint/format/search checks, and independent review |
| 2026-07-10 | 4 | Started the manager export transport migration after confirming the Phase 1 gate and re-reading the binding export, RPC, CLI, MCP, and daemon HTTP contracts. | implementation and direct acceptance proof pending |
| 2026-07-10 | 3 | Completed the fixed-set catalog-driven MCP adapter, shared value request construction, structured error/result boundary, and real stdio/fake-gateway coverage for all three registrations. | commits `7bee540ca`, `e15839cda`; 75 focused tests, Rust 1.85 check, exact fixture/routing assertions, lint, formatting, and dependency-boundary proof |
| 2026-07-10 | 3 | Started the fixed-set MCP adapter after confirming the Phase 1 and 2 gates and re-reading all binding MCP, CLI, HTTP, operation, and implementation contracts. | implementation and direct acceptance proof pending |
| 2026-07-10 | 2 | Completed one core-only CLI package with three feature-isolated binaries, exact set help/routing, migrated consumers/tests, and removal of all legacy CLI packages. | commits `73e5fa612`, `2fa2943fc`, `7aab1e035`; 67 focused tests, three isolated artifact/tree checks, wrapper/launcher, lint, formatting, and dependency-audit proof |
| 2026-07-10 | 2 | Started CLI package consolidation after confirming the Phase 1 catalog/visibility gate and re-reading the binding CLI, operation, and implementation contracts. | implementation and direct acceptance proof pending |
| 2026-07-10 | 1 | Completed exact public catalogs, visibility boundaries, canonical snapshot ownership, runtime adapter rename, and caller cleanup without starting Phase 2. | commits `4bda5df70`, `0217248e5`; 201 focused Rust tests, fake-gateway, formatting, compile, and boundary-search proof |
| 2026-07-10 | 1 | Started the catalog and visibility boundary after confirming Phase 0 complete and reading all companion contracts. | implementation and direct acceptance proof pending |
| 2026-07-10 | 0 | Created the phase-gated execution tracker from the approved design contracts. | documentation only |

## Related documents

- [[mcp]] — detailed MCP public tools and target adapter structure.
- [[cli]] — detailed CLI commands, package structure, and migration mapping.
- [[http]] — detailed daemon HTTP route/response/removal contract.
- [[operation-contract]] — concise catalog shared by all surfaces.
- [[implementation-spec]] — LOC budget, rationale, and non-goals.
