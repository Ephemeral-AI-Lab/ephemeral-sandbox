# layer_stack — Harsh Architecture Review

**Scope:** `backend/src/sandbox/layer_stack/` (21 files, ~2,366 LOC across 7 sub-packages).
**Focus:** naming, folder/file layout, import-chain depth, extensibility, inheritance/interface use, future flexibility.
**Verdict (TL;DR):** Internals are carefully written and the storage semantics are clearly thought through. The packaging, type design, and extension story are not. The module is a procedural toolkit dressed up as 7 packages, with one god-class (`LayerStackManager`) at the entry and an OCI/overlayfs world-view leaked into every public type. None of it is "wrong" — but none of it is ready to grow.

---

## CRITICAL — extensibility / inheritance / interface

### C-1. `LayerChange` is a fake hierarchy implemented via `__new__` dispatch
`layer/change.py:34-71` defines `LayerChange` as a base class whose constructor returns a different subclass based on a string `kind`:

```python
def __new__(cls, path, kind=None, ...):
    if cls is not LayerChange:
        return super().__new__(cls)
    if kind == "delete":
        return object.__new__(DeleteLayerChange)
    if kind == "write":
        return object.__new__(WriteLayerChange)
    ...
def __init__(self, path, kind, content_hash, source_path):
    del path, kind, content_hash, source_path   # base eats args
```
Every subclass *also* defines `__init__` and a duplicate `kind: Literal[...]` field. This is the worst of three worlds:
- It’s not a proper ABC (no `abstractmethod`, no `Protocol`).
- It’s not a discriminated union (no `match`-on-kind).
- It’s a string-keyed factory pretending to be a class hierarchy.

The comment on line 35 calls it a "compatibility factory" — a hint it was patched in during migration, never cleaned up.

**Consequence:** adding a new layer-change type (e.g. `xattr`, `hardlink`, `chmod`, `chown`) requires editing **at least 6 places**:
1. `LayerChangeKind` Literal in `change.py:15`.
2. `LayerChange.__new__` dispatch in `change.py:54-62`.
3. A new `@dataclass(frozen=True, init=False)` subclass with hand-rolled validation.
4. `LayerPublisher._write_change` dispatch in `publisher.py:175-188`.
5. `_prepare_changes` dispatch in `publisher.py:229-266`.
6. `MergedView` index handling (`whiteouts` / `opaque_dirs` are hard-coded).

Plus **3 callers outside the module** that already `isinstance`-switch on the variants (`occ/merge/direct.py`, `occ/merge/gated.py`, `occ/routing/orchestrator.py`). Open-closed is dead.

**Fix:** make `LayerChange` a real ABC (or, better, a frozen dataclass of `LayerChangeKind | LayerChangeOp`) and put the per-kind write logic on the variant itself: `change.apply(layer_dir)`, `change.digest_into(hasher)`. Then `_write_change` and `_prepare_changes` reduce to one loop, and `MergedView` no longer encodes whiteout/opaque-dir as first-class concepts of the *change* type vs. the *index* type.

---

### C-2. `LayerStackManager` is a god class
`manager.py:69-296` (228 LOC) owns:

- process-wide writer lock (`_acquire_storage_writer_lock`),
- on-disk directory creation,
- the manifest file handle,
- an `RLock`,
- the `LeaseRegistry`,
- the `MergedView`,
- the `LayerPublisher`,
- the `SquashWorker`,
- commit-staging allocation / cleanup,
- layer GC (`_unreferenced_layers`, `_remove_layers`),
- transaction creation (`LayerStackTransaction`),
- 5 read passthroughs (`read_bytes`, `read_text`, `read_symlink`, `list_dir`, `materialize`) that just forward to `MergedView`,
- the head-publish facade (`publish_changes`).

It has no protocol, no constructor injection beyond `storage_root`, and no test seam. The five read passthroughs add zero value — they exist because the caller doesn’t want to know `MergedView` is a thing. That coupling means you can’t evolve `MergedView`'s signature without touching the manager.

`LayerStackTransaction` reaches into `manager._lock`, `manager._manifest_file`, `manager._publisher` directly — these are not even formally `friend` access; they’re `_`-private attributes punched through.

**Fix:** define and inject:
```python
class LayerStorage(Protocol):  # filesystem / S3 / inmem
    ...
class ManifestStore(Protocol):
    ...
class SnapshotMaterializer(Protocol):
    ...
class ChangePublisher(Protocol):
    ...
class LayerStackManager:
    def __init__(self, *, storage, manifest_store, materializer, publisher, ...):
```
That is the difference between "this is a module" and "this is a system component."

---

### C-3. Zero abstract interfaces, anywhere
Search of the module: **0 `abc.ABC`**, **0 `Protocol`**, **0 `@runtime_checkable`**, **1 injection seam** (`LayerPublisher(id_factory=...)`). Every collaborator is a concrete class.

This means:
- You cannot swap the FS backend without rewriting `LayerPublisher`, `SquashWorker`, `MergedView`, `WorkspaceBaseBuilder`. The overlayfs whiteout / opaque-dir vocabulary is baked into the *types*, not into a single backend module.
- You cannot mock `LayerStackManager` for an integration test without spinning up a real `tempdir`. Every test that touches OCC has to construct a real storage root.
- You cannot run a second backend in parallel (e.g. compare CRDT-merge against overlay-merge in eval) without forking the manager.

This module is the storage spine for `occ`, `overlay`, `runtime`, and `plugin` (35+ importing files). It deserves contracts.

---

### C-4. Manifest format has no version field
`manifest/model.py` serializes `{"version": int, "layers": [...]}`. `version` is the *manifest version counter* (monotonic CAS token), **not** a schema version. There is no `"schema_version": 1` and no `from_dict` upgrade path. Any non-trivial format change (per-layer digest field, content-addressable layer paths, layer kind taxonomy) needs a one-way migration that callers have to detect themselves.

`from_dict` already added a defensive WR-04/WR-08 check for torn writes — but there is no policy for "manifest written by a newer version of the daemon." Future-flexibility cost: real.

---

### C-5. Overlayfs vocabulary leaks into public types
`opaque_dir`, whiteout, "lowerdir" — these are kernel overlayfs concepts. They show up in:

- the `LayerChangeKind` Literal (`"opaque_dir"`),
- `PrepareWorkspaceSnapshotResult.lowerdir` (a public field name),
- `MergedView.materialize(link_ok=True)` (the comment explicitly justifies it w.r.t. overlay lowerdirs),
- the `WHITEOUT_PREFIX` / `OPAQUE_MARKER` constants exported by `view/merged.py` *and* re-exported from `layer/index.py` (two sources of truth).

If you ever back this with btrfs subvolumes, ZFS snapshots, OCI tar layers, or a CRDT/CAS store, you’re renaming public API. The merge semantics belong in **one** backend-specific implementation behind a protocol; the public type should be something kernel-agnostic ("hide this subtree below this layer", "shadow this path").

---

### C-6. `WORKSPACE_BASE_LAYER_ID = "L000001-base"` collides with allocated layer IDs
`workspace/base.py:31` hardcodes the base layer ID with the same `L{version:06d}-...` prefix that `_default_layer_id` uses for runtime layers (`publisher.py:214`). The base happens to be safe today because base is built first and runtime layers start at version 2 — but the *type system* doesn't enforce that. Use a distinct prefix (`B…-base`, like squash checkpoints in `maintenance/squash.py:107`), or namespace the base layer dir under `layers/base/` instead of `layers/L000001-base/`.

---

## HIGH — folder & file structure

### H-1. Seven packages for ~2.3K LOC, most holding a single file
```
layer_stack/
├── __init__.py                  23
├── filesystem.py                40
├── manager.py                  413
├── commit/
│   ├── __init__.py               1   ← "Layer-stack commit staging contracts."
│   └── staging.py               15   ← one frozen dataclass with 2 fields
├── lease/
│   ├── __init__.py               1
│   └── registry.py              76
├── maintenance/
│   ├── __init__.py               1
│   └── squash.py               107
├── view/
│   ├── __init__.py               1
│   └── merged.py               314
├── layer/
│   ├── __init__.py               0   ← empty, no docstring
│   ├── change.py               189
│   ├── index.py                 78
│   └── publisher.py            330
├── manifest/
│   ├── __init__.py              33
│   ├── model.py                 98
│   └── store.py                 57
└── workspace/
    ├── __init__.py               0
    ├── base.py                 439
    └── binding.py              150
```
`commit/staging.py` is **15 lines** for one dataclass. `lease/registry.py`, `maintenance/squash.py`, `view/merged.py` each hold exactly one module. The package-per-module pattern is dead weight: longer import paths, more `__init__.py` files to keep in sync, no real grouping benefit. `manifest/` and `workspace/` (and arguably `layer/`) are justified; the rest are over-foldered.

**Fix:** flatten to:
```
layer_stack/
├── __init__.py
├── _paths.py               (was filesystem.py)
├── change.py               (was layer/change.py)
├── index.py                (was layer/index.py)
├── publisher.py            (was layer/publisher.py)
├── manifest.py             (merge model.py + store.py)
├── view.py                 (was view/merged.py)
├── lease.py                (was lease/registry.py)
├── squash.py               (was maintenance/squash.py)
├── transaction.py          (extract LayerStackTransaction from manager.py)
├── manager.py
└── workspace/
    ├── __init__.py
    ├── base.py
    └── binding.py
```
That’s ~12 files instead of 21, no behaviour change, and the import paths drop one segment each.

---

### H-2. `LayerStackTransaction` lives in `manager.py`, not `transaction.py`
`manager.py:299-367`. The transaction directly mutates `manager._lock`, peeks at `manager._manifest_file`, and calls `manager._publisher.publish_layer_locked(...)`. Burying it in `manager.py` is what *lets* it punch through privacy. Move it to its own file with an explicit `_ManagerHandle` parameter passed in, and the friend-class smell becomes a real interface.

---

### H-3. `filesystem.py` is a generic name for a private helper
Three free functions (`join_layer_path`, `remove_path`, `resolve_storage_path`). The name `filesystem` collides with the concept name globally — anyone reading `from sandbox.layer_stack.filesystem import ...` would assume there's a `Filesystem` class. Rename to `_paths.py` or `internal/paths.py` and mark `__all__` defensively.

---

### H-4. `view/merged.py` re-exports constants it imports
`view/merged.py:22` exports `OPAQUE_MARKER` and `WHITEOUT_PREFIX` in `__all__` even though it imports them from `layer.index`. Now there are **two** import paths for the same constant. Pick one canonical source (`layer.index`) and stop re-exporting.

---

### H-5. `LayerStackStorageError` is defined in `view/merged.py`
A *storage* domain error living inside the *view* module is a layering inversion. It belongs in `manifest/` (where `ManifestConflictError` already lives) or a dedicated `errors.py`. Currently any module that wants to catch it has to `from sandbox.layer_stack.view.merged import LayerStackStorageError` — coupling read implementation details into error-handling sites.

---

## MEDIUM — naming inconsistencies

### M-1. Suffix taxonomy is incoherent
- `LayerStackManager` (manager)
- `LayerPublisher` (publisher)
- `LeaseRegistry` (registry)
- `SquashWorker` (worker)
- `MergedView` (view)
- `LayerStackTransaction` (transaction)
- `CommitStagingArea` (area)

Six distinct suffixes for what are functionally just service objects. Pick a convention (e.g. `LeaseStore`, `SquashService`, `MergeReader`) and stick to it. Right now `Worker`, `Publisher`, and `Manager` are aspirational; nothing about `SquashWorker` is more "worker-y" than `LayerPublisher`.

### M-2. `staging.py` is too vague
`commit/staging.py` defines `CommitStagingArea`. The file name `staging.py` already exists conceptually in two other places (publisher's `staging_dir`, manager's `STAGING_DIR`). A reader can’t tell from `from sandbox.layer_stack.commit.staging import ...` whether they’re importing a class, a path, or a function. Rename to `commit_staging_area.py` or move into `manifest/` next to `STAGING_DIR`.

### M-3. `publish_layer_locked` leaks contract into the name
`LayerPublisher.publish_layer_locked(...)` — the `_locked` suffix means "caller must hold the manager lock." That’s a contract, not a method name. The lock-holding caller is `LayerStackTransaction.publish_layer`, which already proves it holds the lock by being inside `with manager._lock`. A cleaner design passes a `LockToken` value (newtype) that only `LayerStackTransaction.__enter__` can produce, and `publish_layer` accepts it; the lock contract is then enforced by the type system, not by docstrings and naming convention.

### M-4. `_safe_request_part` and `_log_rmtree_failure` are module-level in `manager.py`
Both are utility free functions buried after class definitions. The first is reused (`prepare_workspace_snapshot`, `allocate_commit_staging`). Move into `_paths.py` next to the path utilities; manager.py is already 413 lines.

### M-5. Idempotency timing key is double-recorded
`publisher.py:218-226` writes both `layer_stack.publish.prepare_changes_s` and `layer_stack.publish.digest_check_s` with the same elapsed value. Either the names mean different things and the implementation is wrong, or they mean the same thing and one of them is dead. Pick one.

### M-6. `LayerChangeKind = Literal["write", "delete", "symlink", "opaque_dir"]` mixed conventions
Three of the four are verbs (`write`, `delete`, `symlink`); `opaque_dir` is a noun. Either all verbs (`opaque`) or all describe the artifact (`file_write`, `path_delete`, `symlink_create`, `opaque_dir`). Inconsistency makes the discriminator harder to scan.

### M-7. `link_ok` is a bad keyword
`MergedView.materialize(..., link_ok=True)`. Two interpretations: "is linking allowed?" vs. "did linking succeed?" The actual meaning is closer to "use hardlinks instead of copies." `share_inodes=True` or `hardlink=True` would be unambiguous.

---

## MEDIUM — import / dependency chain

### M-8. Inconsistent depth in import paths
Internal consumers reach into both `sandbox.layer_stack.manifest` (the package façade) and `sandbox.layer_stack.manifest.model` (the implementation file). E.g. `manifest/store.py:9` imports `Manifest, empty_manifest` from `.model` — but external callers go through the façade. Pick one: either the façade is real (and `model.py` becomes `_model.py`) or it isn’t.

### M-9. `__init__.py` exports a partial vocabulary
`sandbox/layer_stack/__init__.py` re-exports `LayerChange`, `LayerRef`, `Manifest`, `ManifestConflictError`, `LayerStackManager`, `PrepareWorkspaceSnapshotResult`. **Missing:**
- The four variants (`WriteLayerChange`, `DeleteLayerChange`, `SymlinkLayerChange`, `OpaqueDirLayerChange`) — but callers need them for `isinstance` dispatch.
- `LayerDelta`, `aggregate_layer_changes` (exported as if public, never re-exported here, never used internally).
- `LayerStackStorageError` (callers need to catch it).
- `WorkspaceBinding`, `WorkspaceBindingError`, `require_workspace_binding`.
- `CommitStagingArea`.
- `LayerStackTransaction`.

Callers therefore hop into deep paths anyway. The façade is decorative.

### M-10. `aggregate_layer_changes` + `LayerDelta` are dead/speculative
Defined in `change.py:176-189`, exported via `__all__`, **never imported inside `layer_stack/`** and only imported in two OCC files (which themselves chain into the publisher’s already-deduplicating loop). Either the dedup logic should live in `_prepare_changes` (it does — see `publisher.py:108-110`) or this should be deleted. Two implementations of "last-write-wins per path" inside the same module is asking for divergence.

### M-11. 35+ external import sites with no narrowed public surface
`grep -l "from sandbox.layer_stack"` returns 35 files across `occ/`, `overlay/`, `runtime/daemon/`, `plugin/`. They reach into:
- `sandbox.layer_stack.manifest.model` (1 site, leaks the split)
- `sandbox.layer_stack.layer.change` (`normalize_layer_path`, `LayerChange`, `LayerDelta` — primitives that should sit at the root)
- `sandbox.layer_stack.workspace.binding` (`require_workspace_binding` — domain-level entry point that should be at the root)
- `sandbox.layer_stack.manager` directly (bypassing the `__init__.py`)

You have no firewall between *implementation* modules and *consumer* modules. Add an `__all__` in every leaf module, document the canonical import path in the facade, and ratchet down to one segment for the 10 most common symbols.

---

## LOW — assorted

### L-1. `__init__.py` files are inconsistent
- `commit/__init__.py`: docstring only (1 line).
- `lease/__init__.py`: docstring only (1 line).
- `maintenance/__init__.py`: docstring only (1 line).
- `view/__init__.py`: docstring only (1 line).
- `layer/__init__.py`: completely empty.
- `workspace/__init__.py`: completely empty.
- `manifest/__init__.py`: real façade (33 lines).
- `layer_stack/__init__.py`: real façade (23 lines).

Pick a convention: either every package gets a façade, or none do.

### L-2. `WorkspaceBaseIncompleteError` extends `WorkspaceBindingError`
`workspace/base.py:38`. The "incomplete" error is about the *base build*, not about a *binding*. The inheritance is convenient for catch-blocks but semantically wrong — a binding may be intact while the base build is incomplete, and vice versa.

### L-3. `LayerStackManager._unreferenced_layers` sorts candidates by `layer_id`
`manager.py:283`. Sort order matters because `_remove_layers` calls `MergedView.evict_layer_index` in iteration order, but the actual deletion order is irrelevant to correctness (each layer dir is independent). Either document *why* the order matters, or stop sorting and skip the allocation.

### L-4. Magic constant retry loops
`publisher.py:167` and `squash.py:88` both retry up to 100 times to find an unused layer ID. The retry-on-collision pattern is identical; extract `_allocate_unique_layer_paths(prefix_fn, …)` and share it. Right now any bugfix has to be applied twice.

### L-5. `_PreparedLayerChange` is module-private but the dispatch is type-checked
`publisher.py:36-39` defines a private dataclass that only `LayerPublisher` uses. Fine — but `_prepare_changes` (line 229) is also module-level and accepts a `Sequence[LayerChange]`. Make `_PreparedLayerChange` nested inside `LayerPublisher`, or stop pretending the dispatch is private — there’s no encapsulation boundary today.

### L-6. `WorkspaceBinding.relative_layer_path` has dual semantics
`workspace/binding.py:51-67`. It accepts both "repo-relative" paths and "workspace-absolute" paths, dispatching on a leading `/`. That’s a parser hidden inside an accessor, with no name signal. Split into `path_from_relative()` and `path_from_absolute()`, or accept only one shape and force the caller to convert.

### L-7. Threading lock cache is module-global
`manager.py:40-41` holds `_STORAGE_WRITER_LOCKS: dict[str, int]` and `_STORAGE_WRITER_LOCKS_LOCK` at module scope. Two `LayerStackManager(storage_root=...)` calls in the same process for the same path return the same `fd`, but the cache is never *cleaned up* when a manager is garbage-collected. In a long-running process that opens many transient stacks (eval harnesses), the fd leak is real.

### L-8. Tests would benefit from an in-memory backend
None exists. Every unit test against `layer_stack` has to use `tempfile.mkdtemp`. An `InMemoryLayerBackend` is the smallest extensibility win that pays off in test speed and stability immediately.

---

## Summary of fix order

| Priority | Change | Effort | Payoff |
|---|---|---|---|
| 1 | Real ABC/Protocol for `LayerChange`; move per-kind logic onto variants. Remove `__new__` factory. | M | unlocks adding `chmod`/`xattr`/`hardlink` without 6-site edits |
| 2 | Split `LayerStackManager`: extract `LayerStackTransaction`, `LayerStorage`, `ManifestStore` protocols. | M-L | test seam, multi-backend, multi-host story |
| 3 | Flatten over-packaged dirs: `commit/`, `lease/`, `maintenance/`, `view/` → flat files | S | shorter imports, less ceremony |
| 4 | Make `__init__.py` the single canonical import surface (LayerChange variants, errors, binding, transaction). | S | external sites stop reaching into `*.model` / `*.binding` / `*.merged` |
| 5 | Introduce `schema_version` in manifest JSON; document migration policy. | S | future-proof binary compat |
| 6 | Rename overlayfs-specific public types to backend-agnostic names; isolate kernel vocabulary in one backend module. | M | btrfs/ZFS/OCI swap-in story |
| 7 | Drop `WORKSPACE_BASE_LAYER_ID` prefix collision; use `B…-base` like squash checkpoints. | S | invariant in code, not in convention |
| 8 | Consolidate retry-on-collision loop between `publisher` and `squash`. | S | one bug-fix site |
| 9 | Decide: `LayerDelta`/`aggregate_layer_changes` are public or dead. | S | one source of truth for dedup |

---

**Bottom line.** The module reads like a careful procedural rewrite of overlayfs semantics, then *file-system-organised* into packages as if each free function were a service. The hard part — concurrency, atomicity, manifest CAS — is done well. The soft part — types as contracts, packages as boundaries, the public-vs-private firewall — is missing. Today that costs you nothing because there is exactly one storage backend, one process, and one OS. The minute any of those plurals show up, the rewrite cost is "everything inherits from a Literal."
