# Sandbox Event Tracing and Response Contract

Status: Proposed (rev 4 — destructive posture; verified against the four
audit-system rules: ingestion decoupling, storage immutability,
serialization/schema evolution, visibility/lineage — 13 confirmed gaps closed.
Rev 4 adds the six-scope explorer sweep of every printed/response-visible/
dropped datum, dispositioned as drop / populate / add with spot-checked code
anchors — see "Inventory-verified deltas" — and the Extension Model: normative
rules for introducing new trace surfaces without eroding the audit contract)
Date: 2026-06-12
Scope: `sandbox/crates` (Rust) + host-side trace persistence; additive TS contract notes only.
Inputs:
- `sandbox/docs/sandbox-response-observability-findings.md` (response-surface inventory; path corrected in rev 4)
- `sandbox/docs/sandbox-event-tracing-response-plan.md` (parallel draft; identity model, seq chain, phase vocabulary, declassification, and fail-closed rule merged here)
- Verified live scan of dispatch, transport, host forwarding, command session, plugin, workspace, LayerStack/OCC, and e2e helpers (anchors inline)

## Implementability and Progress

Verdict: **implementable**. The plan fits the current sandbox shape because the
daemon can emit bounded span facts through the `tracing` facade without a
subscriber cost in tests/ns-runner code, request traces can ride the existing
one-request-one-response protocol as an internal sidecar, and durable SQLite
ingest stays in the host where request forwarding and fail-closed decisions
already live. The risky parts are migration ordering, not missing primitives:
the mixed-wire decoder must land before any response family flips, and e2e
assertions must move to trace/store helpers before legacy `timings` assertions
are deleted.

Implementation constraints:

- No daemon hot-path persistence, host RPC, SQLite, fsync, or unbounded JSON
  allocation is allowed in dispatch/rule/subsystem decision paths.
- Transport tracing covers the full call chain: gateway inbound UDS
  read/parse/route/write, host outbound TCP or docker-exec fallback, and daemon
  inbound accept/read/auth/dispatch/write. Host-side transport failures must
  still produce audit entries even when the daemon never sees the request.
- Resource stats are sufficient for the first implementation only if O(files)
  tree walks stay outside always-on before/after pairs and every missing source
  records a typed availability/error marker.
- The e2e suite is part of the migration, not a post-migration cleanup. Each
  family flip must replace that family's response/timing assertions with
  trace/store assertions in the same change.

Progress tracker:

| Phase | Scope | Status | Exit proof |
| --- | --- | --- | --- |
| 0 — spec and retarget map | Implementability verdict, progress tracker, phase checklists, and e2e retargeting contract | Complete | `git diff --check -- docs/plans/sandbox-event-tracing-and-response-contract_SPEC.md`; no stale legacy identity/event names |
| A — contracts first | `eos-trace`, protobuf, bounded detail, typed response envelope skeletons | Not started | Contract/golden tests pass before host/daemon work depends on them |
| B — host store | SQLite audit store, hash chain, projections, fail-closed request-start ingest | Not started | Store unit tests and acceptance query plan checks pass |
| C — daemon propagation | request trace propagation, root/dispatch/op spans, sidecar/export/drainer | Not started | daemon tests prove wire failures/cancel paths produce trace records |
| D — subsystem events/resource stats | layerstack/OCC/overlay/command/isolated/plugin/checkpoint/resource events | Not started | focused crate tests plus live trace queries for every required subsystem fact |
| E — response + e2e migration | mixed decoder, family flips, e2e helpers and suite retargeting | Not started | each flipped suite asserts envelope/meta/store facts instead of flat timings |
| F — debt deletion | remove legacy response/timing helpers and compatibility adapter | Not started | grep gates prove no legacy branches remain |
| G — TS mirror | TypeScript contract/schema mirror | Not started | `eos-agent-core` typecheck/lint/test |
| H — heartbeat monitoring | status snapshots, heartbeat projection, CLI watch | Not started | heartbeat e2e proves bracketing and daemon-unreachable rows |
| I — transcript archival | stdin/progress/final transcript archival and loss events | Not started | transcript byte/digest e2e and TTL-loss test |
| J — operator lineage views | `trace show`, `trace verify`, heartbeat lineage views | Not started | CLI/e2e reconstruct traces from store data only |

## Posture: Destructive, Clean-Slate

Owner directive: the existing code records stats, values, and parameters
poorly — the recording mechanism and the response envelope are both assumed
unsuitable. This plan therefore **replaces** rather than preserves. It treats
tracing as a performance/audit/observability-critical capability: when a bug
occurs inside the sandbox, every internal step must be transparent and
traceable after the fact. Opaque internals are the failure mode this plan
exists to eliminate.

This supersedes the findings doc's preserve-first guidance. The findings doc
remains the inventory of *what information exists today*; it no longer
constrains *the shape it is delivered in*.

What gets deleted (not wrapped, not shimmed long-term):

| Deleted | Anchor | Replaced by |
| --- | --- | --- |
| `OpResponse::Success(serde_json::Value)` untyped bodies | `eos-operation/src/core/response.rs:6-11` | `OperationEnvelope<T>` tagged union with per-family typed results |
| `error: ()` serialized as `null`; `mutation_source: None` as `""` | `core/workspace_outcome.rs:153-189` | honest structs; quirk serializers deleted outright |
| Ad-hoc `json!` envelopes + `success: bool` branching | `core/response.rs:61-85`, `dispatch/dispatcher.rs:99-113`, `protocol.rs:146-151` | one envelope renderer; `status` discriminant |
| Flat dotted-key `timings` maps threaded as `&mut Map<String, Value>` through every layer | `dispatcher.rs:142-162`, `runtime/response.rs:75-93`, settle/OCC/plugin sites | spans as the **single source of truth** for durations; response meta derived from the trace record |
| `merge_runner_timings` key aliasing (`workspace.mount_s` → `command_exec.mount_workspace_s`) | `runtime/response.rs:84-93` | one canonical step vocabulary, no aliases |
| Pretty-JSON final-response crash files as the only command audit | `eos-command-session/src/session.rs` | trace store entries + bounded final-state events (transcript files stay for raw output) |

In-repo consumers that migrate in lockstep (verified — there are no others):
`eos-sandbox-gateway`, `eos-sandbox-host` (including the `e2e_support`
`is_success`/`error_kind` helpers), and the retargeted e2e suites (flat
`timings.`/`resource.` assertions exist today across those suites). The TS
workspace has no daemon-facing code yet, so the new contract is its day-one
contract.

## Decision Summary

| Decision | Choice | Why |
| --- | --- | --- |
| Instrumentation backbone | `tracing` 0.1 facade + `tracing-subscriber` 0.3 custom Layer; new crate `eos-trace` | Third-party-preferred; macros are no-ops without a subscriber (ns-runner process, library tests); spans land at the existing phase boundaries and **replace** the timing-map plumbing |
| Timing source of truth | The span tree. Response `meta` (duration, step summary, modules touched, resource summary) is rendered **from the trace record**, never hand-inserted | One measurement, one vocabulary; drift between response and trace becomes impossible by construction |
| Identity | `trace_id` (host-minted, propagated in the request envelope) ≠ `request_id` (replaces legacy `invocation_id`, one request/response) ≠ `span_id`; plus a per-trace monotonic `seq` event chain and cross-request link rows | A long-lived chain (exec → stdin → poll → settle) is one `trace_id` across many `request_id`s; replay is `WHERE trace_id=? ORDER BY seq`; concurrency lives in the span tree, not the chain |
| Event delivery | Hybrid: request-scoped events ride the response as an internal `_trace_events` sidecar (host-ingested, gateway-stripped); background traces (reaper settles, sweeps) buffer in a bounded spool drained via `sandbox.trace.export` | Sidecar = zero loss window and zero extra round trips for request traces; drain op covers work that has no response to ride. The one-request-one-response protocol (`server.rs:262-287`) permits exactly this combination |
| Hot-path ingestion | Daemon spans/events are bounded in-memory facts only: no SQLite, fsync, host RPC, blocking serialization, unbounded JSON allocation, or cross-sandbox lock waits in rule/dispatch/subsystem decision paths | Core sandbox decisions stay decoupled from audit persistence; overflow is explicit (`dropped_traces`, `dropped_children`, `truncated`) instead of silently slowing the decision engine |
| Canonical serialization | Protobuf `TraceBatch` / `SandboxStatusSnapshot` schemas generated by `prost`; JSON is allowed only for human exports and bounded projection fields | Schema-evolvable, low-overhead payloads for sidecars, export drains, and immutable storage; avoids `serde_json::Value` becoming the audit contract |
| Persistence | SQLite (`rusqlite`, bundled, WAL) on the **host** at `state_dir/sandbox-traces.sqlite` (0600/0700), with append-only hash-chained `audit_entries` as the canonical record and relational tables as query projections | Audit = immutable payload history + chain reconstruction + joins + aggregation. SQL serves lookup; cryptographic chain/seals serve tamper evidence. JSONL only as a derived export, never the system of record |
| Persistence strictness | **Fail-closed for mutating ops**: host records the request-start audit entry before forwarding; if that write fails, the mutating op is not forwarded. Read-only ops proceed with a `trace_degraded` marker | Audit-critical framing: an untraceable mutation is worse than a refused one |
| Workspace route | 4-valued, trace-only: `ephemeral_workspace` \| `isolated_workspace` \| `fast_path` \| `none` | Owner decision. `fast_path` = data-plane work directly against LayerStack with no workspace (direct file merge/read); `none` = pure control plane. Never used for runtime branching — observability only |
| Response contract | Single typed envelope, `status ∈ {ok, running, rejected, cancelled, timed_out, error}` tagged union; domain payload under `result`, fault under `error`, everything else under `meta` | Most readable: one switch tells the consumer what happened; no `success:false`+error-kind double decode; no null pairs |
| Compatibility | None preserved. A short-lived v1 flattening adapter exists only as a migration vehicle inside the phase ladder and is **deleted** in the final phase | No technical debt is the explicit goal; all in-repo consumers migrate in lockstep |

### Non-negotiable audit rules

These are implementation gates, not aspirations:

| Rule | Spec commitment | Verification gate |
| --- | --- | --- |
| A. Decouple rule execution from audit logging | Mechanism crates only emit typed span/event facts into in-memory span state or bounded spool buffers. Host persistence, SQLite locks, fsync, hash sealing, export writing, and transcript archival never run inside the daemon rule/dispatch hot path. | A hot-path unit/bench gate asserts representative dispatch + route decisions remain sub-millisecond with tracing enabled and an intentionally slow host store; no daemon crate may depend on `rusqlite` |
| B. Data storage and immutability | `audit_entries` is append-only, stores canonical protobuf payload bytes, chains every entry by SHA-256, and seals contiguous segments with a signer key. Query tables can update/rebuild, but they are projections. | Store tests verify chain continuity, segment signatures, tamper detection, projection rebuild from `audit_entries`, and mutating-op fail-closed behavior when the pre-forward append fails |
| C. Serialization and schema evolution | Protobuf schemas in `eos-trace/proto/eos/trace/v1` define trace batches, events, spans, resources, links, heartbeats, and response trace refs. JSON is not a daemon-host audit payload. | Golden protobuf compatibility tests keep old fixtures decodable; new fields must be optional/additive or a new schema version |
| D. Visibility and lineage | `trace_id`, `request_id`, `span_id`, host `seq`, `trace_links`, heartbeat rows, indexes, and operator views are mandatory. Every decision can be replayed as both a sequence and a causal tree. | Acceptance queries plus `trace show <trace_id>`, `trace verify`, and `sandbox status --watch` e2e checks must pass before deleting legacy timing surfaces |

## Part A — Event Tracing

### Identity model

| Identity | Source | Meaning |
| --- | --- | --- |
| `trace_id` | Host-minted (uuid4) when starting a user-visible call; propagated to the daemon in the request envelope; reused across every op of a long-lived chain | One user-visible sandbox interaction or one long-lived session chain |
| `request_id` | New top-level request identity; replaces today's `invocation_id` (`protocol.rs:118-135`) | One daemon request/response |
| `span_id` | Daemon `AtomicU64` (never reuse `tracing::span::Id` — the Registry recycles them) | One timed unit, parented into a per-request tree |
| `seq` | Host-assigned at ingest, monotonic per `trace_id` | Durable observation order; gap-free even when daemon batches arrive late |
| `daemon_boot_id` | uuid4 per daemon process | Exposes respawn gaps in audit |
| `host_boot_id` | uuid4 per host process | Exposes host-restart gaps — the host is the single writer and seq assigner, so its own crashes are first-class audit facts |

Two views over the same data, both first-class:

| View | Query | Use |
| --- | --- | --- |
| Timeline chain | `WHERE trace_id=? ORDER BY seq` | Audit replay, "what happened next", total-elapsed narrative |
| Causal tree | `span_id`/`parent_span_id` | Nested/parallel work, subsystem ownership, per-step durations |

Cross-op links (`trace_links` rows) tie long-lived resources into chains:
`command_session_id`, `workspace_handle_id`, `plugin_service_instance_id`,
`layer_manifest_version`.

For a single forwarded sandbox op, the trace timeline starts at gateway UDS
accept and continues through gateway catalog routing, host forwarding, host
TCP/docker-exec fallback, daemon transport, dispatcher, op adapter, and the
owning subsystem. These are layer-specific events under one `trace_id`, with
`request_id` identifying the one request/response inside that trace. If the
chain stops before the daemon receives the request, the trace still closes with
host/gateway transport events and an explicit failure outcome.

Chain continuity is **host bookkeeping, specified here**, keyed by what later
requests actually carry (verified against the op adapters — isolated ops are
keyed by `caller_id`; `workspace_handle_id` is currently visible in the enter
response, isolated command-settle response, and persisted `manager.json`, but
follow-on op routing is still by `caller_id`):

- `command_session_id → trace_id`: populated from exec responses, consulted
  when later args carry the id (`write_stdin`, `read_progress`/`poll`,
  `collect`, `cancel`), pruned at settle/collect.
- `(sandbox_id, caller_id) → {workspace_handle_id, trace_id}`: populated when
  the isolation-enter response returns `workspace_handle_id`, consulted
  pre-forward for any subsequent op whose args carry that `caller_id` while
  the entry is open (command exec, file ops, isolation status/exit,
  workspace-run cancel) — deliberately mirroring the daemon's
  `command_binding_for(caller_id)` routing key (`op_adapter/command.rs:99`,
  `op_adapter/files.rs:181`); the exit op records into the chain, then prunes
  the entry. Caller-map attribution is predictive — the daemon's recorded
  `route_selected {kind, reason}` is the truth; the host also prunes on
  ingested exit responses and on exported `IsolatedSweep` background traces,
  and a chain-attributed op that returns a non-isolated route is visible in
  `trace_requests` (chain `trace_id` beside actual route). Divergence is
  observable, never silent.

Both maps are rebuildable from `trace_links` after a host restart. Phase D adds
the missing daemon-side origin fields to `ActiveCommand` (today it stores
session/workspace state, not `request_id`/`trace_id`), so background settle traces
carry the chain id even when the host never polls.

Per-kind link semantics — chain links continue a `trace_id` across ops; tag
links only correlate across otherwise-unrelated traces:

| link_kind | Written when / from | Trace reuse |
| --- | --- | --- |
| `command_session` | Host, at exec-response ingest; id enters the chain map above | Chain |
| `workspace_handle` | Host, at isolation-enter ingest; chained via the `(sandbox_id, caller_id)` map above | Chain |
| `plugin_service` | Host, at sidecar/export ingest: plugin ensure/status spans and `service_started`/`service_health_checked` events carry `service_instance_id` (`eos-plugin/src/service.rs:130`) as a required typed field; `PluginService` background roots carry it the way `CommandSettle` roots carry `command_session_id` | Tag only — never enters a chain map |
| `manifest_version` | Host, at sidecar/export ingest: `snapshot_acquired` (version read against) and `publish_layer_finished`/`auto_squash_finished` (version produced) carry the manifest version as a required typed field | Tag only |

### Flow

```
caller ──UDS JSON line──> gateway
                      │ read/parse route; mint or adopt trace_id/request_id
                      │ record gateway.transport + gateway.route events
                      ▼
              in-process Engine::forward
                      │ INSERT request-start audit entry ── fail ⇒ mutating op NOT forwarded
                      │ record host.transport TCP/fallback events
                      ▼
              TCP JSON line + auth token
                      │ or docker-exec thin client fallback to runtime.sock
                      ▼
              daemon transport (server.rs)
                      │ root span `op_request` opened BEFORE read_request_line
                      │ (wire failures — bad JSON, too-large, timeout, auth — close it
                      │  with status=error; every accepted connection yields a trace)
                      ▼
              spawn_blocking → dispatch ── span `dispatch` {op_resolved, parse, fallback}
                      ▼
              op adapter ── span `op.<family>.<verb>` {workspace_route recorded at decision site}
                      ▼
              subsystems ── spans + phase events (layerstack / overlay / command / isolated / plugin)
                      │     resource_stats events (cgroup, /proc, tree stats)
                      ▼
              root closes ── full TraceRecord assembled; envelope `meta` rendered FROM it
                      │      (the response write itself is observed host-side: received_at,
                      │       rtt, response_persisted — a record cannot describe its own delivery)
                      ▼
              response + `_trace_events` sidecar (protobuf TraceBatch) ──> host: ingest, assign seq,
                      │                              update projections (outcome, received_at, rtt)
                      │                              strip sidecar before gateway/caller response
                      ▼
   background work (reaper settle, sweeps) ──> bounded spool ──> `sandbox.trace.export`
                              drain SCHEDULED by sidecar `spool_pending`/heartbeats, RUN by the
                              host's background drainer; exhaustive (synchronous) only at release()
```

### Transport connection lifecycle

Request traces must cover the connection lifecycle from caller to gateway,
gateway to host, and host to daemon. The daemon cannot report a failed host
connect, the host cannot observe gateway UDS parse/route/write failures unless
the gateway records them, and the gateway cannot observe daemon-internal
auth/decode timing without the daemon span. All three layers are mandatory and
joined by `trace_id`/`request_id` when a request is successfully forwarded.

| Side | Span/event | Required fields |
| --- | --- | --- |
| Gateway inbound UDS | `gateway.transport.accepted`, `request_read`, `parse_failed`, `response_written`, `write_failed` | `gateway_connection_id`, `surface` (`client` or `operator`), `socket_path`, `request_bytes`, `read_duration_us`, `response_bytes?`, `write_duration_us?`, `error_kind?` |
| Gateway route/catalog | `gateway.route_selected`, `gateway.route_rejected`, `gateway.engine_forward_started`, `gateway.engine_forward_finished`, `gateway.engine_forward_failed` | `op`, `sandbox_id?`, `route` (`host`, `daemon`, `plugin_fallback`, `rejected`), `visibility`, `mutates_state`, `duration_us`, `error_kind?` |
| Host outbound TCP | `host.transport.connect_started` / `connect_finished` / `connect_failed` | `sandbox_id`, `endpoint`, `resolved_addr`, `attempt_index`, `timeout_ms`, `error_kind?`, `connect_duration_us` |
| Host outbound retry/fallback | `host.transport.retry_scheduled`, `endpoint_refreshed`, `fallback_chain_started` | `attempt_index`, `delay_ms`, `old_endpoint?`, `new_endpoint?`, `reason` |
| Host request write | `host.transport.request_written` / `write_failed` | `request_bytes`, `protocol_version`, `auth_token_present` (boolean only), `write_duration_us`, `error_kind?` |
| Host response read | `host.transport.response_read` / `empty_response` / `decode_failed` / `read_failed` | `response_bytes`, `read_duration_us`, `response_digest?`, `error_kind?` |
| Host fallback thin client | `host.transport.exec_client_started`, `exec_client_finished`, `exec_client_failed`, `daemon_respawn_started`, `daemon_respawn_finished`, `daemon_respawn_failed` | `sandbox_id`, `container`, `remote_socket_path`, `mutates_state`, `uncertain_outcome`, `duration_us`, `error_kind?` |
| Daemon inbound accept | `daemon.transport.accepted` | `connection_id`, `listener_kind` (`unix` or `tcp`), `local_addr?`, `peer_addr?`, `daemon_boot_id` |
| Daemon request read/auth/decode | `read_started`, `read_finished`, `auth_checked`, `decoded` | `connection_id`, `is_tcp`, `request_bytes`, `read_duration_us`, `auth_required`, `auth_ok`, `protocol_version?`, `error_kind?` |
| Daemon response write | `response_write_started`, `response_write_finished`, `response_write_failed`, `shutdown_finished` | `connection_id`, `response_bytes`, `write_duration_us`, `shutdown_duration_us?`, `error_kind?` |

Security rules:

- Auth tokens are never recorded, hashed, or length-recorded. Only
  `auth_token_present`, `auth_required`, and `auth_ok` are visible.
- Socket addresses are operational metadata. They are recorded because wrong
  endpoint, stale Docker port, and unauthorized TCP are common sandbox failure
  modes.
- Gateway socket paths and surfaces are operational metadata. They are recorded
  because client/operator routing, catalog visibility, and parse failures are
  part of the audit chain.
- A host-side connect/write/read failure writes a trace outcome even when there
  is no daemon sidecar. The daemon-side span is absent in that case by design;
  the absence itself is queryable through the host transport events.

### New crate `sandbox/crates/eos-trace`

- `record.rs` — typed DTOs: `TraceId`, `RequestId`, `SpanUid`, `TraceRecord`,
  `SpanRecord`, `EventRecord`, `WorkspaceRoute`, `TraceKind`
  (`OpRequest | CommandSettle | SessionSweep | IsolatedSweep | PluginService`),
  closed `SpanKind` enum with exhaustive `subsystem()` mapping
  (`Wire | Dispatch | Op | LayerStack | Overlay | CommandSession | Workspace |
  Plugin | Control`), bounded-detail helpers (sizes/hashes/refs, never raw
  blobs).
- `proto/eos/trace/v1/*.proto` + `codec.rs` — canonical protobuf payloads:
  `TraceBatch`, `TraceSpan`, `TraceEvent`, `TraceResource`, `TraceLink`,
  `RequestStart`, `SandboxStatusSnapshot`, `ResponseTraceRef`, and
  `AuditEntry`. Rust DTOs
  convert into the protobuf schema at sidecar/export/store boundaries; protobuf
  bytes are what the immutable log hashes and seals. JSON is derived after
  ingest for query fields and operator export only.
- `spool.rs` — bounded background-trace buffer (default 4 MiB, drop-oldest,
  `dropped_traces` counter); per-span field budgets with an explicit
  `truncated` flag so one pathological request cannot evict its siblings.
- `layer.rs` — `TraceSpoolLayer: Layer<Registry>`: span state in Registry span
  extensions (`on_new_span` captures fields via `Visit`, `on_record` lands late
  fields like `workspace_route`, `on_event` appends, `on_close` pushes children
  into parents; a closing root assembles the `TraceRecord`). The transport
  closes the root **immediately before envelope render** and calls
  `take_finished(trace_id) -> TraceRecord` exactly once: that record is both
  the source for envelope `meta` and the sidecar payload. The response write
  itself is deliberately outside the record — a record cannot describe its own
  delivery; the host observes it (`received_at_ms`, `host_rtt_ms`,
  `response_persisted` / `response_missing`). Request-scoped records never
  enter the spool; the spool is background-only. Roots with
  `trace_exempt = true` (the export op itself) are skipped.

Crate ownership boundaries (merged from the parallel draft):

| Owner | Responsibility |
| --- | --- |
| `eos-trace` | Storage-neutral DTOs, spool, subscriber layer, route/kind enums, bounded-detail helpers |
| `eos-operation` | Envelope + per-family result DTOs — contract shape, not persistence |
| `eos-daemon` | Root/dispatch/op spans, sidecar assembly, subscriber install, export op |
| mechanism crates (layerstack, workspace, command-session, plugin, overlay) | Emit spans/events at their own phase boundaries; no persistence or policy deps |
| `eos-sandbox-host` | SQLite store, request-start fail-closed rule, sidecar ingest + seq assignment, degraded/uncertain records, export drains |
| `eos-sandbox-gateway` | Declassification: strip `_trace_events` from client-facing responses; operator/debug trace lookup only |
| `@eos/db` / `@eos/contracts` | TS mirror of schema + Zod envelope schemas when the TS host lands |

### Workspace route taxonomy (4-valued, trace-only)

`workspace_route.kind` is an observability attribute recorded at the verified
decision sites. It must never become runtime control flow again.

| Kind | Meaning | Decision site |
| --- | --- | --- |
| `ephemeral_workspace` | One-op ephemeral/overlay route with capture → OCC publish semantics (includes plugin oneshot overlay) | `op_adapter/command.rs` `ExecTarget::Ephemeral` branch; `op_adapter/plugin.rs` overlay path |
| `isolated_workspace` | Caller-keyed isolated workspace; private upperdir; no publish | `command_binding_for` hits in `op_adapter/command.rs` and `op_adapter/files.rs` `route_file_op`; isolation enter/exit lifecycle ops |
| `fast_path` | Data-plane work directly against LayerStack with **no workspace**: direct file merge/read (`FileRoute::Direct`), checkpoint base/commit/binding ops, layer-metrics manifest reads | `route_file_op` direct arm; `op_adapter/checkpoint.rs` |
| `none` | Pure control plane — no workspace and no LayerStack data-plane work: ready, heartbeat, cancel, in-flight/session counts, plugin ensure/status, isolation status/list, workspace-run cancels, trace export | adapter classification table (each op family declares its default; `route_file_op`-style late recording overrides where the route is dynamic) |

Edge calls, decided here: `sandbox.checkpoint.layer_metrics` is `fast_path`
(reads the live manifest); `commit_to_git` stays `fast_path` even though it
mounts an overlay worktree internally — that mount is a projection detail
(visible as its own span), not an agent workspace; `sandbox.isolation.status`
is `none` (registry read, no workspace entry); plugin `ensure`/`status` are
`none` (service control plane) while registered plugin overlay ops are
`ephemeral_workspace`.

### Detail-capture principle

Assume nothing useful is recorded today. Every span records its **inputs' key
parameters and outputs' key results** as typed fields — op args summary (paths,
caller, flags), manifest versions, lease ids, changed-path counts, exit codes,
kill reasons, byte counts, depths, veth/cgroup names, worker exit codes, PPC
message ids. Bounded by rule: sizes, hashes, counts, ids, and references to
content that already exists elsewhere (transcripts, response rows) — never raw
stdout/stderr, file contents, or plugin result blobs in trace events.

Capture budgets (named configurable defaults; overflow records
`{ truncated: true, sha256, original_len }`, never a silent drop):

| Field | Default budget |
| --- | --- |
| `request_start.args_summary` | 4 KiB |
| span `fields_json` | 2 KiB |
| event `details_json` | 1 KiB |
| `trace_requests.response_summary` | 2 KiB |
| heartbeat `details_json` | 4 KiB |
| tree walk entry budget | 50,000 filesystem entries per standalone walk; overflow records partial counts + `truncated` |
| per-record sidecar total | 64 KiB — request records cannot spool, so overflow drops children with `dropped_children`, never the root |

Resource stats placement is part of the budget contract:

- **Always-on before/after pairs are only for O(microseconds) kernel gauges.**
  `command.session.wait` and `plugin.overlay.run` emit two
  `resource_stats {phase: "before"|"after"}` events for cgroup CPU/memory/io,
  cgroup pressure/event counters where available, and daemon RSS. The raw
  counters are stored; deltas are query-time math.
- **Tree walks are never part of those pairs.** Recursive `TreeResourceStats`
  walks are O(files), so they attach only to the span that already paid for the
  walk (`overlay.capture_upperdir`, teardown inspection, explicit operator
  inspection). Capture-path stats are gathered during the existing capture walk,
  not by a second pass.
- **Cgroup deltas are approximate under concurrency.** The cgroup is
  sandbox-wide, so every paired sample records `inflight_requests`; contended
  deltas are visibly contended, not falsely per-command.
- **`memory.peak` keeps kernel semantics.** It is a high-water mark since cgroup
  creation unless reset support exists on the running kernel. The trace records
  it as a raw gauge with those semantics; it never claims a per-command peak.
- **Heartbeat gauges stay beside chains.** Heartbeat rows are audit-backed time
  series keyed by `sandbox_id` + time window, not request-chain events.

`ResourceStats` payload contract:

| Section | Included fields | Cost / semantics |
| --- | --- | --- |
| `meta` | `stats_kind` (`cgroup_process`, `tree`, `host`, `mount_cost`), `phase?`, `source`, `source_available`, `read_error?`, `parse_error?`, `sampler_duration_us`, `inflight_requests` | Required on every stats payload; missing sources are explicit |
| `cgroup.cpu` | all numeric `cpu.stat` counters, at least `usage_usec`, `user_usec`, `system_usec`, `nr_periods`, `nr_throttled`, `throttled_usec` when present | Raw cumulative gauges; query-time deltas expose CPU burn and throttling |
| `cgroup.memory` | `memory.current`, `memory.peak`, `memory.events` counters (`low`, `high`, `max`, `oom`, `oom_kill`, `oom_group_kill` when present), optional `memory.swap.current`/`memory.swap.peak` | Raw gauges/counters; `memory.peak` remains cgroup-lifetime high-water unless kernel reset support is explicitly used and recorded |
| `cgroup.io` | summed `io.stat` totals: `rbytes`, `wbytes`, `rios`, `wios`, `dbytes`, `dios` | Raw cumulative gauges; query-time deltas |
| `cgroup.pressure` | optional PSI totals from `cpu.pressure`, `memory.pressure`, `io.pressure` (`some`/`full` totals and averages where available) | Cheap optional contention signal; absent on unsupported kernels with `source_available=false` |
| `process` | daemon `rss_bytes`, `max_rss_bytes` from `/proc/self/status` | Daemon process gauge, not child-command RSS |
| `tree` | `bytes`, `file_count`, `dir_count`, `symlink_count`, `entry_count`, `truncated`, `read_error_count`, `first_error_path?` | Only on spans that already perform or explicitly request a bounded walk |
| `mount_cost` | `layer_count`, `fsconfig_calls`, `duration_us`, `upperdir_empty_bytes` | O(depth) mount audit, not a resource gauge pair |

This is enough for the first implementation because it covers CPU burn,
throttling, memory pressure/OOM, IO volume, daemon RSS, tree-size effects,
contention visibility, and missing-source honesty without adding O(files) work
to hot paths. The main deferred additions are per-child `wait4` rusage and
network namespace byte counters; both require new collection points and should
land only if a trace question cannot be answered from cgroup deltas plus
existing command output facts.

Phase-event vocabulary (events inside spans; merged from the parallel draft —
the required minimum for emission compliance; new names are introduced by
adding a row here first, per the Extension Model's vocabulary governance):

| Module | Required events |
| --- | --- |
| `host.protocol` | request_received, request_persisted, forward_started, forward_finished, response_missing, uncertain_outcome, trace_degraded |
| `gateway.transport` | accepted, request_read, parse_failed, response_written, write_failed |
| `gateway.route` | route_selected, route_rejected, engine_forward_started/finished/failed |
| `host.transport` | connect_started/finished/failed, retry_scheduled, endpoint_refreshed, fallback_chain_started, exec_client_started/finished/failed, daemon_respawn_started/finished/failed, request_written/write_failed, response_read/read_failed, empty_response, decode_failed |
| `daemon.transport` | accepted, read_started, read_finished, auth_checked, decoded, response_write_started/finished/failed, shutdown_finished |
| `daemon.dispatch` | dispatch_started, op_resolved, parse_finished, plugin_fallback_checked, dispatch_finished |
| `workspace.route` | route_selected {kind, reason} |
| `layerstack` | binding_loaded, snapshot_acquired, lease_released, lease_release_failed, manifest_read, auto_squash_started/finished {error on failure}, auto_squash_skipped {reason} |
| `occ` | commit_started, validate_groups_finished, publish_layer_finished, conflict_detected {path, reason, observed_version?, observed_state?} per conflicting file, commit_finished |
| `overlay` | workspace_prepared, mount_started/finished {layer_count, fsconfig_calls, duration_us, upperdir_empty_bytes}, capture_started/finished {failing_path on error, bytes, file_count, dir_count, entry_count, truncated}, unmount_finished |
| `command_session` | prepared, spawned, yielded, stdin_written {bytes, wait_ms, waited_for_output}, progress_read, cancelled, timed_out, reaped {kill_reason, signal}, settled, session_artifact_written/failed, final_persisted, final_persist_failed, transcript_failed |
| `isolated_workspace` | enter_started, holder_started, network_configured {dns_fallback_applied}, status_read, exit_started, teardown_phase_finished (×4; kill_holder carries {holder_was_alive, exit_status}), exited {mountinfo_scan_error?}, recovery_started/finished {manager_json_error?, orphan_cleanup_error?} |
| `plugin` | ensure_started, package_checked, setup_finished {exit_code?, output_tail?, spawn_error?}, service_started {stderr_path}, service_health_checked {state, restart_count, refresh_count, last_error}, ppc_message_sent/received, overlay_started/finished, callback_request/response |
| `file` | read_started/finished, mutation_started, edit_applied, write_applied |
| `checkpoint` | worktree_mode_selected {mode}, git_command_finished {argv_summary, exit_code, stderr_tail} |
| `resource` | resource_stats {ResourceStats payload above; per-source error markers — a failed read is never a silent absence} |

### Inventory-verified deltas (rev 4)

Six read-only explorer sweeps (dispatch/transport/core envelope; command
session; layerstack/overlay/workspace/ns-runner; plugin/PPC; cross-crate
print/log/file surfaces; host/gateway/e2e consumers) inventoried every datum
that is printed, logged, written to a file, or sent in a response today, plus
everything computed and then dropped. Load-bearing anchors were spot-checked
in source. Three dispositions: **drop/dedupe** (redundant emissions),
**populate** (fields that exist but are hardcoded empty), **add** (facts
computed today and discarded before any durable surface — now mandatory
capture). The phase-event vocabulary and heartbeat snapshot sections already
reflect the adds; the tables below are the audit trail from finding to
disposition.

Drop / dedupe (beyond the posture table's deletions):

| Redundant today | Evidence | Disposition |
| --- | --- | --- |
| `timings.command_exec.total_s` ≡ `timings.api.exec_command.dispatch_total_s` (≡ `api.exec_command.total_s` on isolated settle) — one elapsed value, triple-keyed | `eos-operation/src/command/settle.rs` | Gone with flat timings; the one duration lives on the `command.session.wait` span |
| Runner `workspace.{mount,tool}_s` re-keyed to `command_exec.{mount_workspace,run_command}_s`; command settle has a second merge helper | `runtime/response.rs:75-94`, `command/settle.rs:334-344` | Delete both helpers with flat timings; the sweep confirmed this aliasing is still live |
| Tree-walk `truncated` flags are fake: current resource stats hardcode `truncated = 0` and there is no entry budget behind the walk | `runtime/response.rs:38-47`, `command/settle.rs:355-371` | Delete fake flags; replacement walk stats use the named entry budget above and a real `truncated` fact |
| Direct file routes receive fake all-zero run/workspace/upperdir tree stats even when no tree exists | `runtime/response.rs:169-176` | Do not emit tree-stat keys unless the span actually paid for a walk; absence means not sampled, not empty |
| Plugin `ensure` re-embeds status-view facts: `operation_routes`, `services`, `service_processes`, `running_service_processes`, `connected_ppc_routes`, `connected_ppc_services` overlap with `status.loaded_plugins[]` / top-level status | `op_adapter/plugin.rs:95-126,137-143` | One typed `PluginServicesView` DTO shared by both family results — the duplication becomes one type |
| Plugin status emits inert compatibility fields: `runtime_warmed: false`, `pending: []`, `service_processes[].process_started: false` | `op_adapter/plugin.rs:102,133,210` | Delete unless backed by real runtime state before the family flips |
| `mutation_source` written then post-hoc overridden | `runtime/response.rs` | Quirk serializers deleted (posture table) |
| `error_id` minted insert-if-absent at two independent sites | `dispatcher.rs:133-138`, `core/response.rs:105` | Single mint point in the envelope renderer |
| `manifest_version`/`lease_id`/`layer_paths`/`root_hash` duplicated across Lease/Snapshot/Handle/OverlayHandle internals | `eos-layerstack` structs | Internal-only, not a wire problem; the wire carries each once via span fields + `trace_links` |

Populate (exists-but-empty — the sweep found no producer, so these must gain
real content, not be dropped):

| Field | Today | Rev-4 contract |
| --- | --- | --- |
| `warnings` | Gateway hardcodes `[]` (`gateway.rs:448`); nothing in-repo reads it | `meta.warnings` rendered from real `warn`-level trace events |
| `error.details` | Hardcoded `{}`; error chains flatten to one lossy string at the wire boundary (source context, io kinds, child exit codes lost) | `OperationFault.details` carries structured context: `source_chain[]`, `io_kind`, `path`, exit codes where present |

Add (computed today, dropped before any durable surface; phase letter = where
the capture lands):

| Lost today | Anchor | Captured as |
| --- | --- | --- |
| Auto-squash read/can-plan/squash failures swallowed: read errors become "no plan", `can_squash` errors become false, and `stack.squash(max_depth).ok().flatten()` hides failures | `eos-layerstack/src/commit/worker.rs:478-522` | `auto_squash_finished {error}` and `auto_squash_skipped {reason, error?}` (D) |
| Squash-skip reason (too shallow, min-reduction unmet, lease-blocked, planner returned none, live-prefix race, post-commit release failure) unobservable | squash planner / stack apply path | `auto_squash_skipped {reason}` and `lease_release_failed` (D) |
| OCC conflict reports aggregate outcome only; per-file reason is flattened and observed version is present only on some CAS/manifest conflicts | `commit/worker.rs` validate path | `conflict_detected {path, reason, observed_version?, observed_state?}` + the same detail in `OperationFault.details` (D/E) |
| Command settle emits a response-visible wrong stat: `resource.layer_stack.manifest_path_count` is sourced from manifest depth | `eos-operation/src/command/settle.rs:230-233` | Typed resource summary derives path/layer counts from one shared sampler with a golden test (D/E/H) |
| DNS fallback decision discarded: `let _dns_fallback_applied` | `isolated_workspace/manager/lifecycle.rs:51` | `network_configured {dns_fallback_applied}` (D) |
| Holder liveness at teardown unknown (crashed earlier vs killed now) | lifecycle teardown | `isolated.exit.kill_holder` fields `{holder_was_alive, exit_status}` (D) |
| Capture-walk abort loses the failing path; only an error string survives | `eos-workspace/src/shared/capture.rs` | `capture_finished` error detail `{failing_path}` (D) |
| Capture-settle does a second tree walk after `capture_upperdir` already enumerated changes | `eos-operation/src/command/settle.rs:41-49` | Capture bytes/file/dir/entry counts during the existing capture walk; no duplicate pass (D) |
| `mountinfo_reference_count_after` silently `None` on scan failure — indistinguishable from a clean zero | teardown inspection assembly | explicit `{mountinfo_scan_error}` marker on `exited` (D) |
| Service-cache gauges (hits/misses/creates/evictions, `lock_wait_s_total/max`) visible only when an operator happens to call `layer_metrics` | `eos-layerstack/src/service.rs:22-36,214-240` | Heartbeat layerstack section samples `cache_snapshot()` continuously (H) |
| Plugin service state machine (`starting\|ready\|refreshing\|stale\|restarting\|stopped\|failed`), `restart_count`, `refresh_count`, `last_error` are serialized in `services[]` but missing from `service_health`, heartbeat, and lifecycle trace events | `eos-plugin/src/service_registry.rs:15-40`, `op_adapter/plugin.rs:149-170` | Preserve the typed `services[]` result, add `service_health_checked` fields + heartbeat `details_json` (D/H) |
| Plugin service worker stdout/stderr → `Stdio::null` — a crashing service is forensically silent | `eos-operation/src/plugin/process.rs:64-66` | Per-service stderr file in-sandbox, path recorded on `service_started`, archived at release beside `daemon.log.jsonl` (D/I) |
| Kill signal number lost or normalized into exit codes; service worker status also keeps only `status.code()` | session/worker wait sites, plugin service status | `{signal}` on `reaped`, settle facts, and worker/service exits (D) |
| Plugin setup success output is dropped; nonzero exit output is flattened into one error string; spawn failures have command/cwd/error but no exit/output fields | `eos-operation/src/plugin/package.rs` | `setup_finished {exit_code?, output_tail?, spawn_error?}` (budgeted) (D) |
| `commit_to_git` git-step semantics stringified into error messages; exit codes unmapped | `op_adapter/checkpoint.rs` | `checkpoint` events `git_command_finished {argv_summary, exit_code, stderr_tail}` (D) |
| Transport facts dropped: gateway UDS read/parse/write, catalog route decision, host TCP connect latency/retry/fallback, docker-exec thin-client fallback, endpoint refresh, request write/read duration, daemon accept id, TCP peer address, request/response byte counts, response write/shutdown failures | `eos-sandbox-gateway/src/gateway.rs`, `eos-sandbox-host/src/protocol.rs`, `eos-sandbox-host/src/host.rs`, `transport/server.rs` | Gateway `gateway.*` events + host `host.transport.*` events + daemon `daemon.transport.*` events; daemon root span gains `{connection_id, listener_kind, peer_addr?, request_bytes}`; host `response_persisted` payload gains `response_len` beside `response_digest` (C) |
| Pre-listen daemon init failures have no channel (`--log-file` receives raw stdout/stderr only) | `eosd/src/daemon.rs` | Crash-log fmt layer installs before the listener binds; boot events `config_loaded`, `listen_bound` (C) |
| stdin path knows byte length and performs bounded progress waits, but no wait/backpressure duration is computed or surfaced | session stdin path | `stdin_written {bytes, wait_ms, waited_for_output}` (D) |
| `metadata.json`, runner request/result files, `final.json`, transcript open/write/remove, reader-drain timeout, and lease-release failures are best-effort or silent | command prepare/session/process/runtime | `session_artifact_written/failed`, `final_persist_failed`, `transcript_failed`, `lease_release_failed` events (D/I) |
| Orphan recovery synthesizes a generic error; isolated `manager.json` read/parse/schema failures and orphan cleanup errors are dropped | `command/runtime.rs:60-102`, isolated manager recovery | Recovery events carry recovered `final.json` / `manager.json` facts; dir retention gated per phase I (D/I) |
| cgroup/procfs gauge read failure indistinguishable from "gauge absent on this platform" | `runtime/response.rs:198-270` | `resource_stats` per-source error markers (D) |
| CPU throttling, memory events/OOM, PSI pressure, source availability, and sampler duration are not response-visible today | cgroup/procfs samplers | Add to `ResourceStats` so slow/failed ops can distinguish resource pressure from command behavior (D/H) |
| Cheap OS gauges are currently emitted as response-only flat timing keys, not paired around the command/plugin work they describe | `runtime/response.rs:198-270` | Before/after `resource_stats` pairs on `command.session.wait` and `plugin.overlay.run`, projected to `trace_resources` (D) |
| Host RAM-pressure probe falls back silently when `/proc/meminfo` cannot be read | isolated workspace manager | `resource_stats {host_meminfo_error?}` / heartbeat marker (D/H) |
| ns-runner `tool_result` is projected into response shell fields, but trace lacks parse status, size, and digest | plugin overlay path | Bounded `{tool_result_present, parsed, len, sha256}` span field (D) |

Confirmed already-covered by rev ≤3 mechanisms (no new spec text): sweeper-
cancelled session visibility (`CommandSettle` background roots, context rule
3); the request-side parsed facts that are used for registration/routing but
not durable or response-visible today — legacy `invocation_id` (renamed
`request_id` here), `caller_id`, `background`, `is_tcp`, protocol version, raw args (`RequestStart` +
`args_digest` + root-span fields, phases B/C); plugin audit-field
parse-and-drop (`op.plugin` span captures them); response-write delivery facts
(host-observed by design).

Consumer evidence that these dispositions are safe: the host branches only on
`success`/`error.kind` (`e2e_support`); the gateway passes daemon responses
through and hardcodes the empty fields above; the `timings.`/`resource.`
readers are already scheduled for per-family assertion rewrites (phase E); and
host/e2e code today *polls* for command completion, lease accounting, and
session cleanup — workarounds the heartbeat snapshot and background settle
traces make event-driven.

### Span taxonomy (timed tree; verified anchors)

| Step | Span kind(s) | Key fields | Anchor |
| --- | --- | --- | --- |
| gateway inbound UDS | gateway events (host-side trace root starts here) | gateway_connection_id, surface, socket_path, request_bytes, op, sandbox_id, route, visibility, mutates_state, response_bytes, parse/write errors | `eos-sandbox-gateway/src/gateway.rs:327-350` |
| gateway to host | in-process `Engine::forward` events | op, sandbox_id, mutates_state, duration, outcome/error_kind | `eos-sandbox-gateway/src/gateway.rs:125-163,221-239` |
| host outbound transport | host-side events (not daemon spans) | endpoint, resolved_addr, attempt_index, connect/write/read durations, request_bytes, response_bytes, error_kind; present even when daemon never receives the request | `eos-sandbox-host/src/protocol.rs:90-112`, `host.rs:430-517` |
| daemon inbound wire message | root `op_request` (closes before envelope render) + `daemon.transport.*` events | connection_id, listener_kind, local_addr/peer_addr, op, request_id, trace_id, caller_id, is_tcp, read/auth/decode/write durations, request/response bytes | `eos-daemon/src/transport/server.rs:206-231,262-287` |
| dispatch | `dispatch`; `op.plugin.dynamic` for the registered-plugin fallback | builtin op, outcome, error_kind | `dispatcher.rs:31-64,66-80` |
| op | `op.<family>.<verb>` per `builtin.rs` arm | workspace_route (recorded late via `Span::record`), parsed-args summary | `eos-daemon/src/dispatch/builtin.rs` |
| layerstack | `layer_stack.acquire_snapshot`, `layer_stack.auto_squash`, `occ.commit` (children `validate`, `publish`) | manifest_version, depth before/after, gated/direct path counts | `eos-layerstack/src/commit/worker.rs:328-399,478-522` |
| overlay | `overlay.capture_upperdir`; ns-runner mount/tool recorded as fields from `RunResult` (separate process — no synthetic spans) | changed_path_count, tree bytes | `eos-workspace/src/shared/capture.rs:27-36` |
| command session | `command.session.spawn/wait`, `command.settle`; background root `command.settle` for the reaper path | command_session_id, kill_reason, exit_code, origin request_id | `eos-operation/src/command/service.rs:60-90,340,383-396` |
| isolated lifecycle | `isolated.enter.{spawn_ns_holder,open_ns_fds,install_veth,mount_overlay,configure_dns,create_cgroup}`; `isolated.exit.{kill_holder,teardown_veth,cgroup_rmdir,rmtree_scratch}` | per-phase durations (replaces `phases_ms`), inspection facts | `eos-workspace/src/isolated_workspace/manager/lifecycle.rs:21-63` |
| plugin | `plugin.ensure/status`, `plugin.overlay.{acquire,setup,run,capture,publish}`, `plugin.ppc.round_trip` | plugin id, op name, worker_exit_code, message ids; the request audit fields plugins currently parse and drop | `eos-daemon/src/op_adapter/plugin.rs`; `eos-operation/src/plugin/overlay.rs:140-176` |

### Context propagation rules

The architecture is async-accept + synchronous dispatch on `spawn_blocking`
(`server.rs:350`). Four explicit rules:

1. **Root**: the `op_request` span opens in `handle_connection` **before**
   `read_request_line`, fields `Empty`, recorded after decode — wire-level
   failures (bad JSON, too-large, read timeout, TCP auth) are built before
   dispatch ever runs (`server.rs:270-281,295-318`) and must still close a
   trace. Move the `Span` into the `spawn_blocking` closure and `enter()`;
   from there the op is synchronous and context flows on the thread stack. A
   registry-aborted request leaves a root trace with
   `error_kind = "cancelled"`.
2. **OCC commit worker** (own thread, `worker.rs:144`): the queued work item
   carries `Span::current()` captured at enqueue; the worker enters it, so
   `occ.commit.*` parents under the requesting op.
3. **Background reaper/sweeper threads**: no ambient span; their roots become
   standalone traces (`CommandSettle`/`SessionSweep`/`IsolatedSweep`) that
   carry `command_session_id` + origin `request_id` + the chain's `trace_id` after
   phase D extends `ActiveCommand` beyond today's session/workspace state. This
   covers the path that today produces **no observable record at all**:
   sweeper-cancelled sessions where `publish_completion = false`
   (`service.rs:392`).
4. **PPC reader thread**: phase D extends the pending-call entry (today only
   `reply_tx` + callback handler) so `PendingCalls::register` captures
   `Span::current()` (the owning op's span — `round_trip_with_callbacks` runs
   on the op's `spawn_blocking` thread); `callback_handler_for_message` returns
   it with the handler, and the reader thread enters it around callback
   handling. Every callback-driven OCC
   publish therefore parents under the owning op's trace by construction, and
   the nested `occ.commit` enqueue (rule 2) captures the correct span
   transitively. An unresolvable callback (unknown/ambiguous parent id) is
   refused before any handler runs — no mutation is possible on that path;
   the refusal opens a bounded `PluginService` root carrying the plugin
   `service_instance_id` and the claimed parent id. To support this,
   `parent_message_id` is promoted from an opaque body convention
   (`ppc.rs:15-17`) to a typed `Option<String>` field on `PpcMessage` in
   phase D, deleting the body re-parse in callback routing.

`eosd ns-runner` is a separate process and is not instrumented; its mount/tool
timings arrive via `RunResult` as span fields. Test-determinism rule: thread-
local `set_default` subscribers do not reach `spawn_blocking`; daemon trace
tests use `with_default` on current-thread paths or a per-test global default.

### Hot-path ingestion contract

The sandbox decision engine is not allowed to wait for audit persistence.
Instrumentation inside daemon dispatch, route selection, OCC validation,
LayerStack reads, overlay setup/capture, plugin dispatch, command-session state
transitions, and isolated-workspace lifecycle code follows these rules:

1. `tracing` span/event calls record bounded typed fields into in-process span
   state only. No daemon hot-path module may depend on `rusqlite`, open audit
   files, write JSONL exports, sign audit seals, or call back to the host to
   persist trace data.
2. The subscriber layer enforces per-span and per-record budgets before storing
   a field. Oversize values become `{ truncated: true, sha256, original_len }`
   summaries; they do not allocate an unbounded `serde_json::Value`.
3. Request-scoped records are assembled after the operation decision has
   completed and immediately before envelope render. That post-decision encode
   is measured as trace overhead, but it cannot change the decision outcome.
4. Background roots use a bounded spool with non-blocking `try_push` semantics.
   On overflow the oldest background trace is dropped, `dropped_traces` is
   incremented, and the next successful export records the loss. Request-scoped
   sidecars are never dropped silently.
5. Host-side SQLite locks, hash-chain updates, segment signing, WORM export, and
   transcript archival live outside daemon execution. The host may fail closed
   before forwarding a mutating op, but it cannot slow a mutation after the
   daemon has started executing it.

### Host / Container Boundary

The audit system is intentionally split by trust and latency boundary. Host
code owns durability, sequence assignment, fail-closed forwarding, and operator
queries. Container code owns low-latency span capture for the sandbox decision
engine and returns bounded trace batches to the host.

| Boundary side | Owns | Must never own |
| --- | --- | --- |
| Host side: `eos-sandbox-gateway` + `eos-sandbox-host` | gateway UDS events, catalog routing events, `request_start`, TCP/docker-exec transport events, Docker lifecycle/registry facts, SQLite `audit_entries`, seq assignment, hash chain/seals, sidecar ingest, response digest, sidecar stripping, background export drainer, heartbeat monitor, transcript/archive pulls, operator query/CLI views | daemon dispatch decisions, LayerStack/OCC mutation logic, command/session state transitions inside the sandbox, plugin PPC callback execution |
| Container side: `eosd` + `eos-daemon` + operation crates | daemon inbound transport events, `op_request` root span, dispatch spans, op-adapter spans, subsystem events, `resource_stats`, bounded in-memory request trace assembly, bounded background spool, `_trace_events` sidecar, `sandbox.trace.export` drain payloads | SQLite, fsync, hash sealing, WORM export, host DB migrations, response-sidecar stripping, gateway UDS routing, Docker/container lifecycle management |
| Cross-boundary contract | `trace_id`, `request_id`, trace context, JSON-line request/response envelope, protobuf `TraceBatch`, `_trace_events` sidecar, `ResponseMeta.trace`, `trace_links` ids | raw auth tokens, unbounded stdout/stderr, raw file contents, host-only store internals in daemon DTOs |

Ownership consequences:

- Host-only failures (gateway parse, forbidden route, connect refused, stale
  Docker port, docker-exec fallback failure) produce host/gateway trace events
  and close the trace without daemon spans.
- Container-only failures (bad TCP auth, daemon decode, dispatcher parse,
  subsystem panic/error before response render) produce daemon sidecar events if
  the daemon can write a response; otherwise crash-log JSON lines and
  `daemon_boot_id` gaps become the evidence.
- The host may refuse a mutating op before forwarding if `request_start` cannot
  be durably appended. The container cannot make that decision because it does
  not own audit durability.
- The container may drop/truncate bounded trace children under pressure, but it
  must report `dropped_children`, `truncated`, or `dropped_traces`. The host
  records those loss markers in the immutable audit chain.

### Transport

Implementation shape:

1. Gateway `handle_connection` creates a `HostTraceBuilder` as soon as a UDS
   connection is accepted. If the request cannot be parsed, the builder mints a
   `trace_id` and generated `request_id`, records gateway parse/write events,
   returns an `Error` envelope, and appends the host-side `TraceBatch` plus
   `response_persisted`.
2. Parsed requests carry or receive a `request_id`. Gateway records route
   selection and calls `Engine::forward` with `{trace_id, request_id}`. Gateway
   never writes SQLite directly; the host-owned trace recorder is passed into
   the gateway process and is the only persistent writer.
3. `SandboxHost::forward` appends `request_start` before forwarding. This is
   the fail-closed gate for mutating ops. It then records host transport events
   as they happen: cached endpoint use, connect attempts/backoff, endpoint
   refresh, TCP write/read, docker-exec thin-client fallback, daemon respawn,
   and uncertain-outcome transitions.
4. Host-to-daemon TCP carries the same `request_id` and `trace` field plus the
   TCP auth token. The docker-exec fallback carries the same payload without
   `_eos_daemon_auth_token`, because the thin client talks to the daemon's
   container-local Unix socket.
5. The daemon creates its `op_request` root span from the request trace context.
   Its sidecar contains daemon transport, dispatch, op-adapter, subsystem, and
   resource events. The host ingests that sidecar before recording
   `host.transport.response_read`/`response_persisted`, so timeline replay shows
   daemon work between host write and host read.
6. The gateway/caller response never exposes `_trace_events`. Direct daemon
   e2e clients see the sidecar until they migrate to host/store helpers.

Request carries `request_id` instead of legacy `invocation_id` and gains an
optional `trace` envelope field (top level — a deliberate wire change under the
destructive posture):

```json
{"op":"sandbox.command.exec","request_id":"req_9f2c…",
 "trace":{"trace_id":"tr_6b1a…","parent_span_id":null},
 "args":{"cmd":"make test","caller_id":"run_1","layer_stack_root":"/eos/layer-stack"}}
```

Normal host-to-daemon TCP message (auth token shown only to document the wire;
the audit store records only `auth_token_present: true`):

```json
{"op":"sandbox.command.exec","request_id":"req_9f2c…",
 "trace":{"trace_id":"tr_6b1a…","parent_span_id":null,"capture_budget_version":1},
 "args":{"cmd":"make test","caller_id":"run_1","layer_stack_root":"/eos/layer-stack"},
 "_eos_daemon_auth_token":"<redacted>"}
```

docker-exec thin-client fallback payload:

```json
{"op":"sandbox.command.exec","request_id":"req_9f2c…",
 "trace":{"trace_id":"tr_6b1a…","parent_span_id":null,"capture_budget_version":1},
 "args":{"cmd":"make test","caller_id":"run_1","layer_stack_root":"/eos/layer-stack"}}
```

Responses carry the internal sidecar, stripped by the gateway before any
client sees it (direct daemon clients — the e2e pool — see it and assert it).
The public envelope remains JSON while the internal audit payload is canonical
protobuf bytes, base64-wrapped only because the current daemon transport is a
JSON line protocol:

```json
{"status":"ok","result":{…},"meta":{…},
 "_trace_events":{"schema":"eos.trace.v1.TraceBatch","encoding":"protobuf+base64",
                  "batch_b64":"CiR0cl82YjFh…","spool_pending":2}}
```

Gateway/caller response after host ingest strips the sidecar:

```json
{"status":"ok","result":{…},
 "meta":{"protocol_version":2,"op":"sandbox.command.exec",
         "request_id":"req_9f2c…",
         "trace":{"trace_id":"tr_6b1a…","root_span_id":1,
                  "store":"local_sqlite","event_count":24},
         "workspace_route":{"kind":"ephemeral_workspace"},
         "duration_ms":83.7,
         "modules_touched":["gateway","host","transport","dispatch","op","command_session"],
         "steps":[{"kind":"gateway.forward","duration_us":94120,"status":"ok"},
                  {"kind":"op.command.exec","duration_us":78100,"status":"ok"}],
         "resource_summary":{"cpu_usage_usec_delta":4312,"io_wbytes_delta":4096},
         "warnings":[]}}
```

Observed trace timeline for one successful forwarded op:

| Order | Stored as | Producer | Observable fact |
| --- | --- | --- | --- |
| 1 | `trace_batch` / `gateway.transport.accepted` | gateway | UDS connection accepted; client/operator surface |
| 2 | `trace_batch` / `gateway.transport.request_read` | gateway | request bytes and read duration |
| 3 | `trace_batch` / `gateway.route_selected` | gateway | catalog route, visibility, `mutates_state` |
| 4 | `trace_batch` / `gateway.engine_forward_started` | gateway | in-process call into `SandboxHost::forward` |
| 5 | `request_start` audit entry | host | `trace_id`, `request_id`, op, sandbox_id, args digest/summary, mutates_state; fail-closed gate before daemon forwarding |
| 6 | `trace_batch` / `host.transport.connect_started/finished` | host | endpoint, attempt index, connect duration |
| 7 | `trace_batch` / `host.transport.request_written` | host | request bytes, protocol version, auth-token-present boolean |
| 8 | `_trace_events` sidecar / `daemon.transport.accepted` | daemon | TCP/UDS listener, connection id, daemon boot id |
| 9 | `_trace_events` sidecar / `daemon.transport.read_finished/auth_checked/decoded` | daemon | request bytes, auth outcome, protocol version |
| 10 | `_trace_events` sidecar / spans | daemon | `dispatch`, `op.<family>.<verb>`, subsystem spans/events, `resource_stats` |
| 11 | `_trace_events` sidecar / `daemon.transport.response_write_finished` | daemon | response bytes and write duration |
| 12 | `trace_batch` / `host.transport.response_read` | host | response bytes, read duration, response digest |
| 13 | `response_persisted` audit entry | host | exact response bytes digest, status/error, sidecar ingested |
| 14 | `trace_batch` / `gateway.engine_forward_finished` | gateway | host forward outcome and duration |
| 15 | `trace_batch` / `gateway.transport.response_written` | gateway | caller response bytes and write duration; sidecar stripped |

Observed trace timeline for a pre-daemon TCP failure:

| Order | Stored as | Producer | Observable fact |
| --- | --- | --- | --- |
| 1 | `trace_batch` gateway events | gateway | UDS read, parse, route, `Engine::forward` start |
| 2 | `request_start` audit entry | host | request accepted for forwarding; mutating ops fail closed if this row cannot be written |
| 3 | `trace_batch` / `host.transport.connect_started/failed` | host | endpoint, attempt, timeout/error |
| 4 | `trace_batch` / retry/fallback events | host | backoff, endpoint refresh, docker-exec attempt, respawn attempt if needed |
| 5 | `response_persisted` audit entry | host | `status:"error"` or `status:"rejected"` with `error.kind` such as `sandbox_unavailable` or `uncertain_outcome` |
| 6 | `trace_batch` / `gateway.transport.response_written` | gateway | error envelope delivered to caller |

No daemon sidecar is expected in the failure case above; that absence is the
diagnostic signal that the request never reached daemon transport.

The base64 wrapper is not the audit format. Host ingest decodes once, stores the
exact protobuf bytes in `audit_entries.payload`, and populates relational
projection tables from the decoded DTOs. If the daemon transport later becomes a
framed binary protocol, the same `TraceBatch` bytes ride without base64 and the
store schema does not change.

`spool_pending > 0` in a sidecar — or in a heartbeat snapshot once phase H
lands, covering idle sandboxes that receive no forwards — **schedules** a
drain; the host never issues `sandbox.trace.export` on the forwarding caller's
thread, because a drain is a whole extra daemon round trip that must not be
charged to an unrelated agent request. A host-owned background drainer
performs the export round trips: single-flight per sandbox, oldest-first,
looping on `remaining_traces` until empty; drain requests arriving mid-flight
coalesce into the current loop. Deferral is audit-safe by construction: `seq`
is host-assigned at ingest, and spool overflow during the window is already
explicit via `dropped_traces`. The export op is a new catalog op, `Internal`
visibility — the gateway never routes it; in-sandbox callers cannot observe
the audit stream. Export drain is transactional (records removed only after
successful serialization) and `max_bytes`-bounded. Exhaustive drains stay
synchronous where they are teardown correctness rather than latency:
`release()` (`host.rs:122`) stops the sandbox's drainer, then drains to empty
before container removal; the e2e pool keeps its own drain helper (it bypasses
`SandboxHost::forward` — `eos-e2e-test/src/pool.rs:213`).

Loss accounting is explicit everywhere: `dropped_traces` (spool overflow),
`dropped_children`/`truncated` (per-trace caps), `daemon_boot_id` gaps
(daemon crashes), `host_boot` entries + startup reconciliation (host
crashes/restarts), `response_missing`/`uncertain_outcome`/`trace_degraded`
host rows (transport failures). Audit shows gaps; it never silently lies.

Crash forensics: the daemon also installs a `tracing-subscriber` fmt layer
writing JSON lines to the existing `--log-file` (today it only captures raw
stdout/stderr redirection). When the daemon dies mid-op, the log file holds
the structured events that never reached a sidecar or the spool.

### Host persistence (`eos-sandbox-host/src/trace_store.rs`)

Storage layout under the host `state_dir` (0700):

```
<state_dir>/
  sandbox-traces.sqlite          # immutable audit log + query projections, all sandboxes
  sandboxes/<sandbox_id>/        # per-sandbox artifact folder (bulky files, not records)
    daemon.log.jsonl             # structured crash log (fmt layer output, pulled at release/crash)
    plugins/<service_instance_id>.stderr.log  # service worker stderr (today Stdio::null), pulled at release/crash
    exports/trace-<trace_id>.jsonl   # derived human-shareable exports, rebuilt from SQLite
    sessions/<command_session_id>/   # archived command session artifacts
      transcript.log             # PTY output (teed from progress reads + settlement tail fetch)
      stdin.log                  # archived at forward time — host sees stdin first
```

One database, not one per sandbox — deliberately. Cross-sandbox audit ("all
failed plugin-overlay ops touching isolated workspaces last week, any
sandbox") is a core query; SQLite cannot join across hundreds of per-sandbox
files without `ATTACH` gymnastics, and the host process is already the single
writer for every sandbox it owns. `sandbox_id` is a keyed column on every
table. The per-sandbox **folder** exists for what does not belong in a database:
crash logs, derived JSONL exports, and transcript/stdin bulk artifacts.

Inside the database, `audit_entries` is the canonical append-only record. The
relational tables below are projections maintained for fast queries and operator
views. Projection rows may be updated or rebuilt; `audit_entries` may only
append. Per-sandbox deletion is therefore two operations: delete projection rows
and artifact folders when policy allows, then prune sealed `audit_entries`
segments only after the configured retention/export requirement has been met.

`sandbox-traces.sqlite`: 0600 file, WAL, single-writer behind a `Mutex`. No
trait seam — one backend; tests use temp dirs.

```sql
-- Connection-open pragma set (synchronous/foreign_keys are per-connection settings, not schema):
PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;

CREATE TABLE IF NOT EXISTS audit_entries (
  audit_seq             INTEGER PRIMARY KEY AUTOINCREMENT,
  sandbox_id            TEXT NOT NULL,
  trace_id              TEXT NOT NULL,
  request_id            TEXT,
  entry_kind            TEXT NOT NULL,              -- request_start|trace_batch|response_persisted|heartbeat
                                                    -- |transcript_ref|loss|trace_degraded|projection_rebuilt
                                                    -- |host_boot|prune|seal
  schema_name           TEXT NOT NULL,              -- eos.trace.v1.TraceBatch, etc.
  schema_version        INTEGER NOT NULL,
  received_at_ms        INTEGER NOT NULL,           -- host clock
  payload               BLOB NOT NULL,              -- canonical protobuf bytes
  payload_sha256        TEXT NOT NULL,
  prev_global_sha256    TEXT,                       -- total host-owned chain
  prev_sandbox_sha256   TEXT,                       -- per-sandbox chain for scoped verification
  entry_sha256          TEXT NOT NULL UNIQUE,       -- hash over header + payload + prev hashes
  segment_id            TEXT,
  key_id                TEXT,
  signature             BLOB                        -- present for seal entries; NULL for ordinary rows
);
CREATE TABLE IF NOT EXISTS audit_segment_seals (
  segment_id       TEXT PRIMARY KEY,
  first_audit_seq  INTEGER NOT NULL,
  last_audit_seq   INTEGER NOT NULL,
  root_sha256      TEXT NOT NULL,
  key_id           TEXT NOT NULL,
  signature        BLOB NOT NULL,
  sealed_at_ms     INTEGER NOT NULL,
  export_ref       TEXT                             -- WORM/object-lock path or external anchor id
);
CREATE TABLE IF NOT EXISTS trace_requests (
  request_id       TEXT PRIMARY KEY,          -- one daemon request/response
  trace_id         TEXT NOT NULL,
  sandbox_id       TEXT NOT NULL,
  op               TEXT NOT NULL,
  family           TEXT NOT NULL,             -- catalog OpFamily
  caller_id        TEXT,
  args_summary     TEXT,                      -- budgeted JSON projection of request args (from request_start)
  args_digest      TEXT,                      -- sha256 of the full args bytes as forwarded
  workspace_route  TEXT CHECK (workspace_route IN
    ('ephemeral_workspace','isolated_workspace','fast_path','none') OR workspace_route IS NULL),
  status           TEXT,                      -- envelope status; NULL = in flight; 'uncertain' after
                                              -- startup reconciliation of a prior boot's orphans
  error_kind       TEXT,
  sent_at_ms       INTEGER NOT NULL,          -- host clock, written BEFORE forward (fail-closed gate)
  received_at_ms   INTEGER,
  host_rtt_ms      INTEGER,
  duration_us      INTEGER,                   -- daemon request span duration (advisory clock)
  daemon_boot_id   TEXT,
  host_boot_id     TEXT NOT NULL,             -- host process that forwarded this request
  modules_touched  TEXT,                      -- JSON array of subsystems (denormalized rollup)
  response_digest  TEXT,                      -- sha256 of the exact response wire bytes as received
                                              -- (sidecar included), computed once at ingest — never
                                              -- from a re-serialized Value
  response_summary TEXT                       -- bounded JSON summary, not the full payload
);
CREATE TABLE IF NOT EXISTS trace_spans (
  trace_id        TEXT NOT NULL,
  request_id      TEXT,                       -- NULL for background traces
  span_id         INTEGER NOT NULL,
  parent_span_id  INTEGER,
  kind            TEXT NOT NULL,              -- SpanKind wire spelling
  subsystem       TEXT NOT NULL,
  status          TEXT NOT NULL DEFAULT 'ok',
  started_us      INTEGER NOT NULL,
  duration_us     INTEGER NOT NULL,
  fields_json     TEXT,
  PRIMARY KEY (trace_id, span_id)
);
CREATE TABLE IF NOT EXISTS trace_events (
  trace_id    TEXT NOT NULL,
  seq         INTEGER NOT NULL,               -- host-assigned, monotonic per trace
  request_id  TEXT,
  span_id     INTEGER,
  module      TEXT NOT NULL,
  event       TEXT NOT NULL,
  level       TEXT NOT NULL DEFAULT 'info',
  ts_us       INTEGER NOT NULL,
  details_json TEXT,                          -- bounded
  PRIMARY KEY (trace_id, seq)
);
-- trace_resources: time-series gauge samples, deliberately no PRIMARY KEY — rows have no per-row
-- identity (concurrent spans may emit the same kind in the same microsecond); duplicate detection
-- and integrity belong to the audit chain, since this is a projection rebuildable from audit_entries.
CREATE TABLE IF NOT EXISTS trace_resources (
  trace_id TEXT NOT NULL, request_id TEXT, span_id INTEGER,
  ts_us INTEGER NOT NULL, kind TEXT NOT NULL, values_json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS trace_links (
  trace_id  TEXT NOT NULL,
  link_kind TEXT NOT NULL,                    -- command_session|workspace_handle|plugin_service|manifest_version
  link_id   TEXT NOT NULL,
  request_id TEXT,
  PRIMARY KEY (trace_id, link_kind, link_id, request_id)
);
CREATE TABLE IF NOT EXISTS sandbox_heartbeats (
  sandbox_id        TEXT NOT NULL,
  ts_ms             INTEGER NOT NULL,           -- host clock
  daemon_boot_id    TEXT,                       -- NULL ⇒ snapshot op failed (sandbox unreachable)
  reachable         INTEGER NOT NULL,           -- 0/1
  uptime_s          REAL,
  -- layerstack
  manifest_version  INTEGER, manifest_depth INTEGER,
  active_leases     INTEGER, storage_bytes INTEGER, layer_dirs INTEGER, staging_dirs INTEGER,
  -- workspace / overlay
  open_isolated     INTEGER, overlay_mounts INTEGER,
  -- command sessions
  active_sessions   INTEGER, running_sessions INTEGER, completed_unclaimed INTEGER,
  -- plugin
  plugin_services_ok INTEGER, plugin_services_failed INTEGER,
  -- resources (cumulative gauges; host derives rates from deltas)
  cpu_usage_usec    INTEGER, cpu_throttled_usec INTEGER, cpu_nr_throttled INTEGER,
  memory_current_bytes INTEGER, memory_peak_bytes INTEGER,
  memory_oom_events INTEGER, memory_oom_kill_events INTEGER,
  io_rbytes         INTEGER, io_wbytes INTEGER, process_rss_bytes INTEGER,
  -- daemon internals
  inflight_requests INTEGER, spool_pending INTEGER, spool_dropped_total INTEGER,
  details_json      TEXT,                       -- bounded long tail (per-service health, per-session ids)
  PRIMARY KEY (sandbox_id, ts_ms)
);
CREATE INDEX IF NOT EXISTS idx_hb_time         ON sandbox_heartbeats(ts_ms);
CREATE INDEX IF NOT EXISTS idx_audit_trace     ON audit_entries(trace_id, audit_seq);
CREATE INDEX IF NOT EXISTS idx_audit_sandbox   ON audit_entries(sandbox_id, audit_seq);
CREATE INDEX IF NOT EXISTS idx_audit_request   ON audit_entries(request_id);
CREATE INDEX IF NOT EXISTS idx_requests_trace  ON trace_requests(trace_id);
CREATE INDEX IF NOT EXISTS idx_requests_sent   ON trace_requests(sent_at_ms);
CREATE INDEX IF NOT EXISTS idx_requests_status ON trace_requests(status);
CREATE INDEX IF NOT EXISTS idx_spans_kind      ON trace_spans(kind);
CREATE INDEX IF NOT EXISTS idx_spans_request   ON trace_spans(request_id);
CREATE INDEX IF NOT EXISTS idx_events_request  ON trace_events(request_id);
CREATE INDEX IF NOT EXISTS idx_resources_trace ON trace_resources(trace_id, ts_us);
CREATE INDEX IF NOT EXISTS idx_links_id       ON trace_links(link_kind, link_id);
CREATE INDEX IF NOT EXISTS idx_events_span    ON trace_events(trace_id, span_id);
```

The per-request indexes are plain — deliberately not partial and not composite:
NULL `request_id` background rows are never matched by `request_id = :request_id` lookups,
and per-request span counts are small enough that the `ORDER BY started_us` sort is
trivial.

Resource event wrapping:

- A `resource_stats` fact is first an `EventRecord` inside the currently
  entered span. It therefore carries the same `trace_id`, `request_id`, and
  `span_id` as lifecycle events, then receives host-assigned `seq` during ingest.
  The immutable payload in `audit_entries` is canonical; `trace_events` and
  `trace_resources` are rebuildable projections over the same fact.
- Before/after samples pair structurally by `(trace_id, request_id, span_id,
  kind, phase)`, not by timestamp guessing. The canonical timeline query
  interleaves `resource_stats` rows with `spawned`, `stdin_written`, `reaped`,
  and settle events in observed order.
- In-daemon mounts (`isolated.enter`, `commit_to_git` worktree mounts) emit
  `mount_started`/`mount_finished` events on the owning span, with
  `{layer_count, fsconfig_calls, duration_us, upperdir_empty_bytes}`. This makes
  the O(depth) mount claim queryable: duration should correlate with layer count,
  not workspace content bytes, and heartbeat `storage_bytes` must not jump from a
  mount alone.
- ns-runner mounts stay fields, not fabricated events. The runner is a separate
  process; its `workspace.mount_s`/`workspace.tool_s` timings arrive via
  `RunResult` and land in the parent span `fields_json`, still hash-chained but
  not pretending to be daemon-observed timeline moments.
- Tree stats are span fields on the operation that paid for the walk
  (`overlay.capture_upperdir`, teardown inspection, explicit operator
  inspection). They are never always-on `resource_stats` pairs.
- Heartbeats are the deliberate exception: they are beside-chain time-series
  rows, each backed by its own `heartbeat` audit entry, joined to traces by
  `sandbox_id` plus time window.

Immutability and compliance contract:

- Every host-observed fact first becomes an `audit_entries` row with canonical
  protobuf bytes. Query tables are populated in the same transaction when
  possible; on startup the host can rebuild every projection from
  `audit_entries`.
- `entry_sha256 = sha256(canonical_header || payload_sha256 ||
  prev_global_sha256 || prev_sandbox_sha256)`. This gives both a total host
  chain and an independently verifiable per-sandbox chain.
- Segment sealing is mandatory before retention pruning or external export:
  contiguous `audit_seq` ranges are sealed into `audit_segment_seals` with a
  signer key (`key_id`) and exported/anchored via `export_ref`. A local SQLite
  file alone is not a regulatory immutability guarantee if an attacker can
  rewrite the whole file; the segment seal is the durable tamper-evidence unit.
- Pruning operates only on whole sealed segments — never partial segments,
  never unsealed entries; `audit_segment_seals` rows are never pruned — and
  appends a `prune` tombstone entry **before** deleting rows, recording the
  pruned `segment_id`s, first/last `audit_seq`, entry and trace counts, and
  each pruned segment's `root_sha256`. The tombstone is hash-chained and later
  sealed like any entry, so deletion lives inside the auditable record;
  `trace verify` bridges a pruned segment via seal + tombstone and reports it
  as "pruned (sealed, anchored)" rather than tamper — verification stays
  total after pruning.
- Ordinary events never mutate prior audit rows. Corrections and projection
  rebuilds append new entries (`loss`, `trace_degraded`, `projection_rebuilt`,
  `seal`) instead of editing history.
- Raw stdout/stderr, file contents, and plugin result blobs still stay out of
  protobuf payloads. The immutable entry stores refs, byte counts, hashes, and
  truncation markers; bulky artifacts live under the per-sandbox artifact tree.

Store and schema versioning:

- `trace_store.rs` stamps `PRAGMA user_version = <store schema rev>` on
  create. On open, an older version runs forward-only migrations in one
  transaction; a **newer** version refuses to open — the host never writes
  through a schema it does not understand (consistent with fail-closed: a
  refused store halts mutations). Projection tables may migrate by
  drop-and-rebuild from `audit_entries`; `audit_entries` and
  `audit_segment_seals` DDL changes must be additive-only.
- Ingest skew rule: the host derives the `schema_version` column from the
  sidecar's declared schema name (`eos.trace.v1.TraceBatch` → 1). An unknown
  schema/version (daemon newer than the host during rolling dev) still
  appends the `audit_entries` row — canonical bytes are never dropped — but
  skips projections and appends a `loss` entry marking `projection_skipped`;
  a later host re-runs projection rebuild to backfill.

Write sequencing and strictness:

1. A `request_start` audit entry and `trace_requests` projection row are inserted
   and durably committed **before** forwarding (WAL + `synchronous=FULL`
   syncs the WAL on every commit, so a successful request-start append is
   power-loss durable before the mutating op runs; at human-agent op rates
   plus one heartbeat row per 10 s per sandbox, the per-commit fsync is
   immaterial on a single-host store); insert failure ⇒ mutating ops are not
   forwarded (read-only ops proceed, marked `trace_degraded`). The
   `request_start` payload is `eos.trace.v1.RequestStart { op, request_id,
   trace_id, sandbox_id, caller_id, host_boot_id, args_summary (canonical
   JSON bytes, budgeted), args_len, args_digest (sha256 of the full args
   bytes), truncated }` — computed by the host from the request it already
   holds, zero daemon hot-path cost; `trace_requests.args_summary`/`args_digest`
   are projections of it. The daemon request span keeps its separate parsed-args
   summary: raw-args-as-sent (host) vs args-as-parsed (daemon) diverge
   exactly when parse bugs occur — audit signal, not duplication.
   Mutability comes from catalog metadata (`OpContract.mutates_state`,
   `eos-operation/src/core/catalog.rs` / `ops.json`); dynamic `plugin.*` ops
   are not in the static catalog and **default to mutating** — fail-closed.
2. Sidecar ingest decodes one protobuf `TraceBatch`, appends a `trace_batch`
   audit entry, assigns `seq` in arrival order after the host's own
   `request_received`/`forward_started` events, then updates projections. Host
   appends `response_persisted` (or `response_missing`/`uncertain_outcome`) last,
   so the chain is gap-free and authoritative even when daemon batches retry.
   `response_digest` is computed here, over the exact framed response bytes as
   received from the daemon (sidecar included) — never over a re-serialized
   `Value` (serde_json is `preserve_order` across the wire crates; bytes-as-
   received is the only defined digest input) — and is carried in the
   `response_persisted` entry payload, so it is hash-chained and survives
   projection rebuild. The digest is an ingest-time commitment binding what
   the daemon sent: joinable against direct-daemon wire copies (the e2e pool
   sees pre-strip bytes), deliberately not against gateway-stripped client
   copies.
3. Host clock is truth (`sent_at_ms`/`received_at_ms`/`host_rtt_ms`); daemon
   timestamps are advisory (`daemon_boot_id` disambiguates respawns).
4. Heartbeat snapshots append `heartbeat` audit entries before inserting the
   `sandbox_heartbeats` projection row. A failed snapshot appends a loss/failure
   entry and still inserts a `reachable = 0` projection row.
5. On startup the host appends a `host_boot` audit entry recording its new
   `host_boot_id`, rebuilds/repairs projections, then reconciles orphans:
   every `trace_requests` row with `status IS NULL` from a prior `host_boot_id` is
   necessarily orphaned (a restarting host has no in-flight forwards) — for
   each, append an `uncertain_outcome` loss entry (`entry_kind = 'loss'`,
   payload referencing `request_id`, `trace_id`, and the orphaning boot) with a
   host-assigned `seq` in that trace's chain, then set the projection row to
   `status = 'uncertain'`. Append-before-update, so projection rebuild
   reproduces `status = 'uncertain'`.

Acceptance queries (phase gates assert these run, return correct shapes, and
execute via an index: a phase B test runs `EXPLAIN QUERY PLAN` on each and
fails if any trace-table access is a SCAN rather than a SEARCH — bundled
rusqlite pins the SQLite version, so the plan-shape assertion is
deterministic in-repo):

```sql
-- (1) Replay one user-visible call as a timeline
SELECT seq, module, event, details_json FROM trace_events
WHERE trace_id=:trace_id ORDER BY seq;

-- (2) Per-step durations + subsystems touched for one request response
SELECT s.kind, s.subsystem, s.duration_us/1e3 ms, s.fields_json
FROM trace_spans s WHERE s.request_id=:request_id ORDER BY s.started_us;

-- (3) Full long-running command lifecycle across requests (exec → stdin → polls → background settle)
SELECT o.request_id, o.op, o.status, o.sent_at_ms FROM trace_requests o
JOIN trace_links l ON l.trace_id=o.trace_id
WHERE l.link_kind='command_session' AND l.link_id=:session_id
ORDER BY o.sent_at_ms;

-- (4) All failed plugin-overlay requests touching isolated workspaces, last 7 days
SELECT * FROM trace_requests
WHERE family='Plugins' AND status IN ('error','rejected')
  AND workspace_route='isolated_workspace'
  AND sent_at_ms > (strftime('%s','now')-7*86400)*1000;

-- (5) Fetch the immutable chain slice backing one trace
SELECT audit_seq, entry_kind, payload_sha256, prev_global_sha256,
       prev_sandbox_sha256, entry_sha256
FROM audit_entries
WHERE trace_id=:trace_id ORDER BY audit_seq;

-- (6) Pair the raw before/after gauges for one request span
SELECT b.values_json AS before_values, a.values_json AS after_values
FROM trace_resources b
JOIN trace_resources a
  ON a.trace_id=b.trace_id AND a.request_id=b.request_id
 AND a.span_id=b.span_id AND a.kind=b.kind
WHERE b.request_id=:request_id
  AND json_extract(b.values_json,'$.phase')='before'
  AND json_extract(a.values_json,'$.phase')='after';

-- (7) Mount cost audit: duration should scale with layer_count, not content bytes
SELECT json_extract(details_json,'$.layer_count') AS layer_count,
       json_extract(details_json,'$.fsconfig_calls') AS fsconfig_calls,
       json_extract(details_json,'$.duration_us') AS duration_us
FROM trace_events
WHERE event='mount_finished'
ORDER BY layer_count, duration_us;
```

Retention: `prune_before(ms)` ships unwired (audit store; policy is an open
question); when wired it operates only on whole sealed segments and appends
the `prune` tombstone described above before deleting — it refuses to delete
unsealed `audit_entries`. A derived
`trace-<trace_id>.jsonl` export command provides the human-shareable text form
from projections + protobuf payloads — JSONL is a view, never the record.

TS join story: `request_id` is a 32-hex uuid4 (valid OTel trace-id format); id and
timestamp columns stay OTel-format-compatible so the TS agent (OTel JS per the
migration index) joins its run audit logs to this store without schema
migration. We take the format compatibility, not the OTel Rust SDK.

### Continuous monitoring: heartbeat snapshots

Per-request traces answer "what happened during this request"; they cannot answer
"what state is the sandbox in *right now* / was in at 14:32". That is a
separate, time-series capability:

**Daemon side** — new builtin op `sandbox.status.snapshot` (`Internal`
visibility, `workspace_route = none`, `trace_exempt`). It is an aggregation
over collectors that already exist, plus two small additions:

| Snapshot section | Source (existing unless marked new) |
| --- | --- |
| layerstack: manifest version/depth, active leases, storage bytes, layer/staging dirs, service-cache gauges (hits/misses/creates/evictions, lock-wait total/max) | `op_adapter/checkpoint.rs:39-49` (`layer_metrics` internals, called directly) + `eos-layerstack/src/service.rs:214` (`cache_snapshot()`) |
| workspace: open isolated workspaces (ids, age, last_activity) | isolation registry (`isolation.list_open` internals) |
| overlay: active overlay mount count | **new** — `/proc/self/mountinfo` scan, same source the teardown inspection already reads |
| command sessions: active/running/completed-unclaimed counts, per-session {id, status, age} | command registry (`command.count` + session table internals) |
| plugin: per-service health (probe status, accepted, pid alive), state (`starting\|ready\|refreshing\|stale\|restarting\|stopped\|failed`), restart/refresh counts, last_error, setup failures | plugin registry (`plugin.status` internals + `eos-plugin/src/service_registry.rs:15-40` state fields, summarized) |
| resources: `ResourceStats` heartbeat subset — cgroup CPU usage/throttling, memory current/peak/OOM events, IO r/w bytes, daemon RSS; PSI pressure and optional swap stats in `details_json` until promoted by a query | the samplers in `runtime/response.rs:198-270` — **consolidated into one shared `eos-trace` sampler**, deleting the duplicate in `settle.rs:254` |
| daemon internals: uptime, boot_id, in-flight requests, spool pending/dropped totals | dispatcher uptime, in-flight registry, trace spool counters |

The snapshot is a typed `SandboxStatusSnapshot` DTO in `eos-trace` (shared
with the host) and encoded as protobuf for host ingest, not a loose JSON map.
Cost: registry reads + three procfs/cgroupfs file reads — no workspace or
LayerStack mutation, safe at short intervals.

**Host side** — `HeartbeatMonitor` in `eos-sandbox-host`: one interval task
per acquired sandbox (`HostConfig.heartbeat_interval_ms`, default 10 000; 0 =
disabled) that calls the snapshot op and inserts a `sandbox_heartbeats` row.
Semantics:

- **Liveness**: a failed/timed-out snapshot still inserts a row with
  `reachable = 0` and NULL gauges — silence is recorded, never inferred. A
  `daemon_boot_id` change between consecutive rows marks a respawn.
- **Rates**: cumulative counters (cpu_usage_usec, io bytes) are stored raw;
  utilization/rates are derived at query time from row deltas — the store
  never loses the raw gauge to pre-computation.
- **Status derivation** (query-time view, not stored): `degraded` when plugin
  services report failures, leases pile up beyond config, or spool drops are
  growing; `unreachable` after N consecutive `reachable = 0` rows. Thresholds
  live host-side; the daemon only reports facts.
- A snapshot reporting `spool_pending > 0` schedules a background-drainer
  pass (see Transport) — heartbeats cover idle sandboxes that receive no
  forwards, e.g. sweeper-cancelled sessions the host never polls.
- Heartbeat rows are **not** traces: they are a time series beside the trace
  tables, joinable by `sandbox_id` + time window (e.g. "show heartbeats
  bracketing this slow op"). They are still audit-backed: every successful or
  failed snapshot appends an `audit_entries` row before the projection insert.

Monitoring queries the schema must answer:

```sql
-- Current state of every sandbox (latest row per sandbox)
SELECT * FROM sandbox_heartbeats h
WHERE ts_ms = (SELECT MAX(ts_ms) FROM sandbox_heartbeats WHERE sandbox_id = h.sandbox_id);

-- CPU/memory trend for one sandbox over the last hour (rates from deltas)
SELECT ts_ms,
       (cpu_usage_usec - LAG(cpu_usage_usec) OVER w) * 1.0
         / ((ts_ms - LAG(ts_ms) OVER w) * 1000) AS cpu_util,
       memory_current_bytes
FROM sandbox_heartbeats WHERE sandbox_id = :id
  AND ts_ms > (strftime('%s','now')-3600)*1000
WINDOW w AS (ORDER BY ts_ms);

-- What was the sandbox doing when this op was slow?
SELECT * FROM sandbox_heartbeats
WHERE sandbox_id = :sandbox_id
  AND ts_ms BETWEEN :op_sent_at_ms - 30000 AND :op_received_at_ms + 30000;
```

Operator surface: `eos-sandbox-host` (or `xtask`) gains `sandbox status
[<sandbox_id>] [--watch]` rendering the latest snapshot per sandbox and
tailing new rows — the human view over the same table.

### Command transcripts: live plane vs archive plane

Command sessions currently write `metadata.json`, runner request/result files,
`transcript.log`, and `final.json` in the session dir
(`eos-operation/src/command/prepare.rs:100-119`). Normal finish persists
`final.json` and removes the transcript; orphan recovery removes the whole dir
later. `read_command_progress` tail-reads the transcript just-in-time
(`eos-command-session/src/session.rs:183-184`). The archive contract therefore
has to capture artifacts and failures at their actual lifecycle points, not just
at final directory destruction.
Two consumers with **different requirements** — naming them dissolves the
push-vs-pull dilemma:

| Consumer | Requirement | Served by |
| --- | --- | --- |
| Agent progress reads (`read_command_progress`) | Zero-latency truth, right now | The in-sandbox transcript, pulled per request — **unchanged**. A host mirror is always stale; it can never serve this |
| Audit archive | Completeness + durability | Host copy whose deadline is **destruction time, not real time** |

Neither a daemon push channel nor a faster heartbeat is the answer:

- **No push.** The daemon is a pure server (one request/response per inbound
  connection, `server.rs:262`); push means daemon-initiated egress plus a
  host-side listener — exactly the connectivity a sandbox must not have. It
  also buys freshness the audit plane doesn't need.
- **No heartbeat cranking.** Heartbeats are a fixed-size state time series;
  transcripts are unbounded bulk data. Raising the snapshot frequency to chase
  transcript bytes conflates the two planes and still loses the
  destruction race.

Instead, **destruction-gated archival** with a free progressive tee:

1. **stdin needs no transfer at all**: the host forwards `write_stdin` and
   archives the payload at forward time — it sees stdin before the sandbox
   does. Stored as `sandboxes/<sandbox_id>/sessions/<session_id>/stdin.log`
   with ts + request_id prefixes.
2. **Tee what already crosses the wire**: every `read_command_progress`
   response carries transcript lines the agent paid for anyway; the host tees
   them into `sessions/<session_id>/transcript.log`, tracking the archived
   byte offset. Zero extra round trips during the session.
3. **Settlement fetch of the un-teed tail**: at settle/collect the host
   fetches the remaining bytes by offset (ranged `sandbox.command.transcript`
   read, `Internal` visibility), bounded by
   `transcript_archive_max_bytes` (config; truncation recorded with a
   `truncated` marker + full-length sha256 so tampering/loss is evident).
4. **Destruction is gated on the archive ack**: the session dir survives the
   reap until the host confirms the archive (the completion already waits in
   the completed buffer for collection — the dir adopts the same holding
   pattern), with a TTL fallback so an absent host cannot fill sandbox disk;
   a TTL-fired deletion writes a `transcript_lost` event into the trace chain
   instead of losing it silently. Isolated-workspace exit orders an archive
   step **before** `rmtree_scratch`. The orphan-recovery path
   (`runtime.rs:60-102`) retains the dir under the same gate.

The database stores references, never the bulk (bounded-detail rule):
`trace_links` ties `command_session` → trace; the settle trace appends a
`transcript_ref` audit entry and projection detail
`{transcript_path, stdin_path, bytes, sha256, truncated}`. JIT progress reads
keep hitting the daemon; auditors read the archived files; the two planes never
trade their requirements against each other.

### Libraries

| Crate | Version | Where | Role |
| --- | --- | --- | --- |
| `tracing` | 0.1 (MIT) | daemon, operation, layerstack, workspace, command-session, plugin, eos-trace | span/event facade |
| `tracing-subscriber` | 0.3, `registry`,`std`,`fmt`,`json` (MIT) | eos-trace, eos-daemon | Registry + custom Layer; JSON fmt layer for the crash log |
| `rusqlite` | 0.40 `bundled` (MIT) | eos-sandbox-host only | host store; daemon binary unaffected |
| `prost` / `prost-build` | workspace-pinned | eos-trace | protobuf DTO generation and canonical audit payload encoding |
| `base64` | workspace-pinned | eos-daemon/eos-sandbox-host | temporary JSON-line transport wrapper for protobuf sidecars |
| `sha2` (already in workspace) | — | host | response digests, payload digests, hash-chain entries |
| signer (`ed25519-dalek` or host signer provider) | workspace-pinned / configured | eos-sandbox-host | segment-seal signatures over immutable audit ranges |
| existing serde/serde_json/uuid/thiserror/tokio | — | — | reused |

Rejected: OTel Rust SDK (pre-1.0 churn in the static daemon binary; format
compatibility kept instead); `tracing-chrome`/`tracing-tracy` (profiling
viewers, not audit persistence); `minitrace`/`fastrace` (thread-local-only
parenting fights the `spawn_blocking`/commit-worker handoffs); `sqlx`/`diesel`
(overkill for one single-writer store).

## Part B — Response Contract

One envelope for every op. `status` is the single discriminant; arms carry
`result` XOR `error` (never null pairs); everything cross-cutting lives in
`meta`. Rendered in `eos-operation/src/core/envelope.rs`, replacing
`OpResponse` and every ad-hoc `json!` site.

```rust
#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum OperationEnvelope<T: Serialize> {
    Ok       { result: T, meta: ResponseMeta },
    Running  { result: T, meta: ResponseMeta },  // accepted; continues via a linked resource
    Cancelled{ result: T, meta: ResponseMeta },  // settled facts of the cancelled work
    TimedOut { result: T, meta: ResponseMeta },
    Rejected {                                   // domain refusal: OCC conflict, policy, isolated-gate
        error: OperationFault,
        #[serde(skip_serializing_if = "Option::is_none")]
        result: Option<T>,                       // partial domain facts when work happened before the
        meta: ResponseMeta,                      // rejection — e.g. a command that ran (exit 0, output
    },                                           // captured) but lost its OCC publish
    Error    { error: OperationFault, meta: ResponseMeta }, // parse/transport/internal/unexpected
}

#[derive(Serialize)]
pub struct ResponseMeta {
    pub protocol_version: u8,                    // 2
    pub op: String,                              // catalog name or dynamic plugin.* op
    pub request_id: RequestId,
    pub trace: TraceRef,                         // { trace_id, root_span_id, store: "local_sqlite", event_count }
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caller_id: Option<CallerId>,
    pub workspace_route: WorkspaceRoute,         // { kind, reason? }
    pub duration_ms: f64,                        // derived from the request span — not hand-inserted
    pub modules_touched: Vec<Subsystem>,         // derived from the span tree
    pub steps: Vec<StepSummary>,                 // derived: { kind, duration_us, status } per direct child span
    pub resource_summary: ResourceSummary,       // bounded rollup from ResourceStats: changed paths, depth, cpu/io deltas, throttling, OOM/pressure markers
    pub warnings: Vec<String>,
}

#[derive(Serialize)]
pub struct OperationFault {
    pub kind: String,        // rejected → op-policy vocabulary; error → protocol vocabulary
    pub message: String,
    pub details: serde_json::Value,              // {} when empty, never null
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_id: Option<String>,                // internal errors only; explicit
}
```

`meta` is **rendered from the request's span tree** (closed immediately before
envelope render; the wire write event lands host-side). Timing/resource facts
appear exactly once, in one vocabulary — the span kinds. There is no parallel
hand-maintained timings map anywhere.

`OperationFault.details` is structured, never a flattened string: faults
preserve the error source chain (`source_chain[]`), io error kind, offending
path, and child exit codes where present. The rev-4 sweep found every wire
error today collapses its chain into one lossy string and that the gateway
hardcodes `details: {}` — both quirks die with the old envelope.

Per-family `result` shapes (typed DTOs in each family's `contract.rs`; the
direction, merged from the parallel draft):

| Family | `result` shape |
| --- | --- |
| Files | `{ file: {…} }` for reads; `{ mutation: { status, published, changed_paths, changed_path_kinds, conflict? } }` for writes/edits |
| Command | `{ command: { status, exit_code, output_ref, command_session_id }, mutation?: {…} }`; `Running` omits `mutation` |
| Checkpoint | `{ checkpoint: {…} }` — layer metrics are domain data, not metadata |
| Isolated workspace | `{ isolated_workspace: { open, workspace_handle_id, workspace_root, lifetime_s?, evicted_upperdir_bytes?, inspection? } }` |
| Plugin | `{ plugin: {…}, overlay?: {…} }` — worker result stays under `result.plugin`; ensure/status share one typed `PluginServicesView` instead of duplicating the services snapshot |
| Control / workspace-run | narrow typed counts/readiness/cancellation objects |

Example success (file write, fast path):

```json
{"status":"ok",
 "result":{"mutation":{"status":"committed","published":true,
           "changed_paths":["src/a.rs"],"changed_path_kinds":{"src/a.rs":"write"}}},
 "meta":{"protocol_version":2,"op":"sandbox.file.write","request_id":"req_6b1a…",
         "trace":{"trace_id":"tr_6b1a…","root_span_id":1,"store":"local_sqlite","event_count":9},
         "workspace_route":{"kind":"fast_path"},
         "duration_ms":4.2,
         "modules_touched":["dispatch","op","layer_stack","occ"],
         "steps":[{"kind":"dispatch","duration_us":310,"status":"ok"},
                  {"kind":"op.file.write","duration_us":3650,"status":"ok"}],
         "resource_summary":{"changed_path_count":1,"layer_stack_manifest_depth":3},
         "warnings":[]}}
```

Status mapping rules (total over today's observable states):

| Today | Envelope status | Where the detail lives |
| --- | --- | --- |
| command `running` | `running` | `result.command` + `command_session` link |
| command `ok`, publish committed | `ok` | `result.mutation.status = committed` |
| command `ok`, publish lost OCC (aborted_version/overlap) | `rejected` **with partial `result`** (output, exit_code, discarded paths) | `error.kind = occ_conflict`; `result.command` keeps the facts |
| command `cancelled` / `timed_out` | `cancelled` / `timed_out` | `result` carries settled facts (output so far, kill reason) |
| mutation `accepted` (pre-commit OCC ack) | `ok` | `result.mutation.status = accepted` — domain data, not an envelope state |
| `Refused(OpError)` policy refusals, isolated-gate, lifecycle-in-progress | `rejected` | `error.kind` keeps the op-policy vocabulary |
| parse/transport/auth/unknown-op/internal | `error` | `error.kind` keeps the protocol vocabulary; `error_id` for internal |

Example rejection (OCC conflict):

```json
{"status":"rejected",
 "error":{"kind":"occ_conflict","message":"path contended: src/a.rs",
          "details":{"conflict_file":"src/a.rs","reason":"aborted_overlap"}},
 "meta":{"…":"…","workspace_route":{"kind":"ephemeral_workspace"}}}
```

Framer note: `op` lives inside `meta`, never top-level — `decode_value`
classifies any object with a top-level `op` key as a Request
(`eos-daemon/src/wire/message.rs:139`). `WireMessage` disambiguation is
updated to the new shapes (`status` + `error`) in the same phase that lands
the envelope.

Migration mechanics: a v1 flattening adapter (typed envelope → today's flat
shape) exists **only** while families migrate, so each family can flip
independently; gateway/host/e2e assertions for a family are rewritten in the
same PR that flips it. The adapter and `is_success`/`error_kind` helpers are
deleted in the final phase. No deprecation period beyond the ladder itself —
nothing outside this repo consumes the wire today.

## Extension Model — Introducing New Trace Surfaces

The system is generic at five seams — the identity model (ids carry no domain
semantics), the trace object (module/event are data, not schema), the store
(`audit_entries` holds `schema_name` + `schema_version` + opaque canonical
bytes; projections rebuild), delivery (the root span opens at the transport
layer, so every op is traced before any op-specific code runs), and the
instrumentation facade (mechanism crates depend on `tracing` only). It is
deliberately **closed** everywhere vocabulary lives, because an audit trail
whose names drift stops being replayable. Extensions are therefore additive
and mechanical; everything else is a design change (see Non-goals).

### Extension recipes

| Scenario | You add | Inherited unchanged | Gate |
| --- | --- | --- | --- |
| New op in an existing family | Catalog entry (`OpContract` incl. `mutates_state`), `op.<family>.<verb>` span at the adapter, route classification row | Root trace, fail-closed `request_start`, sidecar, store, replay queries, CLI | e2e trace assertion: op appears with route recorded; catalog test |
| New op family | Family `result` DTO in its `contract.rs`; op spans; vocabulary rows if it owns new events | The envelope — the six `status` arms are total; domain states go in `result`/`error.kind`, never new arms | Golden envelope serialization; family e2e |
| New subsystem / mechanism crate | `tracing` macros only; `SpanKind` variants + exhaustive `subsystem()` mapping in `eos-trace`; one vocabulary row per module here | Spool, budgets/truncation, ingest, indexes, `trace verify` | Rule A hot-path gate (no persistence deps — `rusqlite` ban list grows with the crate); span-tree test via `with_default` |
| New background work | One `TraceKind` variant; standalone root carrying its link ids | Spool, export op, host drainer, loss accounting | e2e: trace arrives via export with the chain `trace_id` |
| New event in an existing module | A name in that module's vocabulary row + typed bounded details, in the same change as the instrumentation | Storage — events are rows, zero DDL | Replay query returns it; details fit the budget table |
| New span field | Typed, bounded, recorded at the decision site (late via `Span::record` where dynamic) | — | Field appears in goldens; budget respected |
| New gauge / snapshot section | Additive protobuf field on `SandboxStatusSnapshot`; a heartbeat column only if queried/indexed, else `details_json` | Heartbeat audit chain, `--watch`, query-time rate derivation | Additive-only proto gate; heartbeat e2e |
| New audit `entry_kind` | Name + payload schema under `proto/eos/trace/v1` | Hash chain, sealing, pruning, startup rebuild | Projection-rebuild test covers the new kind |
| New link kind | `link_kind` spelling + a row in the per-kind semantics table declaring **chain vs tag** | Rebuildable chain maps | Chain-continuity test when chain-kind |
| New consumer | Reads projections/exports only; direct-daemon clients aside (e2e pool), never parse `_trace_events` — the gateway strips it | OTel-format-compatible ids and timestamps | — |
| Schema/payload field change | Defer to audit rule C: optional/additive or a new schema version | Old fixtures stay decodable | Golden protobuf compatibility tests |

### Vocabulary governance

1. **This spec is the registry.** A span kind, event name, link kind, trace
   kind, or route value exists when its row exists here; the change that adds
   instrumentation updates the spec row in the same PR. The per-module lists
   are the *required minimum for emission compliance*; emitting a name with no
   registry row is a review defect, not an extension.
2. **Names are append-only facts.** Past-tense events (`*_started`,
   `*_finished`, `*_failed`, `*_skipped`); failures are explicit events or
   `{error}` fields, never a silent absence of the success key. A wrong name
   is superseded by a new row — never renamed or reused with new semantics,
   because stored protobuf strings must replay forever.
3. **Closed sets stay closed.** Envelope `status` (6 arms), `WorkspaceRoute`
   (4 values), `Subsystem`, `SpanKind`, `TraceKind` extend only by enum
   variant in `eos-trace` with exhaustive mapping — compile-checked, which is
   the drift guard phase F relies on.
4. **Budgets are named.** A new field fits an existing budget row or adds a
   named one to the capture-budget table; overflow always records
   `{truncated, sha256, original_len}`.
5. **Bounded-detail rule applies to every extension**: sizes, hashes, counts,
   ids, refs — never raw stdout/stderr, file contents, or result blobs in
   trace payloads; bulk goes to the per-sandbox artifact tree with a ref
   entry.

### Non-goals (design changes, not extensions)

Each of these requires revisiting this spec, not a local patch:

| Non-goal | Why closed | The path if it ever becomes real |
| --- | --- | --- |
| Second storage backend / store trait seam | One single-writer SQLite is load-bearing for seq + fail-closed | Re-ingest history from `audit_entries` canonical bytes; no seam needed in advance |
| Second writer / multi-host store | `seq` assignment and chain maps assume one host process | Partition by store; revisit trace-minting ownership (Open Question 2) |
| Plugin-minted span kinds or event names | Closed vocabulary is the audit guarantee | Plugin detail rides bounded `details` under the `plugin` module rows |
| New envelope `status` arm | Six arms are total over observable outcomes | New outcomes map to existing arms + `error.kind`/result detail |
| Daemon-side persistence of any trace data | Audit rule A (hot-path decoupling) | None — this is permanent |

## Phased Plan

Every phase must update the progress tracker above when it lands. A phase is
not complete until its acceptance criteria and verification checklist pass in
the live checkout.

### Phase 0 — spec and e2e retarget map

Scope: make this artifact implementation-ready before code changes start.

Acceptance criteria:

- [x] The head section states whether the plan is implementable and names the
  highest-risk migration constraints.
- [x] The progress tracker exists before design details so future implementers
  can mark phase status without rereading the whole spec.
- [x] Each implementation phase below has acceptance criteria and verification
  commands, not only prose.
- [x] The e2e migration is targeted at the live suite directories:
  `core`, `daemon`, `eos-layerstack`, `ephemeral_workspace`,
  `workspace-publish-gate`, `workspace-runtime-command-session`,
  `workspace-runtime-isolated`, `plugin`, `pressure`, plus shared `support`;
  `assets` and `unit` remain fixtures/unit checks, not migration suites.
- [x] Names are consistent: `request_id`, `trace_requests`, and
  `resource_stats`; legacy names appear only as current-code migration notes.

Verification:

- [x] `git diff --check -- docs/plans/sandbox-event-tracing-and-response-contract_SPEC.md`
- [x] `rg -n "resource_sampl(e|ed)|\\bop_i[d]\\b|trace_op[s]" docs/plans/sandbox-event-tracing-and-response-contract_SPEC.md` returns no normative uses.
- [x] `find sandbox/crates/eos-e2e-test/tests -maxdepth 1 -mindepth 1 -type d -print | sort` matches the retarget map above.

### Phase A — contracts first

Scope: create the typed audit/response contracts before implementation code
depends on them.

Acceptance criteria:

- [ ] `sandbox/crates/eos-trace` owns `TraceId`, `RequestId`, `SpanUid`,
  `TraceRecord`, `SpanRecord`, `EventRecord`, `TraceResource`, `TraceLink`,
  `WorkspaceRoute`, `TraceKind`, `SpanKind`, bounded-detail helpers, and the
  closed subsystem mapping.
- [ ] `eos-trace/proto/eos/trace/v1` defines protobuf `TraceBatch`,
  `TraceSpan`, `TraceEvent`, `TraceResource`, `TraceLink`, `RequestStart`,
  `SandboxStatusSnapshot`, and `ResponseTraceRef`; JSON is not a daemon-host
  audit payload.
- [ ] `eos-operation` exposes `OperationEnvelope<T>`, `ResponseMeta`,
  `OperationFault`, and per-family result DTO skeletons without leaking raw
  `serde_json::Value` as the public response contract.
- [ ] Bounded-detail budgets are enforced in shared constructors; truncation
  records `{truncated, original_len, sha256}`.
- [ ] A short-lived v1 flattening adapter exists only inside this migration
  phase ladder and is marked for deletion in phase F.

Verification:

- [ ] `cargo test -p eos-trace -p eos-operation`
- [ ] Protobuf golden compatibility fixtures decode successfully after schema
  regeneration.
- [ ] Envelope/adapter golden tests cover `ok`, `running`, `rejected`,
  `cancelled`, `timed_out`, and `error`.

### Phase B — host store

Scope: make the host the durable, single-writer audit authority.

Acceptance criteria:

- [ ] `eos-sandbox-host` owns `trace_store.rs` and DDL for append-only
  `audit_entries`, hash chain fields, segment seals, trace/span/event/resource/
  link/heartbeat projections, and `user_version` migrations.
- [ ] Mutating ops append and durably commit `RequestStart` before forwarding;
  if that append fails, the op is not forwarded.
- [ ] Read-only ops can proceed with a chained `trace_degraded` marker when
  the store is unavailable.
- [ ] Sidecar ingest assigns gap-free host `seq` per `trace_id`, rebuilds
  projections from canonical protobuf bytes, and exposes lookup helpers by
  `trace_id`, `request_id`, and link ids.
- [ ] Host startup records `host_boot`, reconciles prior incomplete rows to
  `uncertain`, and refuses to open newer `user_version` databases.
- [ ] Pruning writes tombstone/range proof entries and never removes unsealed
  canonical data.

Verification:

- [ ] `cargo test -p eos-sandbox-host`
- [ ] Store tests cover fail-closed mutating forwarding, read-only degraded
  forwarding, tamper detection, segment-signature verification, projection
  rebuild, startup reconciliation, and refuse-newer-version behavior.
- [ ] SQLite posture test asserts `journal_mode=WAL`, `synchronous=FULL` on the
  live connection used for request-start appends.
- [ ] `EXPLAIN QUERY PLAN` tests cover all acceptance queries in this spec.
- [ ] Seal -> prune -> verify reports the pruned range and still validates the
  retained chain.

### Phase C — daemon propagation

Scope: move trace identity and span capture through the daemon without durable
daemon-side persistence.

Acceptance criteria:

- [ ] `Request` carries a trace field encoded by host/gateway with
  `trace_id`, `request_id`, parent/link hints, and capture budget version.
- [ ] Gateway UDS handling starts or adopts the request trace, records inbound
  read/parse/write events, records catalog route decisions, and wraps the
  in-process `Engine::forward` call with start/finish/failure events.
- [ ] Host forwarding records outbound TCP connect/retry/fallback, request
  write, response read, empty response, decode failure, and timeout facts in
  the host audit store even when no daemon response/sidecar is received.
- [ ] `handle_connection` opens the root `op_request` span before reading the
  request line and records `connection_id`, listener kind, `peer_addr`,
  `local_addr`, `request_bytes`, bad JSON, oversized payloads, timeouts, auth
  failures, cancellations, response-write failures, and shutdown failures.
- [ ] Dispatch and `op.<family>.<verb>` spans are emitted at the existing
  dispatcher/op-adapter boundaries, with route decisions recorded at the
  decision site.
- [ ] Finalization assembles a protobuf sidecar for request traces; background
  roots use a bounded non-blocking spool drained by `sandbox.trace.export`.
- [ ] The host background drainer is single-flight per sandbox and never runs
  on the request-forwarding caller's thread.
- [ ] Crash-log formatting is installed before the listener binds and records
  `config_loaded` and `listen_bound`.

Verification:

- [ ] `cargo test -p eos-daemon`
- [ ] Gateway tests cover UDS malformed JSON, unknown op, forbidden surface,
  catalog daemon route, host route, plugin fallback route, response-write
  failure, and `Engine::forward` success/error event emission.
- [ ] Wire tests cover accepted requests, decode failures, oversized messages,
  auth failures, timeouts, cancellations, and response-write/shutdown failure
  handling.
- [ ] Host transport tests cover connect refusal, connect timeout, endpoint
  refresh after stale Docker port, retry backoff, write failure, empty
  response, response decode failure, and read timeout.
- [ ] Trace tree tests use current-thread `with_default` and assert root,
  dispatch, op-family, sidecar, and export-spool behavior.
- [ ] Store replay test reconstructs one successful daemon op as:
  gateway UDS -> catalog route -> host forward -> TCP connect/write/read ->
  daemon accept/read/auth/decode -> dispatch -> op_adapter -> subsystem ->
  daemon response write -> host response read -> gateway response write.
- [ ] A dependency guard proves daemon crates do not depend on `rusqlite` or
  host-store modules.
- [ ] A hot-path unit/bench test asserts representative dispatch/route
  decisions stay bounded with tracing enabled and a slow host store.

### Phase D — subsystem events and resource stats

Scope: replace response-only metrics/logging with typed subsystem facts and
queryable `resource_stats`.

Acceptance criteria:

- [ ] LayerStack/OCC emit worker-handoff spans, manifest/lease/squash events,
  per-file conflict detail, and squash skip/fail reasons.
- [ ] Overlay emits mount/capture/unmount events, mount cost fields, capture
  walk counters, failing paths, and real truncation.
- [ ] Command session adds `ActiveCommand` origin `trace_id`/`request_id`,
  background `CommandSettle` roots, stdin wait/backpressure facts, kill
  signals, artifact write/failure events, and transcript failure events.
- [ ] Isolated workspace emits enter/status/exit/recovery lifecycle facts,
  DNS fallback, holder liveness, manager JSON errors, mountinfo scan errors,
  and orphan cleanup results.
- [ ] Plugin/PPC emits setup/service health/state facts, typed
  `parent_message_id`, service stderr path, overlay/callback parent handoff,
  and bounded `tool_result` parse/size/digest facts.
- [ ] Checkpoint/git operations emit command step events with exit codes and
  bounded stderr tails.
- [ ] `ResourceStats` covers cgroup CPU/memory/io/PSI where available,
  daemon RSS/HWM, tree stats, mount cost, source availability/error markers,
  sampler duration, and `inflight_requests`.
- [ ] Always-on before/after pairs are limited to cheap kernel gauges around
  `command.session.wait` and `plugin.overlay.run`; tree walks occur only on
  spans that already perform or explicitly request a bounded walk.

Verification:

- [ ] Per-crate focused tests for `eos-layerstack`, `eos-workspace`,
  `eos-command-session`, `eos-plugin`, `eos-operation`, and `eos-daemon`.
- [ ] Live e2e trace query: file `fast_path` request produces route, OCC, and
  no-workspace facts.
- [ ] Live e2e trace query: ephemeral exec produces command, overlay, capture,
  resource before/after, changed-path, and response meta facts.
- [ ] Live e2e trace query: isolated enter/exec/status/exit produces one chain
  with lifecycle, command, teardown, and heartbeat-adjacent facts.
- [ ] Live e2e trace query: sweeper-cancelled command session yields a
  `CommandSettle` background trace linked to the original command session.
- [ ] Live e2e trace query: plugin callback-driven OCC publish parents under
  the owning plugin op trace.
- [ ] Resource query returns before/after rows for command/plugin spans, real
  tree-walk truncation when the budget is exceeded, and mount cost
  `layer_count`/`fsconfig_calls`/`duration_us`.

### Phase E — response and e2e migration

Scope: destructively migrate responses while keeping mixed-wire decode safe
inside the migration window.

Acceptance criteria:

- [ ] Step 0 lands a shape-aware host/gateway decoder:
  `status` present -> new envelope path; otherwise legacy path. Legacy
  `is_success`/`error_kind` behavior is confined to the legacy branch before
  any family flips.
- [ ] Shared e2e trace support lands before family flips:
  `sandbox/crates/eos-e2e-test/tests/support/trace.rs` plus host
  `e2e_support` helpers where daemon-side sidecar capture is needed.
- [ ] Shared helpers decode `OperationEnvelope`, extract `trace_id` and
  `request_id`, assert gateway-facing responses do not expose `_trace_events`,
  drain `sandbox.trace.export`, query the host trace store, and compare
  response `meta.trace_ref` against stored spans/resources/events.
- [ ] Family flips proceed in this order: control/checkpoint -> files ->
  isolated -> command -> plugin. Each flip rewrites that family's gateway,
  host, and e2e assertions in the same change.
- [ ] `timings.*`, `resource.*` flat keys, and response `success` branching are
  not accepted in a flipped family; equivalent checks use span durations,
  `trace_resources`, `trace_events`, `trace_requests`, and response
  `meta.resource_summary`.
- [ ] Generated e2e inventory docs under `sandbox/crates/eos-e2e-test/tests`
  are regenerated only after the migrated tests reflect the new contracts.

Verification:

- [ ] Mixed-wire decoder unit test covers legacy success, legacy error, new
  `ok`, new `running`, and new `error` where legacy `is_success` would have
  misclassified the response.
- [ ] `cargo test -p eos-e2e-test -- --list`
- [ ] After each family flip: `cargo test --workspace` or the narrow owning
  package set plus the focused live e2e suite listed in the retarget matrix.
- [ ] Final migration gate:
  `EOS_LIVE_E2E_IMAGE=<image> cargo test -p eos-e2e-test --features e2e -- --test-threads=1 --nocapture`
- [ ] `rg -n "timings\\.|\\bis_success\\(|\\berror_kind\\(" sandbox/crates/eos-e2e-test sandbox/crates/eos-sandbox-host sandbox/crates/eos-sandbox-gateway`
  returns only legacy-adapter code until phase F, and nothing after phase F.

E2E retargeting matrix:

| Suite | Current legacy surface to replace | New audit/tracing assertions |
| --- | --- | --- |
| `tests/support` | ad-hoc JSON helpers and `is_success`/`error_kind` wrappers | `TraceTestHarness`, envelope decoder, store query helpers, `assert_request_span`, `assert_event`, `assert_resource_stats_pair`, `assert_chain`, `assert_sidecar_stripped`, `drain_trace_export` |
| `core` | direct file timing/OCC counters, wire error helpers | envelope status/error variants, `request_id`, response `meta.trace_ref`, route `fast_path`, file/OCC events, gateway stripping of `_trace_events` |
| `daemon` | dispatch timing smoke, built-in op success/error branching, inflight/heartbeat wire contracts | host transport events for connect/retry/read failures, daemon root trace for every accepted/wire-failed request, TCP auth failure facts, `RequestStart` store rows, cancellation status, export-drain behavior, heartbeat snapshots from audit store |
| `eos-layerstack` | `resource.layer_stack.*` timing/resource keys and manual phase sums | layerstack/OCC spans, manifest/lease/squash events, mount cost, cache/resource projections, chain reconstruction from store |
| `ephemeral_workspace` | exec route timing keys and upperdir/run-dir resource keys | command/overlay/capture spans, before/after `resource_stats`, changed-path events, response `resource_summary`, no fake tree stats |
| `workspace-publish-gate` | nested `timings.occ.*` route heuristics | OCC route-selected events, conflict/drop/publish facts, per-file conflict detail, no-publish fast-path trace evidence |
| `workspace-runtime-command-session` | command matrix timing/resource telemetry and structured status inferred from response shape | command-session lifecycle events, stdin/progress/collect/cancel spans, `CommandSettle` background trace, transcript/archive refs, resource pairs |
| `workspace-runtime-isolated` | teardown timings, discarded-byte counters, status metrics | isolated enter/status/exit/recovery events, holder liveness, DNS fallback, manager/mountinfo errors, heartbeat bracketing, chain links by caller/workspace handle |
| `plugin` | plugin ensure/status duplicated response facts and overlay timing keys | plugin setup/service-state/health events, service stderr ref, PPC parent message id, overlay/callback spans, plugin service trace links |
| `pressure` | JSON resource report reading flat timing/resource keys | trace-store resource report built from `trace_resources`, leak counters from store/status projections, contention via `inflight_requests`, cgroup/source-error visibility |

Focused live e2e commands for Phase E flips:

- [ ] `cargo test -p eos-e2e-test --features e2e --test core -- --nocapture`
- [ ] `cargo test -p eos-e2e-test --features e2e --test daemon -- --nocapture`
- [ ] `cargo test -p eos-e2e-test --features e2e --test eos-layerstack -- --nocapture`
- [ ] `cargo test -p eos-e2e-test --features e2e --test ephemeral_workspace -- --nocapture`
- [ ] `cargo test -p eos-e2e-test --features e2e --test workspace-publish-gate -- --nocapture`
- [ ] `cargo test -p eos-e2e-test --features e2e --test workspace-runtime-command-session -- --nocapture`
- [ ] `cargo test -p eos-e2e-test --features e2e --test workspace-runtime-isolated -- --nocapture`
- [ ] `cargo test -p eos-e2e-test --features e2e --test plugin -- --nocapture`
- [ ] `cargo test -p eos-e2e-test --features e2e --test pressure -- --nocapture`

### Phase F — debt deletion

Scope: remove migration-only compatibility once all in-repo consumers use the
new contracts.

Acceptance criteria:

- [ ] Delete the v1 flattening adapter, `is_success`, `error_kind`,
  `OpResponse`, `merge_runner_timings`, flat timing/resource helpers, quirk
  serializers, and `json!` response envelopes.
- [ ] `SpanKind`/event vocabulary exhaustiveness is the only timing/resource
  drift guard.
- [ ] JSON trace payload helpers remain only for human exports/projections, not
  daemon-host audit payloads.
- [ ] Generated inventories/readmes no longer describe timing-key contracts.

Verification:

- [ ] `cargo test --workspace`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `rg -n "timings\\.|merge_runner_timings|\\bis_success\\(|\\berror_kind\\(|OpResponse|success\\s*:" sandbox/crates` returns no legacy contract uses.
- [ ] `rg -n "serde_json::Value" sandbox/crates/eos-daemon sandbox/crates/eos-sandbox-host sandbox/crates/eos-sandbox-gateway sandbox/crates/eos-operation` shows only bounded detail/export/projection code.

### Phase G — TypeScript mirror

Scope: make the TypeScript workspace consume the new contract as its day-one
sandbox contract.

Acceptance criteria:

- [ ] `@eos/contracts` exposes Zod schemas for `OperationEnvelope`,
  `ResponseMeta`, `OperationFault`, `ResponseTraceRef`, and trace/resource
  projection DTOs.
- [ ] `@eos/db` or a sibling trace-db package mirrors the host trace schema
  only where the TypeScript workspace needs to query/report sandbox audit data.
- [ ] Existing run-audit JSONL remains separate; it records agent-run lifecycle,
  not sandbox op audit payloads.
- [ ] No SDK/provider response shape leaks into shared sandbox contracts.

Verification:

- [ ] From `eos-agent-core/`: `pnpm run typecheck`
- [ ] From `eos-agent-core/`: `pnpm run lint`
- [ ] From `eos-agent-core/`: `pnpm run test`

### Phase H — heartbeat monitoring

Scope: add continuous audit-backed status snapshots independent of request
traces.

Acceptance criteria:

- [ ] `sandbox.status.snapshot` op returns protobuf-backed
  `SandboxStatusSnapshot`.
- [ ] A shared `eos-trace` resource sampler deletes the duplicate sampler logic
  currently split across response/settle paths.
- [ ] Host `HeartbeatMonitor` writes `sandbox_heartbeats` projection rows and
  audit entries keyed by sandbox/time/boot ids.
- [ ] Heartbeat resource subset includes CPU usage/throttling, memory
  current/peak/OOM, IO bytes, daemon RSS, optional PSI/swap, layerstack
  service-cache gauges, plugin service state/restart/refresh/last_error.
- [ ] `sandbox status --watch` reads projections, not live-only daemon state.

Verification:

- [ ] `cargo test -p eos-daemon -p eos-sandbox-host`
- [ ] Tests cover unreachable rows, boot-id changes, monotone gauges,
  source-error markers, and projection rebuild.
- [ ] Live e2e: heartbeats bracket an exec plus isolated enter/exit.
- [ ] Live e2e: killing the daemon records `reachable=0`; restarting records a
  new `daemon_boot_id` and keeps the chain verifiable.

### Phase I — transcript archival

Scope: make command transcripts durable audit artifacts without putting raw
stdout/stderr in trace payloads.

Acceptance criteria:

- [ ] stdin archive occurs at forward time with byte offsets and digest refs.
- [ ] Progress reads tee byte ranges into the archive without changing JIT read
  latency semantics.
- [ ] `sandbox.command.transcript` provides bounded ranged tail fetches.
- [ ] Destruction waits for archive ack when possible and writes
  `transcript_lost` on TTL fallback/loss.
- [ ] Isolated exit archives before scratch removal; orphan recovery preserves
  enough state to emit retention/loss facts.

Verification:

- [ ] `cargo test -p eos-command-session -p eos-operation -p eos-sandbox-host`
- [ ] Live e2e: long-running exec with stdin and polls yields a byte-complete
  archived transcript whose sha matches a direct in-session read before settle.
- [ ] Live e2e: host-kill-then-TTL case writes `transcript_lost`.
- [ ] Latency assertion proves JIT progress reads stay within baseline bounds.

### Phase J — operator lineage views

Scope: expose the audit data in operator tools after the store is trustworthy.

Acceptance criteria:

- [ ] `trace show <trace_id>` renders seq timeline, causal tree, status,
  resource summary, response refs, links, and immutable-chain proof.
- [ ] `trace verify [--sandbox <id>]` verifies hashes, segment seals, pruning
  proofs, and projection rebuild.
- [ ] `trace heartbeats <trace_id>` shows heartbeat rows bracketing the
  request/chain.
- [ ] Operator output is derived from store data only, not current daemon state.

Verification:

- [ ] CLI/e2e reconstruct one file op, one command chain, one isolated
  lifecycle, and one plugin overlay from store data only.
- [ ] Tampered DB fixture fails verification with a precise broken seq/hash
  report.
- [ ] Pruned sealed segment fixture verifies retained chain plus tombstone
  range proof.

Phases are small and independently landable to merge around concurrent agent
work on `dispatcher.rs`/`op_adapter`.

## Risks

| Risk | Mitigation |
| --- | --- |
| Destructive wire change breaks an unnoticed consumer | Consumer inventory verified (gateway, host, e2e only; no Rust agent-core usage; no TS daemon client yet); each family flip rewrites its consumers in the same change |
| `TraceSpoolLayer` is bespoke (Registry extensions, `Visit`, parent push, `take_op_tree`) | Isolated small module, landed alone in phase A with focused tree tests |
| Sidecar bloats responses | Protobuf sidecar, per-span field budgets + `truncated` flags; bounded-detail rule (sizes/hashes/refs); sidecar carries records, not raw payloads; base64 wrapper is temporary transport glue only |
| Fail-closed rule turns trace-store outages into op outages | Scoped to mutating ops only (deliberate audit-critical trade); read-only ops degrade with markers; store is local single-writer SQLite — the failure mode is disk-full, which should halt mutations anyway |
| Meta derived from spans at render time (request span must close before envelope) | `take_op_tree` API + a phase C unit test asserting meta.duration equals the request span duration; wire-write event lands host-side by design |
| Cross-thread context loss (OCC worker, future async) | Explicit `Span` handoff + phase D test asserting `occ.commit` parents under the op; pattern documented in eos-trace |
| Daemon crash loses un-exported background traces | Bounded spool + crash-log JSON lines + `daemon_boot_id` gap surfacing; request traces are sidecar-delivered so the loss window is background-only |
| Query projections drift from immutable audit entries | Projection rebuild from `audit_entries` is a phase B test and a host startup repair path; projection-only facts are forbidden |
| Local hash chain can be rewritten by an attacker with full disk control | Segment seals include signer key id + signature and are exported/anchored via `export_ref`; compliance mode requires external/WORM anchoring before retention pruning |
| Host DB growth | `prune_before` exists; JSONL export for archiving; policy open |
| Mixed old/new wire shapes misclassify failures | Phase E starts with a shape-aware decoder because today's daemon wire decoder still treats `success:false` + `error` as `ErrorResponse`, and host `is_success` treats new-shape errors as success unless confined to the legacy branch |
| e2e assertion rewrite volume across suites | Per-family flips keep each rewrite reviewable; replay/chain queries become shared support helpers |
| Host TCP failure has no daemon sidecar | Host outbound transport events are canonical for connect/write/read/decode failures; daemon spans are expected only after inbound accept, so missing daemon sidecar is queryable rather than silent |

## Open Questions

1. Retention policy for `sandbox-traces.sqlite` and sealed audit segments
   (never prune vs age/size cap; WORM export required before pruning).
2. Should `trace_id` minting move into the gateway for multi-client setups, or
   stay in `eos-sandbox-host`? (Recommendation: host owns it; gateway forwards.)
3. Audit segment signer source: host-local 0600 key in dev, OS keychain, or
   external signing service/HSM in compliance deployments. The schema supports
   all via `key_id`; the default needs an implementation choice.
4. Whether all e2e suites run with sidecar assertion helpers or only the trace
   suites — proposal: all, since the envelope migration touches them anyway.
5. Heartbeat retention: raw rows at 10 s intervals are ~8 640/day/sandbox —
   keep raw forever, or downsample to 1-minute aggregates after N days?
   (`prune_before` applies; downsampling needs a decision.)
6. Heartbeat-driven alerting (host process reacting to `degraded`/
   `unreachable`, e.g. notifying the agent runtime) — out of scope here;
   the table is the seam a future watcher consumes.

Resolved since rev 2: PPC callback correlation (formerly open) is now
specified — context-propagation rule 4 plus the typed `parent_message_id`
field on `PpcMessage`, landing in phase D.

## Alternatives Considered

Three designs were drafted independently and adversarially judged (integration
cost / audit value / contract fit). The original winner (`tracing-native`,
8/8/8) was built around preserve-first v1 byte-stability; the owner directive
for a destructive, no-debt plan removed that constraint, which reinstates two
ideas the judges had rejected *only* for compat reasons: the response sidecar
(`owned-contract`'s strongest audit property — the chain rides every delivered
response) and the immediate deletion of the quirk serializers. The losing
designs' remaining grafts are incorporated: closed `SpanKind`/`Subsystem`
enums, host-stamped clocks + request-start audit append-before-send, protobuf
sidecars, hash-chained immutable entries, per-span budgets with `truncated`
flags, e2e pool drain wiring, plugin audit-field capture, and the
sweeper-cancelled-session e2e assertion. The OTel Rust SDK stays rejected
(pre-1.0 churn in the static daemon binary, no in-repo consumer); its id and
schema **format compatibility** is kept for the TS-side join.

The parallel draft `sandbox/docs/sandbox-event-tracing-response-plan.md`
contributed the three-level identity model, the seq-ordered chain + causal-tree
dual view, the cross-op link rows, the per-module phase vocabulary, the
fail-closed persistence rule, gateway declassification, and the six-status
envelope vocabulary — all merged above. Where the two drafts conflicted, this
document resolves: 4-valued `workspace_route` (`fast_path` and `none` split
per owner decision, replacing 3-valued `skip`), spans as the single timing
source (the parallel draft kept response stamping alongside), and hybrid
sidecar+drain delivery (the parallel draft deferred the drain op).
