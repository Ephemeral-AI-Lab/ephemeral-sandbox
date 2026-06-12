# Command Git Metadata OCC Policy

## Purpose

Allow agents to run ordinary Git workflows through `sandbox.command.exec`, including
`git add`, `git commit`, `git commit --amend`, clean `git revert`, clean
`git cherry-pick`, and clean squash workflows, without using
`sandbox.checkpoint.commit_to_git`.

The rule is not "publish everything under `.git`." The rule is: command capture
may publish healthy Git metadata updates through OCC, while destructive Git
metadata damage is rejected and rolled back with the ephemeral overlay.

## Current Behavior

The command path is:

```text
sandbox.command.exec
  -> ephemeral overlay command
  -> capture upperdir
  -> eos_layerstack::service::publish_capture
  -> OCC route policy
```

The current OCC route policy treats `.git` and `.git/*` as `Drop`. That protects
the shared LayerStack from accidental repository deletion, but it also means a
shell command can run `git add` or `git commit` and then lose the resulting
`.git/index`, object, ref, and log changes at publish time.

The desired behavior is command-specific. Direct file/edit APIs and plugin
callbacks must not gain a broad `.git` mutation path.

## Target Rule

`.git` mutations are permitted only for command capture, only through the gated
OCC lane, and only after a Git metadata validator proves the final repository
state is complete and non-destructive.

| Producer | `.git` policy |
| --- | --- |
| `sandbox.file.write` / `sandbox.file.edit` | Reject or drop `.git` paths. |
| Plugin OCC callbacks | Reject or drop `.git` paths unless a future plugin contract explicitly opts in. |
| `sandbox.command.exec` ephemeral capture | Allow validated `.git` changes through gated OCC. |
| Isolated workspace command capture | Keep private; do not publish `.git` to the shared LayerStack. |
| `sandbox.checkpoint.commit_to_git` | Remains separate and unchanged. |

Every command `.git` path must be `Gated`; `.gitignore` must never route `.git`
metadata to `Direct`.

## Allowed Git Workflows

These workflows are allowed when they complete cleanly in one command and the
final repository passes validation:

| Workflow | Expected final `.git` shape |
| --- | --- |
| `git add` plus `git commit` | new objects, updated index, updated refs, updated logs, updated `HEAD` if needed |
| `git commit --amend` | new commit object, ref/log rewrite, updated index |
| clean `git revert` | new commit object, updated index, refs, logs |
| clean `git cherry-pick` | new commit object, updated index, refs, logs |
| clean squash commit, such as `git merge --squash ... && git commit` | updated index followed by normal commit metadata |
| clean rebase/squash completed inside one command | final refs/logs/index updated with no remaining rebase/sequencer state |

"Clean" means the command exits successfully and the final Git repository is not
left in an in-progress merge, revert, cherry-pick, bisect, or rebase state.

## Forbidden Git Metadata Changes

The validator must reject a command capture before OCC publish when any of these
conditions is present:

| Condition | Reason |
| --- | --- |
| `.git` root deletion or `.git` root opaque directory replacement | Destroys the repository. |
| `Delete` or `OpaqueDir` under `.git/objects` | Can remove reachable objects and corrupt history. |
| `Delete` or `OpaqueDir` under `.git/refs` | Can remove branch or tag references. |
| `Delete` or `OpaqueDir` for `.git/HEAD`, `.git/index`, `.git/config`, `.git/packed-refs`, or `.git/shallow` | Removes core repository control files. |
| final `.git/*.lock` or nested lock file remains | Indicates an interrupted Git operation. |
| final `MERGE_HEAD`, `CHERRY_PICK_HEAD`, `REVERT_HEAD`, `REBASE_HEAD`, `BISECT_LOG`, `sequencer/`, `rebase-merge/`, or `rebase-apply/` remains | Indicates an incomplete multi-step operation. |
| `hooks/` writes or executable hook changes | Allows hidden command execution on future Git operations. |
| unreadable, non-UTF-8 path, unsupported special file, device, FIFO, or socket under `.git` | Not a normal Git metadata result. |

This list is intentionally conservative. A later implementation can relax a rule
only with a targeted test that proves the final repo remains healthy and the
relaxed mutation is required by a real Git workflow.

## Validator Semantics

Command finalization must validate Git metadata in two layers.

### 1. Captured Delta Validation

Inspect captured `LayerChange`s before publish:

```text
for each change:
  if path is not .git or under .git:
    continue
  reject forbidden delete/opaque/lock/hook/special-file patterns
  require command Git metadata mode
  force route = Gated
```

This step prevents obviously destructive changes from reaching the OCC queue.

### 2. Candidate Repository Validation

Build the candidate final repository state from the command snapshot plus the
captured delta, then run Git health checks before publish.

Minimum checks:

```text
git rev-parse --git-dir
git fsck --connectivity-only
git status --porcelain=v1 --untracked-files=no
```

The health check must also assert that no in-progress operation markers remain.
If validation fails, return a `git_metadata_protected` or
`git_operation_incomplete` conflict and do not publish any captured changes.

The implementation may use the command overlay worktree directly when it still
contains the final state. If that is not reliable, project the snapshot to a
temporary worktree, apply the captured delta, validate, then delete the temporary
worktree.

## OCC Semantics

Command Git metadata uses the same atomic publish contract as ordinary command
capture:

```text
normal files + .git metadata publish together, or none publish
```

Additional rules:

- All `.git` paths are `Gated`, never `Direct`.
- The command publish remains `atomic = true`.
- Any `.git` conflict rejects the whole command publish.
- Concurrent commands that update the same ref, index, or control file must
  conflict instead of last-writer-wins.
- New Git object files can be gated with an absent base hash. If a same object
  path already exists with identical content, the publish may treat it as
  accepted; differing content at the same object path is a hard conflict.

## Rollback Behavior

Do not silently restore `.git` by writing repair layers.

When validation rejects the command:

1. Return a structured conflict or error, such as `git_metadata_protected`.
2. Publish nothing from that command, including non-Git file changes.
3. Drop the ephemeral overlay.
4. Leave the shared LayerStack unchanged.

This gives restore semantics through transaction rollback: destructive command
effects disappear with the discarded overlay.

## API and Ownership Changes

Implement this as a narrow command publish policy, not as a global LayerStack
permission change.

Target ownership:

| Area | Change |
| --- | --- |
| `eos-layerstack::commit` | Add a Git metadata route policy, for example `GitMetadataPolicy::{Drop, Gated}`. |
| `eos-layerstack::service` | Keep `publish_capture` defaulting to `Drop`; add `publish_command_capture` using `Gated` plus validation. |
| `eos-operation::command::finalize` | Call `publish_command_capture` for ephemeral commands. |
| `eos-operation::file` | Keep direct file/edit `.git` writes rejected or dropped. |
| `eos-operation::plugin` | Keep plugin `.git` callback writes rejected or dropped. |
| `eos-e2e-test` | Replace the old `.git` drop expectation for command capture with validated Git workflow tests; keep direct file `.git` protection tests. |

Avoid pushing Git subprocess logic into the OCC single-writer itself. OCC should
own routing, base hashes, and atomic publish. Command finalization should own
command-specific Git metadata validation because it knows the mutation source.

## Test Plan

Unit tests:

- Route policy: `.git/config` is `Drop` for default publish and `Gated` for
  command publish.
- `.gitignore` cannot route `.git/*` to `Direct`.
- Git validator accepts normal commit metadata writes.
- Git validator rejects `.git` root deletion.
- Git validator rejects object, ref, index, `HEAD`, config, and packed-ref
  deletes.
- Git validator rejects lock files and incomplete sequencer/rebase state.
- Git validator rejects hook writes.

Package tests:

```text
cargo test -p eos-layerstack --all-targets
cargo test -p eos-operation --all-targets
cargo test -p eos-daemon --all-targets
```

Live E2E tests:

- `sandbox.command.exec` can run `git add . && git commit -m "agent commit"` and
  the resulting commit is visible from a later command.
- `git commit --amend` publishes the amended commit metadata.
- clean `git revert --no-edit <sha>` publishes.
- clean `git cherry-pick <sha>` publishes.
- a conflicting cherry-pick is rejected with `git_operation_incomplete` and the
  shared LayerStack remains unchanged.
- `rm -rf .git` exits successfully inside the shell only if the shell permits it,
  but command finalization rejects the publish and later commands still see the
  original repository.
- direct `sandbox.file.write` to `.git/config` remains rejected or dropped.
- two concurrent command commits against the same branch produce one success and
  one OCC conflict, not two last-writer-wins ref updates.

## Non-Goals

- Do not make `.git` generally mutable through direct file APIs.
- Do not use `sandbox.checkpoint.commit_to_git` as the command implementation.
- Do not allow persistent half-completed cherry-pick, revert, merge, or rebase
  state in the initial implementation.
- Do not implement automatic Git repair layers after destructive commands.
- Do not allow hook installation as part of command capture.

## Open Follow-Ups

- Decide whether durable staged-only state from `git add` without `git commit`
  is acceptable. If not, require either a commit or a clean no-op index.
- Decide whether `git gc` and pack pruning should remain forbidden. The initial
  policy should forbid object deletion because agent commit workflows do not
  require pruning reachable objects.
- Decide whether multi-command conflict resolution should use isolated
  workspaces or a future explicit Git transaction/session model.
