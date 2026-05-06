# Three-Server Workspace Replacement - Simplified Plan

**Status:** draft companion
**Date:** 2026-05-06
**Source plan:** `three-server-command-exec-workspace-replacement-integration-plan.md`
**Scope:** How `sandbox.api.{verb}` routes through `layer-stack-server`,
`occ-server`, and `command-exec-server`.

## One-Screen Summary

After setup, `/testbed` has one workspace truth: the layer stack. The real
provider filesystem still exists, but guarded workspace APIs do not treat the
real `/testbed` as truth after workspace base build.

```text
real sandbox filesystem:
  /bin, /usr, /opt, /root, /tmp, ...
  /testbed                  # base/recovery source only after workspace base build

layer-stack-server:
  owns workspace manifests, layers, leases, materialized snapshots, squash, GC

occ-server:
  owns mutation policy, conflict validation, staging, and publish decisions

command-exec-server:
  owns guarded shell execution where the full sandbox filesystem remains
  visible, but /testbed is replaced with a leased layer-stack snapshot
```

The important routing rule is simple:

```text
read_file   -> layer-stack-server
write_file  -> occ-server -> layer-stack-server
edit_file   -> occ-server -> layer-stack-server
shell       -> command-exec-server -> layer-stack-server
                                   -> occ.client.OCCClient -> occ-server
                                   -> layer-stack-server
raw_exec    -> provider/runtime escape hatch, not guarded workspace mutation
status      -> provider/control path; setup starts the three servers
```

## Server Ownership

| Server | Owns | Must not own |
|---|---|---|
| `layer-stack-server` | `workspace.json`, active manifest, layer storage, leases, materialized lowerdir cache, squash, GC, compare-and-publish storage primitive | OCC policy, Git or gitignore decisions, shell command orchestration |
| `occ-server` | write/edit mutation gate, shell-capture changeset gate, typed changes, base-hash checks, gitignore policy, conflict handling, staging, CAS retry loop | layer storage layout, durable leases, shell mount namespace |
| `command-exec-server` | guarded shell request, workspace replacement mount, cwd/env policy, per-command upperdir, upperdir capture, lease lifetime for shell | active manifest ownership, OCC accept/reject policy, layer-stack storage, Git or gitignore decisions |

`occ-server` is bound to `layer-stack-server` through a configured
`workspace_ref` / `layer_stack_root` and narrow layer-stack client protocols.
This is a dependency binding, not ownership of workspace binding. Only
`layer-stack-server` creates and stores `workspace.json`; `occ-server` must
verify that binding before every mutation and fail closed if it is missing.

`command-exec-server` may call narrow layer-stack and OCC clients. It must not
import concrete `LayerStackManager`, `Manifest`, `MergedView`, `OccService`, or
publish internals. It must also not import or implement Git/gitignore policy.

## Git Policy Ownership

All Git-related policy belongs to `occ-server`, behind the OCC
`SnapshotGitignoreOracle` / gitignore oracle boundary.

```text
layer-stack-server:
  stores workspace bytes and manifests
  may store .gitignore as an ordinary workspace file
  must not parse .gitignore
  must not classify ignored/tracked/untracked paths
  must not special-case .git mutation policy

command-exec-server:
  mounts and captures the assigned workspace generically
  must not parse .gitignore
  must not decide whether a path is ignored/tracked/untracked
  must not block/drop/redirect .git writes itself
  must not mount a special .git view as part of command execution

occ-server:
  owns SnapshotGitignoreOracle
  reads .gitignore content from layer-stack snapshots through snapshot readers
  classifies accept/drop/reject/OCC-gated paths
  handles .git mutation policy
  handles gitignored path routing
```

If command execution captures `.git` or gitignored path changes, the capture is
submitted as ordinary workspace-relative changes. OCC decides whether those
changes are dropped, rejected, direct-merged, or OCC-gated. Neither
`layer-stack-server` nor `command-exec-server` should contain Git-specific
branches.

## Stable Workspace Contract

```text
workspace_root   = /testbed
layer_stack_root = /tmp/eos-sandbox-runtime/layer-stack
```

Workspace base build happens once:

```text
real /testbed
  -> deterministic full workspace base
  -> base layer L000001
  -> active manifest version 1
  -> workspace.json
```

Server binding after workspace base build:

```text
layer-stack-server
  owns workspace binding:
    workspace_root   = /testbed
    layer_stack_root = /tmp/eos-sandbox-runtime/layer-stack
    active manifest  = N

occ-server
  is configured against that layer-stack binding:
    workspace_ref -> layer-stack-server + layer_stack_root
    read binding/status through narrow layer-stack protocols
    publish only through layer-stack compare-and-publish
    missing binding or manifest -> fail closed
```

After that:

- guarded reads read layer-stack snapshots, not real `/testbed`
- guarded write/edit publish through OCC
- guarded shell sees `/testbed` as a workspace replacement mount
- raw/setup writes under `/testbed` are blocked after workspace base build
- writes outside `/testbed` are runtime/provider state, not layer-stack truth

## API Route Map

| Public verb | First target server | Other servers touched | Why |
|---|---|---|---|
| `sandbox.api.status.create_sandbox` | provider/control path | starts all three servers, asks `layer-stack-server` to bind/import workspace | setup must create the workspace truth before guarded APIs run |
| `sandbox.api.tool.read_file` | `layer-stack-server` | none | read is pure snapshot access; no OCC policy |
| `sandbox.api.tool.write_file` | `occ-server` | `layer-stack-server` | mutation must pass conflict and publish policy |
| `sandbox.api.tool.edit_file` | `occ-server` | `layer-stack-server` | mutation must compute base bytes and validate edits |
| `sandbox.api.tool.shell` | `command-exec-server` | `layer-stack-server`, then `occ-server`, then `layer-stack-server` | shell needs a workspace-replaced execution environment before OCC sees captured changes |
| `sandbox.api.tool.raw_exec` | provider/runtime path | none in the guarded workspace contract | raw exec is not a guarded workspace mutation path |

## Write/Edit Correctness Without Host Precheck

`write_file` and `edit_file` do not need a separate host-side precheck against
`layer-stack-server`. The submitted item is only a mutation intent. It is not a
trusted storage delta.

Correctness is enforced inside `occ-server`:

```text
host request
  |
  | path/content or path/search-replace intent
  v
occ-server
  |
  | read workspace binding and active manifest through layer-stack protocols
  | normalize path and reject path escape
  | drop .git mutation
  | classify path with snapshot gitignore policy
  | read base bytes/base hash from the selected manifest when needed
  | prepare typed changeset
  v
commit-time validation
  |
  | re-read latest active manifest
  | revalidate the prepared path against latest content
  | stage accepted final bytes
  | publish through layer-stack CAS
  v
accepted layer or explicit rejection
```

Required write checks:

- path must normalize inside the assigned workspace
- `.git` writes must not publish as normal workspace payload
- tracked writes need a base hash from the chosen layer-stack snapshot
- before publish, the tracked path must still match that base hash
- create-only writes must reject if the path exists in the validation snapshot
- publish must be guarded by layer-stack compare-and-publish CAS

Required edit checks:

- path must normalize inside the assigned workspace
- `.git` edits must not publish as normal workspace payload
- OCC must read the file bytes from the layer-stack snapshot
- the file must exist and be UTF-8 text
- each search anchor must match the expected occurrence count
- before publish, OCC must revalidate against the latest active manifest

So the rule is:

```text
No host-side layer-stack precheck.
Yes server-side OCC prepare + revalidate through layer-stack protocols.
```

## Workflow Diagrams

### `sandbox.api.status.create_sandbox`

Setup creates the sandbox and imports the assigned workspace into layer-stack.

```text
host/status API
  |
  | sandbox.api.status.create_sandbox(...)
  v
provider adapter
  |
  | provider.create(...)
  v
real sandbox created with /testbed
  |
  | setup_after_create(sandbox_id, project_dir="/testbed")
  v
runtime setup
  |
  | start/supervise:
  |   - layer-stack-server
  |   - occ-server
  |   - command-exec-server
  v
layer-stack-server
  |
  | bind_workspace("/testbed", layer_stack_root)
  | build_workspace_base()
  v
active manifest version 1
```

Pass bar:

- `read_file` can read seeded workspace content from layer-stack
- base build is complete: no filtering policy and no per-path report contract
- supported raw writes under `/testbed` are blocked after workspace base build

### `sandbox.api.tool.read_file`

Read is the only guarded workspace verb that bypasses OCC.

```text
host sandbox.api.tool.read_file("src/a.py")
  |
  | runtime envelope: op="api.read_file"
  v
thin client
  |
  | route to layer-stack-server
  v
layer-stack-server
  |
  | require workspace binding
  | read active manifest N
  | read merged content for workspace-relative path "src/a.py"
  v
host receives {exists, content, encoding, timings}
```

Why no OCC:

- no typed change is built
- no gitignore mutation policy is needed
- no staging or publish happens
- no conflict can be created by the read

### `sandbox.api.tool.write_file`

Write starts at OCC because the request changes workspace state.

```text
host sandbox.api.tool.write_file("src/a.py", content)
  |
  | runtime envelope: op="api.write_file"
  v
thin client
  |
  | route to occ-server
  v
occ-server
  |
  | require workspace binding
  | get active manifest N from layer-stack-server
  | build typed WriteChange from request payload
  | compute base hash from manifest N
  | classify path: accept / reject / drop / OCC-gated
  | prepare changeset
  v
occ-server publish gate
  |
  | re-read latest active manifest
  | revalidate accepted paths
  | allocate commit staging through layer-stack-server
  | write staged blobs
  | compare_publish_layer(expected_manifest=latest)
  | retry on CAS mismatch
  v
layer-stack-server
  |
  | publish new layer if manifest still matches
  v
host receives {success, changed_paths, conflict, timings}
```

`layer-stack-server` performs the storage CAS publish. `occ-server` decides
which changes are safe to publish.

### `sandbox.api.tool.edit_file`

Edit is the same server route as write, but OCC needs the snapshot bytes to
validate the search/replace intent.

```text
host sandbox.api.tool.edit_file("src/a.py", edits)
  |
  | runtime envelope: op="api.edit_file"
  v
thin client
  |
  | route to occ-server
  v
occ-server
  |
  | get active manifest N from layer-stack-server
  | read base bytes for "src/a.py" from manifest N
  | build typed EditChange values
  | validate search/replace against base bytes
  | prepare changeset
  v
occ-server publish gate
  |
  | revalidate against latest active manifest
  | stage accepted final bytes
  | ask layer-stack-server to compare-publish
  | retry on CAS mismatch
  v
host receives {success, changed_paths, applied_edits, conflict, timings}
```

Edit must not route directly to `layer-stack-server`; otherwise the storage
server would have to own edit semantics and conflict policy.

### `sandbox.api.tool.shell`

Shell enters `command-exec-server` first because the command needs a real
execution environment before OCC can see any changes.

```text
host sandbox.api.tool.shell("pytest -q", cwd="/testbed")
  |
  | runtime envelope: op="api.shell"
  v
thin client
  |
  | route to command-exec-server
  v
command-exec-server
  |
  | ask layer-stack-server to prepare workspace snapshot
  v
layer-stack-server
  |
  | require workspace binding
  | read active manifest N
  | open lease for manifest N
  | get/create materialized read-only lowerdir for manifest N
  | return {lease_id, manifest N, lowerdir}
  v
command-exec-server
  |
  | allocate per-command upperdir + overlayfs workdir
  | create private process/mount namespace
  | keep /bin, /usr, /tmp, /root, ... visible from real sandbox filesystem
  | replace /testbed with:
  |   lowerdir = leased manifest N lowerdir
  |   upperdir = per-command workspace upperdir
  |   workdir  = overlayfs internal workdir
  | enforce cwd/env after replacement
  | run command
  | capture workspace upperdir
  | convert capture to workspace changeset tied to manifest N
  v
occ.client.OCCClient
  |
  | apply_changeset(changeset, snapshot=manifest N)
  v
occ-server
  |
  | prepare from leased snapshot identity
  | revalidate against latest active manifest
  | reject whole shell layer on tracked-file conflict
  | stage accepted changes
  | ask layer-stack-server to compare-publish
  v
layer-stack-server
  |
  | publish accepted layer or report CAS mismatch/backpressure
  v
command-exec-server
  |
  | release lease through layer-stack-server
  v
host receives {exit_code, stdout, stderr, changed_paths, conflict, timings}
```

Shell-specific rules:

- the command sees a frozen `/testbed` view for manifest N
- later publishes do not change that running command's `/testbed`
- writes under `/testbed` are captured from the upperdir
- writes outside `/testbed` are not published to layer-stack
- shell-captured tracked-file conflicts reject the whole shell request layer
- OCC base hashes come from the leased manifest, not the active manifest after
  the shell exits

### `sandbox.api.tool.raw_exec`

Raw exec is deliberately outside the guarded workspace mutation contract.

```text
host sandbox.api.tool.raw_exec(command)
  |
  v
provider/runtime exec path
  |
  | if command may mutate /testbed after workspace base build:
  |   reject
  |
  | if command is outside /testbed and allowed by runtime policy:
  |   execute as provider/runtime state
  v
host receives raw execution result
```

The forbidden outcome is:

```text
raw_exec writes /testbed/src/a.py
then read_file("src/a.py") returns stale layer-stack bytes
```

Fail closed instead.

## Shared OCC Publish Gate

Both API write/edit and shell capture converge here:

```text
typed changes + snapshot identity
  |
  v
occ.client.OCCClient.apply_changeset(...)
  |
  v
occ-server
  |
  | prepare changes against base snapshot
  | classify paths
  | compute base hashes
  | re-read latest active manifest
  | revalidate accepted paths
  | allocate layer-stack staging
  | write staged payloads
  | compare_publish_layer(expected_manifest=latest)
  | retry on CAS mismatch
  v
layer-stack-server
  |
  | policy-blind CAS publish
  v
new active manifest or explicit conflict/rejection
```

Boundary rule:

```text
runtime overlay shell capture
  -> capture_to_changeset
  -> occ.client.OCCClient
  -> occ-server / OccService
  -> layer-stack-server compare-publish
```

Do not bypass the public OCC client boundary by invoking `OccService` directly
from capture conversion.

## Long-Running Shell Example

```text
t0  active manifest = N
t1  shell A leases manifest N
t2  shell A sees /testbed from manifest N
t3  write_file B publishes manifest N+1
t4  shell A still sees manifest N
t5  shell A exits and captures upperdir changes
t6  OCC validates A against current active manifest N+1
t7  A publishes N+2 or rejects with conflict
```

Leases pin readability. Squash may rewrite the active manifest shape, but GC
must not delete layers or materialized lowerdirs needed by an active lease.

## Minimal Migration Shape

```text
Phase 1: bind/import /testbed into layer-stack
Phase 2: add materialized lowerdir cache and lease pins
Phase 3: define narrow layer-stack and OCC client protocols
Phase 4: route shell to command-exec-server with workspace replacement mount
Phase 5: route write/edit and shell capture through OCCClient + occ-server
Phase 6: supervise layer-stack.sock, occ.sock, command-exec.sock
Phase 7: block raw/setup writes under /testbed after workspace base build
Phase 8: add squash, GC, cache, and performance gates
```

## Pass Bar

- `read_file` talks only to `layer-stack-server`.
- `write_file` and `edit_file` enter `occ-server` first.
- `shell` enters `command-exec-server` first.
- `command-exec-server` calls `occ.client.OCCClient` for captured changes.
- `occ-server` depends on narrow layer-stack protocols, not concrete manager
  internals.
- `layer-stack-server` never imports OCC, command-exec, or Git/gitignore policy.
- `command-exec-server` never imports or implements Git/gitignore policy.
- guarded APIs never read or mutate real `/testbed` after workspace base build.
- raw exec under `/testbed` is blocked after workspace base build.
- shell writes under `/testbed` publish only through OCC.
- shell writes outside `/testbed` are not layer-stack workspace truth.
- active leases survive squash and GC.
