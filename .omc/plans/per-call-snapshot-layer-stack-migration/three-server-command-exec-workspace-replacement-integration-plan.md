# Three-Server Command Execution Workspace Replacement Plan

**Status:** draft
**Date:** 2026-05-06
**Scope:** `layer-stack-server`, `occ-server`, `command-exec-server`, and the assigned workspace contract.

## Existing Workspace Bootstrap

A sandbox starts with a real, populated assigned workspace, usually `/testbed`.
Layer-stack is not an empty workspace and is not rebuilt for every shell
command. It is initialized from the existing workspace once:

```text
real assigned workspace with many existing files
  /testbed
    repo files
    dotfiles
    already-present generated files

workspace base build
  -> deterministic workspace walk
  -> full workspace copy
  -> base layer L000001
  -> manifest version 1
  -> workspace.json with active_root_hash and base_root_hash
```

After workspace base build, layer-stack is the workspace source of truth for guarded
APIs. The real `/testbed` remains provider-owned filesystem state and is used
only for explicit rebuild-base or recovery.

For a large existing workspace, the expensive work is workspace base build and cache-miss
snapshot materialization. A cache-hit shell call must mount an existing
read-only materialized workspace snapshot and capture only the command's
workspace upperdir changes.

## Naming Rule

Name the shell side after the execution contract, not the mount mechanism.

Preferred names:

```text
assigned workspace:
  The workspace path under guard, defaulting to /testbed.

workspace-replaced execution environment:
  The guarded shell environment: the full sandbox filesystem remains visible,
  but the assigned workspace path is replaced by a layer-stack snapshot mount.

command-exec-server:
  The runtime service that builds the workspace-replaced execution environment,
  runs guarded shell commands, captures assigned-workspace changes, and submits
  them to OCC.

workspace replacement mount:
  The mounted assigned-workspace view inside the shell environment.
```

Avoid top-level names like `overlay-server` or `command overlay`. Overlayfs is
an implementation detail of the workspace replacement mount. Terms like
`overlayfs lowerdir`, `upperdir`, and `workdir` are still valid inside the
mount/capture implementation.

`command-exec-server` includes the overlayfs workspace replacement implementation
for guarded shell commands. It does not include layer-stack storage ownership or
OCC policy ownership.

## Target Contract

The target design is:

```text
workspace_root   = /testbed
layer_stack_root = /tmp/eos-sandbox-runtime/layer-stack

layer-stack:
  durable source of truth for the workspace repo only

OCC:
  mutation policy and conflict validation for workspace changes

command-exec:
  guarded shell execution environment where the full sandbox filesystem is
  visible, but the assigned workspace path is replaced by a leased layer-stack
  workspace snapshot
```

This means a guarded shell command executes with normal sandbox tools and
runtime state:

```text
/bin, /usr, /opt, /root, /tmp, ...
```

But at the assigned workspace path, it does not see the real sandbox
`/testbed`. It sees a stable workspace view from layer-stack:

```text
/testbed = overlayfs(
  lowerdir = read-only materialized workspace snapshot from manifest N,
  upperdir = per-command workspace capture dir,
  workdir  = overlayfs internal work dir
)
```

The command working directory is still `/testbed` or a path under `/testbed`.
Overlayfs `workdir` is internal scratch space. It is not the command cwd and is
not layer-stack storage.

## Non-Negotiable Decisions

1. `layer-stack` is the source of truth for workspace payload files under
   `/testbed`.
2. `layer-stack` does not own the whole sandbox filesystem.
3. `sandbox.api.read_file`, `write_file`, `edit_file`, and guarded `shell` must
   use layer-stack and OCC for workspace state.
4. Guarded shell runs inside a workspace-replaced execution environment: full
   sandbox filesystem execution capability, but the assigned workspace path is
   overmounted with a leased layer-stack snapshot.
5. Guarded shell requests enter `command-exec-server` first. It opens the
   workspace lease, builds the workspace-replaced execution environment, runs
   the command, captures the workspace upperdir, submits the shell changes to
   OCC, then releases the lease.
6. Write/edit requests do not go through `command-exec-server`. They build
   typed changes and enter the same OCC mutation gate directly.
7. The workspace replacement mount `lowerdir` is only the workspace snapshot for
   one manifest. It is not a copy of `/`.
8. The shell execution environment is the full sandbox filesystem with the
   assigned workspace path replaced.
9. Writes under `/testbed` are captured and published through OCC.
10. Writes outside `/testbed` are runtime/provider state. They are not published
   into layer-stack unless a future root-capture feature explicitly owns them.
11. After workspace base build, supported raw/setup execution must not mutate real
   `/testbed`; block those calls instead of tracking a divergence state.
12. Squash rewrites layer-stack storage shape only. It never reads real
    `/testbed` as truth after workspace base build.

## Vocabulary

```text
assigned workspace:
  The workspace path under guard. Default: /testbed.

workspace_root:
  The configured assigned workspace path. Default: /testbed.

layer_stack_root:
  Runtime storage for manifests, layers, leases, snapshots, staging, metrics.
  It must not live inside workspace_root.

manifest N:
  A layer-stack workspace version.

workspace snapshot lowerdir:
  A read-only materialized view of manifest N. Used as overlayfs lowerdir for
  /testbed.

workspace-replaced execution environment:
  The process/mount namespace for guarded shell. It preserves the full
  provider-owned sandbox filesystem and replaces workspace_root with the
  workspace replacement mount.

workspace replacement mount:
  The mounted /testbed view the command sees.

workspace upperdir:
  Per-command writes made under /testbed.

overlayfs workdir:
  Per-command internal overlayfs bookkeeping directory. Must be on the same
  filesystem as upperdir and must be empty at mount time.

command cwd:
  The process working directory, normally /testbed. This is not overlayfs
  workdir.
```

## Filesystem Model

The real sandbox filesystem remains provider-owned:

```text
real sandbox filesystem
|
|-- /bin
|-- /usr
|-- /opt
|-- /root
|-- /tmp
|-- /proc, /sys, /dev
`-- /testbed
      real checkout used only for initial import and explicit recovery
```

Layer-stack stores only workspace state:

```text
/tmp/eos-sandbox-runtime/layer-stack
|
|-- workspace.json
|-- manifest.json
|-- layers/
|-- staging/
|-- leases/
|-- materialized/
|     `-- manifest-000017-<root-hash>/lower
|-- snapshots/
|-- gc/
`-- metrics/
```

Guarded shell uses a workspace-replaced execution environment:

```text
workspace-replaced execution environment
|
|-- /bin, /usr, /opt, /root, /tmp, ...
|     real sandbox filesystem remains visible
|
`-- /testbed
      overlayfs(
        lowerdir = /tmp/eos-sandbox-runtime/layer-stack/materialized/manifest-000017-<root-hash>/lower,
        upperdir = /tmp/eos-sandbox-runtime/command-exec-runs/<request-id>/workspace-upper,
        workdir  = /tmp/eos-sandbox-runtime/command-exec-runs/<request-id>/workspace-work
      )
```

This environment is "full filesystem with assigned workspace replaced." The
workspace replacement mount `lowerdir` is not that full filesystem. The lowerdir
is only the frozen workspace snapshot.

## Workspace Binding

`layer-stack-server` owns a durable workspace binding:

```json
{
  "schema": "sandbox.layer_stack.workspace.v1",
  "workspace_root": "/testbed",
  "layer_stack_root": "/tmp/eos-sandbox-runtime/layer-stack",
  "guard_scope": "workspace_payload",
  "active_manifest_version": 17,
  "active_root_hash": "sha256:...",
  "base_manifest_version": 1,
  "base_root_hash": "sha256:...",
  "created_at": "2026-05-06T00:00:00Z",
  "updated_at": "2026-05-06T00:00:00Z"
}
```

Rules:

- `workspace_root` is absolute and defaults to `/testbed`.
- `layer_stack_root` is runtime storage and must never sit inside
  `workspace_root`.
- Paths stored in layers are workspace-relative, for example `src/a.py`.
- After workspace base build, guarded APIs do not read the real `/testbed`.
- The real `/testbed` is only an base/recovery source.
- After workspace base build, supported sandbox APIs must not mutate the real `/testbed`
  path. This plan assumes the sandbox environment is trivial: no cron,
  background daemon, or external process edits the assigned workspace behind the
  guarded APIs.

## Workspace Payload Contract

Layer-stack guards the entire intended workspace payload. "Everything under
`/testbed`" means a complete base repo: regular files and symlinks are copied
into `L000001-base`, directories are represented by their entries, and setup
fails before binding if any workspace entry cannot be represented.

Default first migration contract:

```text
copy:
  every regular file
  every symlink target as symlink metadata
  dotfiles and Git metadata as ordinary workspace content

fail before binding:
  sockets, FIFOs, device nodes, or other unrepresentable special files
  workspace entries that disappear during the base walk
```

There is no filtering policy and no per-path report contract. Gitignore remains OCC mutation
policy, not the storage source of truth.

## OCC-Owned Git Policy

All Git and gitignore policy belongs to `occ-server`, behind the OCC
`SnapshotGitignoreOracle` / gitignore oracle boundary.

`layer-stack-server` is Git-blind:

```text
layer-stack-server:
  stores workspace payload bytes and manifests
  may store .gitignore as an ordinary file
  never parses .gitignore
  never decides ignored/tracked/untracked status
  never implements .git mutation policy
  never records git head or git metadata fields in workspace.json
```

`command-exec-server` is also Git-blind:

```text
command-exec-server:
  builds the workspace replacement mount
  runs commands
  captures workspace upperdir changes
  submits captured workspace-relative changes to OCC
  never parses .gitignore
  never decides ignored/tracked/untracked status
  never blocks, drops, redirects, or special-mounts .git paths
  never sets Git-specific execution policy such as GIT_OPTIONAL_LOCKS
```

`occ-server` owns the Git policy:

```text
occ-server:
  owns SnapshotGitignoreOracle
  reads .gitignore content from layer-stack snapshots through snapshot readers
  classifies path routing: accept, reject, drop, direct merge, OCC-gated merge
  handles gitignored path routing
  handles .git mutation policy
```

If a command creates or modifies `.git` paths, `command-exec-server` captures
those changes generically and submits them to OCC. OCC then decides whether to
drop or reject them. If a future feature needs a virtual Git identity for shell
commands, that feature must be designed outside `layer-stack-server` and
`command-exec-server`; it must not put Git policy into either server.

## Server Responsibilities

### layer-stack-server

Owns durable workspace state and storage.

Surfaces:

```text
layer_stack.bind_workspace(workspace_root, layer_stack_root)
layer_stack.get_workspace_binding(layer_stack_root)
layer_stack.build_workspace_base(layer_stack_root, expected_empty=true)

layer_stack.get_active_manifest(layer_stack_root)
layer_stack.read_text(layer_stack_root, path, manifest_version?)
layer_stack.read_bytes(layer_stack_root, path, manifest_version?)
layer_stack.list_dir(layer_stack_root, path, manifest_version?)

layer_stack.open_workspace_lease(layer_stack_root, request_id, ttl_seconds)
layer_stack.heartbeat_lease(layer_stack_root, lease_id)
layer_stack.release_lease(layer_stack_root, lease_id)

layer_stack.prepare_workspace_snapshot(layer_stack_root, request_id, ttl_seconds)
layer_stack.materialize_workspace_snapshot(layer_stack_root, manifest_version)
layer_stack.get_or_create_materialized_lowerdir(layer_stack_root, manifest_version)

layer_stack.allocate_commit_staging(layer_stack_root, request_id)
layer_stack.publish_layer_if_manifest_matches(
  layer_stack_root,
  expected_manifest,
  staged_changes
)

layer_stack.squash(layer_stack_root, max_depth)
layer_stack.collect_garbage(layer_stack_root)
layer_stack.metrics(layer_stack_root)
```

Does not own:

- OCC conflict policy
- Git or gitignore accept/drop/reject policy
- shell command orchestration
- workspace replacement mounts
- process environment policy
- non-workspace filesystem versioning

### occ-server

Owns mutation policy and publish gating for workspace state.

`occ-server` is bound to `layer-stack-server` through the request's
`workspace_ref` / `layer_stack_root` and a narrow layer-stack client. This means
OCC has an explicit dependency on the layer-stack workspace binding, but does
not create or own that binding. `layer-stack-server` remains the only authority
that writes `workspace.json`, owns the active manifest, and stores durable
leases.

At startup, `occ-server` configures the layer-stack client/gateway. For each
mutation request, it must resolve the supplied workspace reference through
`layer-stack-server`, verify the workspace binding and active manifest exist,
and fail closed if either is missing. Missing binding must never fall back to
real `/testbed`.

Surfaces:

```text
api.write_file(workspace_ref, path, content, options)
api.edit_file(workspace_ref, path, edits, options)
occ.apply_changeset(workspace_ref, typed_changes, snapshot, options)
```

Internal responsibilities:

- bind to `layer-stack-server` through narrow client protocols for the requested
  workspace reference
- fail closed when the workspace binding or active manifest is missing
- route typed changes from write/edit/shell into accept, reject, drop, direct
  merge, or OCC-gated merge
- compute base hashes from the relevant layer-stack snapshot supplied by the
  caller
- use a snapshot gitignore oracle, not real `/testbed`
- publish accepted workspace changes through layer-stack CAS
- keep shell-originated changes atomic enough that a tracked conflict publishes
  no partial shell layer

All workspace mutations converge at the same OCC gate. The only difference is
how the typed changes are produced:

```text
write/edit:
  api request
    -> occ-server write/edit endpoint
    -> typed changes from request payload
    -> shared apply_changeset(changes, snapshot=active/current)

shell:
  api request
    -> layer-stack-server prepares workspace snapshot
         verifies workspace binding and active manifest exist
         opens lease for manifest N
         returns materialized lowerdir for manifest N
    -> command-exec-server runs command
         full sandbox filesystem remains visible
         assigned workspace is replaced by the layer-stack snapshot
         output is workspace upperdir changeset for snapshot N
    -> occ-server receives changeset
         revalidates against latest active manifest
         commits accepted changes to latest through layer-stack CAS

shared OCC gate:
  occ-server apply_changeset
    -> OccService.prepare/revalidate
    -> layer-stack publish CAS
```

Does not own:

- `manifest.json`
- layer directories
- durable leases
- GC/squash
- workspace replacement mount implementation
- shell command orchestration
- root filesystem capture/versioning

### command-exec-server

Owns guarded shell execution, workspace-replaced environment construction,
workspace lease coordination, and workspace capture submission.

This service is intentionally not named `overlay-server`. Overlayfs is the
workspace replacement mount mechanism; the service contract is broader: run the
command in the full sandbox filesystem with only the assigned workspace path
replaced by a layer-stack snapshot.

So yes, `command-exec-server` includes the overlay mount/capture implementation
for the assigned workspace. It owns the process namespace, workspace replacement
mount, per-command upperdir, and upperdir capture. It does not own the whole
root filesystem as a layer stack, and it does not decide OCC accept/reject
policy.

Input:

```json
{
  "request_id": "abc",
  "workspace_ref": {
    "workspace_root": "/testbed",
    "layer_stack_root": "/tmp/eos-sandbox-runtime/layer-stack"
  },
  "command": ["bash", "-lc", "pytest -q"],
  "cwd": "/testbed",
  "env": {},
  "env_policy": "workspace_replaced_shell",
  "timeout_seconds": 60
}
```

Behavior:

```text
1. Ask layer-stack-server to prepare a workspace snapshot.
2. Receive lease_id, manifest N, and an opaque read-only lowerdir path for
   manifest N.
3. Allocate per-request workspace upperdir and overlayfs workdir.
4. Create an isolated process/mount namespace for the workspace-replaced
   execution environment.
5. Keep the full sandbox filesystem visible.
6. Make mount propagation private/slave.
7. Mount the workspace replacement at /testbed with the leased workspace
   lowerdir.
8. Validate cwd and env after the assigned workspace path is replaced.
9. Run the command.
10. Capture filesystem changes from workspace upperdir.
11. Build a workspace upperdir changeset tied to snapshot N.
12. Submit the changeset plus snapshot N to occ-server.
13. Release the layer-stack lease after OCC accepts/rejects the changeset.
14. Return shell result plus OCC commit/conflict result.
```

Owns:

- shell request execution
- workspace-replaced execution environment construction
- workspace lease lifetime for shell
- workspace replacement mount construction
- workspace upperdir capture
- shell capture submission to OCC

Does not own:

- active manifests
- layer-stack storage layout
- OCC decisions
- Git or gitignore accept/drop/reject policy
- commit staging
- publish/merge policy
- non-workspace persistence

`command-exec-server` may call layer-stack lease/materialized-lowerdir APIs
and OCC mutation APIs through narrow clients. It must not import concrete
`LayerStackManager`, `Manifest`, `MergedView`, `OccService`, Git/gitignore
policy, publish internals, GC/squash, or active manifest readers.

## Lowerdir and Snapshot Cache

The lowerdir must not be rebuilt from scratch per command.

Correct target:

```text
manifest N active root hash H
  -> materialized lowerdir cache key (N, H)
  -> many command leases can mount the same read-only lowerdir
```

Required cache lifecycle:

```text
open_workspace_lease:
  pins manifest N
  pins or creates materialized lowerdir (N, H)
  returns lowerdir path

release_workspace_lease:
  unpins manifest N
  unpins materialized lowerdir (N, H)

collect_garbage:
  may delete materialized lowerdirs only when no active lease pins them
```

Implementation notes:

- Use one read-only materialized lowerdir per active manifest/root hash.
- Prefer hardlinks/reflinks from layer storage when possible.
- Avoid using hundreds of layer dirs directly as overlayfs lowerdirs.
- Squash/checkpoint should keep manifest depth bounded so materialization is
  cheap.
- Metrics must separate:
  - cache hit/miss
  - lowerdir materialize time
  - workspace replacement mount time
  - command runtime
  - upperdir capture time
  - OCC prepare/commit time

## Long-Running Command Semantics

Example:

```text
t0: active manifest = N
t1: command A leases manifest N
t2: command A sees /testbed from manifest N
t3: command B publishes manifest N+1
t4: command A still sees manifest N under /testbed
t5: command A exits; its upperdir is converted to OCC changes
t6: OCC validates A against current active manifest N+1
t7: A either publishes N+2 or rejects with conflict
```

Workspace isolation guarantee:

- Later workspace publishes do not change a running command's `/testbed` view.
- A command's workspace writes remain in its own upperdir until OCC publishes.
- Publish conflicts are detected against the current active manifest.

Outside-workspace behavior:

- `/tmp`, `/root/.cache`, toolchain paths, and other non-workspace paths remain
  live sandbox filesystem.
- A long-running command can observe outside-workspace changes made by other
  processes unless a separate root-capture mode is added.
- Those outside-workspace writes are not layer-stack workspace truth.

## Shell Environment Contract

`command-exec-server` owns cwd/env enforcement because it owns the final
workspace-replaced execution environment.

Target guarded shell environment:

```text
Always set:
  PWD=/testbed or the resolved cwd under /testbed

Default outside-workspace scratch:
  TMPDIR=/tmp/eos-sandbox-runtime/tmp/<request_id>
  HOME=/tmp/eos-sandbox-runtime/home/<request_id>
  XDG_CACHE_HOME=/tmp/eos-sandbox-runtime/cache/<request_id>

Allowed inherited vars:
  PATH
  LANG
  LC_*
  TERM
  selected provider/toolchain vars that do not point at layer-stack internals

Reject or rewrite:
  env vars pointing at layer_stack_root
  env vars pointing at real workspace paths outside the workspace-replaced
  execution environment
  PYTHONPATH, NODE_PATH, VIRTUAL_ENV values that escape workspace or approved
  dependency roots
```

An env value pointing to `/testbed` is safe only after `command-exec-server`
has replaced the assigned workspace path inside the shell execution
environment.

## Post-Import Workspace Ownership

After workspace base build, layer-stack is the only supported workspace truth. The design does
not keep a long-lived workspace state field because the normal runtime should
not allow supported unguarded writes to real `/testbed` after workspace base build.

Ownership rules:

```text
workspace base build:
  deterministic walk of real /testbed
  write manifest version 1
  store base_root_hash

guarded APIs:
  never read real /testbed after workspace base build
  all workspace writes publish through OCC

guarded shell:
  overmounts /testbed
  cannot mutate real /testbed from inside the workspace-replaced execution
  environment

raw/setup execution:
  may write /testbed only before workspace base build
  must be blocked from writing /testbed after workspace base build

sandbox environment assumption:
  no cron, background daemon, package hook, or external process mutates /testbed
  after workspace base build
  command-exec is the only runtime path that creates a writable /testbed view,
  and that writable view exists only in its private mount namespace

optional scanner:
  deterministic tree hash of real /testbed for explicit audit/recovery
```

Prepare-snapshot rule:

```text
layer_stack.prepare_workspace_snapshot:
  read workspace binding
  read active manifest N
  open lease for N
  get or create materialized lowerdir for N
  return lease_id, manifest N, lowerdir
```

Recovery options:

```text
rebuild_base:
  explicit recovery-only operation
  discard or archive current layer-stack workspace state, then build real
  /testbed as the new workspace base

rebase:
  explicit recovery-only operation
  compute real /testbed diff against the recorded base/active hash, convert to
  typed changes, and publish through OCC if valid

ignore real workspace:
  allowed only for explicit recovery operations that prove real /testbed is not
  intended to be truth
```

## Raw Execution Policy

Raw provider execution is outside the guarded workspace contract.

Allowed final contract:

```text
raw exec outside /testbed:
  allowed according to runtime/provider policy

raw exec under /testbed:
  blocked after workspace base build by API policy

raw exec cannot prove either:
  blocked
```

The plan must never allow:

```text
raw_exec: echo bad > /testbed/src/a.py
api.read_file("src/a.py") silently returns stale layer-stack content
```

## Squash and Checkpoint Semantics

Squash is storage maintenance. It does not change workspace content semantics.

Squash uses contiguous suffix checkpointing. It does not merge arbitrary chunks
between leased manifests, and active leases do not define holes in the active
squash plan.

Depth-bounded squash:

```text
active manifest, newest first:
  [L10, L09, L08, L07, L06, L05, L04, L03, L02, L01]

max_depth = 4

keep newest prefix:
  [L10, L09, L08]

checkpoint old suffix:
  [L07, L06, L05, L04, L03, L02, L01] -> B11

publish:
  [L10, L09, L08, B11]
```

If there are no active leases and `max_depth = 1`, the same algorithm may
collapse the whole active stack into one checkpoint:

```text
active manifest = N
read merged workspace state from layer-stack manifest N
write checkpoint layer C containing latest layer-stack workspace content
publish manifest N+1 = [C]
GC old unleased layers/materialized lowerdirs
```

This is allowed and preferred when no leased manifests are active and the target
depth is one. For larger target depths, squash preserves the newest prefix and
checkpoints only enough of the old suffix to bring depth under budget. "Latest
workspace" here means latest active layer-stack manifest, not the real
`/testbed`.

Active leases exist:

```text
active manifest = N
lease A pins manifest K
squash may publish checkpoint for current active history if safe
GC must retain every layer and materialized lowerdir pinned by lease A
lease A remains readable until release
```

Leases constrain garbage collection, not the active squash chunk. A squash may
rewrite the active manifest while a request still reads an older leased manifest,
but every layer and materialized lowerdir referenced by that lease remains pinned
until the lease releases.

Squash publish is a manifest rewrite transaction with a suffix-CAS guard:

```text
t0 active:
  [L05, L04, L03, L02, L01]

t1 squash plans:
  keep [L05, L04]
  checkpoint [L03, L02, L01] -> B06

t2 writer publishes before squash commits:
  [L06, L05, L04, L03, L02, L01]

t3 suffix still matches, so squash preserves the new prefix:
  [L06, L05, L04, B07]
```

If the current active manifest no longer ends with the planned suffix, squash
must discard the checkpoint and retry later:

```text
planned suffix:
  [L03, L02, L01]

current suffix:
  [B06]

result:
  abort squash; do not publish checkpoint
```

Required invariants:

- A leased manifest remains readable until its lease releases.
- GC must respect both manifest pins and materialized lowerdir pins.
- A request preparing a snapshot either leases the pre-squash manifest or the
  post-squash manifest; it must never observe a half-squashed manifest.
- Squash publish must preserve any newer prefix layers published while the
  checkpoint was being built.
- Squash publish must abort if the planned suffix is no longer the active
  manifest suffix.
- OCC base-hash inference uses the leased manifest identity, not whatever
  manifest is active after squash.
- Squash never imports from real `/testbed`.

## End-to-End Workflows

### Sandbox Setup

```text
host create_sandbox(project_dir="/testbed")
  -> setup_after_create(sandbox_id, "/testbed")
  -> upload runtime bundle
  -> start/supervise layer-stack-server, occ-server, command-exec-server
  -> layer_stack.bind_workspace("/testbed", DEFAULT_LAYER_STACK_ROOT)
  -> layer_stack.build_workspace_base()
     walk /testbed as a full workspace copy
     write base layer
     write manifest version 1
     write workspace.json with root hash
  -> guarded API is ready when the workspace binding and active manifest exist
```

Pass bar:

- `api.read_file("known_repo_file")` returns seeded content before any write.
- `api.read_file` never reads real `/testbed` after workspace base build.
- supported raw/setup mutation under `/testbed` after workspace base build is blocked.
- no background process mutates `/testbed` outside the guarded API path.

### Read

```text
host api.read_file("src/a.py")
  -> thin client routes to layer-stack-server
  -> layer_stack.get_workspace_binding() must exist
  -> layer_stack.read_text(layer_stack_root, "src/a.py")
  -> merged view reads active manifest N
  -> return content
```

Read bypasses OCC because it is pure layer-stack snapshot access.

### Runtime Envelope and OCC Gate

Guarded API calls are not public `raw_exec` operations. The host may use a
low-level provider exec transport to enter the sandbox runtime daemon, but the
payload is a typed runtime envelope:

```text
RuntimeEnvelope:
  op: "api.write_file" | "api.edit_file" | "api.shell"
  layer_stack_root: "/tmp/eos-sandbox-runtime/layer-stack"
  request_id
  actor_id
  description
  args
```

Public `raw_exec` remains the unguarded escape hatch described in the raw
execution policy. It must not be the wrapper for guarded write/edit/shell.

The three guarded mutation verbs differ only in how they produce typed changes
before entering the shared OCC gate.

Write/edit requests do not require a host-side precheck against
`layer-stack-server`. The host payload is a mutation intent, not a trusted
storage delta. `occ-server` is responsible for all workspace-state checks before
publish:

```text
host write/edit request
  -> occ-server
     -> read workspace binding and active manifest through layer-stack protocols
     -> normalize path and reject path escape
     -> drop .git mutation
     -> classify path with snapshot gitignore policy
     -> read base bytes/base hash from the selected manifest when needed
     -> prepare typed changeset
     -> re-read latest active manifest at commit time
     -> revalidate prepared paths against latest content
     -> stage accepted final bytes
     -> publish through layer-stack compare-and-publish CAS
```

For `write_file`, OCC must enforce the request's overwrite/create policy. A
create-only write must reject if the path exists in the validation snapshot; it
is not enough to carry `create_only` metadata through the typed change. For
tracked writes, OCC attaches a base hash from the selected layer-stack snapshot
and rejects with a version conflict if the latest active content no longer
matches that base hash at publish time.

For `edit_file`, OCC must read the target file bytes from the layer-stack
snapshot, verify the file exists, verify it is UTF-8 text, verify each
search/replace anchor and expected occurrence count, then revalidate against the
latest active manifest before publish.

```text
api.write_file envelope
  wraps:
    path
    content
    overwrite/create policy
    actor/description

  runtime endpoint:
    occ-server api.write_file

  OCC prepare work:
    verify workspace binding and active manifest exist
    choose active/current snapshot
    normalize path and apply snapshot gitignore routing
    enforce overwrite/create policy from the request
    attach base hash for tracked writes
    build typed write change from request payload

  shared gate:
    occ.apply_changeset(changes, snapshot)
```

```text
api.edit_file envelope
  wraps:
    path
    search/replace edits
    actor/description

  runtime endpoint:
    occ-server api.edit_file

  OCC prepare work:
    verify workspace binding and active manifest exist
    choose active/current snapshot
    normalize path and apply snapshot gitignore routing
    read target bytes from the chosen layer-stack snapshot
    verify file exists and is UTF-8 text
    verify search/replace anchors and occurrence counts
    build typed edit changes from request payload

  shared gate:
    occ.apply_changeset(changes, snapshot)
```

```text
api.shell envelope
  wraps:
    command
    cwd
    env/env_policy
    timeout
    actor/description

  runtime endpoint:
    command-exec-server api.shell

  layer-stack-server prepare:
    verify workspace binding and active manifest exist
    open layer-stack workspace lease for manifest N
    get or create read-only materialized lowerdir for manifest N
    return lease_id, manifest N, lowerdir

  command-exec-server run:
    mount assigned workspace replacement with lowerdir/upperdir/workdir
    run command in full sandbox filesystem with assigned workspace replaced
    capture workspace upperdir
    output workspace upperdir changeset tied to snapshot N

  occ-server commit:
    receive workspace upperdir changeset plus snapshot N
    revalidate against latest active manifest
    commit accepted changes to latest through layer-stack CAS

  finalization:
    release layer-stack lease after OCC accepts/rejects
    return command result plus OCC result
```

The shared OCC gate is the same regardless of entry path:

```text
occ.apply_changeset(changes, snapshot)
  -> occ-server apply_changeset endpoint
  -> verify workspace binding and active manifest exist
  -> snapshot gitignore/base-hash decisions
  -> CommitStagingStore.allocate_commit_staging()
  -> OccService.prepare/revalidate
  -> CommitPublisher.publish_layer_if_manifest_matches(expected_manifest, staged_changes)
  -> retry prepare/revalidate on CAS miss
  -> return committed paths, conflicts, timings
```

## Root-Capture Extension

Full filesystem capture is a separate runtime/provider feature. It should not be
implemented by putting `/` into layer-stack.

Future optional design:

```text
root overlay:
  lowerdir = provider/runtime root snapshot
  upperdir = per-command root upperdir
  workdir  = per-command root workdir

inside root overlay:
  /testbed = workspace replacement mount from layer-stack manifest N
```

After command:

```text
/testbed changes:
  capture -> OCC -> layer-stack

outside-/testbed changes:
  capture -> runtime artifact, debug record, discard, or provider snapshot
```

This keeps workspace versioning and full-root runtime state separate.

## Migration Phases

### Phase 1 - Workspace Binding and Base Import

Files:

```text
backend/src/sandbox/layer_stack/workspace.py
backend/src/sandbox/layer_stack/importer.py
backend/src/sandbox/runtime/layer_stack_server.py
backend/tests/unit_test/test_sandbox/test_layer_stack/test_workspace_base.py
```

Tasks:

- add `WorkspaceBinding`
- add `workspace.json`
- add import metadata and active-manifest binding fields
- add deterministic import walker with full workspace copy
- add workspace base build from `/testbed`
- fail if `layer_stack_root` is inside `workspace_root`
- keep layer-stack base Git-blind: copy the full repo without Git classification
  without parsing `.gitignore`, computing git state, or branching on Git
  metadata

Pass bar:

- empty stack imports `/testbed` to manifest version 1
- base build stores root hash
- repeated import with existing manifest fails unless explicit reset is passed
- read after workspace base build uses layer-stack only
- layer-stack import contains no Git or gitignore policy code
- oversize/special files fail or report explicitly, never silently disappear

### Phase 2 - Materialized Lowerdir Cache and Lease Pins

Files:

```text
backend/src/sandbox/layer_stack/snapshot_cache.py
backend/src/sandbox/layer_stack/lease_registry.py
backend/src/sandbox/layer_stack/stack_manager.py
backend/tests/unit_test/test_sandbox/test_layer_stack/test_snapshot_cache.py
```

Tasks:

- add `get_or_create_materialized_lowerdir(manifest_version)`
- key cache by manifest version plus root hash
- pin materialized lowerdir through workspace leases
- add GC rules for unpinned lowerdirs
- expose materialization timings and cache hit/miss metrics

Pass bar:

- two leases for the same manifest reuse one lowerdir
- releasing one lease keeps lowerdir pinned if another lease remains
- GC does not delete lowerdir for active leases
- per-command shell no longer rematerializes the whole workspace on cache hit

### Phase 3 - Narrow Layer-Stack Client Protocols

Files:

```text
backend/src/sandbox/runtime/clients/layer_stack.py
backend/src/sandbox/runtime/clients/occ.py
backend/src/sandbox/occ/ports.py
backend/src/sandbox/occ/service.py
backend/src/sandbox/occ/commit_transaction.py
```

Tasks:

- split role protocols:
  - `SnapshotReader`
  - `SnapshotMaterializer`
  - `CommitStagingStore`
  - `CommitPublisher`
  - `WorkspaceLeaseFactory`
  - `WorkspaceBindingReader`
- add a command-exec-facing layer-stack lease client for guarded shell
- add a command-exec-facing OCC mutation client for shell capture submission
- remove direct `LayerStackManager` dependency from OCC internals
- keep Git/gitignore policy in OCC-owned `SnapshotGitignoreOracle`

Pass bar:

- OCC modules type against narrow protocols
- command-exec modules type against narrow layer-stack/OCC clients, not concrete
  managers/services
- only the layer-stack gateway knows concrete transport/storage
- OCC fails closed when the workspace binding or active manifest is missing

### Phase 4 - Workspace-Replaced Shell Execution

Files:

```text
backend/src/sandbox/runtime/command_exec_server.py
backend/src/sandbox/command_exec/workspace_mount.py
backend/src/sandbox/command_exec/env.py
backend/src/sandbox/command_exec/capture/upperdir.py
backend/tests/unit_test/test_sandbox/test_command_exec/test_workspace_mount.py
```

Tasks:

- define `WorkspaceReplacementMountSpec`
- route guarded shell requests to `command-exec-server` first
- open/release layer-stack workspace leases from `command-exec-server`
- create process/mount namespace for guarded shell
- make mount propagation private/slave
- overmount `/testbed` with overlayfs lower/upper/work dirs
- keep full sandbox filesystem visible outside `/testbed`
- enforce cwd/env policy after overmount
- capture only workspace-relative changes
- submit captured workspace changes to OCC before releasing the lease
- keep copy-backed fallback only for unit tests, explicitly marked non-production

Pass bar:

- `pwd` inside shell is `/testbed`
- `python -c 'open("/testbed/x","w").write("x")'` is captured
- `cd /testbed && echo x > y` is captured
- `echo x > /tmp/outside` is not captured by workspace capture
- command can still use `/bin`, `/usr`, and normal toolchains
- `command-exec-server` imports no concrete `LayerStackManager`, `Manifest`,
  `MergedView`, `OccService`, or publish internals

### Phase 5 - OCC Mutation Gate

Files:

```text
backend/src/sandbox/runtime/occ_server.py
backend/src/sandbox/occ/client.py
backend/src/sandbox/occ/mutation_coordinator.py
backend/src/sandbox/occ/workspace_capture.py
```

Tasks:

- route write/edit directly to OCC
- bind occ-server to layer-stack-server through a configured workspace
  reference and narrow layer-stack client/gateway
- fail closed when the layer-stack workspace binding or active manifest is
  missing; never fall back to real `/testbed`
- keep read/status routed to layer-stack-server
- accept shell capture submissions from `command-exec-server` through
  `occ.client.OCCClient.apply_changeset`
- convert workspace upperdir capture to typed changes at the OCC client/gateway
  boundary
- CAS publish retry on active-manifest mismatch

Pass bar:

- concurrent writes to same path produce deterministic accept/reject outcomes
- shell capture conflict publishes no partial shell layer
- command-exec-held lease remains valid through OCC prepare/revalidate/publish
- direct workspace capture to `OccService` is absent from docs and code

### Phase 6 - Three-Server Supervision and Transport

Files:

```text
backend/src/sandbox/control/daemon/command.py
backend/src/sandbox/runtime/supervisor.py
backend/src/sandbox/runtime/thin_client.py
backend/src/sandbox/runtime/server_common.py
```

Tasks:

- supervise `layer-stack.sock`, `occ.sock`, and `command-exec.sock`
- route host calls:
  - `api.read_file` -> `layer-stack.sock`
  - `api.write_file`, `api.edit_file` -> `occ.sock`
  - `api.shell` -> `command-exec.sock`
  - `api.workspace_binding` -> `layer-stack.sock`
- keep command-exec shell routing explicit; command-exec calls OCC only to
  submit captured workspace changes
- remove fork fallback after a feature-flagged soak period

Pass bar:

- killing `command-exec-server` fails only active shell calls
- killing occ-server does not corrupt layer-stack state
- killing layer-stack-server reloads durable workspace binding and fences
  GC/squash until leases are resolved

### Phase 7 - Raw Exec Workspace Blocking and Recovery

Files:

```text
backend/src/sandbox/api/tool/raw_exec.py
backend/src/sandbox/control/ops/runtime_services.py
backend/src/sandbox/layer_stack/workspace_recovery.py
```

Tasks:

- block supported raw exec from writing under `/testbed` after workspace base build
- expose workspace binding/base metadata for diagnostics
- add explicit rebuild-base/rebase recovery APIs
- add optional scanner for discrepancy audits

Pass bar:

- raw mutation under `/testbed` is rejected after workspace base build
- guarded reads never need a persistent workspace status check
- recovery can rebuild base or rebase only through explicit user/API action

### Phase 8 - Squash, Checkpoint, and Performance Gates

Files:

```text
backend/src/sandbox/layer_stack/squash.py
backend/src/sandbox/layer_stack/snapshot_cache.py
backend/src/sandbox/layer_stack/metrics.py
backend/tests/unit_test/test_sandbox/test_layer_stack/test_squash_snapshot_cache.py
```

Tasks:

- squash active layer-stack manifest into checkpoint when depth exceeds budget
- keep leased manifests readable
- keep materialized lowerdirs pinned by active leases
- add performance gates for import, lowerdir cache hit, lowerdir cache miss,
  workspace replacement mount, capture walk, and OCC publish

Pass bar:

- no-leases squash creates `[checkpoint]` from active layer-stack manifest
- squash never reads real `/testbed`
- active leases survive squash and GC
- cache hit shell setup does not scale with workspace size

## Test Matrix

```text
unit:
  workspace binding validation
  deterministic full workspace base
  post-import raw /testbed write blocking
  materialized lowerdir cache pins
  squash with and without active leases
  OCC ports use narrow protocols
  command-exec env/cwd policy enforcement
  command-exec forbidden import fence

integration:
  read seeded /testbed file through layer-stack
  write/edit publish only through OCC
  shell request enters command-exec-server before OCC
  shell absolute /testbed write captured
  shell cwd escape rejected
  shell env /testbed path resolves to workspace replacement view
  outside-workspace writes not captured by workspace capture
  raw workspace mutation is blocked after workspace base build
  long-running command keeps manifest N while active advances to N+1
  shell conflict against advanced active manifest rejects cleanly

live:
  setup_after_create("/testbed") imports base
  pytest command sees /testbed as workspace replacement view
  command can use normal /bin and /usr tooling
  concurrent shell/write/edit remains deterministic
  server restart preserves or fences leases
  lowerdir cache hit avoids full workspace materialization
```

## Performance Gates

Minimum metrics to expose:

```text
layer_stack.import.walk_s
layer_stack.import.bytes
layer_stack.import.files
layer_stack.snapshot_cache.hit
layer_stack.snapshot_cache.materialize_s
layer_stack.snapshot_cache.bytes
command_exec.mount_s
command_exec.command_s
command_exec.capture.walk_upperdir_s
command_exec.capture.changed_bytes
occ.prepare_s
occ.commit_s
occ.publish.cas_retry_count
```

Performance requirements:

- cache-hit shell setup must not scale with total workspace size
- cache-miss lowerdir materialization may scale with workspace size, but should
  be amortized across leases for the same manifest
- upperdir capture should scale with changed paths/bytes, not total workspace,
  on the production overlayfs path
- squash/checkpoint should bound active manifest depth before read/materialize
  costs become pathological

## Final Dependency Rule

```text
host sandbox.api.tool
  -> runtime thin client

thin client
  -> command-exec-server for shell
  -> occ-server for write/edit mutation verbs
  -> layer-stack-server for read/status verbs

occ-server
  -> layer-stack-server through narrow commit/read/status protocols
  -> owns SnapshotGitignoreOracle and all Git/gitignore routing policy

command-exec-server
  -> layer-stack-server through narrow lease/snapshot protocols
  -> occ-server through OCCClient.apply_changeset for shell capture submission
  -> no concrete LayerStackManager / Manifest / MergedView imports
  -> no concrete OccService imports
  -> no Git/gitignore policy

layer-stack-server
  -> no command-exec imports
  -> no occ imports
  -> no Git/gitignore policy
```

## Open Questions

2. Should outside-workspace writes be unrestricted runtime state, or limited to
   approved scratch/cache roots?
3. Which raw execution paths remain after guarded shell is stable?
4. Does recovery need both rebuild-base and rebase in the first migration, or is
   explicit rebuild-base enough?
