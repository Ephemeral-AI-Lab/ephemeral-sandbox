# EOS Agent Core Rust to TypeScript Migration

Status: Proposed
Date: 2026-06-10
Owner: eos-agent-core
Migration direction: Rust -> TypeScript
Project path: `/Users/yifanxu/machine_learning/LoVC/EphemeralOS/eos-agent-core`

This directory is the coordination point for specs that migrate the current Rust
`agent-core/` implementation into the TypeScript project at
`/Users/yifanxu/machine_learning/LoVC/EphemeralOS/eos-agent-core`.

The declaration here is intentionally narrow:

- It creates a home for migration specs.
- It declares `eos-agent-core/` as the TypeScript project path.
- It declares TypeScript as the target implementation language.
- It records the initial recommended third-party stack for the TypeScript
  target.
- It does not change the live Rust implementation, Cargo workspace, tests, or
  runtime behavior.

## Recommended Target Stack

| Concern | Recommendation | Adoption rule |
| --- | --- | --- |
| Workspace layout | `pnpm` workspaces | Baseline for TypeScript packages under the future migration layout. |
| Runtime and language | Node.js TypeScript with strict compiler settings | Baseline for new TypeScript implementation phases. |
| Durable state | `better-sqlite3` + `Kysely` | Baseline for local SQL persistence, migrations, and typed query access. |
| Runtime validation | Zod 4 | Baseline for request DTOs, provider payloads, tool inputs, persisted JSON, and compatibility checks. |
| Test runner | Vitest | Baseline for TypeScript unit and integration tests; authoring rules in Test Authoring Baseline below. |
| Observability | OpenTelemetry JavaScript packages | Baseline for run, attempt, provider, tool, and sandbox-host spans/events. |
| Structured logs | `pino` or the repo-selected structured logging wrapper | Phase-gated until the TypeScript runtime package is introduced. |
| Bounded concurrency | `p-queue` or a small crate-local limiter | Optional; use only for explicit request admission, provider, tool, or agent-run concurrency limits. |

Runtime primitives such as `AbortController` and `AsyncLocalStorage` should be
treated as first-class lifecycle tools even though they are not third-party
libraries. They should back cancellation trees, per-request context, and active
run supervision.

Do not introduce BullMQ, Temporal, Redux, Zustand, XState, Prisma, Drizzle, or
Redis as Phase 00 defaults. They require a later phase spec with a concrete
distributed-worker, UI-state, workflow-orchestration, or database-backend reason.

## Test Authoring Baseline

Applies to every Vitest suite under `eos-agent-core/packages/*`:

- One `describe` block per unit under test, with `it` titles written as
  behavior sentences (for example "retries only before visible output").
  Use `it.each` with templated titles for case tables.
- Use the optional `expect(value, message)` second argument when a single test
  asserts multiple scenarios, repeated counters, or timing bounds, so a failure
  names the step that broke. Single-scenario assertions rely on Vitest's
  default diff output.
- Vitest's annotation API (`context.annotate`) stays unused until a reporter
  that surfaces annotations is configured.

## Spec Index

| Spec | Status | Boundary | Verification |
| --- | --- | --- | --- |
| `phase-00-migration-declaration_SPEC.md` | Proposed | Planning declaration only; no live implementation boundary changes. | Spec records the migration identity, target stack, non-goals, and initial acceptance criteria. |
| `phase-01-project-setup_SPEC.md` | Completed | Setup-only TypeScript metadata under `eos-agent-core/`; Rust `agent-core/` remains behaviorally unchanged. | `CI=true pnpm install --frozen-lockfile`, `pnpm list --depth 0`, `pnpm exec tsc --showConfig`, `pnpm exec vitest --version`, and repo `git diff --check` hygiene. |
| `phase-02-llm-client_SPEC.md` | Completed | Additive TypeScript implementation: `@eos/contracts` minimum message DTOs and `@eos/llm-client` with two SDK-backed clients (Anthropic Messages via `@anthropic-ai/sdk`, OpenAI Responses via `openai`) behind one normalized event union and retry gate; Rust `agent-core/` remains live and unchanged. | `pnpm run check` in `eos-agent-core/`, including golden decode tests replaying both providers' copied SSE fixtures through injected-fetch SDK clients; `git diff --stat -- agent-core` stays empty. |
| `phase-02.5-provider-composition_SPEC.md` | Completed | Restructure within `eos-agent-core/packages/llm-client` only: recompose the two vendor-named clients into wire codecs (`wires/`) x credential schemes (`access/`) behind one generic `LlmStreamClient` and a `profiles.ts` registry (`createLlmClient`); adds `claude_coding_plan`/`codex_coding_plan` profiles and a live e2e harness (`pnpm run test:e2e`) whose codex suite auto-loads `~/.codex/auth.json`; copilot access and openai-chat wire are named seams; Rust `agent-core/` remains live and unchanged. | `pnpm run check` (unit, no network; 105 llm-client tests incl. the shared client-contract kit with golden `exact` bindings) green with all Phase 02 assertions surviving as moves; `pnpm run test:e2e` live codex battery (6 contract scenarios) green on this machine and clean-skip with `CODEX_AUTH_PATH=/nonexistent`; `git diff --stat -- agent-core` stays empty. |
| `phase-03-agent-loop-engine_SPEC.md` | Completed | Additive TypeScript implementation: `@eos/engine` agent-loop spine (thin while loop, dual transcript, event stream, interrupt/steer, non-tool-use termination); Rust `agent-core/` remains live and unchanged. | `pnpm run check` in `eos-agent-core/` (122 tests), including the scripted `MockLlmClient` loop suite (termination, steering, interrupt, tool batches); `git diff --stat -- agent-core` stays empty. |
| `phase-04-tool-framework_SPEC.md` | Completed | Additive `@eos/tool` package (flat Zod tool contract with fail-closed `defineTool` defaults, per-call pipeline, relocated batch executor with terminal-solo policy, external-only hooks with command + callback adapters, `AGENT_TOOLSET` assembly, and the two service-free families — submission and background — with per-family service injection; sandbox/agent/workflow families deferred with their services per decision 21) plus an engine restructure (`tools.ts`/`tool-runner.ts` removed behind one injected `ToolExecutor` port; engine-owned generic `NotificationInbox` + `BackgroundSupervisor` with spawn-site session handles, dispose-on-finish latch, and auto-wait; batch-result normalization; terminal-only exit with `submission` on the outcome — bare text never completes a run); contracts gain `AgentKind`, `AgentRunId`, `SandboxId`, `ToolCallResult`; testkit gains transcript fixtures and scripted tool/session-handle helpers; Rust `agent-core/` remains live and unchanged. | `pnpm run check` in `eos-agent-core/` (223 tests) including the Phase 04 §15 suite (pipeline order, relocated batch-runner suite + terminal-solo, hook exit-code protocol with real spawned scripts, supervisor lifecycle + cancel race + dispose latch, auto-wait woken by settlement and steer, isolated-mode batch snapshot, executor assembly per kind, engine normalization, single serialization point); ported Phase 03 loop suite green under a scripted executor; `git diff --stat -- agent-core` stays empty. |
| `phase-04.5-agent-runtime_SPEC.md` | Completed | `@eos/agent-runtime` composition root: `agent_name`-driven `startRun` for main/subagent/advisor runs with ordered `initialMessages`, startup-loaded `AgentProfileRegistry` + Markdown profile loader (`llm_client_id`, `max_turns`, `agent_kind`, `allowed_tools`, `terminal_tool`, body system prompt; all static validation at load), `LlmClientRegistry` from `.eos-agents/llm_clients.json` (codex CLI auth-file clients, `model_id`, `reasoning_effort`), per-run inbox/supervisor pairs, run registry with parent links, agent tool family (`run_subagent`/`ask_advisor`/`read_agent_run_transcript`) as `startRun` recursion, per-run JSONL transcript writer + byte-offset reader (the runtime is the event stream's single consumer), hook config loading, and the §8 disposal cascade (`caller_disposed`/`model_cancelled`); plus the §11 owned changes: `@eos/tool` binds profile-selected definitions (kind-keyed `AGENT_TOOLSET` deleted), the executor loses its inbox and hook `additionalContext` rides `metadata.hook_contexts` published by the engine loop, and `AgentRunState` gains `agent_name`; server transport/event broadcaster stays deferred; Rust `agent-core/` remains live and unchanged. | `pnpm run check` in `eos-agent-core/` (282 tests) including the Phase 04.5 §13 integration suite (profile loader/registry validation, llm client registry + codex auth-file validation, §4 wiring smoke with profile-selected tool exposure + notification drain, submission end-to-end, subagent round-trip with auto-wait + settlement notification + transcript read, advisor ask + caller-abort cancellation, disposal cascade recording `caller_disposed` and model-initiated cancel recording `model_cancelled`, real spawned read-before-write hook over `transcript_path` with `hook_context` republish, transcript completeness + offset increments); `git diff --stat -- agent-core` stays empty. |
| `phase-05-workflow-orchestration_SPEC.md` | Proposed | Additive `@eos/workflow` package (DB-authoritative workflow -> iteration -> attempt -> plan/work-item aggregate per the companion `docs/plans/workflow_context_projection_SPEC.md` with §3 recorded amendments; depth-parametric markdown brief/spec renderers behind two combinators with revision-stamped projections and prior iterations collapsed to status + reference, virtual projection only; lifecycle orchestration functions + per-workflow serial reconcile queue; scheduler consumes planner/worker submissions from `outcome.submission` via a narrow `AgentLaunchPort` - launch-after-commit from claimed queue rows, settlements without valid submissions synthesized as failures) plus first real `@eos/db` content (`better-sqlite3` + `Kysely` workflow schema, `WorkflowStore` transactions/claims/`loadAggregate`), `@eos/contracts` workflow IDs/status/payload schemas, `@eos/tool` owned changes (workflow family `delegate_workflow`/`query_workflow` over bound functions with the one-open-workflow guard, per-kind planner/worker submission payload schemas filling the Phase 04 reserved seam, `cancel_background_session` type union gains `"workflow"`), and `@eos/agent-runtime` wiring (optional `workflowDb`, launch-port adapter over `startRun` with fresh abort roots, per-run `workflowTools`, one supervisor session per delegated workflow with disposal cascade); no `@eos/engine` changes; Rust `agent-core/` remains live and unchanged. | Planned: `pnpm run check` in `eos-agent-core/` including the Phase 05 §14 suite (companion §12 rendering criteria as case tables; store round-trip on `:memory:`; engine-free lifecycle suite over a scripted `AgentLaunchPort` covering auto-launch, materialization, success/failure/retry/deferred-goal cascades, death synthesis, reconcile serialization, cancel cascade; tool family guards; one `MockLlmClient` runtime end-to-end with auto-wait, `session_settled` drain, `query_workflow` read, and interrupt cascade); `git diff --stat -- agent-core` stays empty. |

## Knowledge Base

| Note | Status | Purpose |
| --- | --- | --- |
| `knowledge/claude-code-tech-stack.md` | Observed | Record local Claude Code source-stack observations and migration takeaways for eos-agent-core. |

## Current Boundary

`agent-core/` remains the live Rust implementation until a later phase spec
replaces or retires each Rust-owned surface with verified TypeScript equivalents
under `eos-agent-core/`. The expected migration specs should preserve explicit
ownership for runtime entry, workflow state, engine/query loop, tool framework,
provider clients, sandbox host API, config, audit, skills, and plugin catalog.

## Tracker Discipline

Every migration phase added under this directory must update this index with:

- the phase spec path,
- the phase status,
- the live implementation boundary it changes,
- and the verification command or artifact that proves the phase.
