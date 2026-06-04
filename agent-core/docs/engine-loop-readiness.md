# Engine-Loop Readiness Report

Scope: answers "what is missing for the engine loop? are advisor agent, workflow
agents, root agent, subagents all ready?" for the `agent-core` Rust runtime
(Phase-6 / engine-only phase). Synthesized from adversarial claim verdicts (C1–C5,
all HOLDS, high confidence) and a completeness-critic pass, with anchors
re-verified against source. File:line anchors are relative to
`agent-core/crates/`.

Verdict polarity note: C1 is a readiness claim — HOLDS means **root is ready**.
C2–C5 are gap claims — HOLDS means **the gap is real** (the agent type is
*not* fully ready). Read the matrix accordingly. All five verdicts are firm
HOLDS; the nuance to preserve is the gap *mechanics*, not verdict uncertainty.

> **UPDATE (supersedes the "root is not advisor-gated" claim below).** After this
> report was generated, `submit_root_outcome` was **intentionally advisor-gated**
> (`eos-tools/src/meta.rs:68` now lists `Hook::AdvisorApproval` alongside
> `RequireNoBackgroundSessions`) — a deliberate divergence from the Python
> backend, which gates only the planner/generator/reducer main-role terminals.
> Consequence: root is **engine-loop ready but no longer completable under the
> always-denying `AdvisorService` stub** — exactly like the workflow agents, root
> now requires a real `AdvisorPort` (gap 2). The success path is proven only with
> an injected approving advisor (`successful_root_keeps_engine_terminal`); the
> stub deny path is covered by `root_terminal_blocked_without_advisor_approval`
> (`eos-runtime/src/tests.rs`). Read Sections 1–3's "root" claims with this
> reversal in mind.

## 1. Headline verdict per agent type

- **Root agent — READY.** Runs the real engine loop and persists a typed
  terminal. Decisive reason: `submit_root_outcome` is **not** advisor-gated
  (`eos-tools/src/meta.rs:68` listed only `Hook::RequireNoBackgroundSessions` in
  the historical snapshot),
  so the always-denying advisor stub cannot block it. The test
  `successful_root_keeps_engine_terminal` passes against the real stub and asserts
  `TaskStatus::Done` with `fail_reason=None` (`eos-runtime/src/tests.rs:164-186`).
- **Workflow agents (planner/generator/reducer) — NOT READY.** They run the
  engine loop, but every terminal they could submit is blocked. Decisive reason:
  three independent blockers compound (advisor-gate denial fires first, then
  `plan_submission=None`, then the runner discards terminals). Net result today is
  an unconditional `no_terminal` report.
- **Advisor agent — NOT WIRED.** It is a stub, not an agent. Decisive reason:
  `AdvisorService::approval_status` unconditionally returns
  `{approved:false, reason:"missing"}` (`eos-engine/src/notifications.rs:226-231`);
  no code spawns a real advisor runner around the engine loop.
- **Subagents — NOT WIRED (track-only).** Decisive reason: `spawn` only calls
  `register_running`, minting an in-memory record; nothing ever executes the
  subagent prompt or populates progress
  (`eos-engine/src/background/supervisor.rs:253-265`, `122-157`).

## 2. Readiness matrix

| Agent type | Runs engine loop? | Produces usable result? | Blockers | Key anchor |
|---|---|---|---|---|
| Root | Yes | Yes (typed terminal persisted, `TaskStatus::Done`) | None | `eos-runtime/src/root_agent.rs:73-85`; terminal not advisor-gated `eos-tools/src/meta.rs:68` |
| Workflow (planner/generator/reducer) | Yes (`RuntimeAgentRunner::run` → `run_ephemeral_agent`) | No — always `no_terminal` | (1) advisor hook denies terminal; (2) `plan_submission=None`; (3) runner discards terminal | `eos-runtime/src/agent_runner.rs:53-105`; `eos-tools/src/meta.rs:69-77`; `eos-runtime/src/tool_context.rs:81` |
| Advisor | No (never spawned) | No — stub returns "not wired" / `approved:false` | No real advisor runner exists | `eos-engine/src/notifications.rs:207-231`; `eos-runtime/src/app_state.rs:473` |
| Subagents | No (register + ack only) | No — empty progress, never executed | No executor/poller; `progress`/`complete` never called in prod | `eos-engine/src/background/supervisor.rs:253-265, 122-157, 220-224, 167-179` |

## 3. The crux

**Root is genuinely ready because its terminal is not advisor-gated.**
`tool_hooks(SubmitRootOutcome)` returns only
`Hook::RequireNoBackgroundSessions` — no `Hook::AdvisorApproval`
(`eos-tools/src/meta.rs:68`). Hooks run before the tool body and a `Deny`
short-circuits (`eos-tools/src/execution.rs:48-62`), but since the advisor hook is
absent from root's chain, the always-denying stub never sees root's terminal. The
root path also reads the real terminal: `run_ephemeral_agent` returns
`run.terminal_result`, and `root_agent.rs` only synthesizes failure when
`run.error.is_some() || run.terminal_result.is_none()`
(`eos-runtime/src/root_agent.rs:73-85`). The empirical test confirms this with no
approving mock injected.

**Workflow terminals are blocked by three independent gates that compound — and
they do not unblock one at a time.** Order matters, because the verdicts describe
different proximate causes for the *same* tool call:

1. **Advisor-gate denial fires first (the cause active today).** All three
   workflow terminals carry `Hook::AdvisorApproval` in their pre-hook chain
   (`eos-tools/src/meta.rs:69-77`, verified). Hooks run before the tool body
   (`execution.rs:48-62`); `run_advisor_approval` denies when the advisor is
   missing or returns `approved:false` (`eos-tools/src/hooks.rs:574-597`), and the
   stub always returns `approved:false, reason:"missing"`. So the terminal is
   denied at the hook, with `policy="advisor_approval"`, before any body code runs.
2. **`plan_submission=None` is a latent *second* blocker.** `build_metadata`
   hardcodes `plan_submission: None` (`eos-runtime/src/tool_context.rs:81`) and
   `RuntimeAgentRunner` passes `workflow_control: None`
   (`eos-runtime/src/agent_runner.rs:71`). The submission tool *bodies* call
   `require_plan_submission()`, which errors `MissingPort`
   (`eos-tools/src/metadata.rs:180-184`). But this body check is **never reached
   today** — the advisor hook already denied. It only surfaces *after* the advisor
   is fixed. This is exactly the "blocked by the stub even after the Phase-7
   typed-terminal fix" framing: the advisor gate is independent of, and survives,
   the `plan_submission` fix.
3. **The runner discards the terminal unconditionally (refinement found beyond the
   verdicts).** `RuntimeAgentRunner::run` returns `AgentRunReport::no_terminal(...)`
   without ever inspecting `run.terminal_result`
   (`eos-runtime/src/agent_runner.rs:100-104`). This is latent today (the terminal
   is `None` anyway because of #1), but it means even a successfully-submitted
   workflow terminal would be thrown away. Capturing typed terminals requires
   reading `run.terminal_result` here.

Consequence: **fixing fewer than all three does not unblock workflow agents.** A
real advisor runner alone leaves `plan_submission=None` (body error); wiring
`plan_submission` alone is shadowed by the advisor denial; both together still hit
the runner that drops the terminal. The orchestrator then routes the
`no_terminal` report to `synthesize_planner_failure`, which closes the planner
task and attempt **without mutating the parent task**
(`eos-workflow/src/attempt/orchestrator.rs:152, 456-467`).

## 4. Gaps to close for full engine-loop readiness (ordered)

Severity legend: BLOCKER (prevents any production run) > MAJOR (a whole capability
non-functional) > MINOR (degraded / spec-surface drift).

1. **BLOCKER — Real LLM client + event source for production.** Without
   `ANTHROPIC_API_KEY`/`OPENAI_API_KEY`, `default_llm_client` returns
   `UnconfiguredLlmClient`, which errors on the first turn; with
   `event_source_factory=None` and an unconfigured client, event-source resolution
   fails hard (`EngineError::MissingEventSource`)
   (`eos-runtime/src/app_state.rs:87-100, 35-42`; `eos-engine` query loop
   `MissingEventSource`). This is a correct fail-fast seam, but a real provider must
   be configured for any agent (including root) to run. Listed first because it
   gates everything; it is config, not missing code.
2. **MAJOR — No real advisor runner.** `AdvisorService` is a stub that always
   denies (`eos-engine/src/notifications.rs:207-231`), wired into every agent's
   metadata (`eos-runtime/src/app_state.rs:473`, `tool_context.rs:83`). Blocks all
   advisor-gated terminals (generator/reducer/planner) and the `ask_advisor` tool.
   Needs an eos-runtime helper that runs an advisor `AgentDefinition` through the
   engine loop and returns a real `approval_status`.
3. **MAJOR — `plan_submission` port unwired for workflow agents.** Hardcoded
   `None` (`eos-runtime/src/tool_context.rs:81`). Until a `PlanSubmissionPort`
   adapter is injected for delegated agents, submission tool bodies error
   `MissingPort` (`eos-tools/src/metadata.rs:180-184`). Documented Phase-7 gate.
4. **MAJOR — `RuntimeAgentRunner` discards typed terminals.** Returns
   `no_terminal` unconditionally without reading `run.terminal_result`
   (`eos-runtime/src/agent_runner.rs:100-104`). Even with #2 and #3 fixed,
   terminals are dropped. Must capture and map `run.terminal_result` to a typed
   `AgentRunReport`.
5. **MAJOR — Subagent execution: register-only, no executor.** `spawn` →
   `register_running` mints an in-memory record; nothing runs the prompt, polls,
   or populates `progress_lines`; `progress`/`complete` are never called in prod
   (`eos-engine/src/background/supervisor.rs:253-265, 122-157, 220-224, 167-179`;
   `background/dispatch.rs:11-23`). Needs a background runner that executes
   subagent prompts through the engine loop and reports progress.
6. **MAJOR — `workflow_control=None` for workflow agents.** Blocks nested
   delegation; workflow agents can only run in isolation
   (`eos-runtime/src/agent_runner.rs:71`; `tool_context.rs:22-23`). Documented
   Phase-7 deferral.
7. **MAJOR — `isolated_workspace=None`.** No `IsolatedWorkspacePort` wired
   (`eos-runtime/src/tool_context.rs:84`); tools cannot create ephemeral isolated
   sandboxes — all execution shares the request's single binding.
8. **MINOR — Background task supervisor has no async execution/polling** beyond
   immediate ack (`eos-engine/src/background/supervisor.rs:114-225`;
   `tool_call/dispatch.rs:11-23`). Shares root cause with gap 5; tracked
   separately because it also affects `launch_background_tool`.
9. **MINOR — No prompt-report recorder instantiated.** `QueryContext.prompt_report`
   is `Option` but `agent_loop` never builds a recorder
   (`eos-engine/src/query/context.rs:79`); no prompt audit trail in production.
10. **MINOR — No production model-selection logic.** `agent.model` falls back to
    the literal `"default-model"` when no registry is seeded
    (`eos-runtime/src/agent_loop.rs:74-84`).
11. **MINOR — Notifications are in-memory only** (`Arc<Mutex<VecDeque>>`), lost if
    not drained; no durable/external delivery
    (`eos-engine/src/notifications.rs:178-205`).
12. **MINOR (spec/API-surface drift — corrected down from the critic's
    "BLOCKER").** The critic flagged "`sandbox_provisioning.rs` missing entirely"
    and "`RequestSandboxBinding`/`RequestSandboxProvisioner` not exposed." This is
    **not** a functional blocker and does **not** affect root readiness:
    - Both types are fully implemented in
      `eos-sandbox-host/src/provisioning.rs:22, 47` (a parallel agent relocated
      provisioning upstream — see `eos-runtime/src/lib.rs:48-50`).
    - `eos-runtime` **does** re-export `RequestSandboxBinding`
      (`eos-runtime/src/lib.rs:50`); it does **not** re-export
      `RequestSandboxProvisioner`.
    - Provisioning works at runtime via the injected `RequestProvisioner` trait:
      `entry.rs` calls `state.provisioner.prepare_for_run(...)`
      (`eos-runtime/src/entry.rs:104-108`; trait at
      `eos-runtime/src/app_state.rs:56-81`).
    The residual is documentation/file-layout drift versus the migration spec and
    a missing `RequestSandboxProvisioner` re-export — a MINOR public-surface
    cleanup. Removing the only blocker-severity item is consistent with
    **root = READY**.

## 5. Already correct (not gaps)

Verified-intentional design choices and false alarms — do not "fix" these:

- **No root workflow; root runs the engine loop directly.** `start_request` mints
  one `Task(role=Root, workflow_id=None)` and `run_root_agent` calls
  `run_ephemeral_agent` directly (`eos-runtime/src/root_agent.rs:73-85`;
  `lib.rs:7-12`). Matches the architecture invariant.
- **No parent-task mutation at workflow close.** `apply_planner_failure` mutates
  only the planner task and closes the attempt
  (`eos-workflow/src/attempt/orchestrator.rs:456-467`); `lib.rs:14-18` forbids
  parent mutation at close.
- **`fail_unfinished_root` uses an atomic compare-and-set** keyed on
  `TaskStatus::Running` and is a no-op when the task is already terminal
  (`eos-runtime/src/root_agent.rs:114-133`) — no double-finalize race.
- **Phase-6 `None` ports are intentional deferrals, not bugs**
  (`plan_submission`, `isolated_workspace`, `workflow_control` for non-root). They
  *are* still capability gaps for full readiness (gaps 3/6/7 above), but they are
  documented and correct for Phase-6.
- **`UnconfiguredLlmClient` and `event_source_factory=None` are fail-fast seams**,
  not bugs (`eos-runtime/src/app_state.rs:37, 97`); tests inject a factory,
  production must supply a provider (gap 1).
- **Background supervisor as track-and-ack is correct for Phase-6** — real
  execution is a deferred background-runner component (gaps 5/8).
- **`no_terminal` synthesis for workflow agents is the correct Phase-6 behavior**
  given the unwired ports; the work is wiring the ports (gaps 2/3/4), not changing
  the synthesis.
