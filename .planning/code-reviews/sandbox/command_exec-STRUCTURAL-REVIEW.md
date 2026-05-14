---
phase: command_exec-structural-review
reviewed: 2026-05-14
depth: harsh / structural
scope:
  - naming convention
  - folder & file structure
  - import dependency chain
  - extensibility, inheritance, interface design
  - future flexibility
files_reviewed:
  - backend/src/sandbox/command_exec/__init__.py
  - backend/src/sandbox/command_exec/contract/__init__.py
  - backend/src/sandbox/command_exec/contract/ports.py
  - backend/src/sandbox/command_exec/contract/request.py
  - backend/src/sandbox/command_exec/contract/result.py
  - backend/src/sandbox/command_exec/workspace/__init__.py
  - backend/src/sandbox/command_exec/workspace/capture.py
  - backend/src/sandbox/command_exec/workspace/environment.py
  - backend/src/sandbox/command_exec/workspace/mount.py
  - backend/src/sandbox/command_exec/workspace/namespace_entrypoint.py
reviewer: Claude (Opus 4.7 1M)
---

# Harsh Structural Review — `backend/src/sandbox/command_exec/`

> **Scope reminder.** This is the harsh review the user requested: naming, folder/file structure, import-path cleanliness, **extensibility, use of inheritance / interfaces, and future flexibility**. Behavior/security findings are noted only where they intersect those axes.

---

## 0. TL;DR

The package is **mis-bounded, over-foldered, under-abstracted, and string-typed**. It looks like a clean DDD-style "contract + workspace" package on the surface and does almost nothing the layout promises:

- The *execution pipeline* lives **outside** the package (in `runtime/daemon/service/shell_runner.py`), so `command_exec/` is not a service — it's a grab-bag of helpers that one orchestrator hand-wires together via **five separate deep import paths** with no facade.
- Two mount strategies (`copy_backed`, `private_namespace`) are dispatched by a hard-coded `if/else` plus a **stderr-JSON-line-sniffing fallback** keyed on exit code `126`. No `MountStrategy` Protocol, no registration, no fallback chain. Adding a third strategy is multi-file surgery.
- Policy (env allowlist, forbidden overlay path chars, workspace-env keys) lives in **module-private `frozenset`s** — not injectable, not tenant-scoped, not testable without monkey-patching.
- `contract/` claims to be a stable boundary but **leaks OCC internals** (`Change`, `ChangesetResult`, `CommitOptions`) and types three public payload fields as `object`, throwing schema away at the contract layer.
- Naming is **inconsistent across the package's own surface**: `CommandExecRequest` → `CommandExecResult` → `ShellProcessResult` → `WorkspaceCapture`. Three different nouns for the outputs of one pipeline.

The package is small (5 source files) yet already shows every symptom of a system that will resist its second strategy, its second tenant, and its second consumer.

---

## 1. Naming Convention — Confused and Inconsistent

### 1.1 The package name doesn't match its own vocabulary
- Docstrings everywhere say *"guarded command execution"* (`__init__.py:1`, `request.py:1`, `result.py:1`). The package is named `command_exec`. Neither module nor class names contain the word "guarded". Pick one term and use it everywhere, or admit the "guarded" branding is aspirational.

### 1.2 Output type names are a salad
In `result.py`:
- `CommandExecResult`
- `ShellProcessResult`
- `WorkspaceCapture`

Three result-shaped types in one file, three naming roots, no common base, no projection between them. A consumer cannot tell from the name whether `ShellProcessResult` is upstream or downstream of `CommandExecResult`. (It's upstream — `CommandExecResult` *should* wrap it conceptually but doesn't even hold a reference; the orchestrator throws it away after capture.) Pick one noun root for outputs of this pipeline.

### 1.3 `mount.py` is a misnomer
`workspace/mount.py` (347 lines) contains:
- a dataclass spec (`WorkspaceReplacementMountSpec`),
- the **public orchestration entrypoint** (`run_workspace_replaced_command`),
- two strategy implementations (`_run_copy_backed_mount`, `_run_private_mount_namespace`),
- a path-rewriter that does **shell-quote tokenization** (`_rewrite_workspace_paths`, `_path_starts_at`),
- a capability probe (`_private_mount_namespace_available`),
- a stderr-JSON-sniffing fallback detector (`_is_namespace_mount_failure`).

It's not `mount.py`; it's `execute_with_fallback.py + spec.py + path_rewrite.py + capability_probe.py` glued together. The filename promises one thing and delivers four.

### 1.4 `environment.py` is two unrelated modules in one
`workspace/environment.py` contains:
- `resolve_workspace_cwd(...)` — a *path containment policy* function used by both strategies,
- `run_command_to_refs(...)` — the *actual subprocess invoker*,
- `_RESTRICTED_ENV_KEYS` and `_command_environment(...)` — a *security allowlist*.

Three concerns share a module because they all "have to do with environment-ish stuff." Split into `cwd_policy.py`, `process_runner.py`, `env_policy.py`.

### 1.5 Stringly-typed mode and error enums
- `mount_mode: str` in `ShellProcessResult` and `WorkspaceCapture`. Values: `"copy_backed"`, `"private_namespace"`. Never an enum.
- `error_kind` in `namespace_entrypoint.py`: `"bad_payload"`, `"mount_failed"`, `"validation_failed"`, `"setup_failed"`, `"unexpected_setup_failed"`, `"command_failed"`. No enum, no namespace — and one of these (`"mount_failed"`) is **parsed back by string compare** in `mount.py:_is_namespace_mount_failure`. That cross-file string contract is invisible to grep-by-symbol and to LSP.

### 1.6 `WorkspaceReplacementMountSpec` — four-word name, zero clarity
A `Spec` that "replaces" "workspace" via "mount" — but what *kind* of mount? Inspecting the fields reveals it's an **overlayfs spec** (`lowerdir`, `upperdir`, `workdir`). Call it `WorkspaceOverlaySpec` (and own the fact that both implementations presuppose overlayfs semantics — the "copy_backed" path is just overlayfs-by-`cp`).

### 1.7 `capture.py` shadows a peer module
`command_exec/workspace/capture.py:capture_workspace_upperdir` is a **17-line wrapper** around `sandbox.overlay.capture.upperdir.capture_changes` that does nothing except branch on `copy_backed` and drop an unused `snapshot_manifest` (`del snapshot_manifest`, line 21 — a parameter that exists only to be deleted). If the wrapper has no logic, delete it. If it has policy, name it for the policy (`capture_overlay_or_copy_backed`).

---

## 2. Folder / File Structure — Over-Foldered for the Code Volume

### 2.1 Two nested subpackages for 5 source files
```
command_exec/
├── __init__.py                  (empty stub)
├── contract/
│   ├── __init__.py              (empty)
│   ├── ports.py                 (2 Protocols + 1 Lease Protocol)
│   ├── request.py               (1 dataclass)
│   └── result.py                (3 dataclasses)
└── workspace/
    ├── __init__.py              (empty)
    ├── capture.py               (1 wrapper fn)
    ├── environment.py           (3 unrelated concerns)
    ├── mount.py                 (4 unrelated concerns)
    └── namespace_entrypoint.py  (out-of-process script)
```
That's **3 dataclasses split into 3 files** under `contract/`, and **4 mixed-concern modules** under `workspace/`. The folder count is doing the opposite of what folders are supposed to do.

A clean shape for the same code, with one less folder and clearer boundaries:
```
command_exec/
├── __init__.py                  (facade: re-export the public types + entrypoint)
├── contract.py                  (Request, Result, Capture, ProcessResult, Modes-as-Enum)
├── ports.py                     (LeaseClient, MutationClient, ExecutionStrategy, CommandExecutor)
├── policy.py                    (cwd + env + path-char policy in one place)
├── strategies/
│   ├── __init__.py
│   ├── copy_backed.py
│   └── private_namespace.py
├── executor.py                  (dispatch + fallback orchestrator — currently buried in mount.py + shell_runner.py)
└── entrypoints/
    └── namespace_helper.py      (the subprocess script)
```

### 2.2 Empty `__init__.py` files are a feature gap, not minimalism
`command_exec/__init__.py`:
```python
__all__: list[str] = []
```
The package **declares no public surface**. Every consumer must memorize deep paths. This is `shell_runner.py:11-17`:
```python
from sandbox.command_exec.contract.ports import OCCMutationClient, WorkspaceLeaseClient
from sandbox.command_exec.contract.request import CommandExecRequest
from sandbox.command_exec.contract.result import CommandExecResult, WorkspaceCapture
from sandbox.command_exec.workspace.capture import capture_workspace_upperdir
from sandbox.command_exec.workspace.mount import (
    WorkspaceReplacementMountSpec,
    run_workspace_replaced_command,
)
```
**Five import statements from one package** to do one thing. That is not a package — it is a federation of modules pretending to be one.

### 2.3 `namespace_entrypoint.py` is buried in `workspace/`
A subprocess script invoked as `python -m sandbox.command_exec.workspace.namespace_entrypoint <payload>` (mount.py:173-176) is **not workspace logic** — it's an out-of-process executable. Treating it as a peer of `capture.py` and `environment.py` confuses readers and packagers. Move it to `entrypoints/`, document the `-m` contract, and pin the dotted path in *one* place.

### 2.4 `.DS_Store` checked into the source tree
`backend/src/sandbox/command_exec/.DS_Store` is present in the working tree. Either git-ignore it project-wide or fix the dev environment, but it shouldn't ride along in a structure review.

### 2.5 Duplicate namespace machinery with a peer package
`sandbox/overlay/namespace/{command.py, mounts.py}` exists. `command_exec/workspace/namespace_entrypoint.py` exists. Both speak overlay namespaces. Either this is overlapping responsibility or one should call into the other — right now they are parallel implementations with **no documented relationship**.

---

## 3. Import / Dependency Chain — Deep, Wide, and Leaky

### 3.1 Internal coupling is fine; external coupling leaks
Inside the package:
- `mount.py` → `contract/request`, `contract/result`, `workspace/environment` ✅
- `capture.py` → `workspace/mount` ❌ — capture reaches into mount.py to get the spec dataclass. The spec is **contract-layer data**, not mount-layer logic.
- `namespace_entrypoint.py` → `workspace/environment` ✅

The `capture → mount` edge means the contract leaks across the workspace boundary backwards. Move `WorkspaceReplacementMountSpec` to `contract.py` and the cycle-flavor disappears.

### 3.2 `contract/ports.py` smuggles OCC's internals into the contract
```python
from sandbox.occ.changeset.prepared import CommitOptions
from sandbox.occ.changeset.types import Change, ChangesetResult
```
"Contract" means "the stable promise this package makes." Pulling three OCC types into the protocol signature couples `command_exec`'s contract to OCC's *internal* type modules. If `sandbox.occ.changeset.types.Change` moves, every consumer of `command_exec.contract.ports.OCCMutationClient` breaks. Either:
- own a `Changeset` value-object in `command_exec/contract.py`, OR
- depend on `sandbox.occ.ports` (a stable façade owned by OCC) and never reach into `occ.changeset.*`.

### 3.3 `Sequence[object]` and `manifest: object` — schema thrown away
- `WorkspaceCapture.changes: Sequence[object]` (result.py:13). The actual type is `Sequence[OverlayPathChange]`. Typing it as `object` defeats every downstream type check.
- `WorkspaceSnapshotLease.manifest: object` (ports.py:15). Same anti-pattern.
- `occ_result: object` in `CommandExecResult` (result.py:26). Same.

You can keep these abstract without `object` — `Protocol`, `TypeVar`, or a re-exported alias. `object` is a thrown-away type contract.

### 3.4 Consumers reach 5 levels deep
See `shell_runner.py:11-17` (quoted above). With a populated `__init__.py`, consumers would write:
```python
from sandbox.command_exec import (
    CommandExecRequest,
    CommandExecResult,
    WorkspaceCapture,
    OCCMutationClient,
    WorkspaceLeaseClient,
    WorkspaceReplacementMountSpec,
    run_workspace_replaced_command,
    capture_workspace_upperdir,
)
```
The package would have a *surface*. Right now it has only *internals*.

---

## 4. Extensibility / Inheritance / Interface — The Biggest Failure

### 4.1 No `MountStrategy` / `ExecutionStrategy` interface
The two strategies — `_run_copy_backed_mount` and `_run_private_mount_namespace` — share an **identical call shape**: `(spec, request, run_dir, timings) -> ShellProcessResult`. They are textbook implementations of the same Protocol. Instead, they are:
- private functions (so untestable as units without `_`-name imports),
- dispatched by hard-coded `if/else` inside `run_workspace_replaced_command`,
- fallback-detected via **stderr-JSON sniffing** of the child process.

Refactor target:
```python
class ExecutionStrategy(Protocol):
    name: str
    def is_available(self) -> bool: ...
    def run(self, *, spec, request, run_dir, timings) -> ShellProcessResult: ...
    def is_recoverable_failure(self, result: ShellProcessResult) -> bool: ...

def run_workspace_replaced_command(
    *, spec, request, run_dir, timings,
    strategies: Sequence[ExecutionStrategy] = DEFAULT_STRATEGIES,
) -> ShellProcessResult:
    for strategy in strategies:
        if not strategy.is_available():
            continue
        result = strategy.run(spec=spec, request=request, run_dir=run_dir, timings=timings)
        if not strategy.is_recoverable_failure(result):
            return result
        timings[f"command_exec.{strategy.name}_fallback"] = 1.0
    raise RuntimeError("no execution strategy succeeded")
```
That is the design `mount.py:47-72` is *trying* to be. Adding a future `FuseStrategy`, `BindMountStrategy`, or `RemoteContainerStrategy` becomes a one-file addition.

### 4.2 Fallback signaling is brittle: exit-code 126 + stderr-JSON
`_is_namespace_mount_failure` (mount.py:296-310) decides whether to retry on the copy-backed path by:
1. exit_code == 126,
2. reading stderr file line by line,
3. JSON-parsing each line,
4. looking for `{"error_kind": "mount_failed"}`.

That entangles **user stderr output** with **out-of-band control signaling**. A user program legitimately exiting `126` and emitting JSON containing `"error_kind":"mount_failed"` (e.g., a tool that prints structured logs) would **silently trigger a retry on the copy-backed path**, changing the semantics of their command. Fix:
- reserve a distinct exit code (e.g., 125 — kernel/coreutils convention reserves 126/127 for "found but not executable" and "not found"), OR
- write a sidecar control file (`run_dir/control.json`) that the parent reads instead of stderr.

### 4.3 Policy is hard-coded module-private state
- `_RESTRICTED_ENV_KEYS` (environment.py:100-112): 9 keys, hard-coded.
- `_WORKSPACE_ENV_KEYS` (mount.py:217): 3 keys, hard-coded.
- `_FORBIDDEN_OVERLAY_PATH_CHARS` (namespace_entrypoint.py:186): hard-coded.
- `GIT_OPTIONAL_LOCKS=0` (environment.py:119): hard-coded **git-specific** env injection in a **general** command-exec module. Why does a workspace runner know about git? Either push it to the caller, or generalize to a "command hint" callback.

All four are good candidates for **injection** via a `CommandExecPolicy` value object so tenants can tighten or loosen the rules without forking.

### 4.4 No facade port for "execute a guarded command"
The package defines `WorkspaceLeaseClient` and `OCCMutationClient` (ports for the package's *dependencies*) but **does not define a port for itself**. Consumers cannot mock "command_exec" — they have to mock `run_workspace_replaced_command`, `capture_workspace_upperdir`, and the contract types separately. A `CommandExecutor` Protocol with one method `run(request) -> CommandExecResult` is the missing keystone.

### 4.5 `WorkspaceReplacementMountSpec.__post_init__` containment bug
```python
scratch_root = Path(self.scratch_root).resolve(strict=False)
for field_name in ("lowerdir", "upperdir", "workdir"):
    ...
    if not path.is_relative_to(scratch_root):
        raise ValueError(...)
```
`Path.is_relative_to(p)` is **true when path == p**. So `upperdir = scratch_root` passes validation. Then `_run_copy_backed_mount` does:
```python
for directory in (upperdir, workdir, merged):
    if directory.exists():
        shutil.rmtree(directory)
    directory.mkdir(parents=True)
```
If a caller (or a bug) constructs a spec with `upperdir == scratch_root`, the scratch tree is **deleted wholesale**. The containment guard must be *strict*: `path != scratch_root and path.is_relative_to(scratch_root)`. Pairwise distinctness of `lower/upper/work` is also worth asserting.

### 4.6 `_private_mount_namespace_available()` is cached forever
`@lru_cache(maxsize=1)` (mount.py:313) caches availability for the lifetime of the process. Three problems:
1. **No DI for tests** — you cannot ask the executor to pretend namespaces are unavailable; you must monkey-patch the private function.
2. **No invalidation** — if the daemon loses `CAP_SYS_ADMIN` (e.g., re-exec into a stricter namespace), the cache lies.
3. **Hides a side effect** — the probe runs `unshare -Urm true` with a 2 s timeout (mount.py:319-326); on first call this latency is hidden inside a "looks pure" function.

Push capability detection to an explicit `StrategyRegistry.bootstrap()` called once at daemon startup: known cost, mockable, invalidatable.

### 4.7 Frozen dataclasses with `object.__setattr__` are smell, not design
`CommandExecRequest.__post_init__` (request.py:24-60) does 8 `object.__setattr__` calls to normalize a frozen dataclass. That works, but the pattern indicates the class is being used as both an input validator and a value object. Split:
```python
@dataclass(frozen=True)
class CommandExecRequest: ...        # pure value object

def make_command_request(...) -> CommandExecRequest:  # validates + normalizes
    ...
```
Then the boundary code is a free function and the data carrier is dumb. Bonus: validation errors and dataclass-construction errors stop being conflated.

### 4.8 The path-rewriter is a workaround masquerading as code
`mount.py:192-281` rewrites occurrences of `workspace_root` (e.g., `/testbed`) into `merged_root` (e.g., `/tmp/...`) inside argv strings and a handful of env vars, **sniffing shell-quote boundaries** to decide where a path token starts and ends. This exists *only because* the copy-backed fallback cannot replace `/testbed` at the filesystem layer the way the namespace strategy can.

Implications:
- It is **shell-aware string parsing**, in Python, in a critical path. The boundary set is hand-curated: `" \t\n\r=:;,&|>(\"'"`. Any unanticipated token (`<`, redirection variants, backticks, `$(...)`, `${var}`) will desync.
- It silently mutates user commands. A command that contains the literal string `/testbed` in a non-path context (an error message, a regex, etc.) gets mutated.
- It belongs in its own module (`fallback_path_rewrite.py`) with explicit tests and a docstring that says: "we only do this because the copy-backed strategy is a lie."

### 4.9 Two ad-hoc timings dicts with a shared key namespace
Both `mount.py` (writes `command_exec.*` keys into a passed `dict[str, float]`) and `namespace_entrypoint.py` (writes a timings JSON file that `_merge_namespace_timings` reads back) collaborate via a **shared key namespace** with **no schema**. A `Timings` object with namespaced accessors would prevent silent collisions and typo drift across the IPC boundary.

### 4.10 Fallback paths to `/tmp` in the helper
`namespace_entrypoint._fallback_ref` (lines 260-267) writes errors/timings to `/tmp/namespace-entrypoint-<key>.txt` when refs are missing from the payload. In a multi-tenant daemon, predictable shared `/tmp` paths are a small foot-gun (symlink races, cross-tenant noise). Either fail loudly or use `tempfile.mkstemp` per invocation.

---

## 5. Cross-Cutting Concerns

### 5.1 The real entrypoint is outside the package
`shell_runner.py` (in `runtime/daemon/service/`) is the *actual* `execute_shell_api` — it owns the lease → mount → run → capture → OCC pipeline. That means `command_exec/` is **not a service**, it's a **toolkit**. Either:
- move the orchestrator into `command_exec/executor.py` and let `shell_runner.py` be a 3-line FastAPI handler shim, OR
- rename the package to `command_exec_toolkit/` and stop pretending it's a self-contained subsystem.

### 5.2 No package-level public surface = no package-level tests boundary
Tests reach into the same deep paths consumers do (`tests/unit_test/test_sandbox/test_command_exec/test_*`). Because there's no facade, tests can't pin a stable surface. A regression in `_rewrite_declared_workspace_refs`'s signature breaks consumers and tests alike with no buffer.

---

## 6. Severity-Ranked Punch List

| #  | Severity     | Concern                                                                                                                                       | Where                                                          |
|----|--------------|-----------------------------------------------------------------------------------------------------------------------------------------------|----------------------------------------------------------------|
| 1  | **Critical** | Scratch-root containment check is non-strict; `upperdir == scratch_root` would let `rmtree` nuke the scratch tree.                            | `mount.py:WorkspaceReplacementMountSpec`, `_run_copy_backed_mount` |
| 2  | **Critical** | Fallback signal is exit_code 126 + stderr-JSON sniffing — collides with legitimate user output.                                               | `mount.py:_is_namespace_mount_failure`                         |
| 3  | **High**     | No `ExecutionStrategy` Protocol; strategies are private functions dispatched by hard-coded `if/else`.                                         | `mount.py:run_workspace_replaced_command`                      |
| 4  | **High**     | `command_exec/__init__.py` exports nothing → consumers reach 5 deep paths to use the package.                                                 | package root                                                   |
| 5  | **High**     | "Contract" leaks OCC internals (`Change`, `ChangesetResult`, `CommitOptions`).                                                                | `contract/ports.py`                                            |
| 6  | **High**     | `Sequence[object]` / `manifest: object` / `occ_result: object` discard types at the boundary.                                                 | `result.py`, `ports.py`                                        |
| 7  | **High**     | `mount.py` is four files in one (spec + dispatcher + strategies + path-rewriter + probe).                                                     | `workspace/mount.py`                                           |
| 8  | **Medium**   | Stringly-typed `mount_mode` and `error_kind` — should be enums; cross-file string contract.                                                   | `result.py`, `namespace_entrypoint.py`, `mount.py`             |
| 9  | **Medium**   | Policy (env allowlist, path-char set, workspace env keys, `GIT_OPTIONAL_LOCKS=0`) is module-private; cannot be injected per tenant/test.      | `environment.py`, `mount.py`, `namespace_entrypoint.py`        |
| 10 | **Medium**   | `_private_mount_namespace_available` is `lru_cache(maxsize=1)` forever; not testable, not invalidatable.                                      | `mount.py`                                                     |
| 11 | **Medium**   | Naming inconsistency: `Command*` vs `Shell*` vs `Workspace*` for outputs of one pipeline.                                                     | `result.py`                                                    |
| 12 | **Medium**   | The package's *actual* orchestrator (`execute_shell_api`) lives **outside** the package.                                                      | `runtime/daemon/service/shell_runner.py`                       |
| 13 | **Medium**   | `capture.py` is a 17-line wrapper that takes an unused `snapshot_manifest`; deletes parameter on entry.                                       | `capture.py:21`                                                |
| 14 | **Low**      | `namespace_entrypoint.py` is buried in `workspace/` despite being a `python -m` subprocess script.                                            | structure                                                      |
| 15 | **Low**      | Fallback `/tmp/namespace-entrypoint-<key>.txt` is predictable shared path.                                                                    | `namespace_entrypoint.py:264-265`                              |
| 16 | **Low**      | `.DS_Store` present in source tree.                                                                                                           | `command_exec/.DS_Store`                                       |
| 17 | **Low**      | Duplicate namespace machinery with `sandbox.overlay.namespace.*`; relationship undocumented.                                                  | cross-package                                                  |
| 18 | **Low**      | `WorkspaceReplacementMountSpec` is in `workspace/mount.py`; it's a value object and belongs in `contract/`.                                   | layering                                                       |
| 19 | **Low**      | Path-rewriter (`_rewrite_workspace_paths`) is shell-quote-aware string mutation in a critical path; needs its own module + tests + "this is a workaround" docstring. | `mount.py:192-281`                                             |

---

## 7. Recommended Refactor (Minimal, Mechanical, No Behavior Change)

### Phase 1 — surface + structure (1–2 hours, no test changes)
1. Promote `command_exec/__init__.py` to re-export: `CommandExecRequest`, `CommandExecResult`, `WorkspaceCapture`, `ShellProcessResult`, `WorkspaceLeaseClient`, `OCCMutationClient`, `WorkspaceReplacementMountSpec`, `run_workspace_replaced_command`, `capture_workspace_upperdir`.
2. Move `WorkspaceReplacementMountSpec` from `workspace/mount.py` to `contract/spec.py` (or collapse contract files into one `contract.py`).
3. Tighten `WorkspaceReplacementMountSpec.__post_init__` containment: require `path != scratch_root` and pairwise distinctness of `lower/upper/work`.
4. Replace `mount_mode: str` and `error_kind: str` with `enum.StrEnum`s, exported from `contract`.
5. Delete `capture_workspace_upperdir`'s unused `snapshot_manifest` parameter.
6. Add `.DS_Store` to `.gitignore` and remove it.

### Phase 2 — boundary integrity (2–4 hours)
7. Define `command_exec.ports.CommandExecutor` Protocol (`run(request) -> CommandExecResult`).
8. Move `execute_shell_api`'s middle layer from `shell_runner.py` into `command_exec/executor.py`; let the handler-side stay as the FastAPI shim.
9. Stop importing `Change`, `ChangesetResult`, `CommitOptions` from `sandbox.occ.changeset.*` in `ports.py`; alias them from a stable `sandbox.occ.ports` facade or own a local `Changeset` value object.

### Phase 3 — strategy interface (4–6 hours)
10. Extract `ExecutionStrategy` Protocol and refactor the two private functions in `mount.py` into `strategies/copy_backed.py` and `strategies/private_namespace.py`.
11. Replace stderr-JSON sniffing with a sidecar control file (`run_dir/control.json`) and a reserved exit code (e.g., 125) for "infrastructure failure, please retry next strategy."
12. Replace `lru_cache` with an explicit `StrategyRegistry.bootstrap()` called from daemon startup.
13. Pull `GIT_OPTIONAL_LOCKS=0` out of `_command_environment`; if the caller wants it, the caller passes it via `request.env_hints` or similar.

### Phase 4 — policy injection (2–3 hours)
14. Replace module-private `_RESTRICTED_ENV_KEYS`, `_WORKSPACE_ENV_KEYS`, `_FORBIDDEN_OVERLAY_PATH_CHARS` with a `CommandExecPolicy` dataclass injected at executor construction. Default value preserves current behavior.

After Phase 1 + 2 alone, consumer code looks like:
```python
from sandbox.command_exec import CommandExecutor, CommandExecRequest
result = executor.run(request)
```
…instead of the current five-line import block.

---

## 8. What to Keep

This package is not all bad — these patterns are healthy and should survive any refactor:
- Frozen dataclasses as value objects (just split validation out).
- Strict `cwd` containment via `os.path.commonpath` + boundary `..`-rejection (`request.py`, `environment.py`).
- The `O_NOFOLLOW | O_DIRECTORY` + `fstat`-recheck dance in `_validate_mount_inputs` (TOCTOU-aware).
- Forbidden overlay-path chars list — the *content* is correct, only its *location* is wrong (should be injectable policy).
- The `_command_environment` allowlist concept (LD_PRELOAD, PATH, PYTHONPATH, etc.) — again, content right, plumbing wrong.

---

## 9. One-Sentence Verdict

> `command_exec/` is a toolkit that wants to be a service but lost the boundary somewhere between the package's empty `__init__.py` and the orchestrator that lives two packages away — fix the facade, the strategy interface, and the contract leak, and the rest is cosmetic.

**End of review.**
