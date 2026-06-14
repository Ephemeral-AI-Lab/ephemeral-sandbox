# Connecting to eos-sandbox

How an external orchestrator — specifically `eos-coding-agent` (TypeScript) —
talks to the EphemeralOS sandbox, what operations it offers, and the
sandbox-side changes that would make that bridge clean, auditable, and typed.

This guide is the practical connection manual. The normative wire contract is
`crates/shared/protocol/PROTOCOL.md` +
`crates/daemon/operation/ops.json`; the target architecture is `docs/SPEC.md`;
the audit/trace design is
`docs/sandbox-event-tracing-response-plan.md`. Where this guide and those files
disagree, those files win.

---

## 1. What eos-sandbox is

One host-side **gateway** process fronts a fleet of Docker sandboxes, each
running one in-container **daemon**. An external caller reaches exactly one Unix
socket; the per-sandbox daemons are unreachable from outside the host.

```
 eos-coding-agent (TS)
        │  ❶ UNIX domain socket, newline-delimited JSON, ONE request per connection
        ▼
 gateway (bin, host side)   decode → visibility-gate → catalog-route → return one JSON line, half-close
        │  ❷ in-process call into the host engine
        ▼
 host   (lib, host side)    sandbox registry + Docker runtime + recovery machine
        │  ❸ loopback TCP (docker-published port) + auth token;  docker-exec thin-client fallback
        ▼
 eosd / daemon (in container)   executes the op: files (LayerStack+OCC), commands (PTY),
                                isolated workspaces, plugins (PPC), checkpoints, trace
```

The connector (eos-coding-agent) only ever sees hop ❶. Hops ❷/❸ — the box hop,
the auth token, the connect-retry/respawn recovery machine, the
`docker exec eosd daemon --client` fallback — are entirely internal to the host
and invisible to the caller.

**Isolation law (load-bearing for every recommendation below):** no compiled
code is shared across the host/box boundary. The complete shared artifact is
`crates/daemon/operation/ops.json`, `crates/shared/protocol/PROTOCOL.md`, and
the owner-local fixtures. A TS client is therefore a pure data client — it
speaks JSON described by those artifacts and links no Rust.

### Surfaces and visibility

The gateway binds **two** sockets and gates every op by the socket's surface:

| Socket | Path | Surface | Reaches ops with visibility |
|---|---|---|---|
| client | `--listen` (default `/tmp/sandbox-gateway.sock`) | `Client` | `public` only |
| operator | `<listen>.operator` (auto, beside it) | `Operator` | `public` + `operator` |

`internal` and `test` ops are reachable from **neither** socket. So a normal
coding-agent runs against the **client** socket and sees only `public` ops; an
operator/observability process uses the **`.operator`** socket to also reach the
`operator` ops (checkpoint metrics, isolation list, and trace query/verify ops).
(`crates/gateway/src/gateway.rs:222-247`.)

### Starting the gateway

```sh
cargo run -p gateway -- serve \
    --listen /tmp/sandbox-gateway.sock \
    --image <docker-image> --platform linux/amd64
```

Both sockets are created `chmod 0600`; access control on hop ❶ is **filesystem
permissions only** (there is no auth field on the client hop).

---

## 2. The wire protocol (client hop)

| Property | Value | Source |
|---|---|---|
| Transport | Unix domain socket, `chmod 0600` | `gateway.rs:499-516` |
| Framing | one compact-JSON object + `\n`, **one request per connection** | `gateway.rs:548-573,679-698` |
| Response | one JSON line, then the server flushes and half-closes (`shutdown(Write)`) | `gateway.rs:606-646` |
| Max request | 16 MiB (`16777216`); over → `request_too_large` | `host/src/protocol.rs:15`, `gateway.rs:691-696` |
| Read timeout | 30 s; empty read → `invalid_request` | `gateway.rs:16,545,685-690` |
| Concurrency | blocking; one OS thread per accepted connection | `gateway.rs:518-536` |

There is **no** multiplexing, keep-alive, or streaming. Each call is: open
socket → write one line → read one line → socket closes.

### Request

```json
{"op":"sandbox.file.read","sandbox_id":"sb-…","invocation_id":"<uuid4hex>","args":{"path":"README.md","caller_id":"run_1"}}
```

| Field | Required | Rules |
|---|---|---|
| `op` | yes | canonical `host.*` or `sandbox.*` name from `ops.json`; non-empty string |
| `invocation_id` | yes | string; canonical request identity; **becomes `meta.request_id`**; uuid4 hex recommended. Doubles as the cancel/heartbeat key for background commands |
| `sandbox_id` | for daemon ops and `host.sandbox.release` / `host.sandbox.status` | string. Absent on `host.sandbox.acquire` / `host.sandbox.list`. Stripped by the host before forwarding to the daemon |
| `args` | yes (may be `{}`) | object; defaults to `{}` if omitted |

Unknown extra top-level fields are silently ignored; top-level `request_id` is
not accepted as request identity. `args` must be an object or the request is
rejected `invalid_request`.

**Routing** is a pure `ops.json` catalog lookup with no per-op branching: the
gate checks visibility, then `served_by` picks host-verb vs daemon-forward. An
unknown op returns `unknown_op`.
(`gateway.rs:249-318`.)

### Response envelope

Every response — host-built or daemon-forwarded — is the same shape, an
externally-tagged union on `status`
(`crates/daemon/operation/src/core/envelope.rs:121-155`):

```json
{
  "status": "ok",
  "result": { "...": "domain payload" },
  "meta": {
    "envelope_version": 2,
    "op": "sandbox.file.read",
    "request_id": "<your invocation_id>",
    "trace": { "trace_id": "…", "request_id": "…", "store": "local_sqlite", "event_count": 12, "degraded": false },
    "workspace_route": { "kind": "ephemeral_workspace" },
    "duration_ms": 4.2,
    "modules_touched": ["dispatch", "layer_stack"],
    "steps": [ { "kind": "…", "duration_us": 900, "status": "ok" } ],
    "resource_summary": { "fields": {} },
    "warnings": []
  }
}
```

| `status` | Carries | Meaning |
|---|---|---|
| `ok` | `result` + `meta` | completed |
| `running` | `result` + `meta` | accepted; continues via a linked resource (rare at the envelope level) |
| `cancelled` / `timed_out` | `result` + `meta` | finalized facts of cancelled/timed-out work |
| `rejected` | `error` + optional `result` + `meta` | **domain** refusal (OCC conflict, policy, isolated gate); `result` keeps partial facts |
| `error` | `error` + `meta` | parse/transport/internal fault |

`error` is `{kind, message, details}`. `meta` is always present and rendered from
the request's trace record (never hand-built beside it on the daemon path).

> ⚠️ **The one gotcha that bites every command/file consumer.** The envelope
> `status` is the *transport* outcome; the *domain* status is nested at
> `result.status`. A running background command comes back as
> `{"status":"ok","result":{"status":"running","command_id":"cmd-…","output":{…}},"meta":{…}}`
> — envelope `ok`, command lifecycle `running`. A `command_not_found` comes back
> `{"status":"ok","result":{"status":"error","output":{"stderr":"command_not_found"}}}`.
> A write OCC conflict can come back at the envelope level (`status:"rejected"`)
> **or** as `result.status:"aborted_version"`. **Always branch on envelope
> `status` first, then on `result.status` for command and file ops.**
> (Confirmed: `daemon_result`→`ok_envelope` wraps any bare result lacking
> `meta`, `builtin.rs:58,95-100`, `op_adapter/mod.rs:26-32,69-78`.)

### Error kinds

`error.kind` is a string. The closed set today, by origin (there is no single
published enum — see TS-DTO-2):

| Origin | Kinds |
|---|---|
| Gateway parse | `invalid_request`, `bad_json`, `request_too_large` |
| Gateway routing | `unknown_op`, `forbidden`, `unknown_sandbox` |
| Host forward path | `sandbox_unavailable` (recovery exhausted; retryable), `uncertain_outcome` (mutation, outcome unknowable; **never retried**), `trace_unavailable` |
| Daemon | `unauthorized` (box hop only), `internal_error` (carries `details.fields.error_id`, 32-hex), `forbidden`, `forbidden_in_isolated_workspace`, `lifecycle_in_progress` |
| Domain `rejected` | `occ_conflict`, `invalid_argument`, isolation kinds (`already_open`, `quota_exceeded`, `host_ram_pressure`, `setup_failed`, …) |

`uncertain_outcome` is special: a mutating op whose delivery became ambiguous
after a transport failure. Treat it as **terminal and non-retryable**; surface it
to the user rather than re-issuing.

### The five correlated IDs

| ID | Minted by | Lives where | Used for |
|---|---|---|---|
| `sandbox_id` | host at `acquire` (`sb-<32hex>`) | top-level request field | routing key + trace partition; never in the response |
| `invocation_id` | **caller**, per request | top-level request field | request correlation; cancel/heartbeat key for background commands |
| `request_id` | host (`= invocation_id` parsed) | `meta.request_id`, `meta.trace.request_id` | echoes your `invocation_id` back; audit cursor |
| `trace_id` | host, fresh uuid4 per forward | `meta.trace.trace_id` | partitions all trace events/spans in the host store |
| `command_id` | daemon when a background command starts | `result.command_id` (running only) | address `write_stdin` / `poll` / `cancel` |

There is no distinct `run_id` type. A "workspace run" is keyed by **`caller_id`**,
where by convention `caller_id == agent_run_id`. `caller_id` groups a run's
commands and isolated workspace so `sandbox.run.end` can tear exactly that run
down. Pass `caller_id` in `args` on every daemon op you want grouped; it defaults
to `"default"` when absent.

---

## 3. Operations offered (the API)

33 ops across 8 families. `H`=host-served, `D`=daemon-served; `pub`/`op`/`int`/`test`
= visibility; `★`=mutates state. A coding agent uses mainly **Files**,
**Command**, **Isolated workspace**, and **Sandbox lifecycle**.

### Sandbox lifecycle — `H`, public (no daemon hop)

| Op | Args | Result |
|---|---|---|
| `host.sandbox.acquire` ★ | *(none)* | `{ sandbox_id }` — provisions a container+daemon |
| `host.sandbox.release` ★ | `sandbox_id` (top-level) | `{ sandbox_id }` — `docker rm -f` + drop registry entry |
| `host.sandbox.status` | `sandbox_id` (top-level) | `{ sandbox_id, container, endpoint, created_by, daemon }` (embedded readiness probe) |
| `host.sandbox.list` | *(none)* | `{ sandboxes: [{ sandbox_id, container, endpoint, created_by }] }` |

### Files — `D`, public

| Op | Args | Result (`result.*`) |
|---|---|---|
| `sandbox.file.read` | `path!`, `caller_id?`, `layer_stack_root?` | `{ workspace, success, content, exists, encoding }` |
| `sandbox.file.write` ★ | `path!`, `content!`, `overwrite?`(=true), `caller_id?`, `layer_stack_root?` | mutation outcome: `{ success, status, changed_paths, changed_path_kinds, mutation_source?, conflict?, conflict_reason?, workspace, published }` |
| `sandbox.file.edit` ★ | `path!`, `edits!: [{old_text, new_text, replace_all?}]`, `caller_id?`, `layer_stack_root?` | same mutation outcome + `applied_edits` |

`layer_stack_root` is required **only** on the direct (non-isolated) route; when
the `caller_id` has an open isolated workspace, the op routes there and the root
is implicit. Mutation `status` ∈ `accepted|committed|rejected|aborted_version|aborted_overlap|dropped|failed`.

### Command — `D`, public (async: exec → poll → collect)

| Op | Args | Result (`result.*`) |
|---|---|---|
| `sandbox.command.exec` ★ | `cmd!`, `caller_id?`, `layer_stack_root?`, `timeout?`/`timeout_seconds?` (**seconds**), `yield_time_ms?` | `{ status, exit_code?, output:{stdout,stderr}, command_id? }`; finalized adds mutation fields |
| `sandbox.command.write_stdin` ★ | `command_id!`, `chars!`, `yield_time_ms?` | command response (same shape) |
| `sandbox.command.poll` ★ | `command_id!`, `last_n_lines?`(=50) | command response, stdout tailed to `last_n_lines`; may finalize completed commands |
| `sandbox.command.cancel` ★ | `command_id!` | command response |
| `sandbox.command.collect_completed` ★ | `command_ids?`, `caller_id?` | `{ success, completions: [{command_id, caller_id, command, result}] }` |
| `sandbox.command.count` | `caller_id?` | `{ success, caller_id, count }` |

Command `result.status` ∈ `running|ok|cancelled|error|timed_out`. `running` means
"poll me": keep the `command_id` and call `sandbox.command.poll` until terminal.

### Isolated workspace — `D` (lifecycle for caller-keyed private workspaces)

| Op | Vis | Args | Result (`result.*`) |
|---|---|---|---|
| `sandbox.isolation.enter` ★ | pub | `caller_id!`, `layer_stack_root!` | `{ success, manifest_version, manifest_root_hash, workspace_handle_id, workspace_root }` |
| `sandbox.isolation.exit` ★ | pub | `caller_id!`, `grace_s?` | `{ success, evicted_upperdir_bytes, lifetime_s, total_ms, phases_ms, inspection }` |
| `sandbox.isolation.status` | pub | `caller_id!` | `{ success, open, … }` |
| `sandbox.isolation.list_open` | op | *(none)* | `{ success, open_caller_ids }` |
| `sandbox.isolation.test_reset` ★ | test | *(none)* | test-only; unreachable on both sockets |

`enter` refuses with `rejected` faults: `active_background_work`, `already_open`,
`quota_exceeded`, `host_ram_pressure`, `setup_failed`.

### Workspace run — `D` (run-scoped teardown)

| Op | Vis | Args | Result (`result.*`) |
|---|---|---|---|
| `sandbox.run.end` ★ | pub | `caller_id!`, `grace_s?` | `{ success, caller_id, cancelled_commands, isolated_exited }` |
| `sandbox.run.cancel_all` ★ | op | `grace_s?` | `{ success, cancelled_commands, isolated_callers_exited }` |

### Control — `D` (in-flight invocation management + trace drain)

| Op | Vis | Args | Result |
|---|---|---|---|
| `sandbox.call.heartbeat` ★ | pub | `invocation_ids?: []` | `{ success, touched }` |
| `sandbox.call.cancel` ★ | pub | `invocation_id?` | `{ success, invocation_id, cancelled, already_done, cleanup_done }` |
| `sandbox.call.count` | pub | `caller_id?` | `{ success, caller_id, count }` |
| `sandbox.runtime.ready` | int | `layer_stack_root!` | readiness probes (host-internal) |
| `sandbox.trace.export` | int | `max_records?`(=64) | trace batch drain (host-internal) |

### Checkpoint — `D`, operator (LayerStack / git materialization)

| Op | Args | Result |
|---|---|---|
| `sandbox.checkpoint.layer_metrics` | `layer_stack_root!` | LayerStack + storage metrics |
| `sandbox.checkpoint.ensure_base` ★ | `layer_stack_root!`, `workspace_root!` | `{ success, created, binding }` |
| `sandbox.checkpoint.build_base` ★ | `… , reset?` | `{ success, created, binding }` |
| `sandbox.checkpoint.commit_to_workspace` ★ | `layer_stack_root!`, `workspace_root!` | `{ success, manifest_version }` |
| `sandbox.checkpoint.commit_to_git` ★ | `… , message!, paths?` | `{ success, committed, commit_sha?, manifest_version, manifest_root_hash, paths, worktree_mode }` |
| `sandbox.checkpoint.binding` | `layer_stack_root!` | `{ success, binding }` |

### Plugins — `D`, public

| Op | Args (key) | Result |
|---|---|---|
| `sandbox.plugin.ensure` ★ | `plugin?, digest?, manifest?, package.*?, start_services?, caller_id?, audit?` | untagged `NeedsUpload` \| `Ready{registered_ops, services, …}` |
| `sandbox.plugin.status` | `probe_services?, probe_timeout_ms?, caller_id?` | `{ loaded_plugins, running_service_processes, service_health, … }` |

Full per-arg/per-field detail is in `crates/daemon/operation/src/*/contract.rs`; the
rendered catalog is `docs/API.md` (regenerate with `cargo run -p xtask -- gen-docs`).

---

## 4. Connecting from eos-coding-agent

### Current state

`eos-coding-agent/src/tools/sandbox/index.ts` ships **7 stub tools** (`read`,
`multi_read`, `write`, `edit`, `exec_command`, `command_stdin`,
`read_command_transcript`), each returning
`{ error: "sandbox daemon bridge is not wired in this build" }`. There is no
socket client, no `sandbox_id` threading, and no lifecycle. The `eos-agent-sdk`
ships no socket/IPC helper (its only network client is fetch-based HTTP for LLM
providers), so the gateway client is **greenfield TS**.

### The bridge, in four moves

```
bootstrap.ts ──constructs──▶ SandboxGatewayClient (node:net UDS, one-line req/resp)
     │                                  ▲
     │ threads client + sandboxId()     │ client.request(op, args, {sandboxId, invocationId, signal})
     ▼                                  │
buildAgentFactory → selectOrdinaryTools → sandboxTools(client, sandboxId) ── 7 tools adapt args → ops
     ▲
pursuit/service.ts run boundary ──▶ acquire on start │ run.end + release on end/interrupt
```

**Move 1 — the socket client (one new module).**
Add `src/tools/sandbox/gateway-client.ts`: a `SandboxGatewayClient` over
`node:net` `createConnection({ path })` (Unix socket — *not* host/port). Per call:
mint `invocation_id` (uuid4 hex), write one compact-JSON line
`{op, sandbox_id, invocation_id, args}` + `\n`, read until the single `\n`/EOF,
`JSON.parse`, validate against a Zod `GatewayResponse` discriminated union
(`{status:'ok'|'running'|…, result, meta}` | `{status:'error'|'rejected', error, meta}`),
open a **fresh connection per call** (the gateway is one-request-per-connection).
Per the workspace rule, keep per-op *result* Zod schemas at this client edge, not
in `src/contracts`. Wire `ctx.signal`: on abort, `socket.destroy()` + reject; add
a bounded connect timeout and a small retry/backoff on `ECONNREFUSED` (gateway
not yet up) — client-side only, since `CONNECT_RETRY_DELAYS_S` is the host→daemon
ladder, not gateway-facing. Classify `sandbox_unavailable` retryable,
`uncertain_outcome` terminal.

**Move 2 — the DI seam.**
Widen `sandboxTools()` to `sandboxTools(client, sandboxId: () => string)` and
thread `client` + the per-run `sandboxId` accessor through `buildAgentFactory →
selectOrdinaryTools` (mirroring the existing `readAgentRun(recordsDir)` /
`runSubagent(factory, subagents)` closure precedents). Source `invocation_id`
from the existing `ctx.toolUseId` (already the documented correlation/idempotency
key — no new `ToolCallContext` field needed). `sandbox_id` is a top-level wire
sibling, never inside `args`, so binding it by closure is exactly right.
Construct one process-level client in `bootstrap.ts`.

**Move 3 — lifecycle binding.**
Bind acquire/release to the **operator run boundary**, which is
`src/workflows/pursuit/service.ts:324-341` (the `.create(...).start({messages})`
site exposing `run.runId`, the abort→`run.interrupt()` listener, and
`run.outcome().then(reconcileRun)`) — **not** `bootstrap.ts`/`agent-factory.ts`
(those only construct specs, never `.start()`). On start: `client.acquire()` →
stash `sandbox_id`. On settlement / failure / interrupt: `sandbox.run.end` with
`caller_id == agent_run_id`, then `host.sandbox.release(sandbox_id)`. Resolve the
caller_id granularity first: the SDK mints a fresh `AgentRunId` per `.start()`,
and one pursuit spawns many child runs under one operator — decide which run owns
acquire/release and how child `agent_run_id`s map to `run.end` scope.

**Move 4 — tool→op adaptation (in `execute()`, never change the daemon).**
Keep the model-facing Zod schemas (they are the LLM contract); adapt to the wire
inside each tool:

| TS tool | Canonical op | Arg reshape |
|---|---|---|
| `read(path, offset?, limit?)` | `sandbox.file.read` | `{path, caller_id}`; apply `offset`/`limit` **client-side** over content (no daemon window today — see TS-DTO-5) |
| `multi_read(paths[])` | N× `sandbox.file.read` | aggregate client-side (no batch op exists) |
| `write(path, content)` | `sandbox.file.write` | `{path, content, caller_id}` |
| `edit(path, old_string, new_string, replace_all)` | `sandbox.file.edit` | `{path, edits:[{old_text: old_string, new_text: new_string, replace_all}], caller_id}` |
| `exec_command(command, cwd?, timeout_ms?)` | `sandbox.command.exec` | `{cmd, caller_id}`; pass tool call identity as top-level `invocationId`; `timeout = ceil(timeout_ms/1000)` **seconds**; fold `cwd` as `cd <cwd> && <command>` — **do not silently drop it** (daemon hardcodes `cwd:"."`) |
| `command_stdin(command_id, input)` | `sandbox.command.write_stdin` | `{command_id, chars: input}` |
| `read_command_transcript(command_id, offset?, limit?)` | `sandbox.command.poll` | `{command_id, last_n_lines}` (no `offset` on the daemon) |

`sandbox_id` is **required** for all 7 (every one is `served_by:daemon`); there is
no happy path without first calling `host.sandbox.acquire`.

**Response adapter — envelope-first.** Branch envelope `status`: `ok`/`running`
→ read `result`; `rejected`/`error` → `{error: error.message}`;
`cancelled`/`timed_out` → `{error}` (or partial `result`). *Then*, for command
ops, branch the inner `result.status`: `running` → `{output, command_id}` poll
handle (not an error); `ok` → `{output}`; `error`/`timed_out`/`cancelled` →
`{error}`. `rejected` is **not** a command status — it lives only at the envelope
and mutation layers.

### Minimal client sketch

```ts
import net from "node:net";
import { randomUUID } from "node:crypto";

export class SandboxGatewayClient {
  constructor(private socketPath = "/tmp/sandbox-gateway.sock") {}

  request(op: string, args: Record<string, unknown>,
          opts: { sandboxId?: string; invocationId?: string; signal?: AbortSignal }) {
    return new Promise<GatewayResponse>((resolve, reject) => {
      const sock = net.createConnection({ path: this.socketPath });
      let buf = "";
      const onAbort = () => sock.destroy(new Error("aborted"));
      opts.signal?.addEventListener("abort", onAbort, { once: true });
      sock.setEncoding("utf8");
      sock.on("connect", () => {
        const line = JSON.stringify({
          op, ...(opts.sandboxId ? { sandbox_id: opts.sandboxId } : {}),
          invocation_id: opts.invocationId ?? randomUUID().replace(/-/g, ""), args,
        });
        sock.write(line + "\n");
      });
      sock.on("data", (d) => (buf += d));
      sock.on("error", reject);
      sock.on("close", () => {
        opts.signal?.removeEventListener("abort", onAbort);
        try { resolve(GatewayResponse.parse(JSON.parse(buf))); }
        catch (e) { reject(e); }
      });
    });
  }
}
```

(Zod `GatewayResponse` and per-op result schemas live beside this module.)

---

## 5. Recommended sandbox-side changes

Each item below was adversarially verified against the code; the form shown is
the **corrected** proposal (several first-draft proposals had wrong premises or
violated the isolation law — those corrections are folded in). Priority is the
guide's, by value-to-effort for the bridge.

### A. Bridge — almost entirely TS-side; the sandbox is nearly ready

The gateway, op catalog, and envelope are sufficient to bridge **today**; the
work is in `eos-coding-agent` (§4). The single optional sandbox-side helper:

| # | Change | Effort | When |
|---|---|---|---|
| BR-4 | Codegen a typed TS op-contract from `ops.json` (a `SANDBOX_OPS` map + public-op union + per-op `mutates_state`), in the **eos-coding-agent build** (no new Rust). Optionally extend `xtask check-contract` with a freshness check **only** as a deliberate cross-tree decision. | S | **after** the bridge client lands and actually uses op-name strings — not before (no duplication exists today) |

> Rejected — **BR-7** ("add `EosConfig.sandboxGatewaySocketPath`"): the premise
> is false (no client hardcodes a path today; none exists). Make the endpoint
> configurable *when* the bridge client is built, not as a dangling config field.

### B. Auditability — host trace is operator-reachable

The host owns a fail-closed, hash-chained SQLite trace store and exposes it on
the operator socket through `host.trace.requests`, `host.trace.show`, and
`host.trace.verify`. Forwarded daemon responses get a host-minted
`meta.trace` receipt with `store="local_sqlite"` and an event count refreshed
from the durable store after terminal response persistence.

| # | Change | Effort/Risk | Verdict |
|---|---|---|---|
| **AUD-3** | Add a **timed background drain** in `SandboxHost::open`: a periodic thread that resolves endpoints for idle sandboxes and `schedule()`s a trace-export drain, reusing the existing single-flight/coalesce machinery. Today the bounded daemon spool drains opportunistically after forwards and explicit trace export calls. | M / med | still valid |
| **TRACE-OPS** | Operator readback is implemented as `host.trace.requests`, `host.trace.show`, and `host.trace.verify`; keep additional audit read surfaces out unless they provide a distinct operator workflow. | done | current |
| **TRACE-RECEIPT** | Host forward responses refresh `meta.trace.event_count` from `TraceStore::event_count_for_trace(trace_id)` alongside the existing `store="local_sqlite"` rewrite. | done | current |
| **SIDECAR-RECOVERY** | Decoded sidecar ingest failures are spooled as bounded pending sidecars and retried by host-local recovery. | done | current |

For a coding agent that wants end-to-end auditability, use the response
`meta.trace.request_id` as the cursor and read it back through the operator
trace routes.

### C. Data transport, types, DTOs, I/O, response format

| # | Change | Effort/Risk | Verdict |
|---|---|---|---|
| **TS-DTO-6** | Reconcile the catalog/wire version and envelope version vocabulary. The catalog/wire version is `1` (`ops.json`, `_eos_daemon_protocol_version` in `args`) and response metadata now uses `envelope_version` for the envelope schema. **Document all three surfaces** (wire / catalog / envelope) in `CONTRACT.md` so a TS author can't confuse catalog versioning with envelope versioning. Optionally add a daemon-side skew guard against its **own** copy of the version constant. | S / low | refine |
| **TS-DTO-2** | Publish a closed **`fault_kinds`** array into `ops.json` (as data, via `ops_json_document()`), unioning daemon `ErrorKind` + gateway API kinds + domain rejection kinds, gated by `check-contract`. **Keep each side's enum local** — do *not* introduce a shared Rust enum the gateway imports from `operation` (that crosses the host/box boundary). TS generates the closed union from the same artifact it already consumes. | M / med | refine |
| **TS-DTO-5** | Add **byte-range windowing** to `sandbox.file.read` (`offset`/`limit` → `{content, next_offset?, eof}`) so large files page instead of hard-erroring at `max_read_bytes` — the one genuinely missing primitive. For `sandbox.command.poll`, **surface the already-existing** `read_output_since` byte cursor via an additive `since_offset` → `{chunk, next_offset, complete}` (the engine already persists the full transcript; only the poll *surface* tails). Both additive, no framing change. | M (read) / S (poll) | refine |
| **TS-DTO-1** | Publish per-op **arg/result JSON Schemas** as a new drift-gated artifact (`op_schemas.json` via `eosd dump-op-schemas`). **Prerequisite:** the input structs are hand-parsed from raw `Value` with aliases/defaults (`timeout`\|`timeout_seconds`, `caller`←`caller_id`, optional-with-default) — a naive `schemars` derive would publish a schema that *lies* about the accepted wire shape. First convert each `parse()` to real `serde` `Deserialize` with `#[serde(alias/default)]`, fixture-pinned, then derive. This is the largest item. | L→XL / med-high | refine |
| **TS-DTO-3** | Replace the hand-rolled `timings`-strip/flatten loops in `CommandResponse::to_wire_value` and `files.rs::mutation_response` with declarative serde (field-level `#[serde(skip)]` on a wire DTO). **Behavior-preserving cleanup only** — do **not** remap the command lifecycle status onto the envelope status arm (that would be a contract change; command/file ops are already enveloped). Net-negative LOC. | M / med | refine |

> Rejected — **TS-DTO-4** ("share one envelope DTO between gateway and daemon"):
> directly violates the isolation law. The gateway is host-side and depends only
> on `host`; `operation` is box-side and pulls in `command`/`layerstack`/`nix`/…
> Importing `OperationEnvelope`/`ResponseMeta` into the gateway would link a heavy
> box crate into the host binary. The hand-built duplication is **deliberate** and
> gated by fixture conformance (`CONTRACT.md:19-23`), not a defect. Any anti-drift
> work must live in the fixtures + `check-contract`, never a crate dependency.

### Sequencing

```
Now (TS side):     BR client (§4) ──▶ BR-4 codegen ──▶ wire 7 tools + lifecycle
Now (docs):        TS-DTO-6(A) doc/rename
High value next:   AUD-3 (timed drain)
When typing TS:    TS-DTO-2 (fault_kinds) ──▶ TS-DTO-1 (schemas, after serde refactor)
Opportunistic:     TS-DTO-5 (paging) · TS-DTO-3 (serde cleanup)
```

The bridge needs **no** sandbox-side change to function. Timed trace draining
and typing (TS-DTO-2/6) are where sandbox-side work most improves the
orchestrator's experience.

---

## 6. Quick probe

```sh
# one op over the client socket
printf '%s\n' '{"op":"host.sandbox.acquire","invocation_id":"probe-1","args":{}}' \
  | socat - UNIX-CONNECT:/tmp/sandbox-gateway.sock

# an operator-only op over the operator socket
printf '%s\n' '{"op":"sandbox.checkpoint.layer_metrics","sandbox_id":"<sb-id>","invocation_id":"probe-2","args":{"layer_stack_root":"/eos/layer-stack"}}' \
  | socat - UNIX-CONNECT:/tmp/sandbox-gateway.sock.operator
```
