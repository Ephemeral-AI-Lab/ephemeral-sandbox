---
title: Sandbox Operation Ownership Migration — Phase Plan
tags:
  - ephemeral-os
  - operation
  - migration
  - phase-plan
  - execution-tracker
status: in-progress
updated: 2026-07-10
---

# Operation ownership migration — phase plan

This is the execution tracker for [[spec|the migration specification]]. The
specification owns the design; this plan owns phase checkboxes, owners,
command evidence, deviations, and approval. The acceptance checkboxes inside
the specification are a summary — update them only from evidence linked
here.

## How this plan works

1. **Phases are strictly sequential.** A phase may not start until the
   previous phase's acceptance checklist is fully checked **and** its gate
   approval row is filled in the dashboard. Phase 0 is the only phase that
   starts `ready`; every other phase starts `blocked`.
2. **Every phase has three lists.** *Change list* = the implementation work,
   checked off as it lands. *Acceptance criteria* = the exit gate, checked
   only with recorded command evidence. *Progress log* = dated evidence rows
   (command, result, output link or snippet, deviations).
3. **Evidence is a command, not a claim.** Each acceptance checkbox names
   its verification command; paste the command and a result excerpt (or a
   link to a committed evidence file) into the progress log before checking
   the box.
4. **Deviations amend the spec first.** If implementation must differ from
   the specification, record the deviation in the phase's progress log,
   amend the spec in the same change, and note the spec section touched.
   Silent divergence fails the phase gate.
5. **Work lands on `main`.** No branches or worktrees (repository rule).
   Each phase is a small series of workspace-green commits; the atomic steps
   called out below (catalog merge, namespace conversion, Phase 6 cutover)
   are each one commit.

## Status dashboard

Statuses: `blocked` → `ready` → `in progress` → `gate review` → `approved`.

| Phase | Title | Status | Started | Gate approved | Approver |
| --- | --- | --- | --- | --- | --- |
| 0 | Characterize, freeze inventory, purge generated weight | approved | 2026-07-10 | 2026-07-10 | Codex |
| 1 | Create contract, narrow protocol in place | approved | 2026-07-10 | 2026-07-10 | Codex |
| 2 | Merge and refeature the catalogs | gate review | 2026-07-10 | — | — |
| 3 | Extract the shared gateway client | blocked | — | — | — |
| 4 | Clean the manager application in place | blocked | — | — | — |
| 5 | Clean the runtime application in place | blocked | — | — | — |
| 6 | Extract observability application, remove multiplexing | blocked | — | — | — |
| 7 | Update documentation, scripts, law statements | blocked | — | — | — |
| 8 | Enforce boundaries and cut over | blocked | — | — | — |

## Standing gate (every phase)

Run after the phase's final change; paste results into the phase progress
log. A phase cannot enter `gate review` while any of these fail.

```bash
cargo check --workspace --all-targets --all-features
cargo test --workspace --all-features          # or the focused subset the phase names
cargo clippy --workspace --all-targets --all-features -- -D warnings   # when a boundary or public behavior changed
cargo fmt --all -- --check
```

LOC evidence uses the specification's counting rule: physical lines of
tracked `src/**/*.rs` plus crate-root `build.rs`, comments and blanks
included; tests, fixtures, manifests, and generated content excluded.

---

## Phase 0 — Characterize, freeze inventory, purge generated weight

**Entry criteria:** none (first phase).
**Ordering rule inside the phase:** all baselines are captured **before**
the destructive purge/move step; the purge and the `e2e/` relocation are one
atomic change.

### Change list

- [x] Record `cargo metadata --format-version 1` (package names, manifest
  paths, features, binaries, dependency graph) as evidence file.
- [x] Record the baseline production-LOC table per source owner using the
  counting rule (reference point: spec tables at HEAD `cc5f9974e`).
- [x] Produce the route audit table: every dispatchable route with domain,
  scope policy, expanded scope kind, visibility, catalog owner, execution
  owner, handler owner, and wire destination; classify each into the four
  route classes (public / canonical internal / transport handshake /
  HTTP-only exception). The audit must include the hidden dispatchable
  `create_workspace_session` and `destroy_workspace_session` routes as
  canonical internal runtime operations.
- [x] Snapshot characterization fixtures: catalog JSON, CLI help output,
  CLI error envelopes and exit codes, MCP tool schemas, console `/api/rpc`
  behavior, internal daemon RPC behavior.
- [x] Run the current unit/integration suites; record the live E2E baseline
  from the current `cli-operation-e2e-live-test/` location.
- [x] Atomic purge/move change: untrack and delete the 7,977 tracked E2E
  report files (4,274,972 lines) and the two tracked
  `web/console/*.tsbuildinfo` files; add durable `.gitignore` rules
  (`e2e/**/test-reports/`, `*.tsbuildinfo`); move the 87 maintained E2E
  files to root `e2e/`; replace parent-count root discovery
  (`core/config.py`, `conftest.py`, `measure.py:18`) with one tested
  root-marker resolver.
- [x] Rerun the E2E smoke from `e2e/` to prove relocation.

### Acceptance criteria

- [x] Every current operation appears in the route audit table with exactly
  one class. *Evidence: the table, cross-checked against `rg` for dispatch
  sites.*
- [x] Every behavior that must remain stable has an executable
  characterization test or recorded fixture. *Evidence: fixture list +
  passing run.*
- [x] Baseline LOC and metadata snapshots are committed as evidence files.
- [x] No tracked generated content remains:
  `git ls-files | rg 'test-reports/|\.tsbuildinfo$'` returns nothing.
- [x] `git ls-files 'e2e/**' | wc -l` reports 87 maintained files (± files
  added by the resolver test); `cli-operation-e2e-live-test/` is absent.
- [x] The root-marker resolver has a test; E2E smoke passes from `e2e/`.
  *Evidence: pytest output.*
- [x] Standing gate passed.

### Progress log

| Date | Item | Command / evidence | Result | Deviations |
| --- | --- | --- | --- | --- |
| 2026-07-10 | Cargo metadata baseline | `cargo metadata --format-version 1 \| jq . \| shasum -a 256`; `shasum -a 256 evidence/phase-0/cargo-metadata.json`; see `evidence/phase-0/baseline-inventory.md` | Both hashes `b34d7358…20c8`; 20 workspace packages, 8 binaries, 59 path-dependency edges. | None. |
| 2026-07-10 | Production LOC baseline | `git grep -n -e '^' df37fe31 -- ':(glob)PACKAGE_DIR/src/**/*.rs' \| wc -l` per package; see `evidence/phase-0/baseline-inventory.md` | `crates/` 39,826 lines; `xtask` 1,439. The spec reference `cc5f9974e` is 39,391 crate lines; all +435 lines are allocated in the evidence table. | None. |
| 2026-07-10 | Route inventory and ownership audit | Declaration, registration, and dispatch `rg` commands in `evidence/phase-0/route-audit.md` | 26 names / 27 expanded keys: 19 public, 6 canonical internal, 1 handshake, 1 HTTP-only exception; every key has one class. | Workspace-session lifecycle routes exposed an omission in the draft spec; correction recorded below. |
| 2026-07-10 | Behavioral characterization | `cargo test -p sandbox-cli --all-features --test compatibility`; focused console unknown/limit tests; focused daemon snapshot/layerstack test; existing exact CLI/MCP/runtime groups in `cargo-test-workspace-baseline.txt` | Added tests passed 2/2, 1/1, 1/1, and 1/1; exact fixtures and hashes are listed in `evidence/phase-0/characterization.md`. | None. |
| 2026-07-10 | Rust and live E2E baselines | `cargo test --workspace --all-features`; `bin/start-sandbox-docker-gateway --rebuild-binary`; `(cd cli-operation-e2e-live-test && python3 -m pytest)`; exact two-node rerun in `pytest-live-targeted-rerun.txt` | Rust: 730 passed, 0 failed, 1 ignored. Live first run: 355 passed, 6 skipped, 1 failed, 1 error; both red nodes then passed 2/2 in 25.12s. Token scan found no gateway-token match in evidence. | Full-run reds were external package-index 404s and a non-reproducible Docker port-readiness transient; retained verbatim as baseline evidence, not a spec deviation. |
| 2026-07-10 | Atomic generated-weight purge and E2E relocation | `git clean -fdX -- cli-operation-e2e-live-test`; `git rm -r` the four inventoried report trees; `git rm web/console/*.tsbuildinfo`; `git mv cli-operation-e2e-live-test e2e`; `git ls-files \| rg 'test-reports/\|\.tsbuildinfo$'`; `[ ! -e cli-operation-e2e-live-test ]` | Deleted 7,977 tracked report files and 2 tracked TypeScript build-info files; generated-content search has no matches; old root is absent. Moved all 87 maintained files and added the resolver plus its test. | None. |
| 2026-07-10 | Root-marker resolver and relocated smoke | `(cd e2e && python3 -m pytest -q core/test_root.py)`; `(cd e2e && E2E_REBUILD_BINARY=0 python3 -m pytest -q -m smoke)`; see `evidence/phase-0/relocation-tests.txt` | Resolver 2/2 passed; relocated smoke 19/19 passed with 346 deselected in 23.22s. | None. |
| 2026-07-10 | Route-audit correction: workspace-session lifecycle ownership | `rg -n 'create_workspace_session\|destroy_workspace_session' crates/sandbox-runtime/operation/src/operation_adapter crates/sandbox-runtime/operation/src/operation.rs crates/sandbox-operations/runtime/tests/catalog.rs` | Both hidden operations are dispatchable daemon-owned runtime routes; classified `SandboxRequired` / `Sandbox` / `Internal` / `Runtime` and added to the normative internal set. | Spec route taxonomy, visibility chokepoints, target tree, and Phase 5 text amended in the same commit; Phase 2, Phase 4, and Phase 5 execution steps updated. |
| 2026-07-10 | Current route-table cross-check | Declaration/literal, handler-registration, dispatch-chain, and hidden-surface `rg` commands in `evidence/phase-0/route-cross-check-current.txt` | Exit 0; current sources still account for 26 names / 27 expanded keys with exactly 19 public, 6 canonical internal, 1 handshake, and 1 HTTP-only class. | None. |
| 2026-07-10 | Phase 0 artifact invariants | `git ls-files \| rg 'test-reports/\|\.tsbuildinfo$'`; `git ls-files 'e2e/**' \| wc -l`; `[ ! -e cli-operation-e2e-live-test ]`; see `evidence/phase-0/artifact-invariants.txt` | Generated-content search returned no output (expected `rg` exit 1); E2E count is 89 = 87 maintained + resolver + resolver test; old root absence check exited 0. | None. |
| 2026-07-10 | Phase 0 standing gate | `cargo check --workspace --all-targets --all-features`; `cargo test --workspace --all-features`; `cargo clippy --workspace --all-targets --all-features -- -D warnings`; `cargo fmt --all -- --check`; see `evidence/phase-0/standing-cargo-{check,test,clippy,fmt}.txt` | All exited 0. Full test summary: 113 result groups, 735 passed, 0 failed, 1 ignored; clippy finished with warnings denied; fmt check was clean. | None. |
| | | | | |

---

## Phase 1 — Create contract, narrow protocol in place

**Entry criteria:** Phase 0 `approved` in the dashboard.

### Change list

- [x] Create `sandbox-operation-contract` at
  `crates/sandbox-operations/contract/` (workspace member added; the
  namespace root transiently also holds the three legacy catalog packages
  until Phase 2).
- [x] Move catalog/spec/scope/route types into the contract
  (`operation.rs`, `family.rs`, `argument.rs`, `document.rs`, `domain.rs`,
  `scope.rs`, `route.rs`); split the application envelope
  (`request.rs`, `response.rs`, `error.rs`) from the wire codec.
- [x] Narrow `sandbox-protocol` in place to
  `{auth,codec,error,framing,handshake,limits}.rs`; preserve package name,
  path, and external response strings.
- [x] Apply all semantic type renames in one change: `CliOperationSpec` →
  `OperationSpec`, `CliOperationFamilySpec` → `OperationFamilySpec`,
  `CliOperationCatalog` → `OperationCatalog`, `CliOperationCatalogDocument`
  → `OperationCatalogDocument`, `CliOperationExecutionSpace` →
  `OperationDomain`, `CliOperationScope` → `OperationScope`, protocol
  `Request`/`Response` → contract `OperationRequest`/`OperationResponse`.
- [x] Move CLI paths, flags, positionals, usage, examples, help, and search
  into `sandbox-cli::projection` (+ `help.rs`); contract retains no CLI
  fields.
- [x] Centralize the daemon readiness handshake in
  `sandbox-protocol/src/handshake.rs`; update
  `sandbox-provider-docker/src/readiness.rs` and daemon
  `rpc/dispatch.rs` to consume it.
- [x] Move `TcpSandboxDaemonClient` and its `ProtocolLimits`-derived
  timeout/deadline enforcement from manager to
  `crates/sandbox-gateway/src/daemon_client.rs`; manager keeps only the
  `SandboxDaemonClient` port.
- [x] Move `LocalSandboxDaemonInstaller`, launch/process/socket helpers, and
  their focused tests to
  `crates/sandbox-gateway/src/local_daemon_installer.rs` and gateway tests;
  manager keeps the `SandboxDaemonInstaller` port and neutral
  `StartedDaemon` DTO.
- [x] Split the 497-line protocol test suite by owner (contract / protocol /
  catalog-to-be / CLI).
- [x] Update all application consumers to contract envelopes directly; no
  deprecated aliases or re-exports.

### Acceptance criteria

- [x] Applications construct and handle `OperationRequest`/
  `OperationResponse` without importing `sandbox-protocol`:
  `cargo tree -i sandbox-protocol -e normal,dev,build` lists no
  `sandbox-manager` or `sandbox-runtime` dependent.
- [x] Manager contains no concrete transport/process adapter:
  `rg 'TcpSandboxDaemonClient|LocalSandboxDaemonInstaller|ProtocolLimits' crates/sandbox-manager/src`
  returns nothing.
- [x] The readiness identifier has one production definition:
  `rg -l 'sandbox_daemon_ready' crates/*/src` names only
  `sandbox-protocol`.
- [x] Semantic renames are complete:
  `rg 'CliOperation|CliSpec|ArgCliSpec' crates/*/src crates/*/*/src` matches
  only `sandbox-cli/src/projection` (CLI-owned types).
- [x] Protocol wire-compatibility tests pass for all behavior not
  explicitly broken later. *Evidence: `cargo test -p sandbox-protocol`.*
- [x] Contract has no workspace dependencies (`cargo metadata`).
- [x] Standing gate passed.

### Progress log

| Date | Item | Command / evidence | Result | Deviations |
| --- | --- | --- | --- | --- |
| 2026-07-10 | Contract crate and application envelopes (changes 1–2) | `find crates/sandbox-operations/contract/src -maxdepth 1 -type f -print \| sort`; `cargo test -p sandbox-operation-contract --all-features` | Contract contains the seven semantic modules, split `request.rs` / `response.rs` / `error.rs`, and `lib.rs`; 10/10 focused tests passed, including missing-domain, unknown-family, and duplicate-operation rejection. | None. |
| 2026-07-10 | Protocol narrowing (change 3) | `find crates/sandbox-protocol/src -maxdepth 1 -type f -print \| sort`; `cargo test -p sandbox-protocol --all-features` | Source tree is exactly `auth.rs`, `codec.rs`, `error.rs`, `framing.rs`, `handshake.rs`, `limits.rs`, and `lib.rs`; 9/9 protocol tests passed. | None. |
| 2026-07-10 | Semantic type and envelope rename (change 4) | `rg 'CliOperation\|CliSpec\|ArgCliSpec' crates/*/src crates/*/*/src`; `cargo check --workspace --all-targets --all-features` | Legacy semantic names produced no matches (expected `rg` exit 1); the full workspace compiled with contract-owned `Operation*` types. | None. |
| 2026-07-10 | CLI projection and compatibility (change 5) | `find crates/sandbox-cli/src/projection -maxdepth 1 -type f -print \| sort`; `cargo test -p sandbox-cli --all-features --test compatibility --test help --test request_builder` | Projection owns `document`, `manager`, `observability`, and `runtime` metadata plus its module root; compatibility 2/2, help 3/3, and request-builder 9/9 passed, including retained `--workspace-root` syntax. | None. |
| 2026-07-10 | Central readiness handshake (change 6) | `rg -n 'daemon_readiness_request_line\|DAEMON_READINESS_OPERATION\|sandbox_daemon_ready' crates/sandbox-protocol/src crates/sandbox-provider-docker/src crates/sandbox-daemon/src`; `cargo test -p sandbox-provider-docker --all-features --test readiness` | The readiness literal is defined only in `sandbox-protocol/src/handshake.rs`; provider request construction and daemon dispatch consume the centralized API; readiness tests passed 7/7. | None. |
| 2026-07-10 | TCP daemon client ownership (change 7) | `cargo test -p sandbox-gateway --test daemon_client`; `cargo test -p sandbox-manager --test manager_router --test manager_export --test manager_core`; `cargo tree -i sandbox-protocol -e normal,dev,build` | Gateway's stalled-response deadline proof passed 1/1; manager router/export/core passed 7/7, 31/31, and 14/14; the dependency tree lists neither manager nor runtime beneath protocol. | None. |
| 2026-07-10 | Local daemon installer ownership (change 8) | `cargo test -p sandbox-gateway --test local_daemon_installer`; `rg 'LocalSandboxDaemonInstaller\|\bnix::\|\buuid::' crates/sandbox-manager/src crates/sandbox-manager/tests` | Gateway launch/process/socket proofs passed 3/3; ownership search produced no matches (expected `rg` exit 1), so manager retains no concrete installer, `nix`, or `uuid` use. | None. |
| 2026-07-10 | Test split and application conversion (changes 9–10) | `cargo test -p sandbox-operation-contract -p sandbox-protocol -p sandbox-cli -p sandbox-mcp -p sandbox-console -p sandbox-manager -p sandbox-runtime -p sandbox-daemon -p sandbox-gateway -p sandbox-provider-docker --all-features`; `cargo fmt --all -- --check` | Every selected owner package test binary passed after both adapter moves; formatting check was clean. | None. |
| 2026-07-10 | Phase 1 code-complete compile | `git diff --check`; `cargo check --workspace --all-targets --all-features` | Diff validation and the full all-target, all-feature workspace compile exited 0. | None. |
| 2026-07-10 | Acceptance: applications are protocol-independent | `cargo tree -i sandbox-protocol -e normal,dev,build` | Exit 0; inverse tree lists CLI, console, daemon, gateway, MCP, and provider-docker only; neither `sandbox-manager` nor `sandbox-runtime` appears. | None. |
| 2026-07-10 | Acceptance: manager has no concrete adapter | `rg 'TcpSandboxDaemonClient\|LocalSandboxDaemonInstaller\|ProtocolLimits' crates/sandbox-manager/src` | No output (expected `rg` exit 1). | None. |
| 2026-07-10 | Acceptance: readiness has one definition | `rg -l 'sandbox_daemon_ready' crates/*/src` | Exit 0; sole result: `crates/sandbox-protocol/src/handshake.rs`. | None. |
| 2026-07-10 | Acceptance: semantic renames are complete | `rg 'CliOperation\|CliSpec\|ArgCliSpec' crates/*/src crates/*/*/src` | No output (expected `rg` exit 1); no legacy semantic or projection type names remain. | None. |
| 2026-07-10 | Acceptance: protocol wire compatibility | `cargo test -p sandbox-protocol` | Exit 0; protocol tests passed 9/9 and doc tests passed. | None. |
| 2026-07-10 | Acceptance: contract dependency closure | `cargo metadata --format-version 1 \| jq -c '.packages[] \| select(.name == "sandbox-operation-contract") \| {name, dependencies: [.dependencies[] \| {name, source}]}'` | Exit 0; dependencies are only registry `serde` and `serde_json`; there are no workspace dependencies. | None. |
| 2026-07-10 | Phase 1 standing gate | `cargo check --workspace --all-targets --all-features`; `cargo test --workspace --all-features`; `cargo clippy --workspace --all-targets --all-features -- -D warnings`; `cargo fmt --all -- --check` | All exited 0; every workspace and doc-test group passed, with one intentional benchmark test ignored; clippy finished with warnings denied and formatting was clean. | None. |
| | | | | |

---

## Phase 2 — Merge and refeature the catalogs

**Entry criteria:** Phase 1 `approved` in the dashboard.
**Atomic step:** the three-package merge, deletions, and dependency
repointing are one commit.

### Change list

- [x] Create `crates/sandbox-operations/catalog/` named
  `sandbox-operation-catalog` with features `manager`, `runtime`,
  `observability` (no defaults); move the three catalog crates into
  feature-gated domain modules; delete the three legacy sibling packages
  (`manager/`, `runtime/`, `observability/`), retaining the
  `crates/sandbox-operations/` namespace root; update workspace
  dependencies, callers, fixtures, and `Cargo.lock`.
- [x] Separate public declarations from canonical internal identifiers
  (`internal/runtime.rs`: `create_workspace_session`,
  `destroy_workspace_session`, `squash_layerstack`,
  `export_layerstack`, `read_export_chunk`; `internal/observability.rs`)
  and from the CLI-owned projection; keep `file_list` in
  `internal/runtime.rs` as the separate HTTP-only exception; `internal`
  is always compiled.
- [x] Build the route manifest
  (`OperationRouteSpec { operation, scope_policy, scope_kind,
  execution_owner, visibility }`) with per-domain slices and the
  all-features unified manifest.
- [x] Add the migration-only `(Sandbox, get_observability)` declaration and
  semantic resolver under `internal::migration`; excluded from the public
  document; no CLI metadata.
- [x] Replace cross-catalog disjointness tests with cross-domain
  route-uniqueness tests gated on all three features.
- [x] In `sandbox-cli`: add bidirectional projection-integrity tests and the
  compatibility-JSON fixture; forward CLI features to catalog domain
  features (`manager = ["dep:clap", "sandbox-operation-catalog/manager"]`,
  etc.).

### Acceptance criteria

- [x] Exactly one catalog package exists; no `sandbox-*-operations` package
  remains. *Evidence: `cargo metadata` package list.*
- [x] `crates/sandbox-operations/` contains exactly `contract/` and
  `catalog/` (client arrives in Phase 3): `ls crates/sandbox-operations/`.
- [x] The merged semantic document contains every public operation exactly
  once; public route keys are globally unique; route expansion is
  deterministic; every declaration has one execution owner. *Evidence:
  `cargo test -p sandbox-operation-catalog --all-features`.*
- [x] Per-binary authority closure holds:
  `cargo tree -p sandbox-cli --no-default-features --features manager -f "{p} {f}"`
  shows `sandbox-operation-catalog` with only the `manager` feature; repeat
  for `runtime` and `observability`.
- [x] The catalog's only workspace dependency is the contract.
  *Evidence: `cargo metadata`.*
- [x] The migration declaration and resolver exist under
  `internal::migration` and are excluded from the public document.
  *Evidence: catalog test.*
- [x] CLI bidirectional projection-integrity tests pass; the CLI-bearing
  compatibility JSON matches the Phase 0 fixture byte-for-byte.
- [x] Standing gate passed. (Handler bijection is deferred to Phases 4–6.)

### Progress log

| Date | Item | Command / evidence | Result | Deviations |
| --- | --- | --- | --- | --- |
| 2026-07-10 | Atomic catalog merge (changes 1–6) | `git show --stat --oneline 509022ea3`; `cargo check --workspace --all-targets --all-features`; `cargo test -p sandbox-operation-contract -p sandbox-operation-catalog --all-features`; `cargo test -p sandbox-cli --all-features --test compatibility --test projection_integrity --test help --test request_builder` | Commit `509022ea3` merged all three catalogs and deleted/repointed all legacy packages in one change; the post-commit workspace check passed; contract 12/12, catalog integrity 5/5 plus all domain suites, compatibility 2/2, projection 1/1, help 3/3, and request-builder 9/9 passed. | None. |
| 2026-07-10 | Package and namespace inventory | `cargo metadata --format-version 1 --no-deps \| jq -r '.packages[].name' \| rg 'sandbox-(operation-catalog\|manager-operations\|runtime-operations\|observability-operations)'`; `ls -1 crates/sandbox-operations` | Package output is exactly `sandbox-operation-catalog`; namespace output is exactly `catalog`, `contract`. | None. |
| 2026-07-10 | Catalog semantic integrity and migration isolation | `cargo test -p sandbox-operation-catalog --all-features` | Integrity 5/5, manager 2/2, observability 2/2, runtime 2/2; excerpts: `public_route_manifest_is_exact_and_policy_consistent ... ok`, `migration_resolver_only_rewrites_sandbox_observability_requests ... ok`, `internal_and_migration_routes_never_leak_into_public_documents ... ok`. | None. |
| 2026-07-10 | Per-binary catalog feature closure | `cargo tree -p sandbox-cli --no-default-features --features manager -f "{p} {f}"`; repeated with `runtime` and `observability` | The catalog line is respectively `sandbox-operation-catalog ... manager`, `... runtime`, and `... observability`; no other catalog domain feature appears in each tree. | None. |
| 2026-07-10 | Catalog dependency closure | `cargo metadata --format-version 1 --no-deps \| jq -r '.packages[] \| select(.name == "sandbox-operation-catalog") \| .dependencies[] \| select(.path != null) \| .name'` | Output is exactly `sandbox-operation-contract`. | None. |
| 2026-07-10 | CLI projection and compatibility | `cargo test -p sandbox-cli --all-features --test compatibility --test projection_integrity` | Compatibility 2/2 and projection integrity 1/1; excerpts: `all_feature_compatibility_catalog_matches_phase_zero_fixture ... ok`, `unknown_operation_errors_and_exit_codes_match_phase_zero_fixture ... ok`, `cli_projection_is_bidirectional_with_public_routes ... ok`. | None. |
| 2026-07-10 | Standing-gate correction: runtime file-list ownership proof | `cargo test --workspace --all-features`; `cargo test -p sandbox-runtime --test service_graph service_graph_workspace_session_source_boundaries_stay_private`; `git show --stat --oneline c7af1e4c2` | The first full run exposed one stale source-structure assertion for the pre-merge local `FILE_LIST`; the focused test passed after aligning the assertion with catalog ownership, and commit `c7af1e4c2` records the test-only correction. | None; implementation already matched the specification. |
| 2026-07-10 | Standing gate | `cargo check --workspace --all-targets --all-features`; `cargo test --workspace --all-features`; `cargo clippy --workspace --all-targets --all-features -- -D warnings`; `cargo fmt --all -- --check` | Check finished successfully; the restarted full workspace and doc-test suite passed with zero failures; clippy finished in 1m38s with no warnings; format check exited 0. | None. |

---

## Phase 3 — Extract the shared gateway client

**Entry criteria:** Phase 2 `approved` in the dashboard.

### Change list

- [ ] Create `sandbox-operation-client` at
  `crates/sandbox-operations/client/`: gateway transport (from
  `sandbox-cli/src/core/client.rs`), discovery config (from
  `sandbox-config/src/configs/cli.rs`, env-and-overrides only), and the
  value-based request builder (from the value half of
  `request_builder.rs`); callers supply resolved semantic specs; the client
  exposes the request-size bound it enforces.
- [ ] Keep argv parsing, help, output, progress, and exit-code behavior in
  `sandbox-cli`; CLI owns catalog-to-flag lookup.
- [ ] Replace the observability mapping table with independent CLI and MCP
  calls to the catalog's `internal::migration` resolver; the client stays
  independent of catalog and applications.
- [ ] Console: preserve `/api/rpc` as a fully scoped request API; validate
  the `OperationRequest` against the public route manifest (plus the
  migration declaration, transitionally); send via the client; take the
  body bound from the client.
- [ ] Repoint MCP and console dependencies to the client; remove their
  `sandbox-cli` dependency; drop `sandbox-cli`'s `sandbox-config`
  dependency; remove `configs/cli.rs` module declarations and move its
  tests to the client crate.
- [ ] Remove all direct `sandbox-protocol` dependencies/imports from CLI,
  MCP, and console.
- [ ] Split request-builder tests by their new owners.

### Acceptance criteria

- [ ] `rg 'sandbox_cli::core'` matches nothing outside
  `crates/sandbox-cli/`.
- [ ] MCP and console manifests do not depend on `sandbox-cli`; CLI, MCP,
  and console manifests do not depend on `sandbox-protocol`; `sandbox-cli`
  does not depend on `sandbox-config`. *Evidence: `cargo metadata`.*
- [ ] `rg 'sandbox_protocol' crates/sandbox-cli/src crates/sandbox-mcp/src crates/sandbox-console/src`
  returns nothing.
- [ ] The client does not import the catalog or switch on operation names:
  `rg 'sandbox_operation_catalog|get_observability' crates/sandbox-operations/client/src`
  returns nothing.
- [ ] Console rejects an operation absent from the public manifest with an
  invalid-request error, and enforces the client-owned body bound.
  *Evidence: `cargo test -p sandbox-console` (new tests).*
- [ ] CLI and MCP produce identical requests through the shared value
  builder for the same inputs. *Evidence: split builder tests green.*
- [ ] Standing gate passed.

### Progress log

| Date | Item | Command / evidence | Result | Deviations |
| --- | --- | --- | --- | --- |
| | | | | |

---

## Phase 4 — Clean the manager application in place

**Entry criteria:** Phase 3 `approved` in the dashboard.

### Change list

- [ ] Rename `operation/cli_definition` to `operations/registry`.
- [ ] Verify the `SandboxDaemonClient` port stays in manager and its
  concrete TCP implementation stays in gateway; same for
  `SandboxDaemonInstaller` / `StartedDaemon`.
- [ ] Split the live E2E raw-gateway helper from a trusted direct-daemon
  lifecycle helper: allowlist the latter to `create_workspace_session` and
  `destroy_workspace_session`, resolve its endpoint through
  `inspect_sandbox` and its token through Docker control-plane metadata
  without logging or persisting the token, convert all lifecycle call sites,
  and move the `file_list` probe to the documented HTTP endpoint.
- [ ] Make the router scope-kind-first; reject gateway-arriving requests
  whose route visibility is internal (manager services keep calling the
  daemon-client port directly for internal forwarding).
- [ ] Depend on the catalog with `manager` + `observability` features;
  import manager public specs, the observability system-snapshot
  declaration, and runtime internal forwarding identifiers from the
  catalog; delete duplicated string literals.
- [ ] Retain only the declared migration route for sandbox observability.
- [ ] Add tests: public route-subset/handler bijection for
  `execution_owner = Manager`, internal registry match, manager-router
  rejection parameterized over every canonical internal route (including
  both workspace-session lifecycle routes), and live direct-daemon
  workspace-session smoke coverage.

### Acceptance criteria

- [ ] Manager workspace dependencies are exactly: contract, catalog,
  `sandbox-runtime-layerstack`. *Evidence: `cargo metadata`.*
- [ ] `rg 'cli_definition' crates/sandbox-manager` returns nothing.
- [ ] Every system-scoped public route with `execution_owner = Manager` has
  exactly one handler. *Evidence: bijection test in
  `cargo test -p sandbox-manager`.*
- [ ] Every canonical internal route is unreachable from public dispatch;
  `export_changes` still succeeds through manager-owned internal forwarding;
  and workspace-session lifecycle smoke succeeds through the trusted direct
  daemon helper. *Evidence: parameterized manager rejection test + export
  E2E/integration run + `cd e2e && python3 -m pytest
  runtime/workspace_session/test_workspace_session.py -m smoke`.*
- [ ] `rg '"export_layerstack"|"read_export_chunk"|"squash_layerstack"' crates/sandbox-manager/src`
  returns nothing (identifiers come from the catalog).
- [ ] Standing gate passed.

### Progress log

| Date | Item | Command / evidence | Result | Deviations |
| --- | --- | --- | --- | --- |
| | | | | |

---

## Phase 5 — Clean the runtime application in place

**Entry criteria:** Phase 4 `approved` in the dashboard.

### Change list

- [ ] Rename `operation_adapter` to `operations/registry`.
- [ ] Replace protocol request/response imports with contract types
  (should already hold from Phase 1; verify and finish stragglers).
- [ ] Split the public registry from the exact canonical internal set
  (`create_workspace_session`, `destroy_workspace_session`,
  `squash_layerstack`, `export_layerstack`, `read_export_chunk`); retain
  `file_list` separately as the HTTP-only exception; key every registry by
  `(scope kind, name)`.
- [ ] Import canonical runtime internal identifiers from
  `sandbox_operation_catalog::internal::runtime` in runtime dispatch (and
  confirm manager forwarding and daemon HTTP `file_list` use the same
  identifiers).
- [ ] Add route-subset/handler bijection and internal-exclusion tests.

### Acceptance criteria

- [ ] Runtime app workspace dependencies are exactly: contract, catalog,
  the four runtime primitives it uses, and `sandbox-observability`.
  *Evidence: `cargo metadata`.*
- [ ] `rg 'operation_adapter' crates/sandbox-runtime/operation` returns
  nothing.
- [ ] Every runtime public entry and every canonical internal entry has
  exactly one handler; public and internal registries are disjoint.
  *Evidence: `cargo test -p sandbox-runtime`.*
- [ ] Canonical internal and HTTP-only operation names have one production
  definition each:
  `rg '"create_workspace_session"|"destroy_workspace_session"|"squash_layerstack"|"export_layerstack"|"read_export_chunk"|"file_list"' crates/*/src crates/*/*/src`
  matches only the catalog's `internal/runtime.rs`.
- [ ] Standing gate passed.

### Progress log

| Date | Item | Command / evidence | Result | Deviations |
| --- | --- | --- | --- | --- |
| | | | | |

---

## Phase 6 — Extract observability application, remove multiplexing

**Entry criteria:** Phase 5 `approved` in the dashboard.
**Atomic steps:** (a) the namespace conversion is one commit; (b) the
multiplexer cutover (client + manager + daemon + console + web + tests) is
one commit.

### Change list

- [ ] Namespace conversion (one commit): relocate the primitives package to
  `crates/sandbox-observability/primitives/` (package name, crate name, and
  content unchanged) and create `sandbox-observability-application` at
  `crates/sandbox-observability/application/`; update workspace member
  paths, `[workspace.dependencies]` path entries, and the
  `bin/start-sandbox-docker-gateway` freshness watch.
- [ ] Extract structured query/response behavior from
  `sandbox-daemon/src/observability/{service,layerstack,view/**}` into the
  application (`query.rs`, `registry.rs`, `response.rs`, `ports.rs`);
  sampling, rotation, and lifecycle stay in daemon.
- [ ] Define the app-owned input port with neutral DTOs for the runtime
  snapshot types; use `sandbox-observability` primitives
  (`Reader`/`RawFilter`) and `sandbox-runtime-layerstack` data types
  (`LayerRef`, `StackObservation`, `LayerDeltaDescription`,
  `LayerDeltaEntryKind`) directly; add the daemon-owned adapter newtype.
- [ ] Move the pure query/structured-response tests from
  `sandbox-daemon/tests/unit/{observability,observability_layerstack}.rs`;
  keep daemon wiring/lifecycle tests in place.
- [ ] Route the six declared `(scope kind, operation)` combinations from
  the manifest: `(system, snapshot)` → manager; `(sandbox, snapshot/trace/
  events/cgroup/layerstack)` → observability application in the daemon.
- [ ] Multiplexer cutover (one commit): delete `get_observability` and the
  synthetic `view` argument from the CLI request path, manager
  (`observability_snapshot`), daemon dispatch, the catalog's
  `internal::migration` module, console validation,
  `web/console/src/api/observability.ts`, MCP tests, and E2E expectations;
  console validation becomes public-manifest-only.
- [ ] Add the shadowing proof: system `snapshot` and sandbox `snapshot`
  route to different owners and cannot shadow each other.

### Acceptance criteria

- [ ] `crates/sandbox-observability/` contains exactly `primitives/` and
  `application/`; the workspace builds with the relocated path.
  *Evidence: `ls` + `cargo metadata`.*
- [ ] Observability application workspace dependencies are exactly:
  contract, catalog, `sandbox-observability`, `sandbox-runtime-layerstack`.
  *Evidence: `cargo metadata`.*
- [ ] `rg 'get_observability'` across `crates/`, `web/console/src`, and
  `e2e/` returns nothing; `rg '"view"'` shows no synthetic observability
  routing.
- [ ] All six routes are served under their concrete names end-to-end;
  the shadowing test passes. *Evidence:
  `cargo test -p sandbox-observability-application -p sandbox-daemon -p sandbox-manager`.*
- [ ] Console `/api/rpc` envelope unchanged; observability `op` values are
  concrete; validation is public-only. *Evidence: console tests +
  comparison against Phase 0 fixtures.*
- [ ] Outward behavior matches the Phase 0 characterization baseline modulo
  the approved behavior changes. *Evidence: fixture diff.*
- [ ] `bin/start-sandbox-docker-gateway --rebuild-binary` succeeds with the
  updated freshness watch.
- [ ] Standing gate passed.

### Progress log

| Date | Item | Command / evidence | Result | Deviations |
| --- | --- | --- | --- | --- |
| | | | | |

---

## Phase 7 — Update documentation, scripts, law statements

**Entry criteria:** Phase 6 `approved` in the dashboard.

### Change list

- [ ] Update root `README.md` (component table, boundary law, pieces list),
  `CLAUDE.md`, `AGENTS.md`, `docs/README/sandbox-runtime.md`,
  `docs/daemon-http/README.md`, console README, E2E README/RUNNING, package
  docs, and
  `docs/obsidian/ephemeral-os/docs/{cli-gateway-manager-runtime.md,ephemeral-os.md}`:
  protocol no longer owns operation vocabulary; adapters no longer use
  `sandbox_cli::core`; catalog paths, namespace directories, and `e2e/` are
  current.
- [ ] Replace every `cargo -p sandbox-*-operations` selector in scripts/CI
  with `cargo -p sandbox-operation-catalog`.
- [ ] Verify the freshness watch covers
  `crates/sandbox-operations/{contract,catalog}` and
  `crates/sandbox-observability/{primitives,application}`.
- [ ] Repoint E2E source assertions
  (`e2e/manager/management/squash/helpers.py`) to
  `crates/sandbox-operations/catalog/src/manager.rs` and
  `crates/sandbox-manager/src/operations/registry/...`.
- [ ] Mark historical plans/reports as superseded or exempt; do not rewrite
  historical evidence.

### Acceptance criteria

- [ ] No normative doc, executable script, manifest, or CI reference names
  a deleted package, an old catalog path, `sandbox_cli::core`, or the old
  E2E location:
  `rg 'sandbox-manager-operations|sandbox-runtime-operations|sandbox-observability-operations|sandbox_cli::core|cli-operation-e2e-live-test' --glob '!docs/obsidian/**/implementation_plan/**' --glob '!**/target/**'`
  returns only explicitly historical documents.
- [ ] Boundary-law statements in `README.md`/`CLAUDE.md` match the
  specification's dependency law and namespace list. *Evidence: doc diff.*
- [ ] `bin/start-sandbox-docker-gateway --rebuild-binary` and the E2E
  source-assertion tests pass after repointing.
- [ ] Standing gate passed.

### Progress log

| Date | Item | Command / evidence | Result | Deviations |
| --- | --- | --- | --- | --- |
| | | | | |

---

## Phase 8 — Enforce boundaries and cut over

**Entry criteria:** Phase 7 `approved` in the dashboard.

### Change list

- [ ] Implement `cargo run -p xtask -- operation-architecture-check`
  backed by `cargo metadata`, enforcing: the layer map with deny-by-default
  package classification; the dependency allowlist over normal, dev, build,
  and optional edges; the single-catalog invariant; per-binary catalog
  feature closure; CLI-metadata confinement; bidirectional projection
  completeness; public/internal route completeness and disjointness;
  visibility chokepoints; the naming policy (with the two grandfathered
  paths `crates/sandbox-runtime/operation` and
  `crates/sandbox-observability/primitives`); and every stale-reference
  gate in the specification.
- [ ] Add checker self-tests proving it fails on: a forbidden edge, an
  unmapped package, a missing/extra handler, a public/internal overlap, a
  missing projection entry, an out-of-closure feature, and a stale path.
- [ ] Measure all 20 crates plus `xtask` with the LOC rule; replace the
  specification's planning ranges with measured values; verify the
  allocation accounts for moved/deleted production source.
- [ ] Verify Phase 2 deletions remain complete; delete any stale
  re-exports, old package-name references, and temporary migration code.
- [ ] Update the specification's acceptance checkboxes from evidence rows
  in this plan; flip the spec status from `draft` to adopted and mark the
  legacy CLI migration plan superseded.

### Acceptance criteria

- [ ] `cargo run -p xtask -- operation-architecture-check` passes, and its
  self-tests demonstrate each failure mode. *Evidence: check output + test
  run.*
- [ ] Full structural matrix passes:
  `cargo metadata`, architecture check, `cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-targets --all-features -- -D warnings`,
  `cargo test --workspace --all-features`,
  `cargo test -p sandbox-operation-catalog --all-features`,
  per-binary `cargo tree` closure checks.
- [ ] Adapter matrix passes: `cargo test -p sandbox-cli --all-features`,
  `-p sandbox-mcp`, `-p sandbox-console`,
  `npm --prefix web/console ci && npm --prefix web/console run build`;
  outputs match the Phase 0 baseline modulo the four approved behavior
  changes.
- [ ] Live proof recorded: full `e2e/` suite per `RUNNING.md` (no subset),
  `bin/start-sandbox-docker-gateway --rebuild-binary`, and the final smoke
  covering one manager operation, one runtime operation, a system-scoped
  snapshot, a sandbox-scoped observability query, an MCP tool call, and a
  console RPC call.
- [ ] `cargo metadata` reports exactly the 20 crate manifest paths from the
  specification's LOC tables plus `xtask/Cargo.toml`.
- [ ] The specification's LOC tables carry measured values; every spec
  acceptance checkbox is checked with a link to an evidence row here.
- [ ] Standing gate passed.

### Progress log

| Date | Item | Command / evidence | Result | Deviations |
| --- | --- | --- | --- | --- |
| | | | | |

---

## Deviation register

Deviations that required amending the specification. Every row must name
the spec section changed in the same commit.

| Date | Phase | Deviation | Spec section amended | Approved by |
| --- | --- | --- | --- | --- |
| 2026-07-10 | 0 | The original design enumerated forwarding internals but omitted the dispatchable `create_workspace_session` and `destroy_workspace_session` lifecycle routes. Classify them as canonical runtime-internal routes and replace live E2E public-gateway access with an allowlisted trusted direct-daemon harness. | Target tree; route taxonomy; Visibility enforcement chokepoints; Phase 5 — Clean the runtime application in place | Codex, 2026-07-10 |
| | | | | |
