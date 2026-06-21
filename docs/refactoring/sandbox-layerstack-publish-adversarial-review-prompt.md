# Sandbox LayerStack Publish Adversarial Review Prompt

Use this prompt after drafting or modifying the LayerStack publish algorithm
spec, and before implementing the publish policy.

```text
You are performing an adversarial, review-only architecture review for the
EphemeralOS LayerStack publish algorithm.

Working directory:

/Users/yifanxu/machine_learning/LoVC/ephemeral-ai/ephemeral-os

Task:

Review the LayerStack publish algorithm spec and its integration spec for
correctness, safety, implementability, and consistency with the live codebase.
Find concrete defects, missing invariants, contradictory requirements,
unimplementable API shapes, race windows, route-policy holes, and test gaps.

This is review-only. Do not edit files. Do not implement fixes. Do not rewrite
the spec. Do not propose broad redesigns unless a concrete defect makes the
current design unsafe or unimplementable.

Required review targets:

- docs/refactoring/sandbox-layerstack-publish-algorithm.md
- docs/refactoring/sandbox-layerstack-service.md
- crates/sandbox-runtime/layerstack/src/stack/ops/publish.rs
- crates/sandbox-runtime/layerstack/src/stack/mod.rs
- crates/sandbox-runtime/layerstack/src/stack/projection/mod.rs
- crates/sandbox-runtime/layerstack/src/model/mod.rs
- crates/sandbox-runtime/layerstack/src/error.rs
- crates/sandbox-runtime/layerstack/Cargo.toml
- Cargo.toml
- crates/sandbox-runtime/workspace/src/model.rs
- crates/sandbox-runtime/workspace/src/overlay/capture.rs
- crates/sandbox-runtime/workspace/src/service/impls/capture_changes.rs
- crates/sandbox-runtime/operation/src/internal/services.rs
- crates/sandbox-runtime/operation/src/internal/workspace_session/service/model.rs
- crates/sandbox-runtime/operation/src/internal/workspace_session/service/impls/capture_session_changes.rs
- crates/sandbox-runtime/operation/src/public/command/service/finalize.rs

Historical reference only:

Use old history to validate intended `.gitignore` behavior, but do not treat old
code as an implementation requirement if it conflicts with the new simplified
policy.

Suggested history probes:

```sh
git grep -n "GitignoreBuilder\|path_is_ignored\|ManifestIgnoreSource\|route_for_path_from_source\|command_gitignore_cannot_route_git_metadata_direct_or_source\|nested_gitignore_is_scoped\|gitignore_resolves_through_published_upper_layer" 3bc33efd9 -- docs crates/daemon/layerstack/src crates/daemon/layerstack/tests/unit/route.rs
git show 3bc33efd9:crates/daemon/layerstack/src/commit/mod.rs
git show 3bc33efd9:crates/daemon/layerstack/tests/unit/route.rs
```

Baseline commands:

```sh
git status --short --untracked-files=all
rg -n "publish_validated_changes|PublishValidatedChanges|LayerProtectedDrop|StaleBaseRevision|publish_layer\\(&changes\\)|git check-ignore|GitignoreBuilder|ignore.workspace" docs crates Cargo.toml
cargo metadata --no-deps --format-version 1 > /tmp/eos-layerstack-publish-review-metadata.json
```

Preserve unrelated changes. If the worktree is dirty, read the relevant dirty
diff before making claims about current behavior.

Review lenses:

1. Boundary and ownership

   Check whether the spec keeps layerstack-owned publish safety inside
   `sandbox-runtime-layerstack` without making that crate depend on operation or
   workspace types. Check whether `LayerStackService::publish_changes` remains a
   thin command-finalization adapter rather than a second policy owner.

2. Atomicity and locking

   Look for time-of-check/time-of-use gaps between source fingerprint validation
   and layer creation. The final active fingerprint validation and manifest
   write must happen under one layerstack writer lock. Flag any spec language
   that would permit validation in operation followed by raw `publish_layer`.

3. Base manifest model

   Check whether the spec gives implementers enough data to evaluate
   base-snapshot `.gitignore` rules and base fingerprints after the active
   manifest advances. Flag any path that still relies on only version/root hash
   or absolute layer path strings.

4. Route order and forbidden paths

   Verify that `.git` paths and layerstack/control paths are checked before
   `.gitignore`. Confirm `.gitignore` cannot route `.git/**`, `pkg/.git/**`, or
   protected control files into direct/source lanes.

5. `.gitignore` semantics

   Stress nested `.gitignore` behavior. Check that the spec requires in-process
   `ignore` crate evaluation, not Git subprocesses, Git index state, or
   checkout-local files. Check edge cases: dir-only rules at any depth, `*` not
   crossing `/`, `**`, `!` reincludes, sealed ignored directories, invalid UTF-8,
   and nested anchored patterns.

6. Source fingerprint correctness

   Check whether the fingerprint model can distinguish absent, file content,
   symlink target, deletes, and future executable mode. Flag any place where
   `MergedView::read_bytes` would collapse symlinks/files or make source OCC
   weaker than the spec claims.

7. Ignored lane safety

   Confirm ignored direct changes publish only after forbidden checks pass and
   source OCC succeeds. Flag any path where ignored changes could publish after
   a source conflict, protected drop, `.git` mutation, storage error, or partial
   publish failure.

8. Opaque directory handling

   Treat opaque directories as hostile. Check whether expansion is bounded,
   whether hidden descendants are routed individually, whether mixed
   source/ignored descendants reject, and whether forbidden/protected descendants
   reject the whole publish.

9. Error and result contracts

   Check whether operation can map layerstack rejection into command
   finalization metadata without losing the conflict path/reason. Flag stringly
   typed placeholders that would block structured caller behavior.

10. Tests and verification

   Check whether the test list covers the real failure modes. Add findings for
   missing tests only when the gap corresponds to a concrete safety invariant or
   likely regression.

11. Consistency with existing docs

   Search for contradictory active docs. Flag active docs that still instruct
   coarse active-root-hash publish, raw `publish_layer` command publication,
   publish-time autosquash, old commit queues, or Git-backed ignore routing.
   Ignore intentionally historical phase docs unless active docs point to them
   as final-state guidance.

Finding rules:

- Findings first, ordered by severity.
- Every finding must cite exact file:line evidence from the live checkout.
- Do not invent bugs. If evidence is weak, say so under "Open Questions" rather
  than listing a finding.
- Prefer fewer, higher-confidence findings over broad concern lists.
- A finding must explain the impact and the concrete condition that triggers it.
- Do not list style nits unless they can cause implementation ambiguity,
  incorrect behavior, or test blind spots.

Severity guide:

- P0: spec permits data loss, corruption, `.git` publication, or non-atomic
  publish.
- P1: spec is unimplementable, internally contradictory, or likely to produce a
  broken command-finalization path.
- P2: important missing invariant, test gap, or ambiguous API shape that could
  lead to a wrong implementation.
- P3: minor clarity issue with low implementation risk.

Output format:

```text
Findings

[P0/P1/P2/P3] Title
Evidence:
- path/to/file:line ...
- path/to/file:line ...
Impact:
Trigger:
Recommendation:

Open Questions

- Question with file:line context, or "None."

No-Issue Checks

- Briefly list major reviewed areas where no issue was found, with file:line or
  command evidence.

Test Gaps

- Concrete missing tests tied to specific invariants, or "None beyond findings."

Review Notes

- Commands run.
- Whether the worktree was dirty.
- Any areas intentionally not reviewed.
```

Stop after the review report. Do not patch files.
```
