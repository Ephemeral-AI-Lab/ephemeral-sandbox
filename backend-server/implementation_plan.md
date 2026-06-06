# backend-server Implementation Plan

Status: draft

Source spec: `backend-server/SPEC.md`

Rule: a phase is complete only when every checklist item in that phase is
checked, the listed verification commands pass, and any skipped item is recorded
as an explicit spec change. Do not start a dependent phase while its predecessor
has an unchecked hard item.

## Progress Tracker

| Phase | Scope | Status | Blocks | Exit proof |
|---|---|---|---|---|
| 0 | Baseline, ownership audit, and migration guardrails | complete | all phases | Phase 0 notes below |
| 1 | Workspace scaffold and crate relocation | complete | 2, 3, 4, 5, 6, 7 | `agent-core` has port-only sandbox deps; backend workspace builds |
| 2 | Agent-core runtime seams | not_started | 4, 5, 6, 7 | production `SandboxGateway` injection and `state_reader()` compile |
| 3 | Backend config, types, store, and migrations | complete | 4, 5, 6, 7 | config/store tests pass with backend DB schema |
| 4 | Sandbox lifecycle manager | not_started | 5, 6, 7 | lifecycle/refcount/delete-guard tests pass |
| 5 | Run launcher, cancellation, reaper, and event bus | not_started | 6, 7 | request launch and replay-safe event persistence tests pass |
| 6 | Observability, audit ingestion, and stats | not_started | 7 | audit/correlation/stats tests pass |
| 7 | HTTP API, streaming API, and OpenAPI | not_started | 8 | API contract and stream replay tests pass |
| 8 | Live E2E, dependency audit, and closeout | not_started | release | Docker-backed backend-to-agent-core-to-sandbox smoke passes |

Status values: `not_started`, `in_progress`, `blocked`, `complete`.

## Acceptance Criteria Map

| Spec AC | Enforced in phases |
|---|---|
| AC1 backend accepts request, binds sandbox, launches agent-core, streams, persists lifecycle | 4, 5, 7, 8 |
| AC2 agent-core has no backend/Docker/daemon-bootstrap deps; only `eos-sandbox-port` | 1, 2, 8 |
| AC3 sandbox builds independently with no backend/agent-core deps | 1, 8 |
| AC4 sandbox API never exposes daemon endpoint, port, or auth token | 3, 4, 7 |
| AC5 event persistence is async-drained and replay-safe | 5, 7 |
| AC6 `AuditSink` persistence is async-drained and non-blocking | 6 |
| AC7 audit/stats keep `tool_use_id` and `sandbox_invocation_id` separate | 2, 3, 6 |
| AC8 audit cursor uses `boot_epoch_id` | 3, 6 |
| AC9 backend reads agent-core state through `RuntimeServices::state_reader()` | 2, 7 |
| AC10 v1 supports only `sandbox_id` as per-request sandbox override | 4, 7 |
| AC11 decentralized config ownership | 3 |
| AC12 multi-crate backend structure is preserved unless a crate lacks ownership | 1, 8 |
| AC13 backend crate test-related files live under `backend-server/crates/<crate>/tests/`, not `src/` | 2, 3, 4, 5, 6, 7, 8 |

## Phase 0 - Baseline And Guardrails

Goal: freeze the live starting point, identify concurrent work, and make the
implementation accountable to `SPEC.md`.

Implementation components:

| Component | Work |
|---|---|
| Worktree audit | Record current `git status --short` and distinguish unrelated concurrent work. |
| Spec anchor | Confirm `backend-server/SPEC.md` contains the API, gateway, event, audit, DB, folder tree, build order, and acceptance sections. |
| Dependency baseline | Record current `agent-core`, `sandbox`, and `backend-server` crate layout before moving code. |
| Verification baseline | Run the narrowest current checks that can run before backend crates exist. |

Hard acceptance checklist:

- [x] `git status --short` is captured in the phase notes.
- [x] No unrelated user/agent changes are reverted or overwritten.
- [x] `backend-server/SPEC.md` passes `git diff --check`.
- [x] `backend-server/implementation_plan.md` passes `git diff --check`.
- [x] Current dependency edges are recorded with `cargo metadata` or targeted
  `rg` scans before relocation starts.
- [x] The phase notes list any pre-existing build failures separately from this
  implementation plan.

Verification commands:

```sh
git status --short
git diff --check -- backend-server/SPEC.md backend-server/implementation_plan.md
rg -n "eos-sandbox-host|eos-protocol|eos-sandbox-api|eos-sandbox-port" agent-core/crates -g Cargo.toml
(cd agent-core && cargo check -p eos-runtime --all-targets)
(cd sandbox && cargo check --workspace --all-targets)
```

Exit gate: the live baseline is documented, and any failure is classified as
pre-existing, introduced, or blocked by concurrent work.

Phase 0 completion notes:

| Item | Result |
|---|---|
| Completed | 2026-06-06 |
| `git status --short` | Clean at verification time. |
| `git diff --check -- backend-server/SPEC.md backend-server/implementation_plan.md` | Passed. |
| Dependency baseline | `agent-core` still has pre-implementation `eos-sandbox-api`, `eos-sandbox-host`, and `eos-obs-collector` / `eos-protocol` edges. Phase 1 owns removing or relocating those edges. |
| `cd agent-core && cargo check -p eos-runtime --all-targets` | Passed. |
| `cd sandbox && cargo check --workspace --all-targets` | Passed. |
| Pre-existing build failures | None observed in Phase 0 verification. |

## Phase 1 - Workspace Scaffold And Crate Relocation

Goal: create the backend workspace shape from `SPEC.md` and move sandbox host
implementation dependencies out of `agent-core`.

Implementation components:

| Component | Work |
|---|---|
| Backend workspace | Add `backend-server/Cargo.toml` and crate manifests for `eos-backend-types`, `eos-backend-config`, `eos-backend-store`, `eos-backend-runtime`, `eos-backend-obs`, `eos-backend-api`, and `eos-backend-main`. |
| Port crate | Add or rename to `agent-core/crates/eos-sandbox-port` with `gateway.rs`, `transport.rs`, `provision.rs`, and `tool_api.rs`. |
| Host relocation | Move `eos-sandbox-host` under `backend-server/crates/` without behavior changes. |
| Obs collector relocation | Move `eos-obs-collector` under `backend-server/crates/` so `agent-core` no longer needs `eos-protocol` for obs normalization. |
| Dependency repair | Update workspace manifests and imports while preserving Rust 2021 / `rust-version = "1.85"`. |

Hard acceptance checklist:

- [x] Backend workspace manifests exist for every crate named in `SPEC.md`.
- [x] `agent-core` no longer depends on `eos-sandbox-host`.
- [x] `agent-core` no longer depends on `eos-protocol` through the obs collector.
- [x] `eos-tools`, `eos-engine`, and `eos-runtime` depend on `eos-sandbox-port`
  for sandbox contracts.
- [x] `sandbox/crates/*` has no dependency on `backend-server` or `agent-core`.
- [x] The move is behavior-preserving: no sandbox lifecycle behavior is changed
  in this phase beyond import/path updates.
- [x] `eos-sandbox-port` public errors and names use port vocabulary, not stale
  API vocabulary, unless a compatibility re-export is documented.

Verification commands:

```sh
rg -n "eos-sandbox-host|eos-protocol" agent-core/crates -g Cargo.toml
rg -n "backend-server|agent-core" sandbox/crates -g Cargo.toml
(cd agent-core && cargo check -p eos-runtime --all-targets)
(cd backend-server && cargo check --workspace --all-targets)
(cd sandbox && cargo check --workspace --all-targets)
```

Exit gate: AC2, AC3, and AC12 dependency claims are true at the manifest level
and the moved crates compile from their new owning workspace.

## Phase 2 - Agent-Core Runtime Seams

Goal: expose the narrow production seams backend needs without moving backend
policy into `agent-core`.

Implementation components:

| Component | Work |
|---|---|
| `SandboxGateway` | Define the object-safe gateway in `eos-sandbox-port` with `transport()` and `provisioner()` accessors. |
| Provisioner seam | Make production runtime construction accept a `SandboxGateway` or production-visible provisioner injection. |
| Runtime state reader | Add `RuntimeServices::state_reader()` returning narrow store handles. |
| Store list APIs | Add `RequestStore::list`, `TaskStore::list_for_request`, and `AgentRunStore::get_for_task`. |
| Correlation source | Ensure sandbox tool execution can persist or emit `tool_use_id`, `sandbox_invocation_id`, `caller_id`, `sandbox_id`, `request_id`, `task_id`, and `agent_run_id` without collapsing identities. |
| Backend test layout cleanup | Move any existing backend-server crate tests, fixtures, fakes, mocks, support modules, or harnesses out of `src/` and into `backend-server/crates/<crate>/tests/`. The known Phase 1 carry-over is `backend-server/crates/eos-sandbox-host/src/daemon_client/tests/`, which must move to `backend-server/crates/eos-sandbox-host/tests/daemon_client/`. |

Hard acceptance checklist:

- [ ] `SandboxGateway` is object-safe and lives in `eos-sandbox-port`.
- [ ] Runtime construction can receive backend's gateway in non-test builds.
- [ ] Runtime construction no longer relies on a `#[cfg(test)] pub(crate)`
  provisioner setter for production composition.
- [ ] `RuntimeServices::state_reader()` is public enough for backend use and
  returns store traits, not `sqlx::SqlitePool`.
- [ ] Store list/query APIs are implemented in `eos-state` and `eos-db`.
- [ ] Model-facing `tool_use_id` and daemon-facing `sandbox_invocation_id` are
  represented as separate values in the runtime/tool path.
- [ ] Existing root request and delegated workflow behavior is unchanged.
- [ ] No backend-server crate has test bodies or test support under `src/`.
- [ ] `backend-server/crates/eos-sandbox-host/src/daemon_client/tests/` has
  been moved to `backend-server/crates/eos-sandbox-host/tests/daemon_client/`
  or removed if its tests were deleted intentionally.

Verification commands:

```sh
(cd agent-core && cargo check -p eos-sandbox-port --all-targets)
(cd agent-core && cargo check -p eos-runtime --all-targets)
(cd agent-core && cargo test -p eos-db)
(cd agent-core && cargo test -p eos-state)
rg -n "pub\(crate\).*provisioner|SqlitePool" agent-core/crates/eos-runtime/src
find backend-server/crates \( -path '*/src/*test*' -o -path '*/src/*fixture*' -o -path '*/src/*mock*' -o -path '*/src/*fake*' -o -path '*/src/*support*' \) -print
```

Exit gate: AC2, AC7, AC9, and AC13 are implementable through typed agent-core
contracts without backend raw SQL access, host crate imports, or backend test
files under `src/`.

## Phase 3 - Backend Config, Types, Store, And Migrations

Goal: add backend-owned DTOs, config, error types, and persistent state before
runtime/API code depends on them.

Implementation components:

| Component | Work |
|---|---|
| Types | Implement `BackendRunStatus`, `RunMeta`, `SandboxView`, pagination, API request/response DTOs, event DTOs, audit DTOs, and stats DTOs. |
| Config | Implement `ServerConfig`, `AgentCoreConfigSource`, `SandboxConfig`, and `ObsConfig` with `backend.yml < local.yml` loading. |
| Store | Implement `run_meta`, `event_log`, `obs_event`, `sandbox_call_correlation`, and `audit_cursor` repositories. |
| Migrations | Add `0001_initial.sql` with the exact tables required by `SPEC.md`. |
| Sanitization | Ensure public sandbox DTOs cannot serialize daemon host, port, internal port, endpoint, auth token, or raw daemon env. |

Hard acceptance checklist:

- [x] `ServerConfig` does not embed `ProvidersConfig` or `WorkflowConfig`.
- [x] Backend config owns only backend deployment and sandbox lifecycle defaults.
- [x] `run_meta` schema contains `status`, `created_at`, `finished_at`, and
  `cancel_reason`.
- [x] `obs_event` contains both `tool_use_id` and `sandbox_invocation_id`.
- [x] `sandbox_call_correlation` has primary key
  `(sandbox_id, caller_id, sandbox_invocation_id)`.
- [x] `audit_cursor.boot_epoch_id` is an integer column.
- [x] `SandboxView` has no credential-bearing fields.
- [x] Store tests prove round-trip persistence for every table.
- [x] `eos-backend-types`, `eos-backend-config`, and `eos-backend-store` keep
  all test files, fixtures, fakes, mocks, and support modules under their crate
  `tests/` directories.

Verification commands:

```sh
(cd backend-server && cargo test -p eos-backend-types)
(cd backend-server && cargo test -p eos-backend-config)
(cd backend-server && cargo test -p eos-backend-store)
rg -n "auth_token|internal_port|DaemonTcpEndpoint|endpoint" backend-server/crates/eos-backend-{types,api,store}
find backend-server/crates/eos-backend-{types,config,store}/src \( -name '*test*' -o -name '*fixture*' -o -name '*mock*' -o -name '*fake*' -o -name '*support*' \) -print
```

Exit gate: AC4, AC7, AC8, AC10, AC11, and AC13 are encoded in backend-owned
types and schema before runtime/API code is built on top.

## Phase 4 - Sandbox Lifecycle Manager

Goal: make backend-server the owner of sandbox setup, binding, refcounting,
delete policy, and teardown.

Implementation components:

| Component | Work |
|---|---|
| `SandboxManager` | Implement backend-owned manager around `eos-sandbox-host` lifecycle, registry, and provisioner. |
| Gateway implementation | Make `SandboxManager` implement `SandboxGateway`. |
| Refcounting | Track active request refs and retained sandbox refs. |
| Delete guards | Reject deletion while active or retained runs reference a sandbox. |
| Sanitized views | Generate `SandboxView` for list/detail APIs without daemon credentials. |
| V1 sandbox args | Support only existing `sandbox_id` binding; do not add image/snapshot/project-dir overrides. |

Hard acceptance checklist:

- [ ] `SandboxManager` owns setup/destroy policy and no agent-core crate owns
  Docker lifecycle policy.
- [ ] `SandboxManager::transport()` and `SandboxManager::provisioner()` share
  the same registry/lifecycle state.
- [ ] Active run acquisition increments sandbox refcount.
- [ ] Run completion/reaper release decrements sandbox refcount exactly once.
- [ ] Delete rejects active or retained sandboxes.
- [ ] Delete never requires or returns daemon auth material.
- [ ] Request-scoped sandbox override accepts only `sandbox_id`.
- [ ] Manager tests cover create, bind existing, release, delete rejection,
  destroy-on-finish, and sanitized view generation.
- [ ] Runtime and sandbox-manager tests live under
  `backend-server/crates/eos-backend-runtime/tests/`, not under `src/`.

Verification commands:

```sh
(cd backend-server && cargo test -p eos-backend-runtime sandbox_manager)
(cd backend-server && cargo check -p eos-backend-runtime --all-targets)
rg -n "image|snapshot|project_dir" backend-server/crates/eos-backend-runtime backend-server/crates/eos-backend-api
find backend-server/crates/eos-backend-runtime/src \( -name '*test*' -o -name '*fixture*' -o -name '*mock*' -o -name '*fake*' -o -name '*support*' \) -print
```

Exit gate: AC1, AC4, AC10, AC13, and the lifecycle half of AC2 are satisfied by
backend-owned runtime code.

## Phase 5 - Run Launcher, Cancellation, Reaper, And Event Bus

Goal: launch requests through `agent-core`, persist backend lifecycle, and make
streaming replay-safe.

Implementation components:

| Component | Work |
|---|---|
| `RunLauncher` | Accept API input, create `run_meta`, acquire sandbox binding, build runtime input, and run `eos_runtime::run_request`. |
| Status resolution | Apply `BackendRunStatus` precedence over agent-core `RequestStatus` exactly as specified. |
| Cancellation | Record backend-local cancellation and signal runtime/reaper cleanup without writing `cancelled` into agent-core state. |
| Reaper | Release sandbox refs, set `finished_at`, and finalize destroy-on-finish. |
| EventBus | Use bounded sync callback enqueue, async drainer, persist-before-broadcast, and high-water replay handoff. |
| Loss markers | Persist or expose `event_stream_gap` when milestone events are dropped. |

Hard acceptance checklist:

- [ ] `POST /api/user-requests` can be backed by `RunLauncher` without direct
  HTTP code in runtime.
- [ ] `run_meta` is written before the runtime task starts.
- [ ] Accepted/running/done/failed/cancelled precedence rules are tested.
- [ ] Cancellation never writes `cancelled` into agent-core `RequestStatus`.
- [ ] Event callback does no async SQLite writes and holds no async locks.
- [ ] Event queue is bounded and has a visible overflow/loss policy.
- [ ] Event drainer persists before broadcasting.
- [ ] Reconnect tests prove no milestone event can fall between replay and live
  subscription.
- [ ] Reaper releases sandbox refs once even when runtime fails or cancellation
  races with completion.
- [ ] Launcher, event bus, and reaper tests live under
  `backend-server/crates/eos-backend-runtime/tests/`, not under `src/`.

Verification commands:

```sh
(cd backend-server && cargo test -p eos-backend-runtime launcher)
(cd backend-server && cargo test -p eos-backend-runtime event_bus)
(cd backend-server && cargo test -p eos-backend-runtime reaper)
rg -n "\\.await|SqlitePool" backend-server/crates/eos-backend-runtime/src/event_bus.rs
find backend-server/crates/eos-backend-runtime/src \( -name '*test*' -o -name '*fixture*' -o -name '*mock*' -o -name '*fake*' -o -name '*support*' \) -print
```

Exit gate: AC1, AC5, and AC13 are proven in backend runtime tests.

## Phase 6 - Observability, Audit Ingestion, And Stats

Goal: persist audit/obs without blocking engine hot paths and join daemon audit
to model-facing tool calls through an explicit bridge.

Implementation components:

| Component | Work |
|---|---|
| `PersistingSink` | Implement `AuditSink` with owned bounded enqueue and async drainer. |
| Correlation writer | Persist `sandbox_call_correlation` before or atomically with sandbox tool calls. |
| Audit ingestor | Pull daemon audit, track `boot_epoch_id`, reset/mark loss on epoch change, and persist unmatched events safely. |
| Stats queries | Implement performance, correctness, agent-runs, and events stats over backend DB plus `state_reader()` data. |
| Loss accounting | Record dropped audit counts, unmatched audit rows, and cursor-loss markers. |

Hard acceptance checklist:

- [ ] `PersistingSink::publish` returns quickly and never awaits.
- [ ] Full audit queue returns `AuditError` and increments dropped-audit count.
- [ ] Drainer owns the payload and does not store borrowed `AuditEvent`
  references.
- [ ] `sandbox_call_correlation` is written for sandbox calls before daemon
  request dispatch.
- [ ] Audit ingestor never copies `sandbox_invocation_id` into `tool_use_id`.
- [ ] Unmatched audit rows persist with null model-facing IDs and an unmatched
  marker.
- [ ] `boot_epoch_id` change resets cursor or records loss before advancing.
- [ ] Stats tests cover matched audit, unmatched audit, queue overflow, drainer
  failure, and daemon reboot.
- [ ] Observability and store support tests live under
  `backend-server/crates/eos-backend-obs/tests/` or
  `backend-server/crates/eos-backend-store/tests/`, not under `src/`.

Verification commands:

```sh
(cd backend-server && cargo test -p eos-backend-obs)
(cd backend-server && cargo test -p eos-backend-store obs audit_cursor sandbox_call_correlation)
rg -n "tool_use_id.*sandbox_invocation_id|sandbox_invocation_id.*tool_use_id" backend-server/crates
find backend-server/crates/eos-backend-{obs,store}/src \( -name '*test*' -o -name '*fixture*' -o -name '*mock*' -o -name '*fake*' -o -name '*support*' \) -print
```

Exit gate: AC6, AC7, AC8, and AC13 are proven by focused backend obs/store
tests.

## Phase 7 - HTTP API, Streaming API, And OpenAPI

Goal: expose the user-facing backend API without leaking internal runtime or
daemon details.

Implementation components:

| Component | Work |
|---|---|
| Router | Build axum router, shared app state, error mapping, and OpenAPI shape. |
| User request API | Implement create/list/detail/cancel/events/stream/task tree/transcript endpoints. |
| Sandbox API | Implement list/detail/delete over sanitized `SandboxView`. |
| Stats API | Implement performance, correctness, agent-runs, and events endpoints. |
| Streaming | Implement SSE and WebSocket using event-log replay and high-water handoff. |
| API tests | Add API contract tests for status precedence, sanitization, pagination, cancellation, and replay. |

Hard acceptance checklist:

- [ ] Every path in `SPEC.md` exists with plural conventional resource names.
- [ ] No route uses `/api/user-request={id}` style paths.
- [ ] `POST /api/user-requests` accepts only v1 sandbox override
  `sandbox_args.sandbox_id`.
- [ ] `/api/sandboxes/*` cannot serialize daemon host, port, internal port,
  endpoint, auth token, or daemon env.
- [ ] User request detail joins backend lifecycle with agent-core state through
  `RuntimeServices::state_reader()`.
- [ ] SSE and WebSocket replay use persisted events and cannot miss events at
  replay/live handoff.
- [ ] API errors do not expose internal daemon credentials or raw SQL errors.
- [ ] OpenAPI/contract tests pin request/response shapes.
- [ ] API, stream, OpenAPI, handler, fixture, and support tests live under
  `backend-server/crates/eos-backend-api/tests/`, not under `src/`.

Verification commands:

```sh
(cd backend-server && cargo test -p eos-backend-api)
(cd backend-server && cargo test -p eos-backend-api api_contract)
(cd backend-server && cargo test -p eos-backend-api stream)
rg -n "user-request=|DaemonTcpEndpoint|auth_token|internal_port|endpoint" backend-server/crates/eos-backend-api
find backend-server/crates/eos-backend-api/src \( -name '*test*' -o -name '*fixture*' -o -name '*mock*' -o -name '*fake*' -o -name '*support*' \) -print
```

Exit gate: AC1, AC4, AC5, AC9, AC10, and AC13 are exposed through tested API
routes.

## Phase 8 - Live E2E, Dependency Audit, And Closeout

Goal: prove the full backend-server -> agent-core -> sandbox flow and close any
dependency or documentation drift.

Implementation components:

| Component | Work |
|---|---|
| Live smoke | Start backend-server, create a user request, stream events, verify agent-core completion, and inspect sandbox cleanup. |
| Docker sandbox proof | Use Docker-backed live sandbox image `sweevo-dask__dask-10042:latest` unless a later spec names a different image. |
| Dependency audit | Re-run dependency scans for agent-core, sandbox, and backend-server. |
| API proof | Exercise sandbox list/detail/delete, stats endpoints, and reconnect replay. |
| Documentation closeout | Update progress tracker statuses and note any deliberate deviations from `SPEC.md`. |

Hard acceptance checklist:

- [ ] Backend binary starts from `backend-server/crates/eos-backend-main`.
- [ ] `POST /api/user-requests` returns `202` with `request_id`.
- [ ] Request streams milestone events and completes through real `agent-core`.
- [ ] Sandbox is created or bound by backend and released by reaper.
- [ ] `/api/sandboxes/{sandbox_id}` returns sanitized `SandboxView`.
- [ ] Stats expose matched audit rows without id collapse.
- [ ] Reconnect with `last_seq` replays persisted events without gaps.
- [ ] Docker live E2E uses `EOS_LIVE_E2E_IMAGE=sweevo-dask__dask-10042:latest`.
- [ ] `agent-core` dependency scan shows no backend or host implementation deps.
- [ ] `sandbox` dependency scan shows no `agent-core` or `backend-server` deps.
- [ ] Progress tracker is updated to `complete` for all phases that passed.
- [ ] Final backend test-layout audit prints no paths under
  `backend-server/crates/*/src/`.

Verification commands:

```sh
(cd backend-server && cargo check --workspace --all-targets)
(cd backend-server && cargo test --workspace)
(cd agent-core && cargo check --workspace --all-targets)
(cd sandbox && cargo check --workspace --all-targets)
(cd backend-server && EOS_LIVE_E2E_IMAGE=sweevo-dask__dask-10042:latest \
  cargo test -p eos-backend-main --test live_e2e -- --ignored)
rg -n "eos-sandbox-host|eos-protocol|backend-server" agent-core/crates -g Cargo.toml
rg -n "agent-core|backend-server" sandbox/crates -g Cargo.toml
find backend-server/crates \( -path '*/src/*test*' -o -path '*/src/*fixture*' -o -path '*/src/*mock*' -o -path '*/src/*fake*' -o -path '*/src/*support*' \) -print
```

Exit gate: AC1 through AC13 are either proven by tests or recorded as explicit
spec deviations with a follow-up plan.

## Cross-Phase Rules

- Keep `lib.rs`, `main.rs`, and `mod.rs` thin. Implementation modules should
  split by actual ownership, not arbitrary line caps.
- Do not add new runtime trait objects unless substitution is required for a
  provider, plugin, test double, or heterogeneous open set.
- Do not add per-request image, snapshot, project-dir, workflow, provider, or
  tool-config overrides in v1.
- Do not expose daemon credentials in any public API DTO.
- Do not write async SQLite from synchronous callbacks.
- Do not use raw SQL access to agent-core DB from backend-server.
- Do not collapse `tool_use_id`, `sandbox_invocation_id`, `caller_id`, or
  `agent_run_id`.
- Do not create sandbox back-dependencies into `agent-core` or backend-server.
- Do not put backend-server tests, fixtures, fakes, mocks, support modules, or
  harnesses under `backend-server/crates/*/src/`; use each crate's `tests/`
  tree, with a narrow `#[path = ...]` declaration in `src/` only when private
  module access is required.
  In this checkout, the required absolute prefix is
  `/Users/yifanxu/machine_learning/LoVC/EphemeralOS/backend-server/crates/<crate>/tests/`.
- Do not revert unrelated concurrent work while moving crates or updating
  manifests.

## Phase Notes Template

Use this template when executing each phase:

```text
Phase:
Status:
Started:
Completed:
Touched files:
Concurrent work observed:
Checklist results:
Verification commands:
Failures:
Spec deviations:
Next phase unblockers:
```

## Phase 1 Execution Notes

```text
Phase: 1 - Workspace Scaffold And Crate Relocation
Status: complete
Touched files:
  - Renamed agent-core/crates/eos-sandbox-api -> eos-sandbox-port (git mv);
    global rename eos_sandbox_api->eos_sandbox_port, SandboxApiError->SandboxPortError
    across all agent-core consumers (eos-tools, eos-engine, eos-plugin-catalog,
    eos-testkit, eos-runtime) and snapshot `source:` headers.
  - New eos-sandbox-port/src/provision.rs: RequestProvisioner trait,
    RequestSandboxBinding, SandboxProvisionError (moved out of eos-sandbox-host);
    re-exported from lib.rs.
  - eos-runtime builder.rs: removed host construction (DaemonClient/
    DockerProviderAdapter/ProviderRegistry/SandboxLifecycle/RequestSandboxProvisioner);
    added Unconfigured{SandboxTransport,Provisioner} erroring defaults (mirror
    UnconfiguredLlmClient); deleted orphaned seed_default_sandbox_provider,
    default_eosd_artifact_dir + its test; removed now-dead workspace_root field/
    setter/local and its two test call sites. sandbox.rs/lib.rs/tests support
    repointed to eos-sandbox-port. Dropped eos-sandbox-host dep.
  - eos-sandbox-host provisioning.rs/lib.rs: consume the port trait, map
    SandboxHostError -> SandboxProvisionError at the trait impl boundary.
  - git mv eos-sandbox-host + eos-obs-collector -> backend-server/crates/.
  - agent-core/Cargo.toml: dropped eos-sandbox-host/eos-obs-collector members +
    path deps; dropped now-orphaned eos-protocol, bollard, tar workspace deps.
    Scrubbed the stale "eos-sandbox-host" comment in eos-tools/Cargo.toml.
  - New backend-server/Cargo.toml workspace + 7 stub crates (eos-backend-{types,
    config,store,runtime,obs,api,main}); external dep versions and lint/profile
    blocks mirrored from agent-core; cross-workspace path deps into
    agent-core (eos-sandbox-port/eos-types/eos-audit) and sandbox (eos-protocol).
Concurrent work observed: another agent marked Phase 0 complete and owns the
  Phase 0 notes; left untouched. Pre-existing untracked `.omc/` tool-state dirs
  were carried by git mv and left as-is (not source, not mine).
Checklist results: all 7 Phase 1 hard items checked.
Verification commands (all pass):
  - rg -n "eos-sandbox-host|eos-protocol" agent-core/crates -g Cargo.toml -> empty
  - rg -n "backend-server|agent-core" sandbox/crates -g Cargo.toml -> empty
  - (cd agent-core && cargo check -p eos-runtime --all-targets) -> ok
  - (cd agent-core && cargo test -p eos-runtime) -> 22 passed
  - (cd agent-core && cargo check --workspace --all-targets) -> ok
  - (cd backend-server && cargo check --workspace --all-targets) -> ok
  - (cd sandbox && cargo check --workspace --all-targets) -> ok
  - cargo clippy on eos-sandbox-port, eos-runtime, backend-server workspace -> clean
  - cargo tree -p eos-runtime (no-dev): links eos-sandbox-port, not host/obs/protocol
Failures: none.
Spec deviations:
  - "behavior-preserving" reading: sandbox lifecycle code (Docker/daemon/provisioner)
    moved intact; only the composition-root wiring changed - eos-runtime's builder
    no longer self-constructs the Docker default and instead requires an injected
    transport/provisioner (erroring placeholders until injection). This is the
    point of the relocation, matching the SPEC build order (relocate first, add
    typed gateway injection next), not a lifecycle behavior change.
  - The Phase 1 "Port crate" component lists gateway.rs; SandboxGateway and
    gateway.rs are deferred to Phase 2 per SPEC build order (gateway lands with
    production-visible injection). No empty stub created. The provisioner()
    builder setter stays #[cfg(test)]; production-visible injection is Phase 2.
Next phase unblockers: Phase 2 can now define the object-safe SandboxGateway in
  eos-sandbox-port, make RuntimeServicesBuilder accept a production gateway
  injection (replace the cfg(test) provisioner setter), and add
  RuntimeServices::state_reader() + store list APIs.
```
