# EOS Agent Core Rust to TypeScript Migration - Phase 01 Project Setup

Status: Completed
Date: 2026-06-10
Owner: eos-agent-core
Migration direction: Rust -> TypeScript
Project path: `/Users/yifanxu/machine_learning/LoVC/EphemeralOS/eos-agent-core`
Rust source boundary: `/Users/yifanxu/machine_learning/LoVC/EphemeralOS/agent-core`

## 1. Intent

Phase 01 defines the project setup phase for `eos-agent-core`. The phase may
create TypeScript workspace metadata, dependency manifests, lockfiles, and
configuration needed to install the approved baseline packages.

Phase 01 does not authorize application implementation. It must not port Rust
modules, create request handlers, create database schemas, write migrations,
implement agent loops, or change the live Rust `agent-core/` behavior.

## 2. Scope

In scope:

- initialize `eos-agent-core/` as the TypeScript project root,
- create package-manager and TypeScript configuration files,
- create package manifests needed for the first workspace boundary,
- install the approved baseline dependencies from Phase 00,
- generate or update the package lockfile,
- add package scripts for verification only,
- document the installed package set and future package boundaries.

Out of scope:

- runtime source implementation,
- database schema or migration files,
- API handlers, CLI commands, daemon clients, provider clients, or agent loops,
- deleting, moving, or editing Rust crates under `agent-core/`,
- changing sandbox contracts,
- adding BullMQ, Temporal, Redux, Zustand, XState, Prisma, Drizzle, TypeORM,
  Redis, or other Phase 00 non-default libraries.

## 3. Setup Boundary

The TypeScript project root is:

```text
/Users/yifanxu/machine_learning/LoVC/EphemeralOS/eos-agent-core
```

The initial setup may create only project scaffolding, package metadata, and
setup-only configuration:

```text
eos-agent-core/
  .gitignore
  package.json
  pnpm-workspace.yaml
  pnpm-lock.yaml
  tsconfig.base.json
  tsconfig.json
  vitest.config.ts
  packages/
    contracts/package.json
    db/package.json
    observability/package.json
    runtime/package.json
    testkit/package.json
```

The package directories above are ownership placeholders for later phases. Phase
01 may add package manifests for them, but it must not add implementation files
under `src/`. Root `tsconfig.json` and `vitest.config.ts` are allowed only as
verification configuration; they must not expose public APIs or runtime behavior.

## 4. Package Installation Plan

Phase 01 should install the baseline stack from Phase 00 with `pnpm`.

Runtime dependencies:

```text
better-sqlite3
kysely
zod
pino
@opentelemetry/api
@opentelemetry/sdk-trace-node
@opentelemetry/sdk-metrics
@opentelemetry/sdk-logs
@opentelemetry/resources
@opentelemetry/semantic-conventions
```

Development dependencies:

```text
typescript
tsx
vitest
@types/node
@types/better-sqlite3
```

Optional or deferred dependencies:

| Dependency | Phase 01 decision | Reason |
| --- | --- | --- |
| `p-queue` | Do not install by default | Phase 00 marks bounded concurrency as optional; install only when a later phase defines a concrete limiter boundary. |
| `kysely-codegen` | Do not install by default | Useful after a database schema exists; Phase 01 does not create schemas. |
| `@opentelemetry/exporter-*` packages | Do not install by default | Export destination should be selected by the runtime or deployment phase. |

Expected installation commands when this phase is executed:

```bash
cd /Users/yifanxu/machine_learning/LoVC/EphemeralOS/eos-agent-core
corepack enable pnpm
pnpm init
pnpm add -w better-sqlite3 kysely zod pino @opentelemetry/api @opentelemetry/sdk-trace-node @opentelemetry/sdk-metrics @opentelemetry/sdk-logs @opentelemetry/resources @opentelemetry/semantic-conventions
pnpm add -Dw typescript tsx vitest @types/node @types/better-sqlite3
```

If `pnpm init` has already created a manifest, the executing phase should edit
the manifest rather than recreating it.

## 5. Package Boundary Intent

The first workspace packages should be empty ownership targets:

| Package | Future responsibility | Phase 01 allowed content |
| --- | --- | --- |
| `@eos/contracts` | Zod schemas, DTOs, typed IDs, compatibility fixtures | `package.json` only |
| `@eos/db` | Kysely database adapter, migrations, run/request persistence | `package.json` only |
| `@eos/observability` | OpenTelemetry setup and structured logging wrappers | `package.json` only |
| `@eos/runtime` | Agent-run supervisor, request admission, cancellation, reconciliation | `package.json` only |
| `@eos/testkit` | Shared TypeScript test fixtures and state-machine tests | `package.json` only |

No package may expose a public API in Phase 01. Public exports are introduced by
later behavior-specific specs.

## 6. Required Scripts

The root `package.json` should define verification scripts only:

```json
{
  "scripts": {
    "typecheck": "tsc --noEmit",
    "test": "vitest run",
    "check": "pnpm run typecheck && pnpm run test"
  }
}
```

These scripts may be present before there is meaningful source code. Phase 01
acceptance should treat them as setup checks, not behavior verification.

## 7. Non-Implementation Rules

Phase 01 must not add:

- `src/` implementation files,
- database schema or migration files,
- HTTP routes or request handlers,
- agent loop, tool loop, provider, or sandbox-host code,
- compatibility shims for Rust APIs,
- generated client code,
- sample agents or example workflows.

Configuration files are allowed only when they support package installation,
TypeScript checking, or test-runner setup.

## 8. Verification

When this phase is executed, use the narrowest setup checks:

```bash
cd /Users/yifanxu/machine_learning/LoVC/EphemeralOS/eos-agent-core
pnpm install --frozen-lockfile
pnpm list --depth 0
pnpm exec tsc --showConfig
pnpm exec vitest --version
```

Repo-level documentation hygiene:

```bash
cd /Users/yifanxu/machine_learning/LoVC/EphemeralOS
git diff --check -- docs/plans/agent-core-rust-to-typescript-migration eos-agent-core
```

Do not run Cargo as Phase 01 verification unless the execution changes Rust
workspace files. The expected Phase 01 work is TypeScript setup only.

## 9. Acceptance Criteria

Phase 01 is accepted when:

- `eos-agent-core/` has TypeScript project metadata and a `pnpm` lockfile,
- the baseline dependencies and dev dependencies are installed through `pnpm`,
- optional dependencies remain deferred unless a later spec justifies them,
- no runtime implementation files are created,
- the Rust `agent-core/` workspace remains behaviorally unchanged,
- the setup verification commands pass or any package-manager/environment
  failure is documented with exact output,
- and this migration directory's `index.md` lists Phase 01.

## 10. Progress Tracker

| Step | Status | Required proof |
| --- | --- | --- |
| Define Phase 01 setup scope | Completed | This spec exists and is listed in `index.md` as completed. |
| Create TypeScript project metadata | Completed | `package.json`, `pnpm-workspace.yaml`, `tsconfig.base.json`, `tsconfig.json`, `vitest.config.ts`, `.gitignore`, and package placeholder manifests exist under `eos-agent-core/`. |
| Install baseline packages | Completed | `package.json` and `pnpm-lock.yaml` record the approved runtime and dev dependencies; optional Phase 00 dependencies remain absent. |
| Verify setup only | Completed | `CI=true pnpm install --frozen-lockfile`, `pnpm list --depth 0`, `pnpm exec tsc --showConfig`, `pnpm exec vitest --version`, and `pnpm run check` passed with `pnpm@10.23.0` on Node `v22.22.0`. |
| Confirm no implementation landed | Completed | No `src/` runtime implementation, DB migrations, routes, provider clients, or agent-loop files were added under `eos-agent-core/`. |
