# Harsh Review — `sandbox/host` + `sandbox/runtime`

**Reviewed paths**
- `backend/src/sandbox/host/` (7 modules, ~1.3k LOC)
- `backend/src/sandbox/runtime/` (24 modules, ~2.8k LOC, all but one under `runtime/daemon/`)

**Scope of the review**
Naming convention · folder/file structure · import dependency chain · extensibility · use of inheritance / interface · future flexibility.

Bug-class findings (correctness, security, perf) are out of scope here and intentionally omitted unless they sharpen a structural point.

**Verdict**
The two packages work, and you can see the design intent (host = orchestrator-side, runtime = bytes that ship into the sandbox; AF_UNIX daemon dispatch via an `OP_TABLE`). But the *seams* between those intents are weak. There are at least four "naming lies", one inverted layer, a side-effect-driven plugin model, and a handler tree that pretends to be extensible while making every new op a three-file coordinated edit. None of this is fatal, but it will rot fast if the API surface keeps growing.

Severity legend: **🟥 high** (will block extensibility / cause real outages) · **🟧 medium** (will slow every PR in this area) · **🟨 low** (style/clarity, fix opportunistically).

---

## 1. Naming convention — **D+**

Names in this package are not descriptive. Several are misleading. A new reader needs `grep` to know what a file owns.

### 1.1 Misleading filenames

| File | What the name suggests | What it actually is |
|---|---|---|
| 🟥 `host/setup.py` | Python packaging / install metadata | Post-create / post-start sandbox bootstrap orchestrator. Shadows the universally-known meaning of `setup.py`. Rename to `bootstrap.py` or `lifecycle.py`. |
| 🟧 `host/context.py` | Request/session context | A `SandboxContextPreparer` Protocol + a factory. The word "context" tells the reader nothing. Rename to `context_preparer.py` or merge into `provider/protocol.py`. |
| 🟧 `host/git.py` | Anything git-related | Exactly one function (`ensure_git`) and a 30-line shell heredoc. Rename `ensure_git.py` or fold into `setup.py`/`bootstrap.py`. |
| 🟧 `host/recovery.py` | A package of recovery primitives | Exactly one function `ensure_running`. Same problem. |
| 🟧 `runtime/daemon/handler/request_context.py` | Per-request context object | Kitchen-sink module: path classifier, `_layer_stack_root` / `_required_single_path` validators, no-follow FS helpers (`read_bytes_no_follow`, `write_text_no_follow`), `_services` factory shim, and `_project_changeset` result projection. None of those are "request context". Split into `paths.py`, `validation.py`, `safe_io.py`, `projection.py`. |
| 🟨 `runtime/daemon/service/workspace_server.py` | A network server | `LayerStackWorkspaceServer` — owns binding + first base build. It's not a server. Rename `workspace_owner.py` or fold into `binding.py`. |
| 🟨 `runtime/daemon/service/layer_stack_client.py` | A network client | `LayerStackClient` is a pure in-process forwarder around `LayerStackManager`. Don't call something `Client` when it makes zero RPCs. |

### 1.2 Private-prefix lies (🟧)

`runtime/daemon/handler/request_context.py` exports `_services`, `_layer_stack_root`, `_required_single_path`, `_project_changeset` via `__all__`, and every sibling handler (`tools/edit.py`, `tools/read.py`, `tools/write.py`, `handler/health.py`, `service/shell_runner.py`) imports them with the leading underscore. The underscore is a lie: these are the module's public API.

Same pattern in `service/occ_backend.py` (`_backend_cache_clear` is in `__all__`-style usage via test harness) and in `service/workspace_server.py` (`_clear_layer_stack_server_caches_for_tests` is the test seam, `_drop_layer_stack_manager` is called by `build_workspace_base`).

**Pick one**: either drop the underscore (it's public) or stop using these across module boundaries (currently mandatory).

### 1.3 Casing inconsistency (🟨)

OCC is spelled three ways in the same file:

```python
# service/occ_backend.py
@dataclass(frozen=True)
class OccBackend:
    occ_service: OccService       # Occ
    occ_client: OCCClient         # OCC
```

`OCCClient`, `OccService`, `OccBackend`. Also `OCCMutationClient` (imported from `command_exec`) vs `OccService`. Pick one — `OCC` (acronym) is the canonical Python style. Cost to fix: ~10-min IDE rename. Cost to leave: every new contributor wastes 30 seconds figuring out which to use.

### 1.4 Names that don't say "what kind"

- `runtime_bundle.py` — fine, but it owns *both* the build-tar logic *and* the remote-dir constants (`BUNDLE_REMOTE_DIR`) that are imported by `daemon_client.py`. The constants are not "runtime bundle" concerns; they are wire-protocol concerns. Move them.
- `daemon_client.py` — actually three things: the launch script generator, the thin-client Python source, and the dispatch/retry/readiness logic. The "client" name is closer to the third concern only.

---

## 2. Folder/file structure — **C-**

### 2.1 `runtime/` is a wrapper around `daemon/` (🟧)

```
sandbox/runtime/
├── __init__.py            (empty)
├── async_bridge.py        ← 300 LOC, nothing daemon-specific
└── daemon/
    ├── __main__.py
    ├── handler/  rpc/  service/
```

The `runtime/` package exists to hold one helper that isn't even daemon-specific (`async_bridge.py` is used by `host/`, not by `daemon/`). It's a one-child wrapper. Either:

1. Promote `async_bridge.py` to `sandbox/async_bridge.py` (shared infra) and rename `runtime/daemon/` → `runtime/`, OR
2. Move `async_bridge.py` *into* `daemon/` if it really belongs there.

As it stands, `runtime/` adds a level without adding a concept.

### 2.2 Asymmetric layout host/ vs runtime/ (🟧)

```
sandbox/host/             — flat, 7 files
sandbox/runtime/daemon/   — handler/, rpc/, service/
```

There's no apparent reason `host/` is flat while `runtime/daemon/` is three-tier. `host/` has at least four distinct concerns:

- bundle build + upload (`runtime_bundle.py`),
- daemon RPC client + spawn (`daemon_client.py`),
- post-create lifecycle (`setup.py`, `git.py`, `recovery.py`),
- provider hooks (`context.py`).

A cleaner layout mirrors `daemon/`:

```
sandbox/host/
├── bundle/         (runtime_bundle.py)
├── rpc/            (daemon_client.py — client of the in-sandbox daemon)
├── lifecycle/      (setup.py renamed bootstrap.py, git.py, recovery.py)
└── provider_hooks/ (context.py)
```

Then `host/` and `runtime/` are visibly peers.

### 2.3 Five-level nesting (🟨)

`sandbox/runtime/daemon/handler/tools/edit.py` is 5 levels deep before you touch a file. Each level adds friction (longer import paths, more `__init__.py` boilerplate, more grep noise). The `handler/tools/` split is artificial — see §2.5.

### 2.4 Trampoline files (🟧)

Two files exist solely to forward-import:

- `daemon/handler/tools/shell.py` (13 lines): `async def shell(args): return await shell_runner.execute_shell_api(args)`. Pure trampoline.
- `daemon/handler/plugins.py` (12 lines): `from sandbox.plugin.handler import plugin_ensure, plugin_status`. Pure re-export.

If you want consistency in `_load_peer_bootstraps`, hook these straight into the `OP_TABLE` registration map. The trampoline files don't add boundaries; they add files to maintain.

### 2.5 `handler/` vs `handler/tools/` is invisible (🟨)

```
handler/
├── health.py        — control op
├── metrics.py       — control op
├── overlay.py       — tool-like
├── plugins.py       — control op
├── workspace.py     — control op
└── tools/
    ├── edit.py      — tool verb
    ├── read.py      — tool verb
    ├── shell.py     — tool verb
    └── write.py     — tool verb
```

`overlay.py` is at the parent level but conceptually a tool verb (executes a shell capture). The intent is "agent-facing tool verbs vs daemon control ops" — fine — but it's not enforced and `overlay` breaks it already. Either move `overlay.py` → `tools/`, or drop the split entirely and rely on the `api.*` op-name prefix as the contract.

### 2.6 `daemon/handler/__init__.py` eagerly imports the entire handler tree

```python
from . import health, metrics, overlay, tools, workspace
```

This forces every handler module (and their transitive imports) to load whenever anyone touches the package — including unit tests that only need one verb. The whole `OP_TABLE` is already side-effect-registered (see §4.4); the eager import is a second copy of that registration. Pick one.

---

## 3. Import dependency chain — **D**

Multiple inverted layers and pervasive function-local imports.

### 3.1 🟥 `host/` depends on `runtime/`

```
host/recovery.py:19   from sandbox.runtime.async_bridge import run_sync
host/git.py:57        from sandbox.runtime.async_bridge import run_sync  (inside function)
host/setup.py:66,88,129  from sandbox.runtime.async_bridge import run_sync  (inside functions)
```

`runtime/async_bridge.py` is shared infra (loop-aware sync bridge). It's not "runtime"-specific. Yet it lives under `runtime/` and `host/` reaches across into it. **The dependency arrow you want — `host → runtime` only via the network/wire protocol — is broken at the source level.** Today, deleting `runtime/` breaks `host/` even before any sandbox starts.

**Fix**: move `async_bridge.py` to `sandbox/async_bridge.py` (or `sandbox/_io.py`). It already imports nothing from `sandbox.*`.

### 3.2 🟧 Pervasive function-local imports

```python
# host/setup.py
def run_runtime_bootstrap(...):
    from sandbox.runtime.async_bridge import run_sync           # local
def ensure_workspace_base(...):
    from sandbox.runtime.async_bridge import run_sync
    from sandbox.host.daemon_client import call_daemon_api      # local
def start_runtime_bundle_upload(...):
    from sandbox.runtime.async_bridge import run_sync           # local
def bootstrap_in_sandbox_runtime(...):
    from sandbox.host.runtime_bundle import ensure_runtime_uploaded  # local

# host/git.py
def ensure_git(...):
    try:
        from sandbox.runtime.async_bridge import run_sync       # local
        from sandbox.provider.registry import get_adapter       # local

# runtime_bundle.py
def _vendor_pathspec(tar):
    import pathspec as _pathspec                                # local
```

Function-local imports are a code-smell that almost always indicates either a circular-import patch or laziness about cold-start cost. Either way, they hide the module's real dependency graph from anyone reading the top of the file. **Move every import in this package to the module top unless there is a *documented* reason it has to be lazy** (e.g., the pathspec import is for vendoring at bundle-build time only; keep that one but comment why).

### 3.3 🟧 `daemon_client.py` ↔ `runtime_bundle.py` coupling

```python
# host/daemon_client.py:10
from sandbox.host.runtime_bundle import BUNDLE_REMOTE_DIR, bundle_hash
```

`BUNDLE_REMOTE_DIR` is a wire-protocol constant (the path inside the sandbox where the socket / pid / log live). It has nothing to do with building the tarball. Pulling it from `runtime_bundle.py` couples the RPC client to the bundle builder. Move to `host/_paths.py` (host-side) or — better — declare it in `runtime/daemon/rpc/server.py` (which is the source of truth for `DEFAULT_SOCKET_PATH`) and import it from there.

This matters because today, to change the socket location, you have to edit three files (`server.py`, `daemon_client.py`, `runtime_bundle.py`'s `BUNDLE_REMOTE_DIR`), in three packages, and hope none drifted.

### 3.4 🟥 Private-attribute reach across module boundaries

```python
# runtime/daemon/handler/health.py
from sandbox.runtime.daemon.handler import request_context
...
handlers_backend = request_context._services(layer_stack_root)
shell_services = shell_runner._services({"layer_stack_root": layer_stack_root})
```

Two siblings call each other's underscore-prefixed function. This works only because Python doesn't enforce visibility. It guarantees the "private" rename will silently break the health probe.

### 3.5 🟧 `OP_TABLE` is a module-global mutable dict

```python
# runtime/daemon/rpc/dispatcher.py:27
OP_TABLE: dict[str, Handler] = {}
```

Filled at import time by `_load_peer_bootstraps()` and mutated again by `plugin.ensure` flushing pending registrations. Consequences:

- No way to construct two independent daemons in the same Python process (tests).
- No way to scope a handler to a tenant / layer-stack-root.
- Adding a registration is a side-effectful import — `from x import y` can mutate `OP_TABLE`.

Replace with an `OpRegistry` instance, owned by the `Daemon` / `Server`. Pass it to peer modules instead of importing it. This is one of the highest-ROI refactors in the package.

---

## 4. Extensibility & inheritance/interface use — **D+**

The package *talks* about extensibility ("Explicit extension point for daemon-scoped feature flags", "peer bootstrap modules", "Plugin-specific ops") but the actual seams are weak.

### 4.1 🟧 Handler "interface" is `Callable[[dict], Any]`

```python
Handler = Callable[[dict[str, Any]], Any]
OP_TABLE: dict[str, Handler] = {}
```

That's the entire handler contract. There is no:
- request schema,
- response schema,
- error contract beyond a string `kind`,
- middleware / interceptor chain,
- per-op auth / quota hook.

Every handler reimplements `args.get("layer_stack_root") → validate → str.strip → ValueError`. Every handler builds `{"success": True, ..., "timings": {...}}` by hand. There is no `BaseHandler` ABC, no decorator (`@op("api.read_file")`), no typed envelope. Result: ~60% of each handler file is boilerplate.

**Minimal fix**: define `class HandlerBase(Generic[ReqT, ResT])` with `request_model: Type[ReqT]`, `response_model: Type[ResT]`, and `async def handle(req: ReqT) -> ResT`. The dispatcher does the JSON ↔ model marshalling. Net deletion: ~200 lines from `health.py` + `metrics.py` + `tools/*` alone.

### 4.2 🟥 Adding a new op is a three-file coordinated edit

To add `api.foo`, you must:

1. Create `runtime/daemon/handler/foo.py` (or `tools/foo.py`).
2. Add it to `_load_peer_bootstraps()` in `dispatcher.py`.
3. Make sure no other peer also claims `api.foo` (otherwise `_load_peer_bootstraps` raises at import time).

There is no registration-by-decorator, no auto-discovery, no per-package registry. Compare to FastAPI's `@router.post("/api/foo")` — that's the standard. Yours is closer to 2010-era Django URL conf written by hand. Plus, since `_load_peer_bootstraps` runs at module import, a syntax error in any new handler breaks the entire daemon at boot.

### 4.3 🟧 `OccBackend` is a duck-typed structural contract enforced only at probe time

```python
@dataclass(frozen=True)
class OccBackend:
    layer_stack: LayerStackClient
    occ_service: OccService
    occ_client: OCCClient
    gitignore: SnapshotGitignoreOracle
    manager: LayerStackManager
```

But `health.py`:

```python
expected_fields = ("layer_stack","occ_client","gitignore","manager")
missing_fields = [f for f in expected_fields if not hasattr(handlers_backend, f)]
```

So the contract is enforced by `hasattr` at runtime, against a frozen dataclass. If you rename `manager` → `layer_stack_manager`, you'll find out via the readiness probe — not via the type system. Either drop the probe (the dataclass already guarantees the fields) or drop the structural check and use `isinstance(backend, OccBackend)`. Doing both is pure noise.

### 4.4 🟧 `LayerStackClient` is a tax, not an abstraction

```python
class LayerStackClient:
    def __init__(self, root: str | Path | LayerStackManager) -> None:
        self.manager = ...
    def read_active_manifest(self): return self.manager.read_active_manifest()
    def read_bytes(self, path, manifest): return self.manager.read_bytes(path, manifest)
    def read_text(self, path, manifest): return self.manager.read_text(path, manifest)
    def materialize(self, dest, manifest): return self.manager.materialize(...)
    def commit_transaction(self): return self.manager.commit_transaction()
    # ...8 more pure forwarders
```

Every method is a forwarder. The `__init__(str | Path | LayerStackManager)` union signature is a smell — a class should not be both "make me from a string" and "wrap an existing instance". The two `prepare_workspace_snapshot` and `release_lease` methods accept `workspace_ref` and immediately `del workspace_ref` it — they're satisfying a Protocol that the underlying manager doesn't have.

The "boundary" doesn't actually narrow the interface — it forwards everything. **Either narrow it (drop methods you don't need) or delete the class and use `LayerStackManager` directly.** Right now it costs ~85 lines and a level of indirection for no measurable isolation gain.

### 4.5 🟧 `RuntimeWorkspaceBindingReader` and `LayerStackWorkspaceServer` are single-method classes

`RuntimeWorkspaceBindingReader` (37 LOC) has exactly one method `require_workspace_binding`. No state. No alternate implementation in the codebase. Why a class? Make it a function `require_runtime_binding(workspace_ref) -> WorkspaceBindingSnapshot`.

`LayerStackWorkspaceServer` (80 LOC) has `_manager` as state but `build_workspace_base()` reassigns `self._manager = get_layer_stack_manager(...)` *after* dropping the cache — so the instance state doesn't even hold a useful invariant. The four public methods could be free functions; the constructor adds nothing beyond pre-resolving a Path.

Both are good candidates for the "if your class has one method and constructor-as-DI, it's a closure" refactor.

### 4.6 🟧 `SandboxContextPreparer` Protocol is too loose

```python
class SandboxContextPreparer(Protocol):
    def prepare_context(self, context: Any) -> None: ...
    async def prepare_context_async(self, context: Any) -> None: ...
```

`context: Any`. A Protocol with `Any` parameters is barely better than `Callable`. What is `context`? Pull it out of the caller and type it: `RuntimeContext` (or whatever the dataclass is). Right now the Protocol catches no mistakes at type-check time.

### 4.7 🟨 No version / capability discovery on the wire

The host bundles a Python tar and uploads it; the daemon imports from `sandbox.runtime.daemon`. There is no `api.version` op, no `api.capabilities` op. A host running a newer bundle than the daemon's process (after a hot-swap) won't notice until a missing op surfaces as `unknown_op` at first use. Add an `api.version` op (also useful for diagnostics).

### 4.8 🟨 Wire protocol is non-evolvable

```
request:  {"op": "...", "args": {...}}\n
response: {"success": true, ...}\n
```

Single line of JSON. No length prefix, no chunking, no streaming. `api.shell` with a 10MB stdout takes a single 10MB JSON response, decoded into a Python dict, all in memory at both ends. The 16 MiB cap is the only safeguard. Once you want streaming logs (which you will), this protocol needs a major version bump. Worth designing the v2 envelope now (header + body, content-type, chunk markers).

### 4.9 🟧 Hardcoded Python version list duplicated

```python
# daemon_client.py — appears TWICE
for py in python3.13 python3.12 python3.11 python3.10 python3; do ...
```

Both `_DAEMON_THIN_CLIENT_LAUNCHER` and `_daemon_launcher` hardcode the same five interpreters. To support python3.14, two strings must change in lockstep. Extract a constant once.

### 4.10 🟥 `_FORWARDED_DAEMON_ENV: tuple[str, ...] = ()` is fake extensibility

```python
# daemon_client.py:25
# Explicit extension point for daemon-scoped feature flags. Keep empty unless
# a runtime setting must restart the daemon when the host value changes.
_FORWARDED_DAEMON_ENV: tuple[str, ...] = ()
```

It's empty. There is no caller that adds to it, no settings hook, no docs telling a contributor *how* to add a forwarded env var. This is a comment claiming extensibility, not an extension point. Either:

- Delete it and stop pretending, or
- Wire it through `sandbox/config.py` so it actually reads from project settings.

---

## 5. Shell-script-in-Python — **F**

Three files generate non-trivial shell scripts as Python triple-quoted strings:

- `host/git.py` — `_GIT_BOOTSTRAP` (29 lines of shell, supports 5 package managers).
- `host/daemon_client.py` — `_DAEMON_THIN_CLIENT_LAUNCHER` (10 lines) + `_DAEMON_THIN_CLIENT_PY` (27 lines of Python embedded in a shell string!) + `_daemon_launcher` (30 lines).

This is the worst kind of code: untyped, unlintable, untestable, no syntax highlighting, no shellcheck, no `python -m py_compile`. The thin-client Python source is *especially* bad — it's a 27-line Python program that exists as a single concatenated Python string literal inside another Python file, then gets `shlex.quote`-d and embedded into a sh -c invocation.

**Fix**: ship these as real files in the bundle (`runtime/scripts/thin_client.py`, `runtime/scripts/install_git.sh`, `runtime/scripts/launch_daemon.sh`). The bundle already contains everything else under `sandbox.runtime.daemon.*`; adding three .py and .sh files is trivial. Then `shlex.quote` a *path*, not a 50-line script.

Cost of the current shape: every change to the thin client requires (a) editing a string, (b) memorizing the bash + Python escaping rules, (c) hoping the heredoc-quote nesting still works, (d) no test coverage because you cannot import a string.

---

## 6. Duplication & dead extension points

- 🟧 `setup_after_create` and `setup_after_start` (host/setup.py:227–250) — identical bodies, different docstrings. The author noticed and made them peers; one delegates to nothing. Collapse: `setup_post_lifecycle(sandbox_id, workspace_root, *, phase: Literal["create","start"])`.
- 🟨 `bundle_hash` and `bundle_hash(bundle=None)` overload — the function has two distinct meanings depending on arg presence, with two distinct caches. Split into `bundle_hash() -> str` (cached, default bundle) and `compute_bundle_hash(bundle: bytes) -> str` (pure helper).
- 🟧 `_BUNDLE_CACHE` and `_BUNDLE_HASH_CACHE` are process-global. Tests need to reach in to reset. No reset-API documented. Add a `clear_bundle_caches()` test seam.
- 🟧 `_BACKEND_CACHE` in `occ_backend.py` is also process-global and unbounded; `drop_backend_cache(layer_stack_root)` is the only eviction. Adding 10k workspaces in a long-lived daemon will OOM. Add an LRU bound or a TTL.

---

## 7. Concrete refactor plan (priority-ordered)

Each step is independently mergeable.

1. **(1 PR, ~30 min)** Move `runtime/async_bridge.py` → `sandbox/async_bridge.py`. Update 5 imports. Removes the inverted layer.
2. **(1 PR, ~1 hr)** Rename `host/setup.py` → `host/bootstrap.py`, fold `host/git.py` and `host/recovery.py` into it (or into `host/lifecycle/`). Three files become one or three-in-a-subpackage; `setup.py` stops shadowing packaging.
3. **(1 PR, ~2 hr)** Extract thin-client Python and daemon-launch sh into real files under `runtime/scripts/`. Ship them in the bundle. Eliminates triple-string shell.
4. **(1 PR, ~1 hr)** Move `BUNDLE_REMOTE_DIR` and friends to a `host/_paths.py`; have `daemon_client.py` and `runtime_bundle.py` both import from there. Breaks the back-coupling.
5. **(1 PR, ~2 hr)** Convert `OP_TABLE` to an `OpRegistry` instance. Pass it to peer modules. Daemon constructs one. Tests can construct their own. Plugin "pending registrations" pattern becomes `registry.register_pending(...)` on the instance.
6. **(1 PR, ~3 hr)** Define `class HandlerBase[ReqT, ResT]` + `@op("api.foo", request=FooReq, response=FooRes)` decorator. Migrate `tools/read|write|edit` (the cleanest cases). Delete ~150 lines of boilerplate.
7. **(1 PR, ~30 min)** Rename `_services`, `_layer_stack_root`, `_required_single_path` in `request_context.py` to drop the underscore (or move them so they're internal to one consumer).
8. **(1 PR, ~30 min)** Delete `LayerStackClient`. Use `LayerStackManager` directly via `OccBackend`. Drop `Client`/`Server` from class names that mislead.
9. **(later)** Add `api.version` + `api.capabilities` ops. Cheap forward-compat win.
10. **(later)** Design wire v2 (length-prefix, streaming for `api.shell`).

---

## 8. What's actually *good* — don't undo this

To balance the harshness:

- The host/runtime split as a **concept** is correct: orchestrator-side vs in-sandbox-side. Keep it.
- `LayerStackWorkspaceServer.ensure_workspace_base` returning `(binding, created: bool)` is a clean idempotent-create pattern.
- Bundle uploading uses base64+chunking with explicit size budget (`_CHUNK_SIZE`) — solid.
- `_validate_envelope` and `_to_jsonable` in `dispatcher.py` are correctly structured. Just extend them with schemas.
- `OccBackend` as a *named tuple of services* (not a god-class) is the right shape — just enforce it via types, not `hasattr`.
- `run_sync_in_executor` is well-documented for *why* `asyncio.to_thread` doesn't work here (the 6.4x → 45x parallelism note is excellent code-archaeology). Keep that comment.
- `_handle_connection` correctly distinguishes `LimitOverrunError` vs `ValueError` vs `TimeoutError` and returns a structured envelope. Defensive in the right way.

---

## Summary scoreboard

| Dimension | Grade | Headline |
|---|---|---|
| Naming convention | **D+** | At least four misleading filenames; private-prefix lies; OCC cased three ways. |
| Folder/file structure | **C-** | Asymmetric host vs daemon; `runtime/` is a one-child wrapper; `handler/` vs `handler/tools/` split is invisible. |
| Import dependency chain | **D** | `host/` imports `runtime/`; private attrs reached across siblings; pervasive function-local imports; `OP_TABLE` is a module-global. |
| Extensibility | **D+** | New op = 3-file coordinated edit; fake extension points (`_FORWARDED_DAEMON_ENV`); shell-in-string makes scripts unmodifiable. |
| Inheritance / interface | **D+** | Handler "interface" is `Callable[[dict], Any]`; classes with one method and no invariants; `Protocol` with `Any`. |
| Future flexibility | **C-** | Wire protocol is non-evolvable; no version/capabilities; bundle cache is global. |
| **Overall** | **C-** | Works today, will rot fast. The refactor plan above is ~12 hours of work, repaid within a month. |
