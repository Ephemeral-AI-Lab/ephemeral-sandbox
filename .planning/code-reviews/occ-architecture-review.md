# OCC Package — Architecture Review (Harsh)

**Scope:** `backend/src/sandbox/occ/` (2,975 LOC, 18 source files, 6 subpackages)
**Lens:** naming, folder/file structure, import-chain length, extensibility, inheritance/interface design, future flexibility.
**Tone:** harsh, as requested. Severity tags: **C** critical, **H** high, **M** medium, **L** low.

---

## TL;DR

The package works, and the *ports* layer (`ports.py`) shows the right instincts. Almost everything else is held together by `isinstance` chains, half-applied prefixes, two parallel routers, and constructor inheritance instead of injection. Three things kill the architecture before anything else gets a hearing:

1. **`isinstance(change, …)` cascades in both `DirectMerge` and `GatedMerge`.** A new `Change` kind requires editing two stagers, the orchestrator, the overlay capture, and the typed `Change` hierarchy. No double-dispatch, no visitor, no `change.apply(stager)` — every new change kind is an open-closed violation by construction.
2. **`Occ` / `OCC` prefix is half-applied and inconsistent** inside a package literally named `occ`. The prefix is noise *everywhere it appears*, and where it doesn't appear the asymmetry is worse.
3. **Two parallel routers (`OccOrchestrator` + `prepare_single_path_changeset`)** with overlapping logic and overlapping protocols (`GitignoreMatcher` vs `SnapshotIgnoreOracle`). The fast path is a copy-paste of the slow path with one fewer loop.

The package is not unsalvageable — it's three weekends of disciplined refactoring away from being clean. But every claim below is something an extensibility-conscious reviewer should flag.

---

## 1. Naming — Inconsistent, Sometimes Misleading [H]

### 1.1 The `Occ` / `OCC` schizophrenia [H]
Inside the `occ` package, classes split into **three** prefix styles:

| Prefix | Class | File |
|---|---|---|
| `Occ` (Pascal) | `OccService`, `OccCommitTransaction`, `OccSerialMerger`, `OccOrchestrator`, `OccLayerStackPorts` | service.py, commit_transaction.py, merge/serial.py, routing/orchestrator.py, ports.py |
| `OCC` (all caps) | `OCCClient`, `OCCMutationService` | client.py |
| No prefix | `DirectMerge`, `GatedMerge`, `PreparedChangeset`, `CommitOptions`, `ContentHasher`, `RouteDecision`, `Change`, `WriteChange`, `EditChange`, `DeleteChange`, `SymlinkChange`, `OpaqueDirChange`, `FileResult`, `FileStatus`, `PathspecGitignoreOracle`, `SnapshotGitignoreOracle` | everywhere else |

Two reasonable answers: drop the prefix entirely (it's in a package named `occ`), or apply it uniformly. The current state is neither. `from sandbox.occ.client import OCCClient` and `from sandbox.occ.service import OccService` in the same module is a code smell shouting at the reader.

### 1.2 Misleading class names [H]
- **`OccOrchestrator`** does not orchestrate. It routes a sequence of changes to `RouteDecision`s and attaches base hashes. It's a **Router**. Calling it an orchestrator inflates its role and steals the name from `OccCommitTransaction` (which actually orchestrates).
- **`OccSerialMerger`** does not merge. It is a **commit queue** with batch coalescing and CAS retry. The merging happens in `OccCommitTransaction.revalidate_and_publish` (which delegates to `DirectMerge`/`GatedMerge`). Three things called "merge", none of which merge in any data-structure sense.
- **`DirectMerge` / `GatedMerge`** don't merge either. They validate and stage. Better names: `DirectStager` / `GatedStager`, or `DirectPolicy` / `GatedPolicy` (if the intent is to dispatch on `RouteDecision`).
- **`result_projection.py`** uses jargon ("projection") for what is plainly a small pile of formatting helpers. Naming this `result_formatting.py` or `result_view.py` would tell a future reader the truth.

### 1.3 Enum value with a double-negative [M]
`RouteDecision.OCC_SKIPPED_MERGE` literally means "this OCC change *skipped* the merge and went through the direct path". The identity of the value is described by what it is *not*. `DIRECT` and `GATED` (or `BYPASS_MERGE` / `MERGE`) carry the same information without forcing the reader to mentally invert. The `OCC_` prefix inside `RouteDecision` is also redundant — the enum lives in `sandbox.occ.changeset.prepared.RouteDecision`.

### 1.4 Misleading types
- `OccLayerStackPorts(SnapshotReader, CommitStagingStore, CommitPublisher, Protocol)` declares itself in its docstring as "Combined in-process migration shape for the current OCC service." That's a polite way to write *"this is the wrong abstraction, we know, please fix later."* A name like `_InProcessLayerStackPorts` and a `# DEPRECATED` flag would at least mark the debt. Right now it looks like a stable API.
- `_FinalKind = Literal["write", "delete", "symlink", "opaque_dir"]` is defined inside `merge/direct.py` and *not* inside `merge/gated.py`, which open-codes the same set. Either both should use it or it should live in `changeset/types.py`.

### 1.5 Naming low-impact nits [L]
- `routing/runtime_ops.py` — "runtime_ops" is a junk-drawer name. The file has two utility functions, both about content hashing on a snapshot. It should not exist as a separate file (more on this in §2).
- `_kept_children_for(rel, …)` — `rel` is unclear without reading the function. Use `dir_path`.
- `final_special_change` (in gated.py) is hidden state with a non-obvious lifecycle.
- `_RESULT_READY_AT = "_occ.serial.result_ready_at_s"` — the leading underscore convention for "internal timing key" is invented here and used nowhere else.

---

## 2. Folder / File Structure — No Coherent Principle [H]

### 2.1 Top-level vs. subpackages [H]
Top of `occ/`:

```
occ/
├── __init__.py          # 4 lines, docstring only
├── client.py            # 71 lines
├── commit_transaction.py# 426 lines  ← biggest file, top-level
├── ports.py             # 100 lines
├── result_projection.py # 90 lines
├── service.py           # 366 lines
├── capture/             # 1 module
├── changeset/           # 4 modules
├── content/             # 2 modules
├── merge/               # 3 modules + facade
└── routing/             # 3 modules
```

The principle is unclear. Why is `commit_transaction.py` at the top while `direct.py` and `gated.py` (which it dispatches into) live under `merge/`? Why is `result_projection.py` a top-level module while `capture/overlay.py` (also an adapter) is a subpackage? Why does `routing/` contain `single_path.py` (a fast-path duplicate of the slow path) and `runtime_ops.py` (two hash helpers)?

A more honest layout, given the actual coupling:

```
occ/
├── __init__.py             # re-export the few real entry points
├── ports.py                # ← the contract surface
├── service.py              # ← the assembly point
├── client.py               # ← workspace-bound facade
├── changeset/              # immutable types only
│   ├── types.py            # Change + FileResult + FileStatus + ChangesetResult
│   ├── builders.py         # factory functions
│   └── prepared.py         # PreparedChangeset, PreparedPathGroup, CommitOptions, RouteDecision
├── route/                  # renamed from "routing"
│   ├── router.py           # one router, batch + single-path branches inside
│   └── overlay.py          # ex capture/overlay.py — overlay→Change adapter
├── stage/                  # renamed from "merge"
│   ├── policy.py           # MergePolicy Protocol
│   ├── direct.py
│   ├── gated.py
│   └── transaction.py      # ex commit_transaction.py
├── queue/                  # ex merge/serial.py + _CoalescedSquashState
│   └── serial.py
└── content/
    ├── hashing.py
    └── gitignore.py        # ex content/gitignore_oracle.py
```

### 2.2 Empty / asymmetric `__init__.py` files [M]
- `routing/__init__.py` — empty.
- `capture/__init__.py` — empty.
- `merge/__init__.py` — re-exports `DirectMerge` and `GatedMerge` but **not** `OccSerialMerger` (which lives in the same package). Asymmetric.
- `changeset/__init__.py` — re-exports everything from the two siblings; this is the only honest one.
- `content/__init__.py` — re-exports content classes but `gitignore_oracle` is its own file, breaking the convention.

Either commit to flat re-exports through `__init__.py` (so importers write `from sandbox.occ.merge import GatedMerge`) or commit to deep imports. Don't half-implement both.

### 2.3 Two top-level files that don't belong here [M]
- **`commit_transaction.py` (426 LOC)** — by content this is a *staging + publish* engine and a private `_LayerChangeStager` helper. It belongs under `merge/` (or, better, `stage/transaction.py`).
- **`result_projection.py`** — caller-facing presentation helpers (`committed_paths`, `conflict_and_status`, `conflict_to_dict`, `gitignore_cache_timings`). It exists to bridge `ChangesetResult` to `ConflictInfo` (an external model from `sandbox.models`). This is a leak: the *engine* package is producing presentation for a specific consumer (`request_context.py`, `shell_runner.py`). It should live with those consumers, not in the engine.

### 2.4 Two parallel routers [H]
`routing/orchestrator.py::OccOrchestrator.prepare_sync` and `routing/single_path.py::prepare_single_path_changeset` are two implementations of the same logic. The "fast path" exists because the slow path materializes a workspace-wide gitignore evaluator, but the duplication is unforced — the single-path version could be a branch inside the orchestrator with `len(changes) == 1` fast-exit. Right now `single_path.py` imports private helpers (`requires_base_hash`, `attach_base_hash`, `BaseHashReader`) from `orchestrator.py`, so the modules are already mutually entangled. Worse, they require two different protocols (`GitignoreMatcher` vs `SnapshotIgnoreOracle`) for what is almost the same query.

---

## 3. Import Dependency Chain — Long Paths, Cross-Wired [M]

### 3.1 Path depth [M]
- `from sandbox.occ.changeset.prepared import …` — 4 segments
- `from sandbox.occ.content.gitignore_oracle import SnapshotGitignoreOracle` — 4 segments, 32-char tail
- `from sandbox.occ.routing.runtime_ops import infer_manifest_base_hash` — 4 segments
- `from sandbox.occ.capture.overlay import overlay_path_changes_to_occ_changes` — 4 segments + 41-char symbol

`service.py` alone has 8 distinct `sandbox.occ.*` imports just to wire dependencies. The problem is not depth per se — it's that the depth buys nothing because each leaf directory has 1–3 files. `content/`, `capture/`, `routing/` should each be a single module:

| Current | Proposed |
|---|---|
| `content/hashing.py` + `content/gitignore_oracle.py` | `content.py` (or split: `hashing.py` + `gitignore.py` at top) |
| `capture/overlay.py` | `overlay.py` |
| `routing/runtime_ops.py` (31 LOC) | merge into `content/hashing.py` or `route/router.py` |
| `routing/orchestrator.py` + `routing/single_path.py` | `route/router.py` |

This flattens four directories and shortens every import.

### 3.2 The 4-level-deep `routing/runtime_ops.py` is the worst offender [M]
- File is 31 LOC.
- Contents are two functions (`content_hash_bytes`, `infer_manifest_base_hash`).
- `content_hash_bytes` simply forwards to `ContentHasher().hash_bytes()` — wraps a wrapper.
- It is imported from two places (`service.py` and `runtime/daemon/handler/tools/write.py`).

This file should not exist. `content_hash_bytes` is a one-line free function; `infer_manifest_base_hash` belongs in `content/hashing.py` or the router.

### 3.3 Internal cross-wiring [M]
The package has no DAG-enforcing convention. `routing/single_path.py` imports from `routing/orchestrator.py`; both import from `content/` and `changeset/`. `service.py` imports from every subpackage. `merge/serial.py` imports `commit_transaction.py` (top-level), which imports from `merge/direct.py` and `merge/gated.py`. Today this happens to work without cycles; a single new edge could introduce one. A package this small should declare its layering and stick to it (e.g., `changeset` ⟶ `content` ⟶ `route` ⟶ `stage` ⟶ `queue` ⟶ `service` ⟶ `client`).

---

## 4. Extensibility & Inheritance — The Critical Problem [C]

### 4.1 `Change` hierarchy uses `isinstance` cascades — open-closed violation [C]
`DirectMerge._stage_group` (direct.py:84–194) and `GatedMerge._stage_group` (gated.py:89–179) each contain an `isinstance(change, …)` chain over `OpaqueDirChange`, `SymlinkChange`, `WriteChange`, `DeleteChange`, `EditChange`. Adding a new change kind (e.g. `ChmodChange`, `RenameChange`, `AppendChange`) means:

1. Add a class in `changeset/types.py`.
2. Add a builder in `changeset/builders.py`.
3. Add an `isinstance` branch in `DirectMerge._stage_group`.
4. Add an `isinstance` branch in `GatedMerge._stage_group`.
5. Update `capture/overlay.py::overlay_path_changes_to_occ_changes` (if the source supports it).
6. Update `routing/orchestrator.py::requires_base_hash`, `attach_base_hash`.
7. Update `routing/orchestrator.py::_attach_chained_base_hashes` for the chain semantics.

Seven edits across five files for one new change kind. The expected design here is **double dispatch / visitor**:

```python
class Change(Protocol):
    path: str
    source: ChangeSource
    def apply_direct(self, ctx: DirectStageContext) -> StageOutcome: ...
    def apply_gated(self, ctx: GatedStageContext) -> StageOutcome: ...
```

…or, less invasive, a single `ChangeHandler` Protocol with two implementations (one per stager) and a dispatch table indexed by `type(change)`. Either way, **adding a kind becomes one file edit + one entry per stager.** The current pattern is the textbook anti-pattern.

### 4.2 `DirectMerge` and `GatedMerge` share a shape but no contract [H]
Both classes expose:

```python
def stage_group(
    self,
    group: PreparedPathGroup,
    *,
    active_manifest: Manifest,
    stage_write: StageWrite,
    stage_write_from_path: StageWriteFromPath | None = None,
) -> tuple[FileResult, LayerDelta | None]: ...
```

…yet **there is no `MergePolicy` / `Stager` Protocol** uniting them. `OccCommitTransaction._validate_group` (commit_transaction.py:195–241) dispatches with `if/elif` on `RouteDecision` and calls `self._direct` or `self._gated` directly. This is dispatch by enum where it should be polymorphic. A `Mapping[RouteDecision, Stager]` would let a future `OverlayMerge`, `BinaryDiffMerge`, or `LFSMerge` be added by registry without modifying the transaction.

### 4.3 Frozen dataclasses with custom `__init__` defeat themselves [H]
`Change` and its subclasses are declared `@dataclass(frozen=True, init=False)` and then use `object.__setattr__` in custom `__init__`s. This:

- Disables `dataclasses.replace()` for `Change` subclasses (which is why `WriteChange.with_base_hash` and `DeleteChange.with_base_hash` exist as hand-rolled copy methods).
- Means new fields require updating three places: the class body annotation, the custom `__init__`, and the manual copy method.
- Loses the type-safe constructor that `@dataclass(frozen=True)` would have generated.

If you need value-object semantics + custom validation, either use `attrs` with validators, or write a normal class with `__slots__` and `__hash__`. The current approach is the worst of three worlds.

### 4.4 The `WriteChange` "eager vs lazy bytes" leak [H]
`WriteChange` carries `_eager_content`, `content_path`, `precomputed_hash`, `base_hash` — four fields whose interaction is documented in a 13-line docstring (types.py:30–46). This conflates **transport** (where the bytes live) with **intent** (what the user wants done). Two cleaner designs:

- Split the type: `WriteChange(bytes_payload)` and `WriteChangeFromPath(content_path, precomputed_hash)`, both implementing a `WriteIntent` Protocol.
- Make payload a separate value object: `WriteChange(path, payload: WritePayload)` where `WritePayload` is `EagerPayload(bytes) | DiskPayload(path, hash)`.

Right now `final_content` reads from disk on every access for lazy instances (types.py:80–93). Two consecutive reads of `change.final_content` will do two disk reads. This is also why `DirectMerge._stage_group` (direct.py:99–105) and `GatedMerge._stage_group` (gated.py:102–125) re-fetch `bytes(change.final_content)` — they don't know if it's cheap.

### 4.5 Hidden capability discovery via `getattr` [H]
Three places in the package use `getattr(..., "method_name", None)` to probe optional protocol methods at runtime:

- `service.py:200` — `squash = getattr(self._layer_stack, "squash", None)`
- `routing/orchestrator.py:182` — `snapshot_oracle = getattr(oracle, "is_ignored_in_snapshot", None)`
- `result_projection.py:75–79` — `getattr(gitignore, "cache_hits", 0)`

Each one is a sign that the declared Protocol is **dishonest**. Either:
- the method is part of the contract (then it belongs in the Protocol),
- or it's optional (then declare an `Optional[Maintenance]`-style extension Protocol and inject it),
- or it's not supposed to be used (then delete the probe).

Duck-typing-with-getattr is the worst answer because the call site silently no-ops when the implementer changes.

### 4.6 `OccLayerStackPorts` is the wrong unifying interface [H]
The three real ports (`SnapshotReader`, `CommitStagingStore`, `CommitPublisher`) are properly separated. Then they get **welded together** by `OccLayerStackPorts(SnapshotReader, CommitStagingStore, CommitPublisher, Protocol)` so that `OccService` can accept "one thing". The own docstring admits this: "Combined in-process migration shape for the current OCC service." This forces every implementor to be all three, instead of letting the caller wire three distinct collaborators. `OccService.__init__` already destructures it manually for `OccCommitTransaction` (service.py:43–47):

```python
self._transaction = OccCommitTransaction(
    snapshot_reader=layer_stack,
    staging=layer_stack,
    publisher=layer_stack,
)
```

That's the right shape — accept three ports. The combined Protocol exists only to save callers from typing three constructor arguments. Delete it and force the explicit wiring.

### 4.7 `OCCClient` is hand-rolled decorator with broken contract [M]
- `OCCMutationService` Protocol (client.py:14–26) declares two async methods.
- `OccService` (service.py) has **four** publicly relevant methods: `apply_changeset`, `commit_prepared`, `apply_changeset_sync`, `commit_prepared_sync` — plus `prepare_changeset` and `prepare_changeset_sync`.
- `OCCClient` wraps two of them and re-implements the binding check at each call site.

If you add a new method on `OccService`, you must (a) update the Protocol, (b) add a forwarder in `OCCClient`, (c) duplicate the `require_workspace_binding` call. The whole class would be replaced by ten lines of `__getattr__`-based decoration, or — better — by moving binding enforcement into the *service*'s call-site rather than a separate facade.

### 4.8 Construction is non-injectable [M]
`OccService.__init__` takes only `gitignore` and `layer_stack`. It then *constructs* `OccOrchestrator`, `OccCommitTransaction`, and `OccSerialMerger` internally. There is **no way** to inject:

- A different orchestrator (e.g. for a no-gitignore mode).
- A different transaction (e.g. multi-process Phase 06).
- A different serial merger (e.g. fairness-aware queue).
- A different `ContentHasher` (e.g. blake3).
- A different `AUTO_SQUASH_MAX_DEPTH` value (it's a module constant).

The class admits the future-flexibility intent (the `Phase 05 — bounded CAS-mismatch retry budget` docstring in `merge/serial.py:21–35` is forward-looking) but provides no plug points to honor it. When Phase 06 multi-process arrives, you will be editing this constructor.

### 4.9 `OccSerialMerger` starts a daemon thread in `__init__`, no shutdown [M]
- `__init__` calls `threading.Thread(...).start()` (serial.py:59–64).
- There is no `close()`, `shutdown()`, or `__del__`.
- The thread loops forever on a `Queue.get()` and is never woken to exit.
- Tests cannot deterministically tear it down.
- A `OccService` cannot be safely re-created in the same process.

Daemon-thread-in-constructor is acceptable for a single-process app, but the package's *own design notes* anticipate multi-process futures. Add `start()` / `stop()` and let the caller own lifecycle.

---

## 5. Future Flexibility — What Will Break Next [H]

### 5.1 Hardcoded timing keys, no enum [M]
Strings like `"occ.commit.snapshot_s"`, `"occ.serial.queue_wait_s"`, `"occ.gated.read_current_s"` are scattered throughout. A new sub-metric is a free-form string with no shared registry. When you rename one (e.g. to migrate to OpenTelemetry), grep is your only friend. A `TimingKey` enum or namespacing helper would prevent typo drift and let monitoring systems consume a stable list.

### 5.2 Auto-squash logic is squatting inside `OccService` [H]
`OccService._auto_squash_after_publish*` is **90 lines (service.py:133–225)** of locking, recheck state, getattr-probing, and timing emission. This is maintenance behaviour, not commit behaviour. It belongs behind a `MaintenancePolicy` injectable:

```python
class MaintenancePolicy(Protocol):
    def after_publish(self, result: ChangesetResult) -> Mapping[str, float]: ...

class AutoSquashPolicy(MaintenancePolicy): ...
class NoopMaintenance(MaintenancePolicy): ...
```

Then `OccService.commit_prepared` calls `self._maintenance.after_publish(result)` and is done. Bonus: the `_CoalescedSquashState` becomes private to the policy where it belongs. The current shape pushes the squash internals up into the service surface (the `getattr(self._layer_stack, "squash", None)` probe).

### 5.3 CAS retry budget admits the abstraction is in the wrong place [M]
`MAX_OCC_CAS_RETRIES` is a module constant in `merge/serial.py`. Its docstring openly says *"In the current single-process architecture the per-root publisher lock makes mid-transaction CAS races structurally impossible … the constant exists so multi-process Phase 06+ topologies inherit a named, testable limit."* That's a clear ask for a `RetryPolicy` injection. Provide it now — it's three lines:

```python
@dataclass(frozen=True)
class RetryPolicy:
    max_cas_retries: int = 3

class OccSerialMerger:
    def __init__(self, transaction, *, retry: RetryPolicy = RetryPolicy(), ...): ...
```

### 5.4 Single-path fast path is a copy of the orchestrator [H]
Already covered in §2.4 — when the routing rules change (new `RouteDecision`, new gitignore rule), you must update **both** routers. This is a textbook duplication that will rot.

### 5.5 `_LayerChangeStager` is private but is the natural seam [M]
`_LayerChangeStager` in `commit_transaction.py` is a `_`-prefixed nested helper. But it's exactly the type a different storage backend would replace (e.g. a remote staging area, a content-addressed store, a deduplicating stager). Promote it to a public `LayerChangeStager` Protocol with one implementation, and the next time you need a remote staging shim it becomes trivial.

### 5.6 `OpaqueDirChange` is opaque to the type system [L]
`OpaqueDirChange.kept_children: frozenset[str]` is computed in `capture/overlay.py::_kept_children_for` by string-prefix splitting. There's no validation that the children are direct children, no `Path` normalization, no test that prefix arithmetic survives a path with `\n` or `..`. A `KeptChildren` value type with construction-time normalization would prevent the data quality issue from leaking into `DirectMerge` / `GatedMerge`.

### 5.7 The Protocol surface lies about its real capability [H]
- `GitignoreMatcher` Protocol declares one method (`is_ignored`).
- Production code requires `is_ignored_in_snapshot` (orchestrator.py:182, single_path.py:24).
- `SnapshotGitignoreOracle` provides both; a hypothetical alternative implementor of the Protocol would silently degrade.
- Same problem with the `squash` capability on `OccLayerStackPorts` (probed by getattr).

If the contract is *"must support snapshot-aware ignore + must support maintenance squash"*, declare it. The current state is "Protocol is a polite suggestion, runtime probes are the real contract."

---

## 6. What's Actually Good

To be fair:

- `ports.py` is the right move. The three split protocols (`SnapshotReader`, `CommitStagingStore`, `CommitPublisher`) carve the seam cleanly. The mistake is welding them with `OccLayerStackPorts`.
- `PreparedChangeset` / `PreparedPathGroup` are frozen, minimal, and post-route — a clean handoff between the router and the transaction.
- The `prepare_changeset` / `commit_prepared` split (so callers can prepare lock-free and only serialize commit) is well-motivated and the docstring on `OccService.commit_prepared` (service.py:74–82) is genuinely good.
- Atomic-vs-batch semantics in `OccSerialMerger._disjoint_batches` and `_combine_prepared` (serial.py:161–195) are correct and the `atomic` flag is honoured.
- `_SMALL_FILE_BYTES_THRESHOLD = 16 * 1024` (commit_transaction.py:38) is one of the few thresholds in the codebase with a comment explaining the measurement.
- Timing instrumentation is disciplined and consistent in style (even if the keys are unstructured).

---

## 7. Recommended Refactor Order

If you do nothing else, do these three in order:

1. **Polymorphic dispatch on `Change`** (§4.1). Replace `isinstance` cascades in `DirectMerge` and `GatedMerge` with a visitor or a `Change.apply(stager)` method. **Single highest-ROI change in the package.**
2. **Introduce `MergePolicy` Protocol** (§4.2) and wire `OccCommitTransaction._validate_group` to a `Mapping[RouteDecision, MergePolicy]`. Future merge policies plug in by registration.
3. **Collapse the duplicate router** (§2.4) into one `Router` with a `prepare(changes: Sequence[Change])` that takes a fast branch when `len(changes) == 1`. Drop `SnapshotIgnoreOracle` Protocol; merge into `GitignoreMatcher` (which already needs `is_ignored_in_snapshot` to be honest about its capability).

After those, the cosmetic-but-corrosive fixes:

4. Pick one prefix policy (no prefix is best in a package named `occ`). Rename `OCCClient` → `Client`, `OccService` → `Service`, `OccCommitTransaction` → `CommitTransaction` — and rename the *Protocol* `CommitTransaction` (ports.py:49) to `CommitTransactionPort` to break the collision.
5. Rename `OccOrchestrator` → `Router`, `OccSerialMerger` → `CommitQueue`, `DirectMerge` / `GatedMerge` → `DirectStager` / `GatedStager`. Rename `OCC_SKIPPED_MERGE` → `DIRECT` and `OCC_GATED_MERGE` → `GATED`.
6. Inline `routing/runtime_ops.py` (31 LOC, two helpers) into the router.
7. Promote `_LayerChangeStager` to a public protocol with one implementation.
8. Extract `MaintenancePolicy` from `OccService`'s auto-squash block.
9. Make `OccService.__init__` inject orchestrator, transaction, queue, hasher (with defaults). Phase 06 will thank you.
10. Drop `OccLayerStackPorts`. Force the explicit three-port wiring at the call sites — there are only two of them (`OccService.__init__` and `OccCommitTransaction.__init__`).

---

## Severity summary

| Tag | Count | Examples |
|---|---|---|
| **C — Critical** | 1 | `isinstance` cascade over `Change` subclasses (§4.1) |
| **H — High** | 11 | Half-applied prefix (§1.1), misleading class names (§1.2), no `MergePolicy` (§4.2), frozen-dataclass abuse (§4.3), `WriteChange` transport leak (§4.4), `getattr`-probing protocols (§4.5), `OccLayerStackPorts` welding (§4.6), `OccService` not injectable (§4.8), auto-squash inside service (§5.2), single-path duplication (§5.4), dishonest Protocols (§5.7) |
| **M — Medium** | 9 | Double-negative enum (§1.3), empty inits (§2.2), top-level files misplaced (§2.3), path depth (§3.1), `routing/runtime_ops.py` (§3.2), `OCCClient` thin wrapping (§4.7), thread lifecycle (§4.9), timing keys (§5.1), CAS retry placement (§5.3), private `_LayerChangeStager` (§5.5) |
| **L — Low** | 3 | Misc naming (§1.5), `OpaqueDirChange` validation (§5.6), `_FinalKind` inconsistency (§1.4 part) |

Total: 24 distinct issues across 18 files.

The package is in better shape than the issue count suggests — most fixes are local — but it is **one new `Change` kind away** from making the `isinstance` cascades unbearable, and **one new port consumer** away from making `OccLayerStackPorts` actively destructive.
