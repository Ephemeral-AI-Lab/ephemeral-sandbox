# Crate `eos-occ` — Class Inventory

> Generated struct/enum/trait reference. Source of truth is the code under
> `sandbox/crates/eos-occ/src/` (or the crate's src dir). Item/field/variant/method
> data is extracted directly from the Rust source; one-line purposes come from
> `///` doc comments (or, where absent, a reviewer summary). Test-only items under
> `#[cfg(test)]` are excluded. This generated inventory is distinct from the
> hand-curated contract docs under `sandbox/docs/contract/`.

**20 items (11 structs, 4 enums, 5 traits, 0 type aliases) across 4 files.**

`eos-occ` is the optimistic-concurrency publish layer of the eosd runtime: it owns the MF-1 single-writer publish DECISION gate, batching N disjoint file-API writes into one manifest CAS attempt per `layer_stack_root`. Its main item groups are route classification and per-path outcomes (`Route`, `OccStatus`, `PublishDecision`, `FileResult`, `ChangesetResult`), the single-writer commit queue with its inverted transaction port (`CommitQueue`, `CommitTransactionPort`, `PreparedChangeset`, `PublishConflict`), the changeset-preparing service plus maintenance/route-provider/daemon-accessor ports (`OccService`, `MaintenancePolicy`, `LayerSquashPort`, `OccRouteProvider`, `AutoSquashMaintenancePolicy`, `OccRuntimeServicesPort`), and the crate-local error algebra (`OccError`).

## Contents

- **`eos-occ/src/commit_queue.rs`** — `PreparedChangeset`, `CommitTransactionPort`, `PublishConflict`, `WorkItem`, `QueueItem`, `CommitQueue`, `CommitWorker`
- **`eos-occ/src/error.rs`** — `OccError`
- **`eos-occ/src/route.rs`** — `Route`, `OccStatus`, `PublishDecision`, `FileResult`, `ChangesetResult`
- **`eos-occ/src/service.rs`** — `MaintenancePolicy`, `LayerSquashPort`, `OccRouteProvider`, `AllGatedRouteProvider`, `AutoSquashMaintenancePolicy`, `OccService`, `OccRuntimeServicesPort`

---

## `eos-occ/src/commit_queue.rs`

#### `PreparedChangeset`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq`  ·  [L59]

A routed changeset ready for the publish transaction (one `PublishDecision` per disjoint normalized path plus typed changes; `atomic` requires every path to validate before any path lands).

**Fields**

| name | type | vis |
|------|------|-----|
| `snapshot_version` | `Option<u64>` | `pub` |
| `path_groups` | `Vec<PublishDecision>` | `pub` |
| `changes` | `Vec<LayerChange>` | `pub` |
| `atomic` | `bool` | `pub` |

#### `CommitTransactionPort`  ·  _trait_  ·  supertraits: `Send`  ·  [L78]

The publish-transaction half of the layer-stack port the queue drives; the daemon injects the layer-stack-backed implementation that revalidates the CAS base and publishes a new manifest version.

<details><summary>Methods (1)</summary>

`revalidate_and_publish`

</details>

#### `PublishConflict`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq`  ·  [L95]

Signals a manifest CAS mismatch (`ManifestConflictError`) so the writer retries against the fresh base.

**Fields**

| name | type | vis |
|------|------|-----|
| `observed_version` | `Option<u64>` | `pub` |

#### `WorkItem`  ·  _struct_  ·  [L102]

One unit of work on the single-writer queue: a prepared changeset plus the reply channel the submitter awaits.

**Fields**

| name | type | vis |
|------|------|-----|
| `prepared` | `PreparedChangeset` |  |
| `reply` | `mpsc::Sender<Result<ChangesetResult, OccError>>` |  |
| `enqueued_at` | `Instant` |  |

#### `QueueItem`  ·  _enum_  ·  [L109]

Either real work or the stop sentinel that drains and exits the worker.

**Variants**: `Work(WorkItem)`, `Stop`

#### `CommitQueue`  ·  _struct_  ·  generics: `<T: CommitTransactionPort + 'static>`  ·  [L118]

Serializes OCC publishes while batching disjoint prepared changesets; owns the `mpsc` producer half, with the consumer half moved into the spawned `occ-commit-queue` thread on `start`.

**Fields**

| name | type | vis |
|------|------|-----|
| `sender` | `mpsc::Sender<QueueItem>` |  |
| `receiver` | `Mutex<Option<mpsc::Receiver<QueueItem>>>` |  |
| `transaction` | `Mutex<Option<T>>` |  |
| `handle` | `Option<std::thread::JoinHandle<()>>` |  |
| `max_batch_size` | `usize` |  |
| `batch_window_s` | `f64` |  |
| `max_cas_retries` | `u32` |  |
| `closed` | `bool` |  |

<details><summary>Methods (6)</summary>

`new`, `with_config`, `start`, `close`, `submit`, `commit_batch`

</details>

#### `CommitWorker`  ·  _struct_  ·  generics: `<T: CommitTransactionPort + 'static>`  ·  [L129]

The dedicated single-writer consumer: blocks for the first item, non-blocking-drains the rest, pays the batch window only with headroom, then commits disjoint batches.

**Fields**

| name | type | vis |
|------|------|-----|
| `receiver` | `mpsc::Receiver<QueueItem>` |  |
| `transaction` | `T` |  |
| `max_batch_size` | `usize` |  |
| `batch_window_s` | `f64` |  |
| `max_cas_retries` | `u32` |  |

<details><summary>Methods (1)</summary>

`run`

</details>

---

## `eos-occ/src/error.rs`

#### `OccError`  ·  _enum_  ·  derives: `Debug, thiserror::Error`  ·  `#[non_exhaustive]`  ·  [L12]

Errors raised by the OCC publish path (crate-local `thiserror` algebra; no `Box<dyn Error>` in the public API).

**Variants**: `QueueClosed`, `QueueNotStarted`, `WorkerStart(String)`, `WorkerPanicked`, `QueueStatePoisoned(&'static str)`, `ReplyDisconnected`, `CasRetryExhausted { attempts: u32 }`, `InvalidOverlayChange { path: String, reason: String }`, `RoutePreparation(String)`, `Cas(CasError)`

---

## `eos-occ/src/route.rs`

#### `Route`  ·  _enum_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize`  ·  `#[non_exhaustive]`  ·  [L19]

Where a single normalized path is routed during preparation; wire strings are exact (`gated`/`direct`/`drop`/`reject`).

**Variants**: `Gated`, `Direct`, `Drop`, `Reject`

#### `OccStatus`  ·  _enum_  ·  derives: `Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize`  ·  `#[non_exhaustive]`  ·  [L40]

Terminal per-path status after the publish transaction resolves; wire strings are exact and include `aborted_version` (the stale-base outcome when the CAS retry budget is exhausted).

**Variants**: `Accepted`, `Committed`, `AbortedVersion`, `AbortedOverlap`, `Dropped`, `Rejected`, `Failed`

<details><summary>Methods (2)</summary>

`is_published`, `is_success`

</details>

#### `PublishDecision`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq`  ·  [L83]

The route + reason a preparer assigned to one normalized path (the per-path half of a `PublishDecision`/changeset input the commit queue consumes).

**Fields**

| name | type | vis |
|------|------|-----|
| `path` | `LayerPath` | `pub` |
| `route` | `Route` | `pub` |
| `base_hash` | `Option<String>` | `pub` |
| `message` | `Option<String>` | `pub` |

#### `FileResult`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq, Eq`  ·  [L99]

Terminal outcome for one path after the publish transaction.

**Fields**

| name | type | vis |
|------|------|-----|
| `path` | `LayerPath` | `pub` |
| `status` | `OccStatus` | `pub` |
| `message` | `String` | `pub` |

#### `ChangesetResult`  ·  _struct_  ·  derives: `Debug, Clone, PartialEq`  ·  [L110]

Aggregate result of a published (or aborted) changeset, with per-path outcomes, the produced manifest version, and Python-compatible `occ.commit.*` timings.

**Fields**

| name | type | vis |
|------|------|-----|
| `files` | `Vec<FileResult>` | `pub` |
| `published_manifest_version` | `Option<u64>` | `pub` |
| `timings` | `BTreeMap<String, f64>` | `pub` |

<details><summary>Methods (1)</summary>

`success`

</details>

---

## `eos-occ/src/service.rs`

#### `MaintenancePolicy`  ·  _trait_  ·  [L26]

Post-publish maintenance hook run after a successful OCC commit (mirrors the Python `MaintenancePolicy` Protocol; implementations are synchronous).

<details><summary>Methods (1)</summary>

`after_publish_sync`

</details>

#### `LayerSquashPort`  ·  _trait_  ·  [L40]

Layer-stack squash capability consumed by `AutoSquashMaintenancePolicy`; a narrow maintenance interface implemented by the daemon's layer-stack-backed adapter.

<details><summary>Methods (2)</summary>

`can_squash`, `squash`

</details>

#### `OccRouteProvider`  ·  _trait_  ·  supertraits: `Send + Sync`  ·  [L58]

Route/base-hash provider used while preparing OCC changesets; the daemon owns the concrete layer-stack/gitignore implementation since this crate must not know daemon workspace bindings.

<details><summary>Methods (2)</summary>

`is_ignored`, `base_hash`

</details>

#### `AllGatedRouteProvider`  ·  _struct_  ·  derives: `Debug`  ·  [L75]

The conservative default route provider: routes every non-`.git` path as gated with an unknown base hash, giving unit tests and custom queues a safe default.

<details><summary>Methods (2)</summary>

`is_ignored`, `base_hash`

</details>

#### `AutoSquashMaintenancePolicy`  ·  _struct_  ·  generics: `<S: LayerSquashPort>`  ·  [L93]

Synchronous layer-stack squash after successful publishes; each policy owns its own squash lock and re-reads the active manifest under the lock before deciding.

**Fields**

| name | type | vis |
|------|------|-----|
| `squasher` | `S` |  |
| `max_depth` | `u32` |  |

<details><summary>Methods (2)</summary>

`new`, `after_publish_sync`

</details>

#### `OccService`  ·  _struct_  ·  generics: `<T: CommitTransactionPort + 'static>`  ·  [L123]

Prepares typed OCC changesets and commits them through the single writer; holds the per-root `CommitQueue` and an optional maintenance policy (exactly one `OccService` per `layer_stack_root`, the MF-1 owner).

**Fields**

| name | type | vis |
|------|------|-----|
| `commit_queue` | `CommitQueue<T>` |  |
| `route_provider` | `Arc<dyn OccRouteProvider>` |  |

<details><summary>Methods (8)</summary>

`new`, `with_route_provider`, `apply_changeset`, `apply_changeset_with_base_hashes`, `prepare_changeset`, `prepare_changeset_with_base_hashes`, `apply_prepared_changeset`, `drop`

</details>

#### `OccRuntimeServicesPort`  ·  _trait_  ·  [L340]

Inverted daemon accessor: the OCC runtime-services bundle keyed per root; implementations MUST return the same bundle (and thus the same queue + storage lease) for a given `layer_stack_root`, never a second writer.

<details><summary>Methods (1)</summary>

`occ_runtime_services`

</details>
