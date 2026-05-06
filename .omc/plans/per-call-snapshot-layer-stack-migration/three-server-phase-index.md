# Three-Server Workspace Replacement Phase Index

**Status:** draft bundle
**Date:** 2026-05-06
**Source:** `three-server-command-exec-workspace-replacement-simplified.md`

This bundle turns the simplified three-server workspace replacement plan into
implementation-sized phase documents. The phases assume the assigned workspace
is `/testbed`, layer-stack storage is outside that workspace, and guarded
workspace APIs stop treating the real `/testbed` as truth after the workspace
base is built.

## Phase Order

| Phase | Document | Outcome |
|---|---|---|
| 01 | `three-server-phase-01-workspace-binding-base-layer.md` | `layer-stack-server` owns `workspace.json`, builds the `/testbed` base, and serves guarded reads from the active manifest. |
| 02 | `three-server-phase-02-materialized-lowerdir-cache-leases.md` | Layer-stack can prepare leased, materialized lowerdirs without rebuilding the workspace per shell call. |
| 03 | `three-server-phase-03-narrow-client-protocols.md` | OCC and command-exec depend on narrow layer-stack/OCC client protocols, not concrete storage or service internals. |
| 04 | `three-server-phase-04-workspace-replaced-shell.md` | Guarded shell enters `command-exec-server`, replaces `/testbed` with a leased snapshot mount, captures workspace upperdir changes, and keeps the rest of the sandbox filesystem visible. |
| 05 | `three-server-phase-05-occ-mutation-gate.md` | `write_file`, `edit_file`, and shell capture converge through `occ.client.OCCClient` and `occ-server` before publishing through layer-stack CAS. |
| 06 | `three-server-phase-06-supervision-transport.md` | Setup supervises `layer-stack.sock`, `occ.sock`, and `command-exec.sock`, and the thin client routes public verbs to the correct server. |
| 07 | `three-server-phase-07-raw-exec-blocking-recovery.md` | Raw/setup execution is prevented from mutating `/testbed` after the base is built, with explicit recovery paths for rebase. |
| 08 | `three-server-phase-08-squash-gc-performance.md` | Squash, GC, cache, and performance gates preserve active leases and bound shell/read costs. |

## Shared Contract

```text
workspace_root   = /testbed
layer_stack_root = /tmp/eos-sandbox-runtime/layer-stack
```

Routing:

```text
read_file   -> layer-stack-server
write_file  -> occ-server -> layer-stack-server
edit_file   -> occ-server -> layer-stack-server
shell       -> command-exec-server -> layer-stack-server
                                   -> occ.client.OCCClient -> occ-server
                                   -> layer-stack-server
raw_exec    -> provider/runtime escape hatch, blocked for /testbed writes after base build
status      -> provider/control path; setup starts and binds the three servers
```

Dependency rule:

```text
command-exec-server
  -> layer-stack-server through narrow lease/snapshot clients
  -> occ-server through OCCClient.apply_changeset
  -> no concrete LayerStackManager, Manifest, OccService, or Git policy imports

occ-server
  -> layer-stack-server through narrow read/staging/publish clients
  -> owns SnapshotGitignoreOracle and all Git/gitignore mutation policy
  -> no layer storage layout or shell namespace ownership

layer-stack-server
  -> no command-exec imports
  -> no occ imports
  -> no Git/gitignore policy
```

## Cross-Phase Pass Bar

- `read_file` talks only to `layer-stack-server`.
- `write_file` and `edit_file` enter `occ-server` first.
- `shell` enters `command-exec-server` first.
- Shell capture calls `occ.client.OCCClient.apply_changeset`, not `OccService`
  directly.
- `occ-server` fails closed when workspace binding or active manifest is
  missing.
- `layer-stack-server` never reads real `/testbed` after base build except for
  explicit recovery operations.
- Raw/setup execution cannot silently mutate real `/testbed` after base build.
- Active leases survive squash and GC.
