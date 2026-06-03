# Subagent — Rust parity remediation plan (PLAN ONLY)

Status: **plan only, do not implement.** Scope: the `agent-core / subagent`
findings in `docs/reviews/rust_parity/REPORT.html` (`areas/subagent.md` +
`areas/subagent.verify.md`) — D1–D9 in full, verified against the Python
reference in `backend/src`.

This plan honors the user's binding constraints, each verified against Python:

1. **One central supervisor owns all background kinds.** The engine
   `BackgroundTaskSupervisor` is the single owner of subagent + workflow records and
   command-session lifecycle — one record store, one precedence latch, one count, one
   notification emitter — and it is the real `SubagentSupervisorPort` impl (on
   `BackgroundSupervisorHandle`). No per-kind supervisor class; workflow is a
   `BackgroundTaskKind`, not a separate subsystem at the handle/count layer. This
   mirrors Python, where one `BackgroundTaskSupervisor` holds `_tasks` / `_workflows`
   / `_command_sessions` and `count_by_agent` sums all of them.
2. **`run_ephemeral_agent` is relocated into `eos-engine` and called directly.** Per the
   advisor lane (`advisor-remediation-PLAN.md` §2a), `run_ephemeral_agent` is an engine
   primitive (a wrapper over `build_query_context` + `run_query`); it moves to
   `eos-engine` with `&AppState` dropped for explicit handles (`agent_run_store`,
   `model_store`, `llm_client`, `event_source_factory`, `agent_registry`, `cwd`). The
   supervisor calls `eos_engine::run_ephemeral_agent(handles, …)` **directly** — no
   runner trait, no runtime-side class, no injection. The dead `dispatch.rs`/`policy.rs`
   path is deleted, not wired; there is no `bg_<n>`/`Agent` kind.
3. **No new mechanism / one shared engine primitive.** The same relocated
   `run_ephemeral_agent` serves advisor (inline), subagent (detached), and workflow.
   Reuse the registry's `AgentType::Subagent` filter, `build_explorer_launch_prompt`,
   and the existing `SubagentSupervisorPort`. The count surface returns a per-kind JSON
   report; the terminal prehook drains all kinds to 0; the supervisor is the single
   emitter of `background_tool.*`.

---

## 0. At a glance

### Key concepts

| # | Concept | One-liner |
|---|---|---|
| K1 | **One central background supervisor** | A single `BackgroundTaskSupervisor` (eos-engine) owns *all* background kinds — subagent + workflow records + command sessions — one ledger, one count, one notifier. No per-kind supervisor. |
| K2 | **`run_ephemeral_agent` is a shared engine primitive** | Relocated `eos-runtime` → `eos-engine` (drops `&AppState`, takes explicit handles). Advisor, subagent, workflow all call the *same* function — no runner trait, no injection. |
| K3 | **Two launch modes over one primitive** | `ask_advisor` = inline/**blocking** (`.await`); `run_subagent` = detached/**async** (`tokio::spawn`). |
| K4 | **Two dispatchers, by statefulness** | Advisor = **dispatch interception** (stateless, needs handles+conversation); subagent = **generic tool path → port → supervisor** (stateful; supervisor carries handles). |
| K5 | **Terminal gate drains, not denies** | The `submit_*_outcome` prehook cancels+settles all kinds to 0, then proceeds. |
| K6 | **Running-only count** | The inflight report counts only `Running` records — why the drain reaches `total == 0` and cancel races are harmless. |
| K7 | **Settle unwedges; abort is hygiene** | Settling the record (leaves `Running`) closes D9; `abort()` only stops runaway compute. |

### Core flow — `run_subagent` (detached async)

```
model ── run_subagent(name, prompt) ──────────────────────────────── generic tool path
  RunSubagent::execute (eos-tools)
   └─ SubagentSupervisorPort::spawn(name, prompt, caller_name, caller_id)
       └─ BackgroundSupervisorHandle::spawn ─► background/subagent.rs (eos-engine)
            1. validate (recursion / exists / is-subagent)          ← AgentRegistry
            2. role = build_explorer_launch_prompt(); initial = [user(prompt)]
            3. register_running(Subagent, agent_id) → "subagent_1"   ─ emit background_tool.started
            4. tokio::spawn { run = eos_engine::run_ephemeral_agent(handles, input).await
                              supervisor.settle("subagent_1", classify(run))  ─ emit completed/failed }
   └─ return "[SUBAGENT LAUNCHED]"   ◄── immediate (non-blocking)

  check_subagent_progress → running → "running";  settled → terminal result ("finished")
  submit_*_outcome → prehook DRAINS agent's kinds (settle + abort) → report.total == 0 → pass

contrast — ask_advisor: dispatch interception → advisor::run_advisor
           → run_ephemeral_agent(handles, …).await   ◄── BLOCKS, returns the verdict inline
```

### Resulting file / folder structure

```
agent-core/crates/
├── eos-tools/src/
│   ├── model_tools/subagent.rs                       EDIT  spawn() passes caller_name+caller_id
│   ├── ports.rs                                      EDIT  spawn sig; count→inflight_report; +drain_for_agent
│   ├── hooks/
│   │   ├── advisor_approval.rs                        (exists — advisor lane)
│   │   └── require_no_inflight_background_tasks.rs    NEW   drain hook (moved out of hooks.rs)
│   ├── hooks.rs                                       EDIT  keep Hook enum + dispatcher; body moves to hooks/
│   └── meta.rs                                        KEEP  prehook wiring on the 4 submit_* unchanged
├── eos-engine/src/
│   ├── background/
│   │   ├── supervisor.rs                              EDIT  rename→BackgroundSupervisorHandle; +agent_id, handles,
│   │   │                                                    run handles, sink; +settle/cancel_by_agent/inflight_report;
│   │   │                                                    −Agent kind/counter/progress_lines/push_progress
│   │   ├── subagent.rs                                NEW   validate + prompt + spawn orchestration + settle
│   │   ├── dispatch.rs · policy.rs                    DELETE (dead)
│   │   ├── heartbeat.rs                               EDIT  command-session completion routes through settle
│   │   └── mod.rs                                     EDIT  drop dispatch/policy re-exports
│   ├── agent_loop.rs                                  NEW   run_ephemeral_agent (relocated)      ◄ advisor lane
│   ├── advisor.rs                                     NEW   run_advisor launcher                  ◄ advisor lane
│   └── tool_call/dispatch.rs                          EDIT  AskAdvisor interception arm           ◄ advisor lane
└── eos-runtime/src/
    ├── entry.rs                                       EDIT  build BackgroundSupervisorHandle w/ handles+sink
    ├── agent_loop.rs                                  DELETE move to eos-engine                   ◄ advisor lane
    └── root_agent.rs · agent_runner.rs                EDIT  call eos_engine::run_ephemeral_agent  ◄ advisor lane
```

### Renames & field updates

| Kind | Old | New |
|---|---|---|
| rename (struct) | `SharedSubagentSupervisor` | **`BackgroundSupervisorHandle`** |
| add (struct) | — | `BackgroundInflightReport { total, subagent, workflow, command_session }` (Running-only) |
| remove (enum variant) | `BackgroundTaskKind::Agent` (`bg_<n>`) | — |
| remove (enum variant, opt) | `StopMode::EarlyStop` | — |
| `BackgroundTaskRecord` +field | — | `agent_id: Option<String>` |
| `BackgroundTaskRecord` −field | `progress_lines: Vec<String>` | — |
| `BackgroundTaskSupervisor` +fields | — | `handles`, run handles, `notification_sink` |
| `BackgroundTaskSupervisor` +methods | — | `settle`, `cancel_by_agent`, `inflight_report` |
| `BackgroundTaskSupervisor` −methods/fields | `push_progress`, `counter` | — |
| `SubagentSupervisorPort::spawn` | `(name, prompt)` | `(name, prompt, caller_name, caller_id)` |
| `SubagentSupervisorPort` count | `background_inflight_count → usize` | `inflight_report → BackgroundInflightReport`; + `drain_for_agent` |

### HEAD → Target diff

| Dimension | Rust HEAD (broken) | Target |
|---|---|---|
| Subagent launch | `register_running` only — no child runs | `tokio::spawn(eos_engine::run_ephemeral_agent)` |
| `run_ephemeral_agent` home | `eos-runtime` (unreachable by port) | `eos-engine` (shared primitive, direct call) |
| Result | `"Running: "` forever | settled terminal → `finished/failed` + JSON |
| Validation | none (falsely promised) | enforced in `background/subagent.rs` before mint |
| Terminal gate | deny-if-count>0 → **phantom wedges forever (D9)** | **drain-to-0** then pass |
| Count | every `Running` record, ignores agent | `BackgroundInflightReport`, Running-only, per-agent/kind |
| Cancel | hard status-flip | settle `Cancelled` + abort (no salvage) |
| Notifications | none from agent-core | supervisor = single emitter of `background_tool.*` |
| Live peek | broken (`progress_lines` never written) | **cut** (bare `running`); fields removed |
| Kinds | `Agent / Subagent / Workflow` | `Subagent / Workflow` |
| Port impl class | `SharedSubagentSupervisor` (stub) | `BackgroundSupervisorHandle` (real, holds handles+sink) |
| Dead dispatch path | `dispatch.rs`/`policy.rs` present, unused | deleted; `enable_background_tasks` writes removed |
| Hook location | `hooks.rs` (monolith) | `hooks/require_no_inflight_background_tasks.rs` (drain) |

**Open item:** §3e command-session count topology — supervisor-local (drop daemon call) vs daemon-authoritative backstop.

---

## 1. Root cause: one structural omission, eight visible symptoms

The Rust port replicated the *shape* of the subagent surface — the three tools
(`subagent.rs`), the typed handle (`StartedSubagent`), the precedence latch and
typed id prefixes (`supervisor.rs`) — but **never wired the one thing that makes
it work: a driver that launches the child agent run and settles the record when
it finishes.** Python has that driver; Rust does not.

Python's mechanism (ground truth):

- `run_subagent` (`backend/src/tools/subagent/run_subagent/run_subagent.py:217`)
  `await run_ephemeral_agent(parent_cfg, role_text=build_explorer_launch_prompt(),
  agent_def=sub_def, persist_agent_run=False, initial_messages=[user(prompt)],
  on_agent_spawned=_on_spawned)`, driven as a real coroutine by
  `dispatch.py → task_supervisor.launch():310` (`asyncio.create_task(coro)`).
- `launch()` adds a `_done_callback` (`task_supervisor.py:329-382`) that settles the
  record to a terminal status via the precedence latch; on terminal-tool success it
  forwards `terminal.output/is_error/metadata` + `subagent_terminal_called=True`
  (`run_subagent.py:246-251`); crash/no-terminal → `is_error`,
  `subagent_terminal_called=False` (`:231-245`).
- **The terminal gate never counts a running subagent.** `run_subagent` declares no
  `context_requirements`, so `uses_sandbox=False`; `count_by_agent`
  (`task_supervisor.py:439-457`) counts only `_running_sandbox_task` (which requires
  `uses_sandbox` *and* a matching `agent_id`) plus command sessions + outstanding
  workflows. A live subagent is excluded by construction.

Rust HEAD deviates at exactly the driver seam, and the deviations compound:

- **(R1) No driver.** `BackgroundSupervisorHandle::spawn` (`supervisor.rs:253-265`)
  only calls `register_running` and returns. `BackgroundTaskRecord`
  (`supervisor.rs:68-88`) has **no** task handle / future / `JoinHandle`. The real
  `run_ephemeral_agent` (`agent_loop.rs:54`) is reachable only by root
  (`root_agent.rs:74`) and delegated-workflow runs (`agent_runner.rs`), never by
  this port.
- **(R2) The phantom never settles.** `complete()` has only the in-module test
  caller (`supervisor.rs:332`); `push_progress()` has **zero** callers. A record
  minted `Running` (`supervisor.rs:153`) stays `Running` forever.
- **(R3) The count is unfiltered.** `background_inflight_count` (`supervisor.rs:301-303`)
  discards `_agent_id` and returns `inflight_count()` = every `Running` record,
  with no `uses_sandbox`/kind predicate.

Net effect (matches the report): a model that calls `run_subagent` once gets
`[SUBAGENT LAUNCHED] … status=running`; `check_subagent_progress` returns
`"Running: "` forever; the findings never arrive (D1/D2/D3); and the pinned
`Running` record (R2) is counted by the no-inflight terminal pre-hook (R3),
**permanently denying that agent's `submit_*_outcome` / `enter|exit_isolated_workspace`**
(D9 — active harm). No `TODO`/`stub`/`Phase` marker flags this, and
`ports.rs:202-204` *affirmatively but falsely* claims the implementor "validates
the agent … and supervises terminal-result delivery out of band."

---

## 2. Design decision: one central supervisor (eos-engine) calling a relocated `run_ephemeral_agent`

**Decision.** Keep the **single** `BackgroundTaskSupervisor` (`eos-engine`) as THE
center for every background kind — subagent, workflow, command session — and make it
the real `SubagentSupervisorPort` impl (on `BackgroundSupervisorHandle`). It owns
registration, validation, prompt assembly, the precedence latch, counts,
notifications, cancel/parent-exit, and settle. The capability it needs to run a child —
`run_ephemeral_agent` — is **relocated into `eos-engine`** (per the advisor lane) and
called **directly**, with the run handles threaded into the supervisor at `entry.rs`.
**No injected runner trait; no per-kind supervisor class; no runtime-side ledger.**

This is the same seam the rewritten advisor plan uses (`advisor-remediation-PLAN.md`
§2a): `run_ephemeral_agent` is just a wrapper over `eos-engine`'s `build_query_context`
+ `run_query`, so it belongs in `eos-engine`; `&AppState` is dropped for explicit
handles (`agent_run_store`, `model_store`, `llm_client`, `event_source_factory`,
`agent_registry`, `cwd`). One engine primitive then serves advisor (inline), subagent
(detached `tokio::spawn`), and workflow. The original port contract already assumed the
implementor lives in `eos-engine` (`ports.rs:202-204`); the stub simply abandoned it.

**What lives where** (answers "which module does the inline `run_ephemeral_agent`
dispatch + prompt build"):

| Responsibility | Crate · module |
|---|---|
| `run_subagent`/`check`/`cancel` tools (thin) | `eos-tools` `model_tools/subagent.rs` |
| `SubagentSupervisorPort` trait | `eos-tools` `ports.rs` |
| Validation + explorer prompt + initial-message split + spawn orchestration | `eos-engine` `background/subagent.rs` (NEW) |
| Ledger, counts (JSON), notifications, launch/settle/cancel, run handles | `eos-engine` `background/supervisor.rs` |
| `run_ephemeral_agent` (the shared loop primitive) | `eos-engine` `agent_loop.rs` (relocated from `eos-runtime` by the advisor lane) |
| Wiring: thread run handles + `NotificationSink` into the supervisor | `eos-runtime` `entry.rs` |

`eos-engine` already depends on `eos-tools` (`ExecutionMetadata`, `ToolResult`) and
`eos-agent-def` (`AgentRegistry`, `AgentType`); once `run_ephemeral_agent` is relocated
there too, the **entire** subagent flow (validate + prompt + register + run + settle) is
engine-resident. `eos-runtime` only constructs the supervisor with the handles from
`AppState` — it hosts no subagent execution.

**Naming.** The port-facing wrapper is renamed `SharedSubagentSupervisor →
BackgroundSupervisorHandle` — it is not subagent-specific (it owns all three kinds, holds
the run handles, and impls both `SubagentSupervisorPort` and
`CommandSessionSupervisorPort`). It stays in `eos-engine/src/background/supervisor.rs`,
next to the `BackgroundTaskSupervisor` state it wraps; the subagent-specific orchestration
lives in the new `background/subagent.rs`.

**Launch mode + dispatch (Q2/Q3).** Both `ask_advisor` and `run_subagent` call the one
relocated `run_ephemeral_agent`; they differ only in *how* they invoke it and *how* they
are dispatched:

| | `ask_advisor` | `run_subagent` |
|---|---|---|
| Dispatch | **interception** — a match arm in `eos-engine/.../tool_call/dispatch.rs::dispatch_assistant_tools` routes `AskAdvisor` → `advisor::run_advisor`, bypassing `execute_tool_once` (it needs the engine handles + live conversation the generic path lacks) | **generic tool path** — `execute_tool_once` → `RunSubagent::execute` → `SubagentSupervisorPort::spawn` → `BackgroundSupervisorHandle::spawn` (the supervisor already carries the handles, so no interception) |
| Mode | **blocking** — `run_advisor` `.await`s `run_ephemeral_agent`, returns the advisor terminal as the tool result | **detached / async** — `spawn` `tokio::spawn`s `run_ephemeral_agent`, returns `[SUBAGENT LAUNCHED]`; result via `check_subagent_progress` |
| Supervisor record | none (stateless one-shot) | tracked / settled / counted / drained |

The asymmetry is principled: interception for the stateless one-shot (advisor), port +
supervisor for the stateful tracked one (subagent).

**Reconciliation with the audit.** The report's D1 Fix says "replace the stub with a
real implementor in `eos-runtime`." This plan **refines** that: the implementor stays
in `eos-engine` (where `ports.rs` already promised it), and only the agent-execution
capability is injected from `eos-runtime`. Observable behavior is identical to the
report's fix; the seam is the one the port already declared. D7's dead dispatch path
is deleted, not wired.

---

## 3. The changes (all faithful ports; anchors are current `main`)

### 3a. Engine supervisor: own subagent task lifecycle (launch + settle) — backs D1/D2/D3

Extend `BackgroundTaskSupervisor` (`supervisor.rs`) so it drives a future the way
Python's `launch()` drives a coroutine, **without** breaking the record's
`Debug, Clone, PartialEq` derives (used by the existing tests):

- Add `pub agent_id: Option<String>` to `BackgroundTaskRecord` (`:68-88`) — the
  owner needed for the agent-scoped count (Python `BackgroundTaskRecord.agent_id`).
  `Option<String>` is `Clone + PartialEq`, so the derives stand.
- Add a side map `handles: HashMap<String, tokio::task::AbortHandle>` to
  `BackgroundTaskSupervisor` (`:107-116`) — **not** on the record (an `AbortHandle` is
  not `Clone`/`PartialEq`; the record stays cloneable). **Role: resource hygiene, not
  correctness.** What unwedges D9 is the cancelling side *settling* the record (it leaves
  `Running`, so the count drops); `abort()` merely stops a runaway child from burning
  compute. Abort-mid-run is fine precisely because the drain already settled the record.
- Extend `register_running`'s signature to take `agent_id` (no new variant), stamping it
  + the kind at mint time (`:126-161`).
- Add a `settle` method that mirrors Python's `_done_callback` precedence latch
  (`task_supervisor.py:329-382`): apply a terminal status **classified by
  `subagent_terminal_called`, not by `result.is_error`** — a subagent that called its
  terminal with `is_error=true` is still `Completed` (the error rides in the payload, and
  `check_subagent_progress` reports `finished`); only crash / no-terminal / exception
  settle to `Failed`. Keep the strict-`>` precedence guard (`:178`) so a cancel-vs-finish
  race resolves to `Completed`. **This is the single on-completion routine:** the spawned
  task builds the `ToolResult` from the run (terminal / crash / no-terminal, per
  `run_subagent.py:231-251`) and calls `settle` — there is no separate "forward terminal"
  step.
- Thread the **run handles** (`agent_registry`, `llm_client`, `model_store`,
  `agent_run_store`, `event_source_factory`, `cwd`) and an `Arc<dyn NotificationSink>`
  into the supervisor at `entry.rs`. The supervisor is the **single emitter** of
  `background_tool.{started,completed,failed,cancelled,delivered}` for ALL kinds
  (Q3 / D8) — the Rust port of Python's `_emit_background_tool`, which lives inside the
  supervisor (`task_supervisor.py:173-210`) and fires from launch/settle/cancel.
  Command-session completion routes through the supervisor's `settle` rather than the
  heartbeat emitting directly, so there is exactly one emission point.

After the advisor lane relocates `run_ephemeral_agent` into `eos-engine`, the supervisor
calls it **directly** with the threaded handles (same crate, no port): it `tokio::spawn`s
the run, stores/aborts the handle, settles the record, and fires the lifecycle
notifications.

### 3b. `eos-engine` subagent orchestration: validate, prompt, drive, forward — D1/D2/D3/D5

A new `eos-engine` module `background/subagent.rs` holds the subagent specifics, and
the `SubagentSupervisorPort::spawn` impl on `BackgroundSupervisorHandle` calls into it.
It uses the `AgentRegistry` (validation) + the run handles the supervisor holds, and
calls the relocated `eos_engine::run_ephemeral_agent(handles, …)` directly — no
`AppState`, no runner trait. (`run_ephemeral_agent` is moved into `eos-engine` by the
advisor lane, `advisor-remediation-PLAN.md` §2a; `agent_loop.rs:54` is its new home.)

**Caller-identity seam (the one cross-crate edit — `eos-tools`).** Widen
`SubagentSupervisorPort::spawn` (`ports.rs`) to
`spawn(agent_name, prompt, caller_agent_name, caller_agent_id)`; update its sole call
site `RunSubagent::execute` (`subagent.rs:100-104`) to pass `ctx.agent_name`
(`metadata.rs:44`) + `ctx.agent_id()` (`metadata.rs:110`); the in-crate
`FakeSubagentSupervisor` (`subagent.rs:229-263`) takes the same params.

`spawn(...)` (orchestrated in `background/subagent.rs`):

- **Validate (D2)** — port of `run_subagent.py:125-150` using the registry the
  supervisor holds: recursion (`registry.get(caller).agent_type == Subagent` → reject,
  `registry.rs:63`) and exists+is-subagent (`agent_name ∈ dispatchable_subagent_names()`,
  `registry.rs:75-79`, one check for both Python branches). Same error texts as Python.
  Runs **before** any record is minted. (The tool-schema enum is a soft hint only.)
- **Seed + split (D5)** — `role_text = build_explorer_launch_prompt()` (port the
  static `explorer_guidance.py` into `background/subagent.rs`) as the run prompt;
  `initial_messages = [Message::from_user_text(prompt)]`; stamp `agent_type=subagent`
  + role into the child `ExecutionMetadata`; no parent scope (`subagent.html:81`).
- **Register** — `register_running("run_subagent", input, Subagent,
  agent_id=caller_agent_id)` → `subagent_<n>` (`supervisor.rs:133-136`); the supervisor
  emits `background_tool.started` (3a notifications).
- **Drive directly (D1)** — `tokio::spawn` a task that calls
  `eos_engine::run_ephemeral_agent(handles, EphemeralRunInput{ agent: sub_def,
  initial_messages, tool_metadata, persist_agent_run: false })` (**no `on_event`** — see
  the live-peek cut below), then `settle`s the record (3a) with the run's outcome mapped
  to a `ToolResult` (terminal → `output/is_error/{…, subagent_terminal_called:true}`;
  crash/no-terminal → error text + `subagent_terminal_called:false`;
  `run_subagent.py:231-251`). Store the `AbortHandle` for cancel (3d). The child run is
  **not** handed the parent's `NotificationService`; `background_tool.*` fires from the
  supervisor's launch/settle (3a), not the child's events — this preserves isolation (Q3).
- **Live peek — cut from v1 (scope cut, documented gap).** Python shows the child's
  last-N messages while running (`_on_spawned` + `format_last_n_messages`,
  `run_subagent.py:56-83,190-204`); Rust's `run_ephemeral_agent` exposes no live
  `agent.messages` handle, so reproducing it means an `on_event` buffer + lock. Not worth
  it for v1: `check_subagent_progress` returns bare `running` while running and the
  terminal result when finished. This removes the `progress_lines` field, `push_progress`
  (already zero callers — dead code), the `format_last_n_messages` port, and the
  `on_event` plumbing. Add later if the mid-run view is wanted.

Return `StartedSubagent { subagent_session_id }`; the `[SUBAGENT LAUNCHED]` ack
(`subagent.rs:63-77`, already parity-good per E1) is unchanged.

### 3c. `progress` / `cancel` taxonomy + payload — D3 (and E5/E6)

`progress` (replace `supervisor.rs:267-287`'s debug string) must reproduce
`control.py::_subagent_status_and_result` (`control.py:64-89`) + the JSON payload
(`control.py:136-146`):

- Map record status → `running` (while `Running`) / `finished` (`Completed` +
  `subagent_terminal_called`) / `failed` / `cancelled` / `terminated`
  (`control.py:64-89`), reading the terminal `result.output` when settled. While running
  it returns bare `running` (no message tail — live peek is cut, 3b).
- Return the `json.dumps(payload, indent=2)` shape `{subagent_session_id, status,
  agent_name, result}` (`control.py:136-146`). **No `Delivered`/`mark_subagent_delivered`
  for subagents** — Python's `collect_completed` intentionally skips `SUBAGENT_TASK_TYPE`
  (`task_supervisor.py:400-404`), so nothing re-delivers them and the `COMPLETED→DELIVERED`
  transition is cosmetic.
- **E5 fix:** a missing session returns `is_error=true` (`control.py:117-122`), not the
  current non-error `ToolResult::ok` (`subagent.rs:145` + `supervisor.rs:273-278`).

`cancel` (D4 + **E6**): see 3d; an unknown-session cancel must return `is_error=true`
(Python `control.py:172-180`), not the current non-error ok.

### 3d. Cancel = settle `Cancelled` + abort the handle — D4

`cancel(id)` (replace the hard status-flip at `supervisor.rs:186-198`): **settle** the
record to `Cancelled` via the precedence latch, then `abort()` the stored handle (3a) to
stop a runaway child. `terminate_for_parent_exit` (`:201-212`) and the terminal drain
(3e) do the same per task. The unwedge comes from the *settle* (the record leaves
`Running`); the abort is resource hygiene.

Cooperative early-stop **salvage** (Python `_request_subagent_early_stop`'s
`stop_mode=early_stop` + `asyncio.sleep(0)` to rescue a partial terminal,
`task_supervisor.py:222-235`) is **cut from v1**: it needs the child loop to observe a
cancellation token, which `run_query` does not expose. Settle `Cancelled` with whatever
the record holds; leave `StopMode::EarlyStop` unused (optionally drop the variant).
Documented gap.

E6: an unknown-session cancel returns `is_error=true` (`control.py:172-180`).

### 3e. The terminal prehook drains all three kinds to 0 — D6 / D9 (Q1 + Q2)

The terminal enforcement is a **single prehook** (`RequireNoInflightBackgroundTasks`,
`meta.rs:55-81`; grep confirms nothing else gates the terminal — `background_inflight_count`
has no other reader). **Relocate the hook body** out of the `hooks.rs` monolith into
`eos-tools/src/hooks/require_no_inflight_background_tasks.rs` — its own file in the
`hooks/` module, mirroring the already-extracted `hooks/advisor_approval.rs` (advisor lane)
and Python's `tools/_hooks/require_no_inflight_background_tasks.py`. `hooks.rs` keeps the
`Hook` enum + dispatcher, which call into the module. The behavior changes from
*deny-if-count>0* to **drain-to-0**:

- **Count surface → JSON report (Q1), Running-only.** The supervisor exposes
  `BackgroundInflightReport { total, subagent, workflow, command_session }`, scoped by
  `agent_id`, serialized to JSON for audit/diagnostics and the post-drain assertion.
  **Each field counts `Running` records of that kind.** This is the load-bearing
  invariant: settling does not remove a record (Running→terminal stays in the map), so
  the drain reaches `total == 0` *only* because the count ignores settled records — and it
  makes the cancel-vs-complete race harmless (either outcome is terminal, not Running).
- **Drain, don't deny (Q2).** On `submit_*_outcome` the prehook reads
  `ctx.subagent_supervisor` (already attached, `metadata.rs:78`) and **cancels +
  settles all three kinds for this agent**, then asserts `report.total == 0` and passes.
  In-process kinds (subagent, workflow) drain via a supervisor `cancel_by_agent` /
  `terminate_for_parent_exit` (port Python `task_supervisor.cancel_by_agent`, `:691`,
  with a grace window); command sessions drain via the daemon cancel op. The `agent_id`
  field (3a) scopes it; the wiring (`meta.rs:55-81`) is unchanged.
- **Uniform invariant (Q2).** All three kinds = 0 at submission. No "exclude subagent
  from the count" — we drain everything, which also settles a stuck/phantom subagent, so
  **D9 is closed structurally by the drain** (independent of D1's real settle, which
  remains the correct steady-state path).
- **Scope.** `enter_isolated_workspace` keeps **reject** semantics per the architecture
  ("enter rejects active sandbox-bound work"); only the four `submit_*_outcome` and
  `exit_isolated_workspace` drain.
- **Divergence from Python (stated, not hidden).** Python *denies-and-retries* and never
  blocks the parent on subagents (`uses_sandbox=False`). Drain instead cancels in-flight
  subagent/workflow work at terminal time — acceptable because the agent chose to
  terminate (it matches `terminate_for_parent_exit` on exit), but it can drop an
  unretrieved subagent result, so an agent that wants the findings must
  `check_subagent_progress` before submitting. This is a deliberate EOS choice (uniform
  "all kinds 0 at terminal"), recorded so a reviewer doesn't read it as a port miss.

### 3f. Delete the dead dispatch path — D7

Remove `eos-engine/.../background/dispatch.rs` and `policy.rs` and their `mod.rs`
re-exports (`mod.rs:5,7,11,13` → `launch_background_tool`,
`is_engine_background_tool`, `needs_background_manager`) — all unreferenced outside
the re-export (grep-confirmed; verify doc D7). Stop setting `enable_background_tasks`
in the 5 set-but-never-read sites (`loop_.rs:282`, `notifications/mod.rs:262`,
`streaming.rs:64`, `tool_call/dispatch.rs:425`, `agent/factory.rs:138`) and drop the
field if nothing else reads it (only the `Debug` impl does, per verify doc). The
supervisor's direct call to the relocated `run_ephemeral_agent` (3b) is the live launch
path; the dead module is not resurrected.

Deleting `dispatch.rs` removes the **only** producer of `BackgroundTaskKind::Agent`
/ `bg_<n>` (`dispatch.rs:16`). `Agent` is a test-only alias in Python too
(`next_alias() → "bg_{n}"`, "for internal supervisor tests"); real command sessions
use daemon-minted `cmd_<n>`, not `bg_<n>`. So also remove the `Agent` variant from
`BackgroundTaskKind` (`supervisor.rs:46-54`), the `counter` field and its match arm
(`:108, 141-144`), and update the `background_ids_use_typed_prefixes` test
(`:366-392`) to assert only `subagent_<n>` / `wf_<n>`. The supervisor's record kinds
reduce to exactly the production set — `Subagent` + `Workflow` — alongside the
`command_sessions` map.

---

## 4. What stays exactly as-is (do not change)

- **The typed-id minting and precedence latch.** `subagent_<n>`/`wf_<n>`
  (`supervisor.rs:128-145`) and `precedence()` RUNNING=0…DELIVERED=4 with strict `>`
  (`:30-38, 178`) are confirmed-correct in isolation (E2/E3); reuse them. (The
  `Agent`/`bg_<n>` kind is **removed**, not reused — see 3f; it has no production
  producer once the dead dispatch path is deleted.)
- **The three tools' input validation.** Blank `agent_name`/`prompt`
  (`subagent.rs:90-99`), `last_n_messages ∈ 1..=10` default 5 (`:49-51, 135`),
  empty-session-id errors (`:110-116`) all match Python (E4); unchanged. The new
  validation in 3b is the *engine-side* recursion/exists/is-subagent gate, additive.
- **The `[SUBAGENT LAUNCHED]` ack** (`subagent.rs:63-77`) — parity-good (E1).
- **The single shared supervisor instance.** `entry.rs:120-141` mints one
  `BackgroundSupervisorHandle` serving the subagent port, the command-session port, and
  the heartbeat (`supervisor.inner()`). It stays the one ledger **and** the one
  `SubagentSupervisorPort` impl; `entry.rs` additionally threads the **run handles**
  (from `AppState`) and the `NotificationSink` into it (§2, 3a). The spawned task settles
  the same records the hook counts (see §6).
- **The no-inflight hook ordering and wiring** (`meta.rs:55-84`) — unchanged; 3e only
  changes what the count returns.

---

## 5. Verification (success criteria)

- **End-to-end:** a root run calls `run_subagent("explorer", …)`; a real child agent
  runs, calls `submit_exploration_result`, and `check_subagent_progress` returns
  `status:"finished"` with the terminal output — no `#[cfg(test)]` fake supervisor.
- **Unwedge / drain (the D9 regression test):** after `run_subagent`, calling
  `submit_root_outcome` drains the subagent (cancel + settle) and proceeds; the
  post-drain `report.total == 0`. A phantom or in-flight subagent no longer wedges the
  terminal — the drain settles it. Proves Q2 (drain) + D1 (settle).
- **Validation (D2):** recursion (a subagent calling `run_subagent`), unknown agent,
  and non-subagent agent each return the Python error text and mint **no** record.
- **Result forwarding (D3):** terminal-called-with-`is_error=true` reports `finished`
  (not `failed`); crash → `failed` + `subagent_terminal_called:false`; no-terminal →
  `failed` + `subagent_terminal_called:false`. Missing-session and unknown-cancel
  return `is_error=true` (E5/E6).
- **Cancel (D4):** a cancel settles the record `Cancelled` and aborts the handle (no
  salvage); the inflight report drops because the record left `Running`. Parent-exit and
  the terminal drain do the same.
- **Audit (D8):** `background_tool.started/completed/failed/cancelled` fire from
  agent-core for `task_kind=subagent`.
- **Dead-code (D7):** `dispatch.rs`/`policy.rs` removed; no `enable_background_tasks`
  writes remain; build is green.
- Port the intent of the Python subagent tests under
  `backend/src/test_runner/tests/.../subagent*` for the taxonomy + unwedge paths.

---

## 6. Coordination / sequencing

- **Cross-lane dependency (the relocation):** this lane depends on the advisor lane
  relocating `run_ephemeral_agent` from `eos-runtime` into `eos-engine`
  (`advisor-remediation-PLAN.md` §2a step 1: drop `&AppState`, take explicit handles;
  update `root_agent.rs`/`agent_runner.rs` call sites). The relocation is **owned by the
  advisor lane**; sequence it first, then this lane calls `eos_engine::run_ephemeral_agent`
  directly. If the lanes run in parallel, share one relocation branch — do not relocate
  it twice.
- **Load-bearing invariant (state it in code + a test):** the spawned subagent task
  settles the **same** `BackgroundTaskSupervisor` that backs the inflight report. There
  is one supervisor instance (`entry.rs`) holding the run handles, not a separate store.
  If a refactor ever gives subagent execution its own record store, the settle becomes
  invisible to the terminal-drain prehook and D9 re-wedges. The §5 regression test
  guards this.
- **Concurrent refactor:** `notifications.rs → notifications/mod.rs` + `notifications/rules/`
  is under active edit (git status); 3f touches the `enable_background_tasks` write at
  `notifications/mod.rs:262`. Rebase onto that refactor; do not stomp `rules/`.
- **Dependency order:** 3a (ledger launch/settle + `agent_id`) → 3b (orchestration) →
  3c (taxonomy) in one lane; 3e (terminal drain + JSON report) can land with 3a and
  **independently unwedges D9 even before 3b** (the drain settles any stuck record) —
  land 3e early to stop the active harm. 3d, 3f follow.
- This is the Phase-1 hard-gate `subagent ⊕ query_engine` lane in `REPORT.md`
  §"Rollout at a glance"; it parallels the `advisor`, `attempt_harness`, and
  `request_completion` lanes and must land before `backend/src` deletion.

---

## 7. Alternatives considered (rejected)

- **A per-kind runtime supervisor class (`RuntimeSubagentSupervisor`).** An earlier
  draft put the `SubagentSupervisorPort` impl in `eos-runtime` as a new class wrapping
  `supervisor.inner()` + `AppState`. Rejected: it is a second, kind-specific supervisor
  — exactly what "one central supervisor" forbids — and it pulls validation, prompt, and
  notification emission up into runtime. Superseded by §2 (engine-resident supervisor
  calling the relocated `run_ephemeral_agent`).
- **An injected `EphemeralAgentRunner` trait (run stays in `eos-runtime`).** An interim
  draft kept `run_ephemeral_agent` in runtime and injected a kind-agnostic runner trait
  into the engine supervisor. Rejected once the advisor lane relocated
  `run_ephemeral_agent` into `eos-engine`: with the run primitive in the same crate the
  supervisor calls it directly and the trait is pure ceremony. The advisor plan rejects
  the symmetric "generic ephemeral-runner port in `ExecutionMetadata`" for the same
  reason. (This was the option the runner-seam question first proposed; superseded.)
- **Wire the dead `dispatch.rs`/`policy.rs` path (D7's other branch).** It mirrors
  Python's `dispatch.py → launch_background_tool → background_tasks.launch(coro)`
  literally, but it is unreferenced scaffolding that adds an indirection layer for
  identical behavior. Rejected: deleted, and the supervisor calls the relocated
  `run_ephemeral_agent` directly.
- **Relocate the whole ledger (record store + count) into `eos-runtime`.** Violates
  constraint #1 and would split the in-flight count away from the command-session /
  workflow records on the engine supervisor, breaking the terminal-drain prehook and the
  heartbeat that both read `supervisor.inner()`. Rejected outright.
