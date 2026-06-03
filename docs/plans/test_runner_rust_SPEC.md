# SPEC: Rust `test_runner` — bridging `sandbox` and `agent-core`

Status: **draft v2 (post adversarial review; code-verified)**
Date: 2026-06-03
Owner doc: this file (`docs/plans/test_runner_rust_SPEC.md`)
Supersedes (for the harness tier): `docs/plans/test_runner_migration_PLAN.md`
(that plan renamed the **Python** `task_center_runner -> test_runner` and kept the
harness + host/API boundary in Python; this spec moves the harness itself to
**Rust**, now that `sandbox/` and `agent-core/` are migrated).

> v2 incorporates a multi-agent adversarial review that verified every
> load-bearing claim against the live code. Corrections from review are marked
> **[rev]**. The net effect is **smaller and more correct**: one crate (not six),
> consumer-side audit normalization (no shared contract crate), one mock surface,
> a one-direction correlation fix, and no changes to production config or
> `eos-runtime::main`.

---

## 0. TL;DR

Build **one new top-level Rust crate, `test_runner/`** (peer to `sandbox/` and
`agent-core/`) that drives a **real** Rust sandbox and the **real** Rust
agent-core engine to test agent execution end-to-end. Modules:

| Module | Job | Primary upstream surface |
|---|---|---|
| `config` | api-client creds via `.env`; sandbox + multi-node sizing in a per-run handle | `eos-config` (+ `providers.active` only) |
| `audit` | **Unified, human-readable, correlated** trace — **bridged consumer-side** | `eos-audit::AuditSink` (in-proc) + `api.audit.pull` (sandbox ring) |
| `agent` | one `MockedLlmClient` injected into the real loop; run→completion/partial; trivial live api smoke | `EventSource` seam; `eos-runtime::start_request` |
| `sandbox` | fast, reusable dask container; single + multi-node; **never mocked** | `eos-sandbox-host` + `/sandbox` wire protocol |

**The two things the user explicitly asked, answered up front:**

1. **"How do I design an audit interface where each module handles its own, then
   bridge them?"** → **Each module keeps its own native audit** (agent-core
   `eos-audit::AuditEvent`; sandbox `eos-protocol::audit` `*Section`). They are
   bridged **only in the `test_runner` collector**, via consumer-side
   normalization into one `TraceEvent` with four facets. **No shared cross-repo
   contract.** The only cross-repo coupling is **one correlation key** (the engine
   `tool_use_id`, which the daemon already echoes). This mirrors the proven Python
   `daemon_event_normalizer.py` + `performance_report.py` shape. **[rev: was a
   shared `eos-audit-contract` crate — deleted as over-engineering.]**
2. **"Reform the audits to be collected nicely, human-readable, reflecting
   semantics / performance / resource usage / correctness."** → A **four-facet
   model** (`Semantics / Performance / Resource / Correctness`) + a tree/summary
   **renderer** in the collector (§7). The four facets map 1:1 to the user's ask
   and to fields both sides already emit (timings→perf, bytes/peak→resource,
   status/conflict→correctness, op→semantics).

The hardest fact — **is the LLM client injectable?** — is **YES, no new seam**: the
loop consumes `Arc<dyn EventSource>`; concrete clients are built only in
`eos-runtime::default_llm_client`.

---

## 1. Goals / Non-Goals

### Goals
1. Run `user request -> root Task -> root agent -> optional delegate_workflow ->
   submit_root_outcome` under test, with the ability to **terminate early**
   (partial result) when a test condition is met.
2. **Mock LLM** tier: scripted thinking/text/tool-call turns injected into the
   **real** engine loop. Sandbox is **never** mocked.
3. **Live api-client** tier: a *trivial* smoke proving `anthropic.rs` and
   `openai.rs` produce well-shaped tool calls + honor the system reminder.
4. **Sandbox** tier: fast reusable dask container; configurable multi-node.
5. **Unified audit**: one correlated, human-readable timeline across both sides.
6. **Centralized config**: api-client (`.env`), sandbox, run params.
7. Preserve the Python harness's test **rigor** (difficulty / complexity /
   load-bearing) while discarding its bad layout.

### Non-Goals (`CLAUDE.md` simplicity rules — over-engineering is a defect equal to under-coverage)
- No new orchestration layer; no peer-to-peer agent comms; no fake agent loop.
- No exhaustive provider matrix for the api-client test (intentionally trivial).
- No port of the over-engineered Python scenarios as-is (`full_stack_adversarial`,
  `full_system_capacity_matrix`, `pack_catalog`, the ~120-file
  `isolated_workspace` explosion). Port **invariant categories**, not file count.
- No Daytona/Minimax client wiring.
- **[rev]** No shared audit-contract crate; no second mock surface; no changes to
  production `CentralConfig` or `eos-runtime::main`.

---

## 2. Key decisions & assumptions

- **A1 — One crate `test_runner/`** (a top-level peer to `sandbox/`, `agent-core/`)
  with internal modules, mirroring the Python package
  (`backend/src/test_runner/{core,audit,agent,scenarios}`). It path-depends on
  agent-core crates (`eos-runtime`, `eos-engine`, `eos-audit`, `eos-sandbox-host`,
  `eos-sandbox-api`, `eos-config`, `eos-workflow`, `eos-state`) and one sandbox
  crate (`eos-protocol`, for typed audit `*Section` deserialization). Direction is
  always `test_runner -> {agent-core, sandbox}`. **[rev: was six crates — premature
  granularity for an internal harness with no external consumer.]**
- **A2 — LLM seam already injectable.** No seam introduction. One mock surface
  (`MockedLlmClient: EventSource`) replaces the client in the loop.
- **A3 — "use sandbox api/commands in `/sandbox`"** = drive the sandbox through its
  **wire protocol** (`eos-protocol` envelope; `api.v1.*` / `api.audit.*` daemon
  ops) via the `eos-sandbox-host` transport. Never reach into LayerStack / OCC /
  overlay internals.
- **A4 — Sandbox is real; the LLM is the only thing ever mocked.**
- **A5 — Container reuse is the default.** A session keeps one warm dask container
  per (instance × node). **[rev]** Per-test reset is the **real** SWE-EVO reset
  (git reset/clean/checkout + `build_workspace_base{reset}`), not overlay-only — see §8.2.
- **A6 — Audit bridge is consumer-side.** Each module keeps native emission; the
  collector normalizes. The only cross-repo change is the correlation key and a
  daemon-side emission *widening* (not a wire change).

---

## 3. Source-side prerequisites & dependencies

> **[rev]** The review found that some bridge work is **agent-core/sandbox feature
> work**, not test-harness work. This section separates the two so the scope
> expansion is explicit, not buried. **Decision needed from the user** on the
> out-of-scope items (they gate specific scenario tiers, not the baseline).

### 3a. Test-enabling bridge changes — IN SCOPE (small, harness-prerequisite)
| # | Change | Where | Why |
|---|---|---|---|
| P1 | Stamp engine `tool_use_id` onto `sandbox_invocation_id` **upstream**; verify every mint/fallback site honors a present id | `eos-engine::tool_call::dispatch` (`metadata_for_call`), `eos-tools::model_tools::sandbox` (drop the `new_v4` fallback when present), `eos-sandbox-host::daemon_client` (`new_invocation_id` — reuse present id) | Per-call audit join. The daemon **already echoes** the wire id as `ToolCallSection.tool_use_id` (`emit_tool_call_event`); the gap is upstream that the id is never populated. **[rev: not a daemon change.]** |
| P2 | `pub testsupport` feature exposing `ScriptedTurn`/`TurnScript`/`MockedLlmClient` (an `EventSource`) | `eos-engine` (model + emit helpers); reference impls `ScriptedSource`/`MockLlmClient` are `#[cfg(test)]` today | Let the external harness build scripted runs without re-implementing the trait (mirrors `eos-workflow/src/testsupport.rs`). **No loop change.** |
| P3 | Add `AppStateBuilder::advisor(Arc<dyn AdvisorPort>)` setter + a `pub testsupport` **`AutoApproveAdvisor`** stub | `eos-runtime::app_state` (setter), `eos-engine`/`eos-tools` (stub; `AdvisorPort` is `Sealed`, so the stub ships in-tree) | **Unblocks gated terminals** (`submit_*_outcome`). Today `AdvisorService::approval_status` **always denies** ("advisor runner not wired"), so every gated scenario would block. An injected auto-approver is the *test-appropriate* fix — **not** building the real advisor runtime. |
| P4 | Wire the **dead** `engine.tool.*` audit path to publish on the injected sink; enrich `AuditNode` from `QueryContext`/`ExecutionMetadata` (request/task/attempt ids) | `eos-engine::query::loop_` + `eos-engine::audit::stream` (`audit_events_from_stream_event` has **zero** callers) | Without this the collector sees only `plugin.*`. Enrichment lets tool rows self-group into the §7 tree without an `eos-state` join. |
| P5 | Daemon-side **emission widening**: add the breadcrumb ids the daemon already receives (`SandboxCaller::identity_block`: workflow/attempt/task) to `ToolCallSection` | `sandbox/eos-protocol::audit` + `eos-daemon::server::emit_tool_call_event` | The ids reach the daemon but are discarded at section-build time (only `agent_id` kept). No wire change. |
| P6 | Add `providers.active: ProviderKind` to `eos-config` (keys stay **env-only**, matching Python) | `eos-config::providers` | **Bugfix**: `default_llm_client`'s `if/else-if` makes OpenAI unreachable whenever `ANTHROPIC_API_KEY` is set. Update the `ProvidersConfig` parity case + insta snapshot. |

### 3b. Agent-core / sandbox migration work the harness DEPENDS ON — OUT OF SCOPE (flagged)
> The **baseline** mock tier (non-gated correctness, no explorer subagents)
> delivers value with only 3a. These gate **specific** tiers and should be carved
> out as explicit dependencies, owned by the agent-core/sandbox migration:

| # | Dependency | Gates which tier | Workaround until done |
|---|---|---|---|
| D1 | `SubagentSupervisorPort::spawn` must drive `run_ephemeral_agent` for `subagent`-role agents (today it only `register_running`, never runs the loop) | Scenarios using `run_subagent`/explorer | Skip explorer-spawning scenarios; baseline uses `delegate_workflow` (a different, working path) |
| D2 | Lifecycle audit emitters (`request/workflow/iteration/attempt started/completed`) in `eos-workflow` (it holds an unused `audit_sink`) | The **full** request→workflow→attempt timeline tree (§7.5) | Tool-level + sandbox-level timeline works; lifecycle nodes joined from `eos-state` instead |
| D3 | **Cooperative cancellation** in tool dispatch (poll `shutdown.is_cancelled()` at await boundaries / thread the token to the daemon client) | Clean **mid-tool** early-abort (§6.4) | Baseline terminates via natural loop exits / between turns; mid-tool abort is best-effort (see §6.4 consistency contract) |
| D4 | Per-test **OCC publish idempotency/atomicity across abort** (verify daemon-side) | Early-abort during a sandbox write | Drive early-abort at turn boundaries, not mid-write |

---

## 4. System architecture

```
                          test_runner/  (ONE new top-level crate)
   ┌─────────────────────────────────────────────────────────────────────────┐
   │  config        audit            agent (mock|api)            sandbox       │
   │  ──────        ─────            ────────────────            ───────       │
   │  RunConfig     Collector        MockedLlmClient             SandboxPool   │
   │  (.env→env)    Timeline+Render  (EventSource)               FastReset     │
   │  run params    normalize.rs     run→completion/partial      MultiNode     │
   │                (TraceEvent)     AutoApproveAdvisor(inj.)     (rollback)    │
   └───────┬───────────┬──────────────────┬───────────────────────┬──────────┘
           │           │                  │                        │
   reads   │  in-proc  │ AuditSink   inject EventSource +    wire: eos-protocol
   .env+yaml│  capture │             advisor stub           api.v1.* / api.audit.*
           ▼           ▼                  ▼                        ▼
   ┌──────────────┐  ┌──────────────────────────────┐   ┌─────────────────────┐
   │  eos-config  │  │          agent-core            │   │   eos-sandbox-host   │
   │ (+providers. │  │  eos-runtime  eos-engine       │   │   (host transport)   │
   │   active)    │  │  eos-audit    eos-llm-client   │   └──────────┬──────────┘
   └──────────────┘  │  eos-workflow eos-state        │              │ TCP/UDS
                     └───────────────┬────────────────┘              ▼
                                     │ api.v1.* (tool_use_id stamped)┌─────────────────┐
                                     └──────────────────────────────▶│  eosd (Rust)    │
                                              api.audit.pull          │  /sandbox crates │
                                     ◀────────────────────────────────│  daemon + ring   │
                                                                      └─────────────────┘
   Bridge (←): engine.* (in-proc AuditSink)  +  sandbox.* (pull, native Sections)
               → collector normalize → ONE TraceEvent timeline, joined on tool_use_id
```

---

## 5. Module: `config`

### 5.1 Current state (`eos-config`)
`CentralConfig { database, sandbox, providers, attempt }`, layered
`defaults < ephemeralos.yaml < env < init`, `#[serde(deny_unknown_fields)]`. Gaps:
no active-provider selector; api keys read inline via `std::env::var` + hardcoded
base_urls in `default_llm_client`; **no `.env` loading** (deliberately removed).

### 5.2 Design — minimal, env-keyed, harness-scoped `.env`
**[rev]** Three corrections from review:
1. **Do NOT re-add a `runner` section to production `CentralConfig`** (it was
   removed as `GC-eos-config-05`; re-adding reverses a recorded decision and breaks
   the schema-parity test). Runner/multi-node knobs live in the harness's own
   `RunConfig`.
2. **Keep api keys env-only** (matching Python `providers.py`: "API keys remain
   env-only"). `eos-config` gains only `providers.active: ProviderKind`. This both
   fixes the OpenAI-unreachable bug **and** keeps secrets out of the serialized
   config — no `env:VAR` placeholder machinery, no `SecretString` plumbing needed.
3. **`dotenvy::dotenv()` is called only in the `test_runner` harness entrypoint**,
   never in `eos-runtime::main`. The harness process hydrates `.env` into the
   process env *before* building `AppState`; `default_llm_client` then reads the
   keys via the existing `std::env::var` path. Real exported env still overrides
   `.env` (dotenvy default → preserves `env > yaml`).

```
.env  (user / Python writes ANTHROPIC_API_KEY=… , OPENAI_API_KEY=…  — manual/external)
   │  test_runner main: dotenvy::dotenv()   (FIRST, harness-only)
   ▼
process env ──▶ default_llm_client: std::env::var(ANTHROPIC_API_KEY|OPENAI_API_KEY)
                CentralConfig.providers.active → picks which client to build
                AnthropicClient::new(base_url, Auth::ApiKey(secret_from_env), retry)
```

### 5.3 `RunConfig` (harness per-run handle — decoupled from `CentralConfig`)
**[rev]** Match the Python shape: `RunConfig` holds only run-scoped fields and does
**not** embed `CentralConfig`; the resolved provider client/config is passed
separately to the runner.

```rust
pub struct RunConfig {
    pub entry_prompt: String,
    pub instance_id: SweevoInstanceId,         // EOS_SWEEVO_INSTANCE
    pub fidelity: Fidelity,                    // Mock | Live
    pub subject: Subject,                       // AgentExecution | SandboxTools | SandboxRpc
    pub load: Load,                             // Single | Multi { nodes: u32 }
    pub reuse_mode: ReuseMode,                  // Fresh | Reuse | ForceFresh
    pub audit_dir: PathBuf,
    pub run_label: String,
    pub max_duration_s: Option<u64>,            // wall-clock cap → early abort
    // live_e2e knobs (concurrent_sandbox_runners, real_agent_max_duration_s,
    // heavy_enabled, capacity_enabled) live here, NOT in production CentralConfig.
    pub live_e2e: LiveE2eParams,
}
```

### 5.4 Files & source-side change
```
test_runner/src/config/
  mod.rs
  run_config.rs     // RunConfig, Fidelity/Subject/Load/ReuseMode
  env_bootstrap.rs  // load_dotenv() + load_central() (harness entrypoint only)
```
Source change: P6 (`providers.active` + parity-case/snapshot update). **[rev: no
`runner` section, no `dotenv_writer.rs`, no credential-resolver plumbing.]**

---

## 6. Module: `agent` (mock + api)

### 6.1 The seam (one mock surface)
```
run_query (eos-engine::query::loop_)
   └─ source: Arc<dyn EventSource> = ctx.event_source         ← INJECT MockedLlmClient HERE
        ├─ ProviderEventSource (prod) ── wraps ── Arc<dyn LlmClient> (Anthropic/OpenAI)
        └─ MockedLlmClient (the only mock) ── holds ── Box<dyn TurnScript>
```
**[rev]** Ship **one** scripted surface — the engine-level `MockedLlmClient`
(`impl EventSource`). It is named per the user's request ("rename to mocked llm
client"). The provider-level `LlmClient` mock and the precedence-footgun guard are
**dropped**; the real encode/adapt path is covered by the §6.5 live smoke.

### 6.2 Scripted-turn model (the primary type is the **branching** one)
```rust
pub struct ScriptedTurn { pub thinking: Option<String>, pub text: Option<String>, pub calls: Vec<ScriptedCall> }
pub struct ScriptedCall { pub name: String, pub input: JsonObject }

/// PRIMARY: result-reading, stateful (interior-mutable). Needed for branching
/// scenarios (root delegation polls check_workflow_status up to ~90 turns).
pub trait TurnScript: Send + Sync {
    fn next_turn(&self, prior: &[ToolResult]) -> Option<ScriptedTurn>;
}
/// Convenience for NON-branching fixtures ONLY — never for delegation/polling.
impl TurnScript for Mutex<std::vec::IntoIter<ScriptedTurn>> { /* ignores prior */ }

pub struct MockedLlmClient { script: Box<dyn TurnScript> }
impl EventSource for MockedLlmClient {
    async fn stream(&self, req: &LlmRequest) -> Result<EngineStream, EngineError> {
        let prior = trailing_tool_results(req);   // scan BACKWARD past appended notifications
        let turn = self.script.next_turn(prior).unwrap_or_else(ScriptedTurn::text_only_eos);
        Ok(emit_stream(turn))   // [ReasoningDelta?, TextDelta?, ToolUseDelta×N, AssistantMessageComplete]
    }
}
```

**[rev] Loop invariants — corrected against `run_query`:**
1. **Always end each turn with `AssistantMessageComplete`** carrying the tool_use
   blocks. Absent → `EngineError("provider stream ended without assistant
   completion")`. *(This — not deltas — is the load-bearing requirement.)*
2. Budget is counted via `streamed_tool_use_ids` de-dup **plus** a second pass over
   the message's tool_use blocks. Deltas are **optional for budget**, but when
   present their `ToolUseId`s **must match** the complete-message block ids
   (mismatch double-counts). Emit deltas-before-complete for production-faithful order.
3. There is **no dispatch-time budget gate**. The only hard ceiling is
   `terminal_submission_failed`: `tool_calls_used + text_only_no_terminal_turns >=
   (tool_call_limit*3 + 1)/2`. `attempt_budget_exhausted` scenarios must be driven
   by that ceiling or by attempt-level orchestration, not a per-call gate.
4. A terminal tool must be the **only** call in its turn (`debug_assert!` in the mock).
5. On script exhaustion, yield a **valid text-only `AssistantMessageComplete`
   every subsequent turn** (never an empty stream) so the `*3/2` ceiling terminates.
6. Leave `agent_name`/`agent_run_id` empty on **all** emitted events; the loop's
   `stamp_identity` fills them.
7. `trailing_tool_results` scans `req.messages` **backward** for the most recent
   user message carrying `ToolResult` blocks (the loop appends a notification/reminder
   user message *after* the results — a naive "last message" read returns the reminder).

### 6.3 Gated terminals & subagents (the §3 prerequisite, surfaced here)
Gated terminals (`submit_generator_outcome`/`submit_reducer_outcome`/
`submit_root_outcome`) call `ask_advisor` → `AdvisorPort::approval_status`. The
harness injects **`AutoApproveAdvisor`** via the new `AppStateBuilder::advisor(...)`
setter (P3). The harness also registers `main`+`helper`+`subagent` agent profiles
into the **immutable** `AgentRegistry` (built once via `AgentRegistryBuilder`,
injected through `AppStateBuilder::agent_registry`). Explorer (`run_subagent`)
scenarios additionally require D1 (out of scope) and are skipped until then.

### 6.4 Run→completion + early-terminate (with the consistency contract)
```
start_request(state, prompt) ─▶ RequestEntryHandle { request_id, root_task_id,
                                  root_agent_task: JoinHandle<()>, state(AppState{shutdown: CancellationToken}) }
  ├ full finish : handle.join().await            (root submits submit_root_outcome)
  └ PARTIAL / TIMEOUT:
      tokio::select! {
        _ = handle.join()       => Completed,
        _ = condition_watcher   => stop(),    // fed by audit events (a tool completed / conflict / N calls)
        _ = sleep(max_duration) => stop(),
      }
      stop() = handle.shutdown(grace)  // cancel token, parent-exit supervisor, await within grace, abort on timeout
```
**[rev] Consistency contract (the abort path is NOT lossless):** the
`CancellationToken` is **not** observed inside tool execution today (D3), so
`JoinHandle::abort()` cuts at the next `.await` — possibly mid-daemon-roundtrip.
Therefore:
- `Partial`/`AbortedByTimeout` outcomes carry **best-effort, possibly-truncated**
  audit (the in-proc `CapturingSink` "0 dropped" guarantee holds only for runs that
  end via normal loop exit, **not** mid-tool abort).
- Prefer terminating at **turn boundaries** (drive the condition off completed-tool
  audit events) over mid-tool abort.
- Clean mid-tool abort requires D3 (cooperative cancellation) + D4 (idempotent OCC
  publish) — flagged dependencies, not baseline.

### 6.5 Live api-client smoke (trivial — one test per provider)
```
api_smoke(provider):
  load .env → build real Anthropic/OpenAI client →
  send 1-turn LlmRequest {system reminder, one tool `echo{message}`, "respond by calling echo"} →
  assert: ToolUseDelta name=="echo" with well-formed input,
          terminated by AssistantMessageComplete{stop_reason: ToolUse}  (system reminder honored → tool, not free text)
```
Gated by key presence (`#[ignore]` + env preflight). Not a matrix.

### 6.6 Files & source-side change
```
test_runner/src/agent/
  mod.rs
  script.rs         // ScriptedTurn, ScriptedCall, TurnScript, emit_stream, trailing_tool_results
  mocked_llm.rs     // MockedLlmClient (impl EventSource)
  advisor_stub.rs   // wires AutoApproveAdvisor + registers main/helper/subagent profiles
  run.rs            // RequestEntryHandle driver + early-terminate (select!)
  api_smoke.rs      // trivial live anthropic/openai test
```
Source changes: P2, P3 (+ D1 flagged for explorer scenarios).

---

## 7. Module: `audit` (the reform — consumer-side bridge, four facets, human-readable)

### 7.1 The bridge answer (the user's confusion-point #1)
**Each module keeps its native audit; the bridge is consumer-side.** No shared
contract crate. The collector defines `TraceEvent` and writes `From<AuditEvent>`
(agent-core) and `From<&Section>` (sandbox) — exactly as Python's
`daemon_event_normalizer.py` + `performance_report.py` do. The **only** cross-repo
coupling is the correlation key (P1) and the daemon emission widening (P5).

### 7.2 The four-facet `TraceEvent` (defined ONLY in `test_runner::audit::normalize`)
```rust
pub struct TraceEvent {
    pub ord: u64,                 // collector-assigned merged-timeline ordinal (the total order)
    pub ts: UtcDateTime,
    pub source: TraceSource,      // Engine | Workflow | Sandbox | Plugin
    pub kind: String,             // "engine.tool.completed", "sandbox.occ.publish", …
    pub node: CorrelationNode,    // join keys (superset of eos-audit AuditNode + agent_role + ordinals)
    pub facets: Facets,
    pub raw: Option<JsonObject>,  // forensic, gated by EOS_AUDIT_FORENSIC_RAW_ENABLED (Python parity)
}
pub struct Facets {
    pub semantics:  Option<Semantics>,   // headline sentence + op + detail
    pub performance: Option<Performance>,// duration_ms, phase_ms map
    pub resource:   Option<Resource>,    // bytes_in/out, peak_resident, changed_path_count, tokens_in/out
    pub correctness: Option<Correctness>,// status, error_kind, conflict, is_terminal
}
```
**[rev] Facet sourcing — honesty about "no new measurement code":** performance,
resource (bytes/peak/paths), and correctness map onto fields **both sides already
emit**. The **one** genuinely-new projection is **token usage** (`UsageSnapshot`
exists on the engine turn event but is never audited) — if the `resource` facet
includes tokens, P4's change list must add an `engine.turn.completed` usage emitter;
otherwise drop the tokens line. *(Recommended: include it — the user explicitly
named "resource usage"; it is a tiny projection of an existing struct.)*

**[rev] Ordering:** the **collector assigns `ord`** at ingest (the only place that
sees both streams). The daemon ring's own `seq`/`lost_before_seq` are kept strictly
for **drop detection**, not cross-source order. No sink-boundary seq is added to
`eos-audit` (it could not total-order across the boot-scoped ring anyway).

### 7.3 Correlation (P1) — direction, not an exact diff
```
engine dispatch: metadata.tool_use_id = Some(tu-…)   (already set)
   │  STAMP upstream onto sandbox_invocation_id  (verify all mint/fallback sites honor a present id)
   ▼
SandboxRequestBase.invocation_id = tu-…
   ▼  (daemon ALREADY echoes wire id)
eosd ToolCallSection.tool_use_id = tu-…   ==   engine.tool node.tool_use_id
   ▼
collector joins both streams on tool_use_id  → per-call correlation (fallback: agent_run_id)
```
The daemon needs **no** tool_use_id change. Mint sites to audit: `eos-tools::
model_tools::sandbox` (`new_v4` fallback) and `eos-sandbox-host::daemon_client`
(`new_invocation_id`) — both must reuse a present id.

### 7.4 Collector files
```
test_runner/src/audit/
  mod.rs
  capturing_sink.rs  // impl AuditSink: in-proc, lossless Vec (agent-core side)
  daemon_puller.rs   // port of DaemonAuditPuller: api.audit.pull cursor/cadence/boot_epoch_id
  normalize.rs       // TraceEvent + From<AuditEvent> + From<&eos_protocol::Section>
  timeline.rs        // merge both streams, assign ord, group by node
  render.rs          // tree + summary  (semantics/performance/resource/correctness)
  jsonl_sink.rs      // RotatingJsonlSink port (canonical sandbox_events.jsonl artifact)
  query.rs           // assertion helpers for Expectation (by kind/node/facet)
```

### 7.5 Human-readable output (sample — the deliverable)
```
REQUEST req-9f3a  "fix dask groupby regression"            [PASS]  42.1s
└─ workflow wf-21 (delegated)  iter 2 / attempt 2          ✔ reducer gate
   └─ task t-7 (executor)  agent_run ar-55
      ├─ engine.tool.completed  write_file  src/groupby.py
      │     semantics : wrote file (overlay capture)
      │     perf      : 12.4ms      resource: +1 path, 3.1 KiB out      correct: ok
      │     └─ sandbox.occ.publish  tool_use=tu-7f… (JOINED)
      │           perf: prepare 1.1 / apply 2.0 / commit 0.8 / publish 0.4 ms
      │           resource: 1 changed path                              correct: ok (no conflict)
      ├─ engine.tool.completed  exec_command  "pytest -q"   correct: error (exit 1)  perf: 8.7s
      └─ engine.tool.completed  submit_generator_outcome    correct: ok (terminal)
SUMMARY  tools 31 (write 8/read 6/exec 4/search 9/terminal 4) · sandbox occ.publish 8 conflict 0 squash 1
         perf agent 38.0s sandbox 4.1s · resource tokens 18.2k/2.1k* · correctness 0 unexpected errors, reducer PASS
         audit: in-proc 0 dropped · ring lost_before_seq 0     (* tokens require P4 usage emitter)
```

---

## 8. Module: `sandbox` (real, fast, reusable, multi-node)

### 8.1 Drive via the `/sandbox` wire (A3)
Orchestrate **only** through `eos-sandbox-host`: provision
(`RequestSandboxProvisioner::prepare_for_run`), lifecycle (`SandboxLifecycle`),
transport (`DaemonClient: SandboxTransport`), tool ops (`tool_api::*` → `api.v1.*`),
audit pull (`api.audit.*`), one-time eosd push (`runtime_artifact::
ensure_eosd_uploaded`, marker-skip `/eos/daemon/.eosd-sha256`).

### 8.2 Fast reuse — **[rev] corrected per-test reset**
The review verified that overlay-only reset **cannot** prevent contamination: the
Python `reset_sweevo_workspace` does `git reset --hard / clean -fd / checkout -f
base_commit` + `build_workspace_base{reset}` (+ `pip install -e .` + daemon rebind),
and `commit_to_workspace` materializes overlays into the repo's `.git`.

```
                       FIRST container in session         EVERY reused test
  docker pull+tag image (snapshot)   ████ one-time         ░ skip (cached tag)
  create + map daemon port           ████ one-time         ░ skip (resume start)
  ensure_eosd_uploaded               ████ one-time         ░ skip (.eosd-sha256 marker)
  ensure_daemon_current (spawn)      ████ one-time         ░ skip (pid+socket liveness)
  ─────────────────────────────── REAL per-test reset ───────────────────────────────
  git reset/clean/checkout base      ████ first use        ████ per-test
  build_workspace_base{reset}        ████ first use        ████ per-test
  pip install -e . / daemon rebind   ████ first use        ████ per-test IF the test mutates
                                                                  site-packages or rebinds /eos/mount
```
**Skip everything cacheable** (image/eosd/daemon/snapshot/base-layer). The
**"active-overlay-only reset" fast path is a precondition, not the default**: it is
safe **only** for tests that never materialize the overlay (`commit_to_workspace`)
and never mutate site-packages. The default per-test path is the full SWE-EVO reset.

### 8.3 Configurable multi-node — **[rev] with partial-failure rollback**
```
  RunConfig.live_e2e.concurrent_sandbox_runners = N  (semaphore cap, pre-acquire quota check)
  SandboxPool::provision_n(N):
     shared:   ONE image/snapshot pull+tag, ONE host artifact_dir (sandbox/dist)
     per-node: distinct SandboxId; label node_index (via set_labels post-create —
               fresh_create_spec only mints a random request-<hex> name)  [rev]
     ROLLBACK: SandboxLifecycle::create registers EAGERLY → if node k<N fails,
               delete+dispose nodes 0..k-1 (else lease/quota leak)        [rev]
  reuse/attach: requires a list()+name-filter discovery step              [rev]
               (prepare_for_run takes an explicit id only; no name path — mirror
                Python _find_existing_sandbox_by_name over ProviderAdapter::list())
  teardown:    release() deletes/disposes all N (no-op all under Reuse/Attach)
```
Verified: `ProviderRegistry.bindings` + `DaemonClient.tcp_cache`/`tcp_locks` are
already per-`SandboxId` → **no new isolation primitive**. The new work is the
N-provisioner, the rollback, the reuse discovery, and per-node labelling.

### 8.4 Files & source-side change
```
test_runner/src/sandbox/
  mod.rs
  pool.rs        // SandboxPool: provision / provision_n (+rollback) / reuse / teardown
  fast_reset.rs  // real per-test reset; cacheable-skip logic
  instance.rs    // SweevoInstance resolve (EOS_SWEEVO_INSTANCE → image/base_commit)
  ops.rs         // thin typed facade over eos-sandbox-api tool_api (+ plugin if that tier is built)
```
Source change: N-provisioner + rollback + reuse discovery (host-side); **plugin
`DaemonOp` variants only if the plugin sandbox tier is built**.

---

## 9. Test taxonomy (preserve rigor; drop the bad layout)

### 9.1 Axes — **[rev] three subjects** (sandbox invariants are daemon-RPC, not loop)
```
  fidelity ∈ {Mock, Live}
  subject  ∈ {AgentExecution,        // through the real engine loop
              SandboxTools,          // engine-driven high-volume tool calls (stability/load)
              SandboxRpc}            // DIRECT api.* daemon RPC (IWS/OCC/overlay/layerstack invariants)
  load     ∈ {Single, Multi{nodes}}  // replaces smoke-vs-full + the capacity mega-scenario
```
**[rev]** The `isolated_workspace`/OCC/overlay/layer-stack invariants are driven by
**direct `api.isolated_workspace.*` / `api.v1.*` daemon RPC**, NOT the engine loop —
hence the explicit `SandboxRpc` subject. Mis-routing them through "scripted tool
calls" would silently shrink coverage to a handful of phrases.

### 9.2 Ported architecture (the good parts)
- `run_pipeline` 5-seam spine; `LifecycleHooks` (`before_run/on_event/after_run/
  on_aborted`); dual report (`PipelineReport` + mode views from typed audit events);
  Scenario-as-data + **real loop**; `_graph_summary` real-state walk on `eos-state`.
- The single most important property: **mock tests drive the REAL loop** (real tool
  dispatch, terminal-alone, budget, real ContextEngine XML envelopes); **graph shape
  is read from persisted `eos-state` rows, never scenario self-report.**

### 9.3 `Expectation` — **[rev] expanded to cover what `FocusedScenarioCase` +
`_assert_tool_and_event_capacity` assert** (each field maps to a real Python assertion)
```rust
pub struct Expectation {
    pub request_status: RequestStatus,
    pub role_task_floors:  BTreeMap<(AgentRole, TaskStatus), u32>,  // status-scoped (done/failed) [rev]
    pub absent_done_roles: BTreeSet<AgentRole>,                     // [rev]
    pub required_event_kinds: Vec<String>,
    pub attempt_count: Option<u32>, pub iteration_count: Option<u32>,
    pub deferred_attempt_bounds: Option<(u32,u32)>,
    pub recursive_workflow_count: Option<u32>,                      // multi-workflow/delegation [rev]
    pub tool_count_floors: BTreeMap<String, u32>,                   // write>=30, read>=20, …
    pub tool_error_floor: Option<u32>,                             // tool_errors_total>=1 [rev]
    pub required_sandbox_events: Vec<String>,                       // occ.publish, squash, conflict, …
    pub dependency_prompt_xml: bool,                               // the real-XML-envelope gate [rev]
    pub sandbox_checks_pass: bool,                                 // [rev]
    pub forbidden_substrings: Vec<String>,    // no-internal-error gate: "internal_error","stale lowerdir",… [rev]
}
```

### 9.4 Scenario catalog (small, orthogonal, high-signal)
- **Mock×AgentExecution**: `initial_workflow`, `initial_messages_capture` (root-request
  envelope) **[rev]**, `dependency_dag_{serial,parallel,diamond,mixed}` **[rev: +mixed]**,
  `dependency_blocked_descendants`, `attempt_retry_{planner,generator,reducer}_failure`,
  `iterative_deferral`, `nested_workflow(_failure)`, `attempt_budget_exhausted`,
  `generator_failure_quiescence`, + 6 `planner_validation` negatives.
- **Mock×SandboxTools**: high-volume scripted tool calls → write/read/exec/search floors.
- **SandboxRpc** (Mock or Live) **[rev]** — enumerate invariant **categories**, table-driven
  (not 120 files): OCC conflict round-trip; overlay capture/publish; auto-squash;
  lease-non-leak; read-only-plugin-no-publish vs write-plugin-publish; finite-command
  vs command-session lifecycle; **IWS**: enter-rejects-active-bg, exit-drains+releases,
  no-OCC-publish, audit-only-writes, daemon-restart orphan-GC (cgroup/netns/scratch/
  veth/lease), quota-one-per-agent / total-cap / host-RAM-gate / TTL-evict, **O(1)
  lowerdir disk (`workspace_tree_bytes==0` regression gate)**, concurrent-enter IP
  non-double-allocation, network hardening (egress masquerade / IMDS+RFC1918 drop /
  inbound reject). State explicitly which buckets are **dropped** vs ported.
- **Live×Sandbox**: parity asserted on **named daemon ops** (provision →
  `ensure_workspace_base` → `api.v1.*` round-trip → `api.audit.pull`) **[rev: not "the
  bench-script flow"; the bench files are churning in the worktree]**.
- **Live×AgentExecution**: api smoke (§6.5) + SWE-EVO real-agent F2P/P2P.

### 9.5 Dropped — **[rev] keep the cross-cutting invariants, drop only the scenario SIZE**
Drop `full_stack_adversarial`, `full_system_capacity_matrix`, `pack_catalog`, the
~120-file IWS layout, smoke-vs-full duplication, the percentile perf aggregator
(→ `benches/`). **But RETAIN as `Expectation`/`Correctness`-facet queries** (they are
cross-cutting correctness, not capacity-only): the **no-internal-error / forbidden-
signature** gate (8 `_invariants.py` files), the **tool_error_floor**, and the **O(1)-
overlay `workspace_tree_bytes==0`** + **`sum(phases_ms) <= total_ms`** regression gates.

---

## 10. Workspace layout

```
test_runner/                       (NEW top-level crate)
  Cargo.toml                       // path-deps → ../agent-core/*, ../sandbox/eos-protocol
  rust-toolchain.toml
  src/
    lib.rs
    config/                        // §5
    audit/                         // §7  (collector owns TraceEvent + normalize)
    agent/                         // §6  (MockedLlmClient, run, advisor stub, api smoke)
    sandbox/                       // §8
    core/                          // run_pipeline spine, LifecycleHooks, reports, Expectation, graph_summary
    scenarios/                     // §9  scenario turn-script data + catalog
  tests/
    mock_agent.rs                  // Mock×AgentExecution
    mock_sandbox.rs                // Mock×SandboxTools
    sandbox_rpc.rs                 // SandboxRpc invariant table
    live_sandbox.rs  live_agent.rs // gated
  benches/                         // perf lane (out of the test taxonomy)
```
Source changes live in their home crates (P1–P6 in agent-core/sandbox; D1–D4 flagged).

---

## 11. SOLID / SRP & simplicity guardrails

| Principle | Where |
|---|---|
| **SRP** | One module per job. Audit **emission** stays in source modules; audit **presentation/normalization** stays in the collector — the bridge is consumer-side, so there is one source of truth per side. |
| **Open/Closed** | New scenario = new `ScriptedTurn` data. New audit source = new `From<…>` in `normalize.rs`. No engine/collector change. |
| **Liskov** | `MockedLlmClient` is a drop-in `EventSource`; `AutoApproveAdvisor` a drop-in `AdvisorPort`; `SandboxPool` honors the `eos-sandbox-host` contract. |
| **ISP** | Four narrow facets; `TurnScript` is one method; `SandboxTransport` is one `call`. |
| **DIP** | Harness depends on traits (`AuditSink`, `EventSource`, `AdvisorPort`, `SandboxTransport`), injected via existing `AppStateBuilder` setters (+ the new `advisor()`). |

**Deliberately NOT built** (over-engineering = defect): shared audit-contract crate;
second mock surface + precedence guard; `runner` section in production config;
`dotenvy` in `eos-runtime::main`; `dotenv_writer` module; `env:VAR`/`SecretString`
credential plumbing (keys stay env-only); per-scenario classes; the 120-file IWS
layout; building the real advisor runtime (an injected stub suffices).

---

## 12. Progress checker

> `[ ]` = todo. **P#** = in-scope source-side prerequisite (§3a). **D#** = flagged
> external dependency (§3b). Phases are independently verifiable.

### Phase 0 — Skeleton
- [ ] `test_runner/` crate compiles with path-deps to agent-core & sandbox
- [ ] `core` `run_pipeline` spine + `LifecycleHooks` + `PipelineReport` stubs

### Phase 1 — Config
- [ ] **P6** `eos-config` `providers.active` (+ parity case + insta snapshot); fix `default_llm_client` selection bug
- [ ] `config`: `RunConfig`, harness-only `dotenvy::dotenv()` (NOT `eos-runtime::main`)
- [ ] verify: `.env` key hydrates env → correct provider client builds; env overrides `.env`

### Phase 2 — Mock agent (baseline)
- [ ] **P2** `pub testsupport` `ScriptedTurn`/`TurnScript`/`MockedLlmClient` (no loop change)
- [ ] **P3** `AppStateBuilder::advisor()` setter + `AutoApproveAdvisor` stub; register main/helper/subagent profiles
- [ ] `agent`: `MockedLlmClient`, `trailing_tool_results` (backward scan), run driver
- [ ] verify: 1-tool mock script reaches `submit_root_outcome`; **gated terminal passes** via stub; budget parity holds
- [ ] verify: condition-watcher terminates a run early → `Partial` (turn-boundary; audit best-effort noted)
- [ ] **D1** (flagged): explorer/`run_subagent` scenarios deferred until `spawn` drives `run_ephemeral_agent`

### Phase 3 — Audit (correlation + dead-path + collector)
- [ ] **P1** stamp engine `tool_use_id` → `sandbox_invocation_id` upstream; verify all mint/fallback sites reuse a present id
- [ ] **P4** wire dead `engine.tool.*` path on the injected sink; enrich `AuditNode` from `QueryContext`; (optional) `engine.turn.completed` usage emitter
- [ ] **P5** widen daemon `ToolCallSection` with the breadcrumb ids it already receives
- [ ] `audit`: `CapturingSink`, `DaemonAuditPuller`, `normalize` (`From<AuditEvent>` + `From<&Section>`), `timeline` (collector-assigned `ord`), `render`
- [ ] `CONTRACT.md`: add audit-pull schema as a coordinated surface
- [ ] verify: a real mock run emits `engine.tool.*` (not just `plugin.*`); engine tool event ↔ its `occ.publish` share `tool_use_id`; §7.5 tree + summary render
- [ ] **D2** (flagged): full lifecycle tree awaits `eos-workflow` lifecycle emitters

### Phase 4 — Sandbox (real, fast, multi-node)
- [ ] `sandbox`: `SandboxPool` provision/reuse over `eos-sandbox-host`
- [ ] fast reset: cacheable-skip + **real** per-test reset (git + `build_workspace_base{reset}`)
- [ ] `provision_n` + semaphore + **partial-failure rollback** + reuse discovery + per-node labels
- [ ] verify: warm reuse skips cacheable setup, no cross-test contamination; N=3 lanes, no quota overrun / lease leak (incl. mid-provision failure)
- [ ] **D3/D4** (flagged): clean mid-tool early-abort awaits cooperative cancellation + OCC idempotency

### Phase 5 — Taxonomy + scenarios
- [ ] expanded `Expectation` + `assert_report`; `graph_summary` real-state walk (all workflows)
- [ ] Mock×AgentExecution catalog (§9.4) as turn-script data
- [ ] Mock×SandboxTools floors; **SandboxRpc** invariant table (categories, ported-vs-dropped stated)
- [ ] retain cross-cutting gates (no-internal-error, tool_error_floor, O(1)-overlay, phase-sum) as facet queries
- [ ] Live×Sandbox parity (named ops); Live×AgentExecution api smoke + sweevo (gated)

### Phase 6 — Cutover
- [ ] `docs/architecture/` page for `test_runner`
- [ ] retire/redirect Python `backend/src/test_runner` after parity; no Python sandbox internals imported

---

## 13. Resolved decisions (former open questions)
- **Q1 — who owns `TraceEvent`?** The **collector** (`test_runner::audit::
  normalize`). No shared `eos-audit-contract` crate; both source repos keep native
  audit types. *(Review: a shared contract duplicates `AuditNode` + native
  `*Section`, two sources of truth.)*
- **Q2 — correlation key?** Reuse the engine `tool_use_id` as the sandbox
  `invocation_id` (it is already the daemon's registry/cancel key and is already
  echoed into `ToolCallSection.tool_use_id`). **No** new caller field —
  `SandboxCaller.tool_id` already exists.
- **Q3 — ordering?** Collector assigns the merged-timeline `ord` at ingest; the
  daemon ring `seq`/`lost_before_seq` stay for drop detection only. No sink-boundary
  seq in `eos-audit`.

### Genuinely open (need a user call)
- **§3b scope**: do D1 (subagent spawn) / D2 (lifecycle audit emitters) / D3 (cooperative
  cancellation) get done as part of this effort, or are they tracked as agent-core
  migration work that the corresponding test tiers wait on? The **baseline** mock +
  sandbox tiers do **not** need them.
```
