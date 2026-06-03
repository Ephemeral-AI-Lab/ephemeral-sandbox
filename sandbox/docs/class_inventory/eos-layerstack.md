# Crate `eos-layerstack` — Class Inventory

> Generated struct/enum/trait reference. Source of truth is the code under
> `sandbox/crates/eos-layerstack/src/` (or the crate's src dir). Item/field/variant/method
> data is extracted directly from the Rust source; one-line purposes come from
> `///` doc comments (or, where absent, a reviewer summary). Test-only items under
> `#[cfg(test)]` are excluded. This generated inventory is distinct from the
> hand-curated contract docs under `sandbox/docs/contract/`.

**22 items (17 structs, 4 enums, 0 traits, 1 type alias) across 7 files.**

`eos-layerstack` is the durable-truth layer of the `eosd` runtime: it owns the single linearization point (one mutable `manifest.json` over immutable, content-addressed layer directories swapped by an atomic pointer write) that ports the Python `backend/src/sandbox/layer_stack` subsystem. Its item groups cover the storage facade and merged read view (`LayerStack`, `MergedView`, `Lease`), the dual-set snapshot lease registry (`LeaseRegistry`, `LayerStackLeaseRecord`), non-destructive checkpoint squashing (`LayerCheckpointSquasher`, `SquashPlan`, `CheckpointSegment`, `SquashPlanEntry`), the dual-layer cross-process/in-process writer lock (`StorageWriterLockLease`, `ExclusiveGuard`, `ReentrantMutex`), workspace base construction and binding (`WorkspaceBaseBuild`, `WorkspaceBinding`, `BaseEntry`), and the error algebra (`LayerStackError`).

## Contents

- **`eos-layerstack/src/error.rs`** — `LayerStackError`
- **`eos-layerstack/src/lease.rs`** — `LayerStackLeaseRecord`, `LeaseRegistry`, `SharedLeaseRegistry`, `LayerRefKey`
- **`eos-layerstack/src/squash.rs`** — `CheckpointSegment`, `SquashPlanEntry`, `SquashPlan`, `LayerCheckpointSquasher`
- **`eos-layerstack/src/stack.rs`** — `Lease`, `MergedView`, `LayerStack`, `ProjectEntry`, `ProjectEntryKind`
- **`eos-layerstack/src/storage_lock.rs`** — `StorageWriterLockLease`, `ExclusiveGuard`, `LockRecord`, `ReentrantMutex`, `ReentrantState`
- **`eos-layerstack/src/workspace_base.rs`** — `WorkspaceBaseBuild`, `BaseEntry`
- **`eos-layerstack/src/workspace_binding.rs`** — `WorkspaceBinding`

---

## `eos-layerstack/src/error.rs`

#### `LayerStackError`  ·  _enum_  ·  derives: `Debug, Error`  ·  `#[non_exhaustive]`  ·  [L14]

Errors raised by the durable layer-stack storage layer.

**Variants**: `ManifestConflict { expected: i64, found: i64 }`, `StorageRootOwned(String)`, `StorageWriterLockClosed`, `InvalidLeaseOwner(String)`, `LockPoisoned(&'static str)`, `InvalidSquashPlan(String)`, `LayerIdAllocation`, `Manifest(String)`, `WorkspaceBinding(String)`, `Storage(String)`, `Cas(CasError)`, `Io(std::io::Error)`

---

## `eos-layerstack/src/lease.rs`

#### `LayerStackLeaseRecord`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq`  ·  [L28]

One active snapshot lease: an id bound to the frozen manifest it pins.

**Fields**

| name | type | vis |
|------|------|-----|
| `lease_id` | `String` | `pub` |
| `manifest` | `Manifest` | `pub` |

#### `LeaseRegistry`  ·  _struct_  ·  derives: `Debug, Default`  ·  [L39]

Tracks active snapshot leases and the layers they retain on disk (mirrors the Python `RLock` + `Counter[LayerRef]` refcount semantics).

**Fields**

| name | type | vis |
|------|------|-----|
| `leases` | `HashMap<String, LayerStackLeaseRecord>` |  |
| `refcounts` | `BTreeMap<LayerRefKey, usize>` |  |

<details><summary>Methods (6)</summary>

`new`, `acquire`, `release`, `leased_layers`, `lease_head_layers`, `active_count`

</details>

#### `SharedLeaseRegistry`  ·  _type alias_  ·  `= Arc<Mutex<LeaseRegistry>>`  ·  [L44]

Crate-internal shared handle to a per-root lease registry; leases outlive individual `LayerStack` values, so daemon paths reopen the registry by root.

#### `LayerRefKey`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, PartialOrd, Ord`  ·  [L165]

Ordered map key identifying a layer ref by `(layer_id, path)` for the refcount `BTreeMap`.

**Fields**

| name | type | vis |
|------|------|-----|
| `layer_id` | `String` |  |
| `path` | `String` |  |

---

## `eos-layerstack/src/squash.rs`

#### `CheckpointSegment`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq`  ·  [L33]

A foldable run of >=2 contiguous layers that collapse into one checkpoint.

**Fields**

| name | type | vis |
|------|------|-----|
| `layers` | `Vec<LayerRef>` | `pub` |

<details><summary>Methods (1)</summary>

`new`

</details>

#### `SquashPlanEntry`  ·  _enum_  ·  derives: `Debug, Clone, PartialEq, Eq`  ·  [L59]

One entry of a squash plan: either a kept single layer or a foldable segment.

**Variants**: `Keep(LayerRef)`, `Segment(CheckpointSegment)`

#### `SquashPlan`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq`  ·  [L71]

A computed squash plan: the active manifest snapshot plus the per-run entries (requires >=1 checkpoint segment).

**Fields**

| name | type | vis |
|------|------|-----|
| `active_version` | `i64` | `pub` |
| `active_layers` | `Vec<LayerRef>` | `pub` |
| `entries` | `Vec<SquashPlanEntry>` | `pub` |

<details><summary>Methods (2)</summary>

`new`, `checkpoint_segments`

</details>

#### `LayerCheckpointSquasher`  ·  _struct_  ·  derives: `Debug`  ·  [L134]

Plans runs between lease heads and projects each run into a checkpoint layer.

**Fields**

| name | type | vis |
|------|------|-----|
| `storage_root` | `PathBuf` |  |
| `view` | `MergedView` |  |

<details><summary>Methods (7)</summary>

`new`, `plan`, `build_checkpoint`, `relabel_checkpoint`, `discard_checkpoint`, `allocate_checkpoint_paths`, `layer_path`

</details>

---

## `eos-layerstack/src/stack.rs`

#### `Lease`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq`  ·  [L53]

Immutable result of an O(1) snapshot: a lease id plus the pinned manifest's existing on-disk layer paths (never a rendered tree).

**Fields**

| name | type | vis |
|------|------|-----|
| `lease_id` | `String` | `pub` |
| `manifest_version` | `i64` | `pub` |
| `root_hash` | `String` | `pub` |
| `manifest` | `Manifest` | `pub` |
| `layer_paths` | `Vec<String>` | `pub` |
| `timings` | `BTreeMap<String, f64>` | `pub` |

#### `MergedView`  ·  _struct_  ·  derives: `Debug`  ·  [L70]

Layered read view over a storage root's manifest (lowest→highest precedence); the pure-read sibling of the overlay mount.

**Fields**

| name | type | vis |
|------|------|-----|
| `storage_root` | `PathBuf` |  |

<details><summary>Methods (7)</summary>

`new`, `read_bytes`, `project`, `layer_dir`, `is_whiteouted`, `lookup_blocked_by_layer`, `apply_layer`

</details>

#### `LayerStack`  ·  _struct_  ·  derives: `Debug`  ·  [L284]

Durable storage facade for one layer-stack root; owns the manifest pointer, lease registry, merged read view, publisher, squasher, and the dual-layer storage-writer lease for its lifetime.

**Fields**

| name | type | vis |
|------|------|-----|
| `storage_root` | `PathBuf` |  |
| `writer_lock` | `StorageWriterLockLease` |  |
| `leases` | `SharedLeaseRegistry` |  |
| `view` | `MergedView` |  |

<details><summary>Methods (19)</summary>

`open`, `storage_root`, `read_active_manifest`, `acquire_snapshot`, `release_lease`, `can_squash`, `squash`, `leased_layers`, `lease_head_layers`, `active_lease_count`, `commit_to_workspace`, `read_bytes`, `read_text`, `publish_layer`, `allocate_layer_paths`, `layer_digest_path`, `head_layer_digest`, `write_layer_digest`, `commit_projection_dir`

</details>

#### `ProjectEntry`  ·  _struct_  ·  derives: `Debug`  ·  [L857]

One filesystem entry collected while projecting a layer directory: its absolute path, layer-relative path, and classified kind.

**Fields**

| name | type | vis |
|------|------|-----|
| `path` | `PathBuf` |  |
| `rel` | `PathBuf` |  |
| `kind` | `ProjectEntryKind` |  |

#### `ProjectEntryKind`  ·  _enum_  ·  derives: `Debug`  ·  [L864]

Classification of a projected layer entry (markers, whiteouts, and real content).

**Variants**: `Opaque`, `LogicalWhiteout`, `KernelWhiteout`, `Directory`, `File`, `Symlink`

---

## `eos-layerstack/src/storage_lock.rs`

#### `StorageWriterLockLease`  ·  _struct_  ·  derives: `Debug`  ·  [L53]

A held cross-process + in-process writer lease for one storage root; RAII releases the `flock` and closes the fd when the last lease for a root drops.

**Fields**

| name | type | vis |
|------|------|-----|
| `key` | `String` |  |

<details><summary>Methods (3)</summary>

`acquire`, `exclusive`, `drop`

</details>

#### `ExclusiveGuard`  ·  _struct_  ·  generics: `<'lease>`  ·  derives: `Debug`  ·  [L157]

In-process exclusive write guard; reentrant on the same thread to avoid the RLock-to-Mutex deadlock trap.

**Fields**

| name | type | vis |
|------|------|-----|
| `lock` | `Arc<ReentrantMutex>` |  |
| `_lease` | `PhantomData<&'lease StorageWriterLockLease>` |  |

<details><summary>Methods (1)</summary>

`drop`

</details>

#### `LockRecord`  ·  _struct_  ·  derives: `Debug`  ·  [L169]

Per-root registry record holding the locked `flock` file handle, the in-process refcount, and the shared reentrant mutex.

**Fields**

| name | type | vis |
|------|------|-----|
| `file` | `File` |  |
| `refcount` | `usize` |  |
| `mutex` | `Arc<ReentrantMutex>` |  |

#### `ReentrantMutex`  ·  _struct_  ·  derives: `Debug, Default`  ·  [L176]

Reentrant mutex (state + condvar) reproducing Python `threading.RLock` same-thread re-entry semantics.

**Fields**

| name | type | vis |
|------|------|-----|
| `state` | `Mutex<ReentrantState>` |  |
| `waiters` | `Condvar` |  |

<details><summary>Methods (2)</summary>

`lock`, `unlock`

</details>

#### `ReentrantState`  ·  _struct_  ·  derives: `Debug, Default`  ·  [L182]

Owner-thread and re-entry depth bookkeeping guarded by the reentrant mutex.

**Fields**

| name | type | vis |
|------|------|-----|
| `owner` | `Option<ThreadId>` |  |
| `depth` | `usize` |  |

---

## `eos-layerstack/src/workspace_base.rs`

#### `WorkspaceBaseBuild`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq`  ·  [L26]

Build result for a workspace base: the durable binding plus per-phase timings.

**Fields**

| name | type | vis |
|------|------|-----|
| `binding` | `WorkspaceBinding` | `pub` |
| `timings` | `BTreeMap<String, f64>` | `pub` |

#### `BaseEntry`  ·  _enum_  ·  derives: `Debug, Clone, PartialEq, Eq`  ·  [L32]

One inventoried source-tree entry (directory, file, or symlink) used to build the immutable base layer.

**Variants**: `Directory { path: String }`, `File { path: String, source_path: PathBuf, size: u64, content_hash: String }`, `Symlink { path: String, link_target: String }`

<details><summary>Methods (2)</summary>

`path`, `kind`

</details>

---

## `eos-layerstack/src/workspace_binding.rs`

#### `WorkspaceBinding`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq, Deserialize, Serialize`  ·  [L19]

Durable binding from a real workspace root to the layer-stack storage root; maps public tool paths onto layer-relative paths.

**Fields**

| name | type | vis |
|------|------|-----|
| `workspace_root` | `String` | `pub` |
| `layer_stack_root` | `String` | `pub` |
| `active_manifest_version` | `i64` | `pub` |
| `active_root_hash` | `String` | `pub` |
| `base_manifest_version` | `i64` | `pub` |
| `base_root_hash` | `String` | `pub` |

<details><summary>Methods (2)</summary>

`layer_path_from_relative`, `layer_path_from_absolute`

</details>
