# Harsh Architecture Review — `backend/src/sandbox/overlay/`

**Scope:** Naming, folder/file structure, import chain shape, extensibility, interface/inheritance use, future flexibility. 8 source files, ~750 LOC. No bug hunting in this pass — that lives in `agents-REVIEW.md` / `tools-REVIEW.md` patterns.

**TL;DR:** The package *works* and has admirable discipline on a few axes (frozen dataclasses, ordered timings dict, explicit `__all__`, contract tests in `test_overlay_dependency_boundaries.py`). But it reads like a package that hard-named itself into a corner. Every label collides with something — `namespace/` doesn't namespace, `runner/` doesn't run, `cli.py` isn't a CLI, the `Protocol`s are private and unused, and three of the four subpackages each hold exactly one module that nobody outside imports. The whole thing could be ~5 files in `sandbox/overlay/` flat and lose nothing. Worse, when you actually need to extend it — second invoker, real kernel-mount backend, alternate capture strategy — the seams don't line up where the directory structure pretends they do.

---

## Verdict by Axis

| Axis | Grade | One-line |
|---|---|---|
| Naming convention | **D** | Names lie about what's inside; abbreviations and full words mix arbitrarily. |
| Folder/file structure | **D+** | 3 subpackages, 7 leaf modules, 5 of which are single-file. Folders bought you nothing. |
| Import path length / chain | **C** | Path depth 4 (`sandbox.overlay.runner.snapshot_overlay_runner.SnapshotOverlayRunner`) for a class that didn't need it. One *function-scoped* import to dodge a cycle that the layout itself created. |
| Extensibility | **D** | Two private `Protocol`s that aren't used as abstractions and aren't exported. `if invoker is None: from … import X; invoker = X(…)` is the extension point. |
| Interface / inheritance | **F** | Zero inheritance. Two `Protocol`s prefixed `_` so they cannot be subtyped from outside. The `Protocol` for `invoke_sync` exists but is never the parameter type. |
| Future flexibility | **D** | The "kernel overlay can replace this later" comment is in `mounts.py:35-39` and `namespace/command.py:11-14` — but no abstraction exists for either to plug in. Backend swap = rewrite, not implementation. |

---

## HIGH — must fix before this package grows

### H-01. `namespace/` is a lie

`namespace/command.py` and `namespace/mounts.py` together comment, twice, that the *real* namespace/kernel-overlay entrypoint lives in `sandbox.command_exec.workspace.namespace_entrypoint` (see `namespace/command.py:11-14` and `namespace/mounts.py:13-15`). What's actually in `sandbox/overlay/namespace/` is the **copy-backed, no-namespace, portable fallback**. The directory name is the opposite of the directory contents.

Future you (or whoever picks this up) will read `sandbox.overlay.namespace.mounts.mount_snapshot` and assume it does a `mount(2)`, `unshare(CLONE_NEWNS)`, etc. It does a `shutil.copytree`. That's a maintenance landmine.

**Fix options (cheapest first):**
1. Rename `namespace/` → `portable/` or `copy_backed/`. Then the dual comment becomes one line of module docstring.
2. If you want both backends co-located, introduce a real interface (`WorkspacePreparer` protocol with `mount(manifest, run_dir) -> MountedSnapshot`) and put `portable.py` + `kernel.py` under it. That gives you the "kernel mount can replace this implementation behind the same return object later" the docstring at `mounts.py:38-39` already promises but doesn't deliver.

### H-02. `cli.py` is not a CLI; it is the worker entrypoint

`cli.py` has `argparse`, `main()`, and `if __name__ == "__main__"`. Looks CLI. But the **only** real production caller is `runner/runtime_invoker.py:13` which imports `execute_request` as a Python function — the `main()` path is dead in-process. The argparse plumbing only fires inside the runtime bundle when the daemon spawns the worker as a subprocess.

So `cli.py`:
- Is named after the smallest part of what it does.
- Mixes `execute_request` (the actual worker, ~70 LOC, called by `runtime_invoker.invoke`) with `parse_args` / `main` (~10 LOC, called only by `python -m`).
- Owns the cleanup tuple `_INTERMEDIATE_RUN_DIRS` at line 27, which by content belongs next to `mount_snapshot` since `mount_snapshot` is the thing that created `lower`/`merged`/`work`.

**Fix:**
- Rename to `worker.py` or `execute.py`. Move `_INTERMEDIATE_RUN_DIRS` and the `shutil.rmtree` block to `namespace/mounts.py` (or a `cleanup_runtime_run_dir(run_dir)` helper next to `mount_snapshot`).
- Keep `argparse` glue, but make it 5 lines that call `execute_request`. If the bundle tooling needs a file named `cli.py`, add a thin shim that does `from sandbox.overlay.worker import main`. Don't keep `main` in the same module as the core orchestrator.

### H-03. `runner/snapshot_overlay_runner.py` — the path stutters

```python
from sandbox.overlay.runner.snapshot_overlay_runner import SnapshotOverlayRunner
```

Four segments, three of them say "overlay" or "runner" (`overlay` → `runner` → `snapshot_overlay_runner` → `SnapshotOverlayRunner`). Compare with what callers actually want:

```python
from sandbox.overlay.runner import Runner          # or
from sandbox.overlay import SnapshotOverlayRunner  # if you want the class name to stay
```

Either flatten the directory or shorten the file, but pick one. The current shape has six occurrences of "snapshot"/"overlay"/"runner" in a single import line. Look at the test files that import this — `test_snapshot_overlay_runner.py:11-12` imports `SnapshotOverlayRunner` and `OverlayShellRequest` from the same module on two lines because the line is too long. That's the smell.

### H-04. `runner/runtime_invoker.py` — the private `Protocol`s exist but don't gate anything

`snapshot_overlay_runner.py:72-87` defines `_RuntimeInvoker` and `_SyncRuntimeInvoker` Protocols. They are:
- **Underscore-prefixed**, so consumers can't import them to declare conformance.
- Not used as parameter types — `__init__` takes `invoker: _RuntimeInvoker | None`, but the public surface `supports_sync` then **duck-types via `getattr(self._invoker, "invoke_sync", None)`** at line 129/132. So the second Protocol is decoration; the actual contract check is `getattr` at runtime.
- Constructed as the extension hook by `if invoker is None: from sandbox.overlay.runner.runtime_invoker import RuntimeInvoker; invoker = RuntimeInvoker(...)` at line 100-103. This is an in-function import to defeat a circular dependency that exists because `runtime_invoker` imports from `snapshot_overlay_runner` and we want the default coupling.

**Result:** there is no real interface, just a one-line Protocol that lies about being load-bearing, plus a circular import worked around by lazy import. If a second invoker (e.g. an in-process invoker for unit tests, or a remote-RPC invoker) is added, what protects you from it forgetting `invoke_sync`? Nothing. The duck-typing branch at line 132 raises `RuntimeError` *only when shell_sync is actually called*. That's the worst kind of "interface."

**Fix:**
- Promote `RuntimeInvoker` to a public abstract base (or keep `Protocol` but export it, name it `OverlayInvoker`, and *use* it as the parameter type with `@runtime_checkable`). Drop `_SyncRuntimeInvoker` entirely — fold `invoke_sync` into the single Protocol as an optional or `NotImplementedError`-raising default on a base class.
- Move the default invoker construction to a factory module so `snapshot_overlay_runner` doesn't import `runtime_invoker` at all and the circular goes away without the function-scoped import trick.

### H-05. Each subpackage is a single-file folder pretending to be a domain

```
overlay/
  capture/
    changes.py     (85 LOC)
    upperdir.py    (277 LOC)
    types.py       (95 LOC)
  namespace/
    command.py     (106 LOC)
    mounts.py      (93 LOC)
  runner/
    runtime_invoker.py        (148 LOC)
    snapshot_overlay_runner.py (160 LOC)
  cli.py           (125 LOC)
```

That's **three subpackages and seven leaf files for ~1100 LOC**. None of the subpackages have an `__init__.py` doing re-exports (per the find), so importers all type the full path. The folder boundaries don't reduce import noise, don't hide implementation, don't represent dependency layers, and don't define swap points.

The dependency-boundary test `test_overlay_dependency_boundaries.py:21-26` literally has to enumerate "(`capture/`, `namespace/`, `runner/`)" as the set it cares about — i.e. the test confirms the directories are a flat enumeration, not a hierarchy.

**Recommendation:** flatten to:
```
overlay/
  __init__.py       # re-exports the 6 public names
  changes.py        # OverlayPathChange + content_hash
  capture.py        # capture_changes + helpers (was upperdir.py)
  types.py          # OverlayCapture + (de)serialization
  mounts.py         # MountedSnapshot + mount_snapshot
  command.py        # CommandResult + run_user_command
  runner.py         # OverlayShellRequest + SnapshotOverlayRunner
  invoker.py        # RuntimeInvoker (default impl)
  worker.py         # execute_request + argparse glue (was cli.py)
```
Nine files, zero subpackages, every import becomes `from sandbox.overlay import …` or `from sandbox.overlay.runner import …`. **Test it against the dependency-boundary test — same assertions hold.**

If you *want* a subpackage, the only honest split is `backend/` ↔ `frontend/` or `kernel/` ↔ `portable/`, i.e. the swap point the docstrings already advertise. That split would have **two implementations per subpackage**, not one.

---

## MEDIUM — naming and structural debt

### M-01. Naming inconsistency: `snapshot_overlay_runner` vs `runtime_invoker` vs `upperdir`

Within one package:
- `snapshot_overlay_runner.py` — 3 nouns, snake-case, redundant with parent dir name.
- `runtime_invoker.py` — 2 nouns, fine.
- `upperdir.py` — 1 compound noun, no underscore (compare with `lower_dir`/`work_dir` *inside* the same files at `mounts.py:41-43`).
- `cli.py` — 1 abbreviation, doesn't describe content.
- `types.py` — generic catch-all name, but it holds exactly one type (`OverlayCapture`) plus two free functions. Should be `capture.py` (already taken) or `overlay_capture.py`.
- `changes.py` vs `upperdir.py` — `changes.py` defines the change *type*; `upperdir.py` *captures* changes. Rename: `change.py` (type) + `capture.py` (extraction). Singular `change` matches the class `OverlayPathChange`.

### M-02. `OverlayPathChange` vs `OverlayCapture` vs `MountedSnapshot` — inconsistent affix discipline

- `OverlayPathChange` — `Overlay` prefix.
- `OverlayPathChangeKind` — `Overlay` prefix.
- `OverlayCapture` — `Overlay` prefix.
- `OverlayShellRequest` — `Overlay` prefix.
- `MountedSnapshot` — no `Overlay` prefix.
- `CommandResult` — no `Overlay` prefix.
- `SnapshotOverlayRunner` — `Overlay` is **infix**, suggesting the runner is for "snapshot overlays" rather than overlay-snapshot.

Pick a rule. Either everything in the package has `Overlay` (and import as `overlay.Capture`, `overlay.PathChange`) or nothing does. The current 4-of-6 hit rate is just noise.

### M-03. `write_overlay_capture` is in `capture/types.py`, not `capture/capture.py`

`types.py:77-84` defines `write_overlay_capture`. It writes a JSON file. That's a side-effecting operation in a module the name `types` promises is pure type definitions. Same file also defines `read_output_ref` at line 87 — a side-effecting reader. Move them to a `capture/serialization.py` or merge with whatever you keep around for runtime serialization. Then `types.py` becomes 50 LOC of dataclass and from_dict.

### M-04. `_RuntimeInvoker` Protocol is async-only; the **same module** also defines `_SyncRuntimeInvoker`. Pick one.

`snapshot_overlay_runner.py:72-87` declares two separate protocols (`invoke` async, `invoke_sync` sync). The default `RuntimeInvoker` implements both. Every test in `test_runtime_invoker_cleanup.py` and the production daemon use the async one. The sync path is reached only via `shell_sync`, which is reached via `runner.supports_sync` check.

Either:
- Always-async — delete `invoke_sync`, `shell_sync`, `supports_sync`, `_SyncRuntimeInvoker`, and the duck-typing branch. Net delete: ~60 LOC across 2 files.
- Single protocol with both methods, `invoke_sync` raising `NotImplementedError` by default. Either way, the two-protocol split is not justified.

### M-05. `OverlayShellRequest` lives in `runner/snapshot_overlay_runner.py` — wrong file

`OverlayShellRequest` is a request *type*. Conventionally it belongs in `types.py` (or `request.py`) and is imported by both the runner and the worker. Putting it in `snapshot_overlay_runner.py` is what forces `runtime_invoker.py:14-17` to do:

```python
from sandbox.overlay.runner.snapshot_overlay_runner import (
    OverlayShellRequest,
    overlay_shell_request_to_dict,
)
```

i.e. the invoker, a sibling of the runner, imports the runner module just to get the request type. Move `OverlayShellRequest` + `overlay_shell_request_{to,from}_dict` to `capture/types.py` (or a new `request.py`) and the chain shortens.

### M-06. `to_dict` / `from_dict` / `write_*` / `read_*` are scattered across three files with no pattern

- `OverlayPathChange.to_dict` / `from_dict` — instance + classmethod (`changes.py:40-63`).
- `OverlayCapture.to_dict` / `from_dict` — instance + classmethod (`types.py:37-74`).
- `OverlayShellRequest` — **does not have** `to_dict`/`from_dict`; instead two free functions `overlay_shell_request_to_dict` / `overlay_shell_request_from_dict` (`snapshot_overlay_runner.py:45-69`).
- `write_overlay_capture` — free function (`types.py:77`).
- `read_output_ref` — free function (`types.py:87`).

Three different serialization conventions for five different types in one package. Either everything is method-on-dataclass or everything is free function. Pick one. The free-function form for `OverlayShellRequest` exists only because it predates the dataclass refactor — fold it back.

### M-07. Path-shortening losses in the public surface

`__init__.py` is **empty** (1-line file per `find`). Compare with `sandbox/layer_stack/` siblings which presumably re-export. Every external caller pays the full path:

```python
from sandbox.overlay.runner.snapshot_overlay_runner import SnapshotOverlayRunner, OverlayShellRequest
from sandbox.overlay.capture.types import OverlayCapture
from sandbox.overlay.capture.changes import OverlayPathChange
```

vs. what they could pay:

```python
from sandbox.overlay import SnapshotOverlayRunner, OverlayShellRequest, OverlayCapture, OverlayPathChange
```

Fill `__init__.py`. The dependency-boundary test in `test_overlay_dependency_boundaries.py:20` walks the directory tree, not the `__all__`, so adding re-exports won't break it.

---

## LOW — surface polish

- **L-01.** `cli.py:39-46` `execute_request` keyword-only signature with 4 params is fine, but `manifest_payload: dict[str, Any]` is then passed through `runtime_invoker._execute_request_with_timings:114-118` which retypes it as `Mapping[str, Any]` then `dict()`-copies it. Pick one type along the chain.
- **L-02.** `runtime_invoker.py:105-111` `_run_dir` sanitization (`isalnum or in ("-", "_")`) is duplicated in spirit by what `OverlayShellRequest.__post_init__` already validates. The runtime-side sanitization is defense-in-depth, but document why or delete one.
- **L-03.** `mounts.py:76-86` `_copy_tree` re-implements `shutil.copytree(..., symlinks=True)` for one extra symlink case. Why not call `shutil.copytree` for each subtree? Either there's a reason (subtle xattr semantics) — write it as a comment, since `_populate_upperdir_from_diff` in `upperdir.py` does the same dance — or call the stdlib.
- **L-04.** `capture/upperdir.py` is **277 lines** and mixes three concerns: (1) materialize a diff into upperdir for the copy-backed path, (2) walk a real overlay upperdir, (3) decode whiteout/opaque conventions. Three top-level concerns in one file deserve three sections at minimum, or three files. The `_populate_*` family at lines 51-148 could live in its own `capture/populate.py`.
- **L-05.** Inline `WR-02`/`WR-03`/`WR-04`/`CR-02` codes (e.g. `upperdir.py:224`, `command.py:75`) refer to plan/review IDs that won't exist for future readers once the planning dir is archived. Either link to the artifact or rewrite the comment as a reason ("a literal `.wh.` entry would crash …", which is already there — drop the prefix).
- **L-06.** `__all__` listed in every module is good. But `cli.py:116-120` exports `execute_request`, `main`, `parse_args` — `parse_args` is genuinely internal to `main`. Drop it from `__all__`.
- **L-07.** `OverlayCapture.timings: dict[str, float] = field(default_factory=dict)` on a `frozen=True` dataclass with `__post_init__` mutating via `object.__setattr__` — works, but the mutable `dict` field on a frozen dataclass is a footgun. Either make it `Mapping[str, float]` and freeze contents at init, or use `Mapping`+`MappingProxyType` for read-only. Currently consumers can mutate `capture.timings` in place and the frozen contract is fiction.

---

## What I would actually do (in order of payoff)

1. **Flatten** the three subdirectories into a flat `sandbox/overlay/` with 8-9 files (H-05, M-07).
2. **Rename** `namespace/` → `portable/`, or — preferred — flatten and add a docstring on `mounts.py` explaining "this is the copy-backed backend; the kernel-mount path lives at sandbox.command_exec.workspace.namespace_entrypoint" once instead of duplicating it twice (H-01).
3. **Rename** `cli.py` → `worker.py`; keep `main`/`parse_args` as a 5-line section at the bottom (H-02).
4. **Move** `OverlayShellRequest` to `request.py` (or merge with `types.py`); kill `overlay_shell_request_{to,from}_dict` free functions in favor of `to_dict`/`from_dict` methods (M-05, M-06).
5. **Promote** `OverlayInvoker` to a public `Protocol` (drop the underscore, drop the sync-variant duplicate, use it as the parameter type). Move default-invoker construction to a factory module so the function-scoped import disappears (H-04, M-04).
6. **Fill** `__init__.py` with re-exports for the ~6 public types and 2-3 public functions (M-07).

After (1)–(6), the dependency-boundary test in `test_overlay_dependency_boundaries.py` needs to be updated to walk the flat layout — that's the right time to also rewrite its `forbidden`-token check, which currently substring-matches `"gitignore"` inside arbitrary source text (a fragile test, separate concern).

---

## What's good (so the harshness is calibrated)

- `OverlayPathChange.__post_init__` validation (`changes.py:23-38`) enforces the kind-vs-fields invariant strictly. Good.
- `_HOST_ENV_ALLOWLIST` (`command.py:19-27`) is small, well-commented, and explicit about what host env is forbidden. Good.
- `_validate_cwd` (`command.py:87-95`) uses `os.path.commonpath` to refuse escapes. Good.
- `_is_overlay_whiteout` / `_has_overlay_opaque_xattr` (`upperdir.py:239-260`) correctly use `stat.S_ISCHR(st.st_mode) and st_rdev == 0`. The `WR-03` comment captures the past bug honestly. Good.
- Timing instrumentation density is excellent — every phase has a `monotonic_now()` boundary and a labeled key. This is the kind of observability you don't usually see in worker code. Keep this.
- Contract tests (`test_overlay_dependency_boundaries.py`) lock the package boundary explicitly. Rare and good.

The bones are sound. It's the *labeling* of the bones that's bad, and the labels make the package costlier to extend than its content warrants.
