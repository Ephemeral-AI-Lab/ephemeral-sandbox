---
title: Reserved `.wh.` Namespace — honest capture, fail-closed publish admission
tags:
  - ephemeral-os
  - layerstack
  - workspace
  - capture
  - publish
  - implementation-plan
status: draft
updated: 2026-07-07
---

# Reserved `.wh.` Namespace — honest capture + fail-closed publish admission

Companion to the live-Docker catalog in `test-case.md` (same folder). This
spec resolves the `.wh.` filename collision found in the 2026-07-07
investigation: workspace capture converts an ordinary user file named
`.wh.foo` into `Delete { path: "foo" }`, and the store accepts `.wh.`-named
writes that every reader then interprets as whiteout markers. Both are
silent-data-loss paths. The resolution is a **reservation**: `.wh.`-prefixed
path components become an explicitly reserved layerstack-internal namespace,
enforced fail-closed at publish admission, and capture stops inventing
changes from dirent *names*.

## Goal

1. A user file (or symlink, or directory) whose name begins with `.wh.` can
   never be silently reinterpreted as a delete or an opaque-directory mask —
   not by workspace capture, not by the sessionless file operations, not by
   any publish route.
2. The reservation is explicit, documented, and fail-closed: a changeset
   containing a reserved component rejects whole with the existing
   `protected_path` class, exactly like `layers/**` or `.layer-metadata`
   today (EZ-10 / MED-10 semantics in the git-policy catalog).
3. Real overlay deletion/opaque detection — char-device 0:0 whiteouts,
   `{trusted,user}.overlay.whiteout` xattr files, `{trusted,user}.overlay.opaque`
   xattr dirs — is untouched and stays the **only** source of `Delete` /
   `OpaqueDir` changes out of capture.
4. Zero storage-format change, zero migration: every existing published layer
   remains valid, and every store-side reader (`MergedView`, `apply_layer`,
   squash `flatten`) keeps honoring logical `.wh.` markers as it does today.

## Non-goals

- **Full support for `.wh.` filenames via escaping.** Rejected; see
  *Alternatives considered*.
- **Rejecting `.wh.` components inside `LayerPath::parse`.** Deferred
  hardening; see *Alternatives considered*.
- **Validating operator seed trees** (`build_workspace_base` input) for
  reserved names. Bootstrap is operator-trusted surface; recorded as a known
  boundary, may be hardened separately.
- **Changing read semantics** for `.wh.` paths (`file_read .wh.foo` stays
  whatever the merged view resolves — `not_found` on Linux stores).

## Background — what the code does today

The store already reserves `.wh.` names *de facto* on every platform; only
admission was never closed. Inventory (exact refs, at the investigated
revision):

| Layer | Behavior | Where |
| --- | --- | --- |
| capture | any basename `.wh.X` (except `.wh..wh..opq`, bare `.wh.`) → `Delete { X }`, content ignored | `workspace/src/overlay/capture.rs:218-224`, `381-385` |
| capture | basename exactly `.wh..wh..opq` → `OpaqueDir { parent }` | `capture.rs:208-216` |
| capture | kernel encodings: char-dev 0:0, `user.overlay.whiteout` xattr → `Delete`; opaque xattrs → `OpaqueDir` | `capture.rs:401-418` |
| store write | `Delete` → char-dev/xattr on Linux, **literal `.wh.<name>` file off Linux**; `OpaqueDir` → literal `.wh..wh..opq` marker file **on all platforms** | `layerstack/src/stack/layer/write.rs:41-55`, `storage/whiteout.rs:21-68` |
| merged view | a `.wh.<name>` sibling dirent masks `<name>` on **all platforms**; `.wh.*` names hidden from listings | `stack/projection/mod.rs:226-229`, `301` |
| projection | any `.wh.*` name → logical whiteout by name alone; **no length guard**, so bare `.wh.` strips to an empty target and `remove_path` (= `remove_dir_all`) hits the parent dir | `stack/projection/apply.rs:24-34`, `104`; `storage/fs.rs:54-67` |
| squash | any `.wh.X` dirent consumed as a whiteout claim on `X`; entry content discarded; flatten **has** the length guard | `stack/squash/flatten.rs:110-115`, `390-397` |
| admission | protected set is `manifest.json`, `workspace.json`, `layers`, `staging`, `.layer-metadata` — `.wh.*` absent | `stack/publish/route.rs:15-25` |
| admission | rejects abort the whole publish; capture drops other than `UnsupportedSpecialFile` are publish-fatal | `stack/publish/plan.rs:56-64`, `155-159` |
| file ops | sessionless `file_write`/`file_edit` validate only via `LayerPath::parse` (accepts `.wh.foo`), then `amend_path` → `plan_publish` | `operation/src/file/service/support.rs:21-33`, `layerstack/src/stack/file_read.rs:95-110` |
| sessions | Linux-only; kernel overlayfs never writes `.wh.` names into an upperdir — every logical name capture sees in production is user-created | `workspace/src/namespace/setns_runner.rs:30-33` |

Confirmed failure cases (all reachable before this change):

- **F1 silent delete** — session file `.wh.foo` → `Delete { foo }`; content
  lost, lower `foo` masked.
- **F2 silent directory mask** — session file `dir/.wh..wh..opq` →
  `OpaqueDir { dir }`; the lower directory's content disappears.
- **F3 fabricated protected delete** — `.wh.manifest.json` →
  `Delete { manifest.json }` → `protected_path` reject of the whole publish
  from a name the user never considered protected.
- **F4 store poisoning via file ops** — sessionless `file_write ".wh.foo"`
  lands a literal file that masks sibling `foo` in merged reads on all
  platforms and is destroyed (converted to a whiteout claim) by the next
  squash.
- **F5 projection wipe** — a published file named exactly `.wh.` makes
  `apply_layer` call `remove_dir_all` on its parent (the projection root for
  a top-level entry).
- **F6 bootstrap edge** — an operator seed tree containing `.wh.` names
  builds a poisoned base layer (out of scope here; see Non-goals).

The planned export feature already assumes the property this spec enforces:
"a user file whose name begins with `.wh.` cannot appear here as content"
(`export_changes/spec.md`, invariant 10). This spec turns that emergent
behavior into an enforced admission invariant.

## Policy

```text
.wh.-prefixed path components are layerstack-internal marker encoding,
never user namespace. One predicate owns the rule: publish admission.
Admission fails closed: a changeset containing a reserved component rejects
whole as protected_path — never silently dropped, renamed, or reinterpreted.
Capture converts kernel metadata (char-dev 0:0, overlay whiteout/opaque
xattrs) into Delete/OpaqueDir; it never converts dirent names into changes.
Store encodings are unchanged. Every existing published layer remains valid.
Every .wh. dirent inside a published layer is, from now on, a genuine marker.
No migration.
```

## Design

Three code changes, one doc pass. Everything else — `LayerChange`,
`LayerPath`, storage encodings, `MergedView`, squash, leases, protocol —
stays as is.

### D1 — publish admission reserves the namespace (the gate)

`crates/sandbox-runtime/layerstack/src/stack/publish/route.rs` —
`is_protected` gains one clause: any path **component** beginning with
`.wh.` is protected. Component-wise, not basename-wise, because a stored
*directory* named `.wh.d` masks its sibling `d` subtree just like a file
marker does (`projection/mod.rs:231-248`), and children like
`a/.wh.d/f.txt` embed the reserved component mid-path.

```rust
fn is_protected(path: &str) -> bool {
    let mut parts = path.split('/');
    let first = parts.next().unwrap_or_default();
    if matches!(
        first,
        "manifest.json" | "workspace.json" | "layers" | "staging" | ".layer-metadata"
    ) {
        return true;
    }
    path.split('/')
        .any(|part| part == ".layer-metadata" || part.starts_with(".wh."))
}
```

Notes:

- `starts_with(".wh.")` covers the bare `.wh.` component (F5), the opaque
  marker name `.wh..wh..opq`, and every `.wh.X`. Names like `.wh` (no
  trailing dot), `.whx`, `x.wh.y`, and `wh.foo` are **not** reserved.
- The rule applies to **every change kind** — `Write`, `WriteFile`,
  `Delete`, `Symlink`, `OpaqueDir` — because `route_change` and
  `plan_opaque_dir` both funnel through `forbidden_path`
  (`plan.rs:129-165`, `167-178`).
- Every user-facing durable-write route inherits the gate with no further
  changes: session finalize (`operation/.../finalize_session.rs` →
  `publish_validated_changes`), one-shot command publishes
  (`command_operations.rs`), and sessionless file ops (`FileService::write`
  / `edit` → `amend_path` → `plan_publish`, `file_read.rs:110`). The only
  unvalidated writer, `LayerStack::publish_layer` (`ops/publish.rs:19`), is
  bootstrap/test-internal and publishes no user-controlled names.
- Reject surface is unchanged vocabulary: `LayerStackError::PublishRejected`
  with `PublishRejectReason::ProtectedPath`, carrying the offending
  `LayerPath` — the CLIs already render this as
  `publish_reject_class: "protected_path"`.

### D2 — capture stops interpreting dirent names

`crates/sandbox-runtime/workspace/src/overlay/capture.rs`:

Delete outright (no cfg-gating, no replacement):

- `LOGICAL_WHITEOUT_PREFIX` and `OPAQUE_MARKER` constants (capture.rs:15-16),
- the `name == OPAQUE_MARKER` branch (capture.rs:208-217),
- the `is_whiteout_marker` branch (capture.rs:218-224),
- `is_whiteout_marker()` (capture.rs:381-385) and `whiteout_target()`
  (capture.rs:387-399).

Keep unchanged:

- `is_overlay_whiteout` — char-dev 0:0 + zero-length
  `user.overlay.whiteout`-xattr file detection (capture.rs:401-413),
- `has_overlay_opaque_xattr` — `trusted./user.overlay.opaque == "y"` on
  directories (capture.rs:415-418),
- the protected-drop machinery (`UnsupportedSpecialFile`,
  `InvalidLayerPath`) exactly as is. **No new drop reason is added.**

Resulting flow for a user-created `.wh.` name in an upperdir: it is captured
as the ordinary thing it is — `WriteFile` for a regular file, `Symlink` for
a symlink, a walked directory for a directory — and publish admission (D1)
rejects the changeset with a legible `protected_path` at the literal path
the user created. Fail closed, honest error, no fabricated `Delete`, no
per-file silent drop.

Rationale for pass-through-then-reject over a capture-side drop: the
existing drop policy already makes any non-`UnsupportedSpecialFile` drop
publish-fatal (`plan.rs:56-64`), so a drop buys no softer behavior — it
would only add vocabulary (`ProtectedPathDropReason` variant + layerstack
mirror + two mapping arms + JSON name) to reach the same reject. One
predicate, one reject path.

Capture-behavior consequence to be aware of: the logical-name branches were
also the unit-test stand-in for whiteouts (fixtures cannot `mknod`
unprivileged — `workspace/tests/unit/overlay_capture.rs:15-16`). Delete
detection tests migrate to `user.overlay.whiteout`-xattr fixtures, which are
settable unprivileged on Linux; capture delete coverage becomes Linux-gated
(the daemon target is Linux; macOS is unit-test-only and loses no production
coverage).

### D3 — `apply_layer` length-guard parity (bare-`.wh.` landmine)

`crates/sandbox-runtime/layerstack/src/stack/projection/apply.rs:104` —
match the guard `flatten` already has (`flatten.rs:392`):

```rust
} else if name.len() > LOGICAL_WHITEOUT_PREFIX.len()
    && name.starts_with(LOGICAL_WHITEOUT_PREFIX)
{
    ProjectEntryKind::LogicalWhiteout
```

After D1 no *new* layer can contain a bare `.wh.` entry, but a historic one
(publishable before this change, F5) must never again resolve to
`remove_path(parent)` — with the guard it projects as the literal file it
is. This is the only reader change; `MergedView` and `flatten` are already
safe against the bare name (`logical_whiteout_path_for_target` always
produces `.wh.<name>` with a non-empty name; `flatten.rs:392` has the length
check).

### D4 — documentation

- `docs/obsidian/ephemeral-os/docs/ephemeral-os.md` `protected_path` bullet
  (line ~94): add `.wh.*`-prefixed path components to the reserved list,
  with the one-line reason: *collides with the overlay/OCI whiteout marker
  encoding layerstack uses inside layer directories*.
- `overlay/capture.rs` module doc: state that capture derives `Delete` /
  `OpaqueDir` **only** from kernel metadata, never from dirent names, and
  why.
- `is_protected` doc comment: enumerate the reserved set including the
  `.wh.` prefix rule.
- `export_changes/spec.md` invariant 10: may now cite the enforced admission
  invariant instead of "capture already collapses such names" (follow-up
  edit in that plan, not this change).

## Invariants (testable)

1. **No fabricated deletes.** Workspace capture emits `Delete` only for
   char-dev/xattr whiteouts and `OpaqueDir` only for opaque-xattr dirs. A
   content-bearing upperdir file named `.wh.foo` or `.wh..wh..opq` never
   yields either.
2. **Fail closed at admission.** Any validated publish containing a change
   whose path has a component starting with `.wh.` rejects whole with
   `protected_path`; the manifest and every layer are unchanged
   (`_assert_stack_unchanged` semantics).
3. **All routes gated.** Invariant 2 holds identically for session finalize,
   one-shot command publish, and sessionless `file_write`/`file_edit`.
4. **Kernel encodings still work.** In-session `rm` / `rm -rf` + recreate
   flows publish and read back exactly as before this change.
5. **Non-reserved lookalikes unaffected.** `.wh`, `.whx`, `x.wh.y`, `wh.foo`
   publish and read back as ordinary paths.
6. **Store readers unchanged, minus the landmine.** Logical `.wh.X` markers
   in existing layers keep masking `X` in `MergedView`, projection, and
   squash; a bare `.wh.` layer entry projects as a file and never triggers a
   directory clear.
7. **Marker purity going forward.** After D1+D2 ship, every `.wh.` dirent
   inside a newly published layer is a genuine marker written by
   `write_kernel_whiteout` / the opaque-marker writer — the property the
   export plan's invariant 10 assumes.

## Compatibility & rollout

- **No storage migration.** Existing layers contain `.wh.` entries only as
  genuine markers (user files were collapsed at capture before this change,
  never stored) — except a hypothetical pre-fix bare-`.wh.` or
  file-op-planted `.wh.foo`, which D3 renders harmless and which an optional
  one-off store scan (`find <root>/layers -name '.wh.*' ! -name
  '.wh..wh..opq'` cross-checked against char-dev/xattr type) can inventory.
- **Behavior change surface.** (a) A session that previously "worked" by
  having its `.wh.foo` silently converted into a delete now rejects at
  finalize — this is the bug being fixed, and the reject names the path.
  (b) `.wh.`-named paths in sessionless file ops now fail with the
  layerstack `protected_path` rejection instead of succeeding-and-poisoning.
- **Reject granularity.** Whole-changeset discard on reject matches every
  existing `protected_path` case (git-policy EZ-10/MED-10). No partial
  publish is introduced.

## Unit test plan (`tests/`, per repo convention — no test code in `src/`)

`crates/sandbox-runtime/workspace/tests/unit/overlay_capture.rs`

- `plain_wh_named_file_is_captured_as_write_not_delete` — upperdir with
  content-bearing `.wh.foo` (and sibling `foo`): captured changes contain
  `WriteFile { ".wh.foo" }` and **no** `Delete { "foo" }`.
- `plain_opaque_marker_named_file_is_not_captured_as_opaque_dir` —
  content-bearing `dir/.wh..wh..opq`: no `OpaqueDir { "dir" }`.
- `bare_wh_file_is_captured_as_write` — file named `.wh.` → `WriteFile`.
- rework `captures_upperdir_files_whiteouts_symlinks_and_opaque_markers`
  (currently pins the collapse via fabricated logical markers, lines 15-16 /
  26-28 / 33-35): Linux-gated replacement fabricates a whiteout as an empty
  file + `user.overlay.whiteout=y` xattr and an opaque dir via
  `user.overlay.opaque=y` xattr; asserts `Delete` / `OpaqueDir` still come
  from kernel metadata.

`crates/sandbox-runtime/layerstack/tests/unit/publish.rs`

- `wh_prefixed_component_rejects_as_protected_path` — each of
  `Write { ".wh.x" }`, `WriteFile`-shaped write at `a/.wh.b`,
  `Delete { ".wh.x" }`, `Symlink { ".wh.link" }`, `OpaqueDir { ".wh.d" }`,
  `Write { ".wh." }`, `Write { "a/.wh..wh..opq" }` → `PublishRejected`
  with `ProtectedPath` at the offending path; manifest unchanged.
- `wh_lookalikes_still_publish` — `.wh`, `.whx`, `x.wh.y`, `wh.foo` commit.

`crates/sandbox-runtime/layerstack/tests/stack.rs` (or `tests/unit/`)

- `bare_wh_layer_entry_projects_as_file_not_directory_clear` — hand-built
  layer dir containing sibling content and a literal file named `.wh.`;
  `MergedView::project` materializes the tree intact with `.wh.` present as
  a file, nothing cleared.

Existing store-side tests (`stack.rs` whiteout/opaque fixtures,
`tests/unit/squash.rs` logical-claim cases) are unchanged — they exercise
marker encoding inside layer dirs, which remains valid.

## Live e2e

See `test-case.md` in this folder: the live-Docker catalog (16 cases,
EZ/MED/CX) implemented under `cli-operation-e2e-live-test/runtime/reserved_paths/`.

## Alternatives considered

**Full `.wh.` filename support via escaping** — rejected. A bijective escape
must be applied at every store boundary simultaneously: capture,
`write_layer_changes`, `MergedView` (`read_entry`, `is_whiteouted`,
`lookup_blocked_by_layer`, `visible_descendants`), `apply_layer`, squash
`flatten`, the file-op path mapper, and the planned export encode/apply. It
changes `layer_digest` inputs (`model/mod.rs:242-246`), requires a
storage-format version bump plus migration of every published layer, and
diverges from the OCI ecosystem, which reserves `.wh.` names in images. All
that to support a filename family whose only real-world producer is overlay
tooling itself.

**Reject `.wh.` components in `LayerPath::parse`** — deferred. It is the
deepest possible gate (unrepresentable-by-construction, same class as the
existing `..` rejection) and would cover reads too, but it changes the error
class of read paths (`not_found` → `invalid_path`), touches a vocabulary
type shared by every consumer, and buys no additional admission safety once
D1 is in: all durable writes flow through `plan_publish`. Revisit if a
bypassing write route ever appears.

**Capture-side protected drop (new `ProtectedPathDropReason` variant)** —
rejected as redundant vocabulary: drops other than `UnsupportedSpecialFile`
are already publish-fatal, so a drop reaches the same whole-changeset reject
as D1 while adding an enum variant in two crates, two mapping arms, and a
JSON name. Pass-through + admission reject is one predicate and one error
path.

**Fail-open per-file drop (FIFO-style `UnsupportedSpecialFile` treatment)**
— rejected: it silently discards user data (the `.wh.foo` content) to save
the rest of the changeset, which is precisely the class of silent behavior
this change removes. Fail closed, tell the user, let them rename.

## Open questions

1. **File-op error surfacing.** Sessionless `file_write` on a reserved path
   will surface `FileOperationError::LayerStack(PublishRejected{ProtectedPath})`.
   Decide at review whether the dispatch layer maps this to
   `invalid_request` (friendlier for a path-shaped error) or leaves the
   generic layerstack error class; the e2e catalog pins whichever is chosen.
2. **Seed-tree validation** (F6): add the same component check to
   `build_workspace_base` collection, or keep bootstrap operator-trusted and
   documented only.
