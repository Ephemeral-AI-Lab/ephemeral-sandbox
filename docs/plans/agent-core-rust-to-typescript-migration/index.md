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
| Test runner | Vitest | Baseline for TypeScript unit and integration tests. |
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

## Spec Index

| Spec | Status | Boundary | Verification |
| --- | --- | --- | --- |
| `phase-00-migration-declaration_SPEC.md` | Proposed | Planning declaration only; no live implementation boundary changes. | Spec records the migration identity, target stack, non-goals, and initial acceptance criteria. |
| `phase-01-project-setup_SPEC.md` | Completed | Setup-only TypeScript metadata under `eos-agent-core/`; Rust `agent-core/` remains behaviorally unchanged. | `CI=true pnpm install --frozen-lockfile`, `pnpm list --depth 0`, `pnpm exec tsc --showConfig`, `pnpm exec vitest --version`, and repo `git diff --check` hygiene. |
| `phase-02-llm-client_SPEC.md` | Proposed | Additive TypeScript implementation: `@eos/contracts` minimum message DTOs and `@eos/llm-client` with two SDK-backed clients (Anthropic Messages via `@anthropic-ai/sdk`, OpenAI Responses via `openai`) behind one normalized event union and retry gate; Rust `agent-core/` remains live and unchanged. | `pnpm run check` in `eos-agent-core/`, including golden decode tests replaying both providers' copied SSE fixtures through injected-fetch SDK clients; `git diff --stat -- agent-core` stays empty. |
| `phase-03-agent-loop-engine_SPEC.md` | Proposed | Additive TypeScript implementation: `@eos/engine` agent-loop spine (thin while loop, dual transcript, event stream, interrupt/steer, non-tool-use termination); Rust `agent-core/` remains live and unchanged. | `pnpm run check` in `eos-agent-core/`, including the scripted `MockLlmClient` loop suite (termination, steering, interrupt, tool batches); `git diff --stat -- agent-core` stays empty. |

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
