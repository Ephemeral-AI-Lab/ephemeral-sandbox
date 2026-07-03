---
title: CLI Split — sandbox-manager-cli vs sandbox-runtime-cli — Migration Spec
tags:
  - ephemeral-os
  - cli
  - migration
  - spec
status: draft
updated: 2026-07-03
---

# CLI Split Migration Spec

Split the single `sandbox-cli` binary (three execution spaces inside
`crates/sandbox-gateway`) into two purpose-built binaries:

- **`sandbox-manager-cli`** — operator surface: fleet lifecycle, squash
  checkpoints, observability.
- **`sandbox-runtime-cli`** — agent surface: drive exactly one sandbox
  (commands, workspace sessions, files).

The wire protocol, routing, and daemon are untouched. This is a client-side
refactor whose only real blocker is compile-time catalog linkage.

Companion: [[operation]] — the full operation/variant/output reference used as
the behavioral contract for before/after equivalence.

---

## 1. Motivation

1. **Two different users.** Manager ops are operator/fleet tooling
   (system scope); runtime ops are an agent driving one chosen sandbox
   (sandbox scope). One binary muddles the audience, flags
   (`--default-sandbox-id` means nothing to manager ops), and help surface.
2. **Dependency hygiene.** `sandbox-cli` cohabits the `sandbox-gateway`
   crate with the gateway server, so it compiles the entire server tree
   (`sandbox-manager` services, `sandbox-provider-docker`/bollard,
   `sandbox-runtime` + namespace/layerstack machinery) just to read static
   help strings and link three catalogs.
3. **Boundary clarity.** README's component table is law; today "the
   `sandbox-cli` protocol client" hides inside the gateway's row. Two thin
   client crates make the boundary explicit and enforceable.

## 2. Current state (as-is)

### 2.1 One crate, two binaries, three spaces

- `crates/sandbox-gateway/Cargo.toml` declares `[[bin]] sandbox-gateway`
  and `[[bin]] sandbox-cli` (`src/cli/main.rs` →
  `cli::output::run_cli`).
- Dispatch: `enum Command { Manager, Runtime, Observability }`
  (`src/cli/output.rs:45-50`). Operation names are **not** clap
  subcommands — they are looked up in a per-space catalog and argv is
  hand-parsed against `ArgSpec`s (`src/cli/request_builder.rs:50-95,
  160-216`).
- Global flags: `--gateway-socket` (default `127.0.0.1:7878`),
  `--gateway-auth-token`, `--default-sandbox-id`, `--progress`.
- `help [OPERATION]` is a CLI-local pseudo-op in every space; `help` is
  reserved as a wire name (`request_builder.rs:71-75`).

### 2.2 Wire contract (unchanged by this migration)

- One newline-delimited JSON request per TCP connection;
  `_gateway_auth` token and `_stream_logs` flag injected client-side
  (`src/cli/client.rs:34-67`).
- **Routing is server-side**: the manager router decides by
  `(scope, manager_owns_operation(op))` —
  `crates/sandbox-manager/src/router/dispatch.rs:10-23`. Sandbox-scoped
  non-manager ops are re-sent verbatim to the in-sandbox daemon with a
  per-sandbox `_daemon_auth` token (`router/forward.rs`,
  `daemon_client.rs`).
- Scope stamping: Manager space → `CliOperationScope::system()`; Runtime
  space → `CliOperationScope::sandbox(id)`
  (`request_builder.rs:78-94`).

### 2.3 Catalog and help machinery

- Spec/catalog/help **types and rendering** live in `sandbox-protocol`
  (`cli_operation_spec.rs`, `catalog.rs`, `help.rs`).
- The three **static registries** live next to their implementations:
  - manager: `crates/sandbox-manager/src/operation/cli_definition/management_operations.rs`
  - runtime: `crates/sandbox-runtime/operation/src/cli_definition/{command,workspace_session,file}_operations.rs`
  - observability: `crates/sandbox-observability-operations/src/cli_definition/` (already **spec-only** — the pattern to copy)
- The CLI links all three catalogs at compile time
  (`request_builder.rs:38-48`).
- `sandbox-protocol/src/help.rs:51,78-80` hardcodes the program name
  `sandbox-cli` into rendered usage text.

### 2.4 The dependency problem

`cargo build -p sandbox-gateway --bin sandbox-cli` pulls: `sandbox-manager`
(and its layerstack dep), `sandbox-runtime` (operation + workspace +
namespace-execution + layerstack + overlay), `sandbox-provider-docker`
(bollard) — none of which the client needs at runtime. A naive split would
reproduce this: `sandbox-runtime-cli` linking manager code (or vice versa)
just for catalog constants.

### 2.5 Operation surface today

| Space | Ops (visible) | Hidden / wire-only |
|---|---|---|
| `manager` | `create_sandbox`, `destroy_sandbox`, `list_sandboxes`, `inspect_sandbox`, `layerstack_squash` | `snapshot` (aggregate; `cli: None`) |
| `runtime` | `exec_command`, `write_command_stdin`, `read_command_lines`, `create_workspace_session`, `destroy_workspace_session`, `file_blame`, `file_read`, `file_write`, `file_edit` | `squash_layerstack` (`cli: None`) |
| `observability` | `snapshot`, `trace`, `events`, `cgroup`, `layerstack` | rewritten to daemon-private `get_observability` (or manager `snapshot` when no `--sandbox-id`) |

Full variants and expected outputs: [[operation]].

## 3. Target state

| Crate | Kind | Job | Must never |
|---|---|---|---|
| `sandbox-manager-operations` | lib (spec-only) | manager CLI operation specs + catalog | contain dispatch or service code |
| `sandbox-runtime-operations` | lib (spec-only) | runtime CLI operation specs + catalog | contain dispatch or service code |
| `sandbox-cli-core` | lib | gateway client, config discovery, catalog request building, response rendering, help plumbing | know any concrete operation or space policy |
| `sandbox-manager-cli` | bin | operator CLI: manager + observability catalogs, system-scope requests, `--progress` streaming | depend on manager/runtime implementation crates |
| `sandbox-runtime-cli` | bin | agent CLI: runtime catalog, sandbox-scope requests, required `--sandbox-id` | depend on manager/runtime implementation crates |
| `sandbox-gateway` | bin+lib | gateway server only | own any CLI client code |

Placement: **all new crates at `crates/` top level.** `sandbox-runtime-cli`
must *not* live under `crates/sandbox-runtime/` — despite the name it is a
host-side protocol client, not a runtime package.

Dependency targets:

- `sandbox-manager-cli` → `sandbox-cli-core`, `sandbox-protocol`,
  `sandbox-config`, `sandbox-manager-operations`,
  `sandbox-observability-operations`, `clap`, `serde_json`, `tokio`, `uuid`.
- `sandbox-runtime-cli` → `sandbox-cli-core`, `sandbox-protocol`,
  `sandbox-config`, `sandbox-runtime-operations`, `clap`, `serde_json`,
  `tokio`, `uuid`.
- Neither transitively reaches `sandbox-manager`, `sandbox-runtime`,
  `sandbox-daemon`, or `sandbox-provider-docker` (verify with
  `cargo tree -p <cli-crate>`).

## 4. Invariants — must not change

- Request/response DTOs, `CliOperationScope` semantics, JSON-line framing,
  `_gateway_auth`/`_daemon_auth`/`_stream_logs` handling.
- Server-side routing matrix (`router/dispatch.rs`) — zero diff.
- Output contract: success JSON line → stdout, exit 0; error envelope
  `{"error":{kind,message,details}}` → stderr, exit 1; local usage errors →
  stderr, exit 2 (see [[operation#Conventions]]).
- Behavior of every operation in [[operation]] — the reference doubles as
  the equivalence checklist for cutover testing.

## 5. Design decisions

### D1 — Observability lives in `sandbox-manager-cli` ✅ recommended

It is operator-facing diagnostics; the aggregate `snapshot` is
manager-owned; routing is server-side so transport is indifferent. This
keeps `sandbox-runtime-cli` a pure "agent drives one sandbox" surface.

*Alternative (rejected):* duplicate the observability space in both CLIs.
Cheap (spec-only crate) but violates prefer-less and forks the help
surface.

### D2 — Catalogs stay compile-time static, via spec-only crates ✅

*Alternative (rejected):* fetch the catalog from the gateway at startup
(`catalog_to_value`/`catalog_from_value` already round-trip). Pros:
version-skew tolerance, thinner clients. Cons: `help` requires a running
gateway; extra round trip on every invocation; needless for repo-local
tooling. Static wins.

### D3 — One shared CLI config section

`sandbox-config/src/configs/cli.rs` keeps a single schema (socket, auth
token) shared by both binaries. The former `default_sandbox_id` field is
**dropped**: `sandbox-runtime-cli` requires an explicit `--sandbox-id` on
every invocation (no `SANDBOX_DEFAULT_ID` env or config fallback), and
`sandbox-manager-cli` never used it. Resolution lives in `sandbox-cli-core`,
so the legacy `sandbox-cli` runtime space inherits the same requirement
during the compat window. Forking the schema would add types for nothing.

### D4 — Compat window, then removal

`sandbox-cli` (three-space form) keeps working through the migration as a
thin shim over `sandbox-cli-core`, and is deleted in the final phase after
the e2e harness and `bin/` scripts are cut over. No long-lived alias.

## 6. Migration plan

Rule: **every phase lands green** (`cargo build && cargo test &&
cargo clippy --all-targets`, plus the Docker e2e suite where noted). Work
on `main`, additive edits, never revert concurrent work.

### Phase 1 — Extract spec-only catalog crates

- [ ] New crate `crates/sandbox-manager-operations`: move the spec consts
      and catalog assembly out of
      `sandbox-manager/src/operation/cli_definition/` +
      `operation/specs.rs`. Dispatch tables (`ManagerOperationEntry`,
      fn pointers) stay in `sandbox-manager` and import the specs.
- [ ] New crate `crates/sandbox-runtime-operations`: same split for
      `sandbox-runtime/operation/src/cli_definition/`. `OperationEntry`
      registrations stay in `sandbox-runtime`; only `CliOperationSpec` /
      `ArgSpec` / family consts move.
- [ ] `sandbox-gateway` CLI consumes catalogs from the new crates
      (`request_builder.rs:38-48`).
- [ ] Exit: zero behavior change; `validate_catalog` tests still pass;
      catalog JSON output byte-identical.

### Phase 2 — Extract `sandbox-cli-core` + parameterize help

- [ ] New crate `crates/sandbox-cli-core`: move `src/cli/client.rs`,
      `src/cli/config.rs`, `src/cli/request_builder.rs`, and the
      space-generic parts of `src/cli/output.rs`
      (`run_request_from_catalog`, `render_response`, `render_error`,
      `cli_log`, help plumbing). `sandbox-gateway` re-exports temporarily.
- [ ] Add a program-name parameter to `render_catalog_help` /
      `render_operation_help` / `search_operation_help` in
      `sandbox-protocol/src/help.rs` (help metadata stays
      protocol-owned; only the binary-name literal becomes a parameter).
      Note: `CliSpec.usage`/`examples` strings in the spec consts embed
      `sandbox-cli ...` — rewrite them per-space in this phase
      (manager examples → `sandbox-manager-cli ...`, runtime →
      `sandbox-runtime-cli ...`) or render them from `path` + program
      name instead of storing full strings.
- [ ] Exit: `sandbox-cli` behavior unchanged (old name still rendered via
      parameter).

### Phase 3 — Add the two binaries

- [ ] `crates/sandbox-manager-cli`: manager + observability catalogs.
      Grammar: `sandbox-manager-cli <op> [args…]` and
      `sandbox-manager-cli observability <op> …` (or flat
      `snapshot|trace|…` — decide at implementation; keep the two
      catalogs' names non-colliding: manager's hidden `snapshot` vs
      observability `snapshot` already share a name — the observability
      rewrite in `request_builder.rs:103-133` must move here intact).
      Keeps `--progress`/`_stream_logs` and the legacy
      `--workspace-root` alias. Always system scope (observability
      per-sandbox views stamp sandbox scope exactly as today).
- [ ] `crates/sandbox-runtime-cli`: runtime catalog only. Grammar:
      `sandbox-runtime-cli --sandbox-id ID <op> [args…]`. `--sandbox-id` is
      required on every invocation — no `SANDBOX_DEFAULT_ID` env or config
      fallback. Always sandbox scope.
- [ ] Exit: both binaries pass a smoke matrix mirroring
      [[operation]] happy paths + local error variants; old
      `sandbox-cli` still green.

### Phase 4 — Cutover

- [ ] `bin/`: add `bin/sandbox-manager-cli` and `bin/sandbox-runtime-cli`
      wrappers (token file → `SANDBOX_GATEWAY_AUTH_TOKEN`, build-or-run,
      same as `bin/sandbox-cli:7-18`); update the build line in
      `bin/start-sandbox-docker-gateway:126` to build all needed bins.
- [ ] e2e harness: switch the argv builders in
      `cli-operation-e2e-live-test/core/cli.py:30-56,190-205` (single
      choke point) and `core/config.py`. Land in the same commit as the
      `bin/` wrappers. Run the full live suite.
- [ ] Docs: README architecture diagram + component table + boundary law;
      CLAUDE.md "Sandbox tools" section; `config/README.md` if it names
      the binary.
- [ ] Exit: full e2e suite green through the new binaries.

### Phase 5 — Remove the old client

- [ ] Delete `src/cli/` and the `sandbox-cli` `[[bin]]` from
      `sandbox-gateway`; drop its now-unused deps (`clap`, catalog crates,
      `sandbox-observability-operations` if unused by the server).
- [ ] Delete `bin/sandbox-cli`.
- [ ] Exit: `cargo tree -p sandbox-manager-cli` / `-p sandbox-runtime-cli`
      show no implementation-crate edges; workspace green; e2e green.

## 7. Risks and gotchas

- **Observability's dual routing.** `snapshot` without `--sandbox-id`
  becomes the manager's hidden `snapshot` op (system scope); with it, a
  daemon `get_observability` view (sandbox scope). This logic
  (`request_builder.rs:97-136`) moves verbatim into
  `sandbox-manager-cli`; easy to drop the no-id branch by accident.
- **Name collision inside manager-cli.** Manager hidden op `snapshot` and
  observability op `snapshot` coexist today only because they live in
  different catalogs. If manager-cli flattens spaces, keep the catalogs
  separate internally.
- **Help-text assertions.** Any tests snapshotting help output (protocol
  `help.rs` unit tests, e2e) will break on the program-name change —
  update expectations in Phase 2/3, not by weakening the renderer.
- **`--progress` placement quirk.** Global flag *and* plucked from manager
  op argv (`take_progress_flag`, `output.rs:322-326`). Preserve both
  accepted forms in manager-cli.
- **Legacy `--workspace-root`.** Alias only for `create_sandbox`
  (`request_builder.rs:270-278`); manager-cli must keep it or the e2e/dev
  muscle memory breaks.
- **Parallel workers.** Other agents edit this repo concurrently; the
  spec-extraction phases touch popular files
  (`management_operations.rs`, runtime `cli_definition/*`). Keep moves
  mechanical, avoid reformatting, land each phase quickly. Note: the
  [[finalize-policy/spec|finalize-policy change]] rewrote the runtime op
  descriptions (`exec_command`, `destroy_workspace_session`) and response
  fields (`workspace_session_id`, `finalize_policy`, `publish_rejected`) in
  these same files — Phase 1 carries that post-finalize-policy text
  verbatim, and [[operation]] is the updated equivalence contract.

## 8. Acceptance criteria

- [ ] Two binaries build and run; `sandbox-cli` fully removed.
- [ ] For every operation and variant in [[operation]]: identical stdout
      JSON shape, stderr envelope, and exit code through the new binaries
      (manager ops via `sandbox-manager-cli`, runtime ops via
      `sandbox-runtime-cli`, observability via `sandbox-manager-cli`).
- [ ] `cargo tree`: neither CLI depends on `sandbox-manager`,
      `sandbox-runtime`, `sandbox-daemon`, or `sandbox-provider-docker`.
- [ ] Zero diff in `sandbox-daemon`, `sandbox-provider-docker`, and the
      manager router; `sandbox-protocol` diff limited to help
      program-name parameterization.
- [ ] Full Docker e2e live suite green through the new binaries.
- [ ] README component table + boundary law updated; help output shows the
      correct per-binary usage lines.

## 9. File inventory

| Action | Path |
|---|---|
| add | `crates/sandbox-manager-operations/` (specs from `sandbox-manager/src/operation/cli_definition/`) |
| add | `crates/sandbox-runtime-operations/` (specs from `sandbox-runtime/operation/src/cli_definition/`) |
| add | `crates/sandbox-cli-core/` (from `sandbox-gateway/src/cli/{client,config,request_builder,output}.rs`) |
| add | `crates/sandbox-manager-cli/`, `crates/sandbox-runtime-cli/` |
| add | `bin/sandbox-manager-cli`, `bin/sandbox-runtime-cli` |
| edit | `sandbox-protocol/src/help.rs` (program-name parameter) |
| edit | spec consts' `CliSpec.usage`/`examples` (per-binary strings) |
| edit | `bin/start-sandbox-docker-gateway` (build line) |
| edit | `cli-operation-e2e-live-test/core/{cli,config}.py` |
| edit | `sandbox-config/src/configs/cli.rs` (drop `default_sandbox_id`; runtime CLI requires explicit `--sandbox-id`) |
| edit | `README.md`, `CLAUDE.md`, workspace `Cargo.toml` members |
| delete (P5) | `sandbox-gateway/src/cli/`, `[[bin]] sandbox-cli`, `bin/sandbox-cli` |
