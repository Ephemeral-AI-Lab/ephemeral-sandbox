# Per-Call Snapshot Layer Stack Migration Plan

This folder splits the migration into implementation-sized phases. Each phase
document has the same shape:

1. task specification
2. main data objects
3. file/folder structure change
4. workflow demonstration
5. naming conventions and rationale

## Phase Order

| Phase | Document | Outcome |
|---|---|---|
| 01 | `phase-01-layer-stack-foundation.md` | Durable manifest, lease, merged-view, layer delta, and publish primitives live under `sandbox/layer_stack/`. |
| 02 | `phase-02-overlay-snapshot-runtime.md` | Overlay becomes only per-call mount, command execution, and upperdir capture. |
| 03 | `phase-03-occ-changeset-routing.md` | OCC accepts typed changesets from write/edit APIs and shell capture adapters. |
| 04 | `phase-04-occ-commit-transaction.md` | OCC final validation and layer publish become one atomic active-manifest transaction. |
| 05 | `phase-05-squash-lease-budget-gc.md` | Squash, lease pressure, and GC preserve leased snapshot readability. |
| 06 | `phase-06-integration-cutover.md` | Public APIs are routed to the new modules and old production paths are removed. |

## Supplemental Architecture Plans

| Document | Outcome |
|---|---|
| `three-server-command-exec-workspace-replacement-integration-plan.md` | Defines `/testbed` as the default assigned workspace, keeps `layer_stack_root` separate, and specifies the three-server command-exec integration where the full sandbox filesystem remains visible while `/testbed` is replaced by a frozen layer-stack snapshot during guarded shell execution. |
| `three-server-command-exec-workspace-replacement-simplified.md` | Simplified companion with per-verb workflows for how `sandbox.api.status.create_sandbox` and `sandbox.api.tool.{read_file,write_file,edit_file,shell,raw_exec}` interact with `layer-stack-server`, `occ-server`, and `command-exec-server`. |
| `real-fs-request-snapshot-overlay-plan.md` | Replaces layer-stack-as-truth with real `/testbed` as truth plus per-request frozen snapshots. |
| `request-snapshot-performance-experiment-plan.md` | Defines the Phase 0 live Daytona experiment for snapshot create time, destroy time, 1/5/10 concurrent creation, and parallel factor before implementation. |

## Three-Server Workspace Replacement Phase Bundle

| Phase | Document | Outcome |
|---|---|---|
| Index | `three-server-phase-index.md` | Linked bundle for the simplified command-exec workspace replacement migration. |
| 01 | `three-server-phase-01-workspace-binding-base-layer.md` | `layer-stack-server` owns `workspace.json`, builds the `/testbed` base, and serves guarded reads from the active manifest. |
| 02 | `three-server-phase-02-materialized-lowerdir-cache-leases.md` | Layer-stack prepares leased, materialized lowerdirs without rebuilding the workspace per shell call. |
| 03 | `three-server-phase-03-narrow-client-protocols.md` | OCC and command-exec depend on narrow client protocols instead of concrete server internals. |
| 04 | `three-server-phase-04-workspace-replaced-shell.md` | Guarded shell enters `command-exec-server`, replaces `/testbed` with a leased snapshot mount, and captures workspace upperdir changes. |
| 05 | `three-server-phase-05-occ-mutation-gate.md` | `write_file`, `edit_file`, and shell capture converge through `OCCClient` and `occ-server`. |
| 06 | `three-server-phase-06-supervision-transport.md` | Setup supervises `layer-stack.sock`, `occ.sock`, and `command-exec.sock`, and the thin client routes public verbs. |
| 07 | `three-server-phase-07-raw-exec-blocking-recovery.md` | Raw/setup execution is blocked from mutating `/testbed` after base build, with explicit recovery paths. |
| 08 | `three-server-phase-08-squash-gc-performance.md` | Squash, GC, cache, and performance gates preserve active leases and bound shell/read costs. |

## Target Dependency Rule

```text
runtime/command_exec -> command_exec + occ + layer_stack
command_exec         -> layer_stack lease client + occ mutation client
occ                  -> layer_stack narrow commit/read/status protocols
layer_stack          -> no command_exec, no occ, no git
```

## Non-Negotiable Design Rules

```text
OCC may prepare concurrently.
Final active-manifest validation + layer publish is atomic.
base_hash comes from the leased snapshot manifest.
Shell-captured tracked writes are strict full-file CAS writes.
Any shell tracked conflict publishes no shell layer.
Squash and GC never remove data needed by active leases.
```
