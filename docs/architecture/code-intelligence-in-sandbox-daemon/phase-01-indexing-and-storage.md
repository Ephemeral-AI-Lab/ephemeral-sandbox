# Phase 1 — In-sandbox indexing + storage skeleton

**Estimated effort:** 5-6 days (3 days engineering + 2-3 days E2E)
**Risk profile:** MEDIUM-HIGH — the first real test of the `$HOME/.cache/eos-ci/` privilege assumption AND the eager-bootstrap hook that reverses the predecessor migration's contract
**Status:** Not started
**Blocks on:** Phase 0 complete

## Goal

Three deliverables:

1. **In-sandbox indexing.** Ship the entire existing `sandbox.code_intelligence` package and invoke the existing `CodeIntelligenceService` with `sandbox=None, transport=None` so local-FS branches activate. Persist snapshot at `$HOME/.cache/eos-ci/<wh>/v1/index.snapshot` (pickle for Phase 1; Phase 3.5 migrates to SQLite).
2. **Eager bootstrap hook** (NEW, see overview "Eager bootstrap contract"). Wire `bootstrap_in_sandbox_ci_runtime` into `SandboxService.create_sandbox` and `SandboxService.start_sandbox` so the bundle is uploaded and the indexer runs synchronously before `create_sandbox` returns. This **reverses** the predecessor migration's "no CI on create" contract.
3. **Compatibility probe** (NEW). Phase 1 ships a one-shot dependency matrix probe (`mkdir`/`unshare`/`sqlite3`/`msgpack`/`git`/etc.) so a new sandbox image can be qualified in one test run before any daemon work depends on it.

For Phase 1, the orchestrator invokes the indexer via `process.exec` (no daemon yet — that lands in Phase 2). The eager hook in Phase 1 just does the bundle+indexer; Phase 2 extends the hook to also spawn the daemon.

## Why second

Three reasons:

1. **Biggest network savings, lowest blast radius.** Indexing today downloads every `.py` file to feed `ast.parse`. Moving that into the sandbox eliminates the largest network cost in the migration. It does NOT touch the OCC pipeline, so even a regression here can't break edits.
2. **Validates the storage-path privilege assumption against real Daytona.** `$HOME/.cache/eos-ci/` is the load-bearing assumption from the overview's gray-area decision #1. Phase 1's E2E asserts `mkdir -p $HOME/.cache/eos-ci/...` works without sudo on a real `dask` swe-evo sandbox. If it doesn't, we discover this BEFORE building a daemon on top of it.
3. **One-shot script first, daemon later.** A standalone CLI is simpler than an asyncio event-loop server. Proving the bundle-shipping pattern, the storage layer, and the orchestrator-side `DaemonBackend.build_index()` integration in Phase 1 reduces Phase 2's surface area to "just" the daemon lifecycle.

## Design choice — bundle the entire package, not hand-picked copies

**Rejected approach:** copy hand-picked modules into daemon-local extracted files, guard with source-equality drift tests. Adds a maintenance treadmill (every change to `extract_symbols` or `WriteCoordinator` needs a sync), risks subtle drift, and produces two source-of-truth copies of the same logic.

**Chosen approach:** ship the **entire** `backend/src/sandbox/code_intelligence/` tree as the bundle. The daemon does:

```python
from sandbox.code_intelligence.service import CodeIntelligenceService
svc = CodeIntelligenceService(
    sandbox_id="local",
    workspace_root=args.workspace_root,
    sandbox=None,         # → InProcessBackend's local-FS branches
    transport=None,       # → no daemon command roundtrips
)
svc.ensure_initialized(wait=True)
```

**Why this works:**
- The existing `CodeIntelligenceService` already has working local-FS code paths (`_read_local`, `_write_local`, `_apply_local_batch_checked`, `collect_local_files`). They're just dormant whenever a sandbox handle is bound.
- No new code paths means no new bugs.
- Drift risk is eliminated **by construction** — the daemon and the orchestrator's in-process backend run literally the same Python code.
- Phase 5 cleanup (~600 LOC deletion) only touches the dead REMOTE branches, not the local ones.

**Trade-offs:**
- Bundle size: estimated +200 KB (~50 Python files in `sandbox/code_intelligence/`). Negligible vs network savings.
- Dead remote branches travel inside the daemon process for Phases 1-4. Harmless; never executed because `sandbox=None`. Cleaned up in Phase 5.

## What ships

| Artifact | File | Purpose |
|---|---|---|
| Bundle helper | `backend/src/sandbox/code_intelligence/daemon/launcher.py` | `_runtime_bundle_bytes()` packs `sandbox/code_intelligence/` + `sandbox/client/async_bridge.py` + **vendored `msgpack/`** (~50KB) |
| Indexing runner | `backend/src/sandbox/code_intelligence/daemon/server.py` | Instantiates `CodeIntelligenceService`; index readiness is exposed through daemon command dispatch |
| Storage layer | `backend/src/sandbox/code_intelligence/daemon/storage.py` | `$HOME/.cache/eos-ci/<wh>/v1/` resolver, atomic snapshot writer, integrity check, **path-confinement guard** |
| Daemon package | `backend/src/sandbox/code_intelligence/daemon/__init__.py` | Daemon package marker |
| `DaemonBackend.build_index()` | `backend/src/sandbox/code_intelligence/backends/` (extended) | Ships payload, runs CLI, downloads snapshot |
| **Eager bootstrap hook** | `backend/src/sandbox/lifecycle/workspace.py` (`bootstrap_in_sandbox_ci_runtime`) | Bundle upload + indexer run, called from `create_sandbox` and `start_sandbox` |
| **Lifecycle integration** | `backend/src/sandbox/lifecycle/service.py` (modified) | `create_sandbox(...)` and `start_sandbox(...)` call the hook before returning when `EOS_CI_IN_SANDBOX=1` |
| Phase 1 live E2E | `backend/tests/test_e2e/test_live_ci_phase1_indexing.py` | Privilege probe + **compatibility matrix probe** + indexing + corruption recovery + **eager-bootstrap timing** |
| Storage unit tests | `backend/tests/test_sandbox/test_code_intelligence/test_storage.py` | Atomic rename, corruption recovery, missing-dir creation, path-confinement guard |
| Index unit tests | `backend/tests/test_sandbox/test_code_intelligence/test_ci_index_runner.py` | Standalone runner against a fixture workspace |
| Lifecycle hook unit tests | `backend/tests/test_sandbox/test_eager_ci_bootstrap.py` | Lifecycle bootstrap runs when the flag is set and skips when the flag is unset or workspace resolution fails |

## Detailed task list

### Task 1.1 — Storage layer

**File to create:** `backend/src/sandbox/code_intelligence/daemon/storage.py`

**API:**

```python
import os, hashlib, pickle, tempfile, errno, logging
from pathlib import Path
from typing import Any

class StorageUnavailable(Exception):
    """Raised when $HOME/.cache/eos-ci/... can't be created (privilege failure)."""
    def __init__(self, errno: int, path: str, message: str) -> None: ...

class StoragePathEscape(Exception):
    """Raised when a write target escapes the state dir confinement."""

def workspace_root_hash(workspace_root: str) -> str:
    """sha256(realpath(workspace_root))[:16]."""
    return hashlib.sha256(os.path.realpath(workspace_root).encode("utf-8")).hexdigest()[:16]

def state_dir(workspace_root: str) -> Path:
    """Resolve $HOME/.cache/eos-ci/<wh>/v1/ and mkdir -p. Raises StorageUnavailable on EACCES."""
    home = Path(os.path.expanduser("~"))
    base = home / ".cache" / "eos-ci" / workspace_root_hash(workspace_root) / "v1"
    try:
        base.mkdir(parents=True, exist_ok=True)
    except PermissionError as exc:
        raise StorageUnavailable(
            errno=exc.errno or errno.EACCES, path=str(base),
            message=f"Cannot create CI state dir at {base} (errno={exc.errno}); "
                    f"running as user={os.getenv('USER')}, HOME={home}",
        ) from exc
    return base

def _confine(state: Path, name: str) -> Path:
    """Resolve `name` under `state` and reject path traversal. Load-bearing for the
    storage boundary — see overview.md."""
    target = (state / name).resolve()
    if state.resolve() not in target.parents and target != state.resolve():
        raise StoragePathEscape(f"path {target} escapes state dir {state}")
    return target

def write_snapshot(state: Path, name: str, payload: Any) -> None:
    """Atomic write: tempfile in same dir, fsync, os.replace.
    Pickle protocol 5; payload may be Any pickleable structure."""
    target = _confine(state, name)
    fd, tmp = tempfile.mkstemp(prefix=f".{name}.", suffix=".tmp", dir=state)
    try:
        with os.fdopen(fd, "wb") as f:
            pickle.dump(payload, f, protocol=5)
            f.flush()
            os.fsync(f.fileno())
        os.replace(tmp, target)
    except Exception:
        try: os.unlink(tmp)
        except OSError: pass
        raise

def read_snapshot(state: Path, name: str) -> Any | None:
    """Load snapshot. On any corruption (EOFError, UnpicklingError, OSError),
    log a warning, unlink the corrupt file, return None so caller rebuilds."""
    target = _confine(state, name)
    if not target.exists():
        return None
    try:
        with open(target, "rb") as f:
            return pickle.load(f)
    except (EOFError, pickle.UnpicklingError, OSError) as exc:
        logging.warning("storage: corrupt snapshot at %s (%s); unlinking", target, exc)
        try: target.unlink()
        except OSError: pass
        return None
```

**Critical contracts:**
- DO NOT silently fall back to `/tmp` on `PermissionError`. Surface `StorageUnavailable` so the Phase 1 E2E can fail loud with the exact errno + `$HOME` value.
- `_confine` is load-bearing: it prevents an daemon command handler from writing `../../../workspace_root/foo` and bypassing the storage boundary. Phase 3 adds a daemon-side bypass-attempt guard test on top of this.

**Note on Phase 3.5 SQLite migration:** the `write_snapshot/read_snapshot` API stays stable. Phase 3.5 swaps the implementation to back onto a SQLite table without changing the call sites in `daemon indexer` or `server.py`.

### Task 1.2 — Indexing CLI

**File to create:** `backend/src/sandbox/code_intelligence/daemon/server.py`

**Contract:** Run as `python -m sandbox.code_intelligence.daemon.ci_index --workspace-root <path> [--file <single>]`. Construct a `CodeIntelligenceService` against `sandbox=None, transport=None` and let the existing local-FS code paths walk the workspace. Persist the resulting symbol index via `storage.write_snapshot`.

**Implementation skeleton:**

```python
import argparse, json, sys, time
from pathlib import Path

from sandbox.code_intelligence.service import CodeIntelligenceService
from sandbox.code_intelligence.daemon.storage import (
    state_dir, write_snapshot, read_snapshot, StorageUnavailable,
)

def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--workspace-root", required=True)
    parser.add_argument("--file", default=None, help="Re-index a single file only")
    args = parser.parse_args()

    started = time.perf_counter()
    try:
        state = state_dir(args.workspace_root)
    except StorageUnavailable as exc:
        print(json.dumps({"ok": False, "error": "storage_unavailable",
                          "errno": exc.errno, "path": exc.path, "message": exc.message}))
        return 13

    svc = CodeIntelligenceService(
        sandbox_id="local",
        workspace_root=args.workspace_root,
        sandbox=None,
        transport=None,
    )

    if args.file:
        # Incremental refresh path
        prior = read_snapshot(state, "index.snapshot") or {}
        gen = svc.symbol_index.refresh(args.file)
        # Patch only the affected file in the snapshot dict
        prior[args.file] = svc.symbol_index.file_symbols(args.file)
        write_snapshot(state, "index.snapshot", prior)
        elapsed = time.perf_counter() - started
        print(json.dumps({"ok": True, "mode": "refresh_one", "file": args.file,
                          "generation": gen, "elapsed_s": round(elapsed, 4)}))
        return 0

    # Full build path
    svc.ensure_initialized(wait=True)
    snapshot = {fp: svc.symbol_index.file_symbols(fp) for fp in svc.symbol_index.indexed_paths()}
    write_snapshot(state, "index.snapshot", snapshot)
    elapsed = time.perf_counter() - started
    print(json.dumps({
        "ok": True,
        "mode": "full_build",
        "file_count": svc.symbol_index.indexed_files,
        "symbol_count": svc.symbol_index.size,
        "snapshot_path": str(state / "index.snapshot"),
        "elapsed_s": round(elapsed, 4),
    }))
    return 0

if __name__ == "__main__":
    sys.exit(main())
```

**Critical detail:** the `CodeIntelligenceService` constructor with `sandbox=None, transport=None` is already supported today (look at `service.py:51-107`). The `SymbolIndex._discover_files` method falls back to `collect_local_files` when `Path(workspace_root).is_dir()` is True. No new code paths needed in the existing service.

### Task 1.3 — Bundle helper (with vendored msgpack)

**File to create:** `backend/src/sandbox/code_intelligence/daemon/__init__.py` (empty) and `backend/src/sandbox/code_intelligence/daemon/launcher.py`

**Vendored msgpack:** the bundle includes msgpack so the daemon works on offline images without `pip install`. Ship the **pure-Python** msgpack (avoid C extension wheel-arch concerns). Locate via `python -c "import msgpack, os; print(os.path.dirname(msgpack.__file__))"` at bundle-build time.

**API:**

```python
import io, tarfile, hashlib, base64, shlex
from pathlib import Path

def _runtime_bundle_bytes() -> bytes:
    """Return tar.gz of the bundle.
    Layout (inside tar):
        msgpack/                                             (vendored — pure Python)
        sandbox/__init__.py                                  (empty marker)
        sandbox/client/async_bridge.py                       (existing — promoted in predecessor)
        sandbox/code_intelligence/**/*.py                    (FULL package)
    """
    import msgpack
    repo_root = Path(__file__).parent.parent.parent.parent  # backend/src/
    sandbox_dir = repo_root / "sandbox"
    msgpack_dir = Path(msgpack.__file__).parent

    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w:gz") as tar:
        # Vendored msgpack — only the .py files (skip compiled .so)
        for path in sorted(msgpack_dir.rglob("*.py")):
            rel = path.relative_to(msgpack_dir.parent).as_posix()
            tar.add(path, arcname=rel)
        # Sandbox markers + async_bridge
        tar.add(sandbox_dir / "__init__.py", arcname="sandbox/__init__.py")
        tar.add(sandbox_dir / "client" / "async_bridge.py", arcname="sandbox/client/async_bridge.py")
        # Full code_intelligence/ tree
        ci_dir = sandbox_dir / "code_intelligence"
        for path in sorted(ci_dir.rglob("*.py")):
            rel = path.relative_to(sandbox_dir).as_posix()
            tar.add(path, arcname=f"sandbox/{rel}")
    return buffer.getvalue()


async def ensure_runtime_uploaded(transport, sandbox_id: str) -> None:
    """Upload bundle to /tmp/eos-ci-runtime/ once per (transport, sandbox_id) pair.
    Idempotent: subsequent calls no-op if a marker file with matching bundle hash exists."""
    bundle = _runtime_bundle_bytes()
    bundle_hash = hashlib.sha256(bundle).hexdigest()
    marker_check = await transport.exec(
        sandbox_id,
        f"test -f /tmp/eos-ci-runtime/.bundle-hash && cat /tmp/eos-ci-runtime/.bundle-hash",
    )
    if marker_check.exit_code == 0 and marker_check.stdout.strip() == bundle_hash:
        return  # Already uploaded
    # Upload + extract + write marker
    encoded = base64.b64encode(bundle).decode("ascii")
    upload_cmd = (
        f"mkdir -p /tmp/eos-ci-runtime && "
        f"cd /tmp/eos-ci-runtime && "
        f"echo {shlex.quote(encoded)} | base64 -d | tar -xzf - && "
        f"echo {shlex.quote(bundle_hash)} > .bundle-hash"
    )
    result = await transport.exec(sandbox_id, upload_cmd, timeout=60)
    if result.exit_code != 0:
        raise RuntimeError(f"runtime bundle upload failed: {result.stdout}")
```

**Bundle size budget:** estimated ~250 KB (200 KB code_intelligence + 50 KB msgpack pure-Python). Alert if total exceeds 1 MB; investigate (likely an accidental `__pycache__` or test fixture inclusion).

### Task 1.3.5 — Eager bootstrap hook (LOAD-BEARING, see overview)

**Files to modify:**
- `backend/src/sandbox/lifecycle/workspace.py` (add `bootstrap_in_sandbox_ci_runtime`)
- `backend/src/sandbox/lifecycle/service.py` (call hook from `create_sandbox` and `start_sandbox`)

**`bootstrap_in_sandbox_ci_runtime` (Phase 1 body):**

```python
async def bootstrap_in_sandbox_ci_runtime(
    sandbox_id: str,
    workspace_root: str,
    *,
    transport: SandboxTransport,
) -> None:
    """Eager CI bootstrap. Called by create_sandbox / start_sandbox.
    Phase 1: upload bundle + run indexer.
    Phase 2: also spawn daemon and wait for socket readiness.
    Phase 3+: spawn daemon (binds socket immediately); index builds in background."""
    if not _ci_in_sandbox_enabled():
        return  # Flag-off path: no-op

    await ensure_runtime_uploaded(transport, sandbox_id)

    # Phase 1: run the indexer once. (Phase 2 will replace this with daemon spawn.)
    cmd = (
        "cd /tmp/eos-ci-runtime && "
        f"python3 -m sandbox.code_intelligence.daemon.ci_index "
        f"--workspace-root {shlex.quote(workspace_root)}"
    )
    result = await transport.exec(sandbox_id, cmd, timeout=300)
    if result.exit_code != 0:
        raise RuntimeError(f"eager CI bootstrap failed: {result.stdout}")
```

**`SandboxService.create_sandbox` integration:**

```python
def create_sandbox(self, ...) -> ...:
    sandbox = self._provision_daytona(...)
    run_sync(bootstrap_in_sandbox_ci_runtime(
        sandbox_id=sandbox.id,
        workspace_root=self._discover_workspace(sandbox),
        transport=self._transport,
    ))
    return sandbox
```

**`SandboxService.start_sandbox` integration:** same hook called after Daytona resume. Existing restart-recovery path (line ~211 in `service.py`) also calls the hook after restart.

**`run_sync` note:** `bootstrap_in_sandbox_ci_runtime` is async; `create_sandbox` is sync today. Use `sandbox.client.async_bridge.run_sync` (promoted in predecessor migration).

**Run command for the indexer (after upload):**
```
cd /tmp/eos-ci-runtime && python3 -m sandbox.code_intelligence.daemon.ci_index --workspace-root <root>
```

### Task 1.4 — `DaemonBackend.build_index()` and `query_symbols`

**File to modify:** `backend/src/sandbox/code_intelligence/backends/`

**Implementation:**

```python
class DaemonBackend:
    async def _ensure_initialized_async(self, *, wait: bool = True) -> bool:
        await ensure_runtime_uploaded(self._transport, self._sandbox_id)
        cmd = (
            "cd /tmp/eos-ci-runtime && "
            f"python3 -m sandbox.code_intelligence.daemon.ci_index "
            f"--workspace-root {shlex.quote(self._workspace_root)}"
        )
        result = await self._transport.exec(self._sandbox_id, cmd, timeout=300)
        if result.exit_code != 0:
            raise RuntimeError(f"ci_index failed: {result.stdout}")
        payload = json.loads(result.stdout.strip().splitlines()[-1])
        if not payload.get("ok"):
            raise RuntimeError(f"ci_index reported failure: {payload}")

        # Resolve the snapshot path (in-sandbox $HOME varies)
        home = await self._resolve_home()
        wh = workspace_root_hash(self._workspace_root)
        snapshot_remote = f"{home}/.cache/eos-ci/{wh}/v1/index.snapshot"

        # Download once, deserialize, cache orchestrator-side
        raw = await self._transport.read_bytes(self._sandbox_id, snapshot_remote)
        self._symbol_cache = pickle.loads(raw)
        self._cached_file_count = payload["file_count"]
        self._cached_symbol_count = payload["symbol_count"]
        self._snapshot_bytes = len(raw)
        self._initialized = True
        return True

    def query_symbols(self, query: str) -> list[SymbolInfo]:
        # Phase 1: orchestrator-side cache (no daemon yet).
        # Phase 2+: this becomes an daemon command.
        needle = query.lower().strip()
        if not needle:
            return []
        results = []
        for symbols in (self._symbol_cache or {}).values():
            for sym in symbols:
                if needle in sym.name.lower():
                    results.append(sym)
        return results
```

**Note:** for Phase 1 we keep the cache orchestrator-side because there's no daemon yet. Phases 2-3 move the cache into daemon memory and `query_symbols` becomes an daemon command.

### Task 1.5 — Phase 1 live E2E

**File to create:** `backend/tests/test_e2e/test_live_ci_phase1_indexing.py`

#### 1.5.A — Privilege probe (LOAD-BEARING)

```python
def test_privilege_probe_home_cache(live_sweevo_env):
    """The most important assertion of Phase 1.
    If $HOME/.cache/eos-ci/ is not writable, the entire migration plan needs amending."""
    h = TimingHarness(phase=1, test_name="privilege_probe")
    env = live_sweevo_env

    with h.step("mkdir_home_cache"):
        code, out = env.exec("mkdir -p $HOME/.cache/eos-ci/test_privilege && ls -la $HOME/.cache/eos-ci/")

    if code != 0:
        home_code, home_val = env.exec("echo $HOME")
        whoami_code, whoami_val = env.exec("whoami")
        umask_code, umask_val = env.exec("umask")
        pytest.fail(
            f"PRIVILEGE FAILURE: mkdir $HOME/.cache/eos-ci failed with exit_code={code}\n"
            f"  output: {out!r}\n"
            f"  $HOME = {home_val!r}\n"
            f"  whoami = {whoami_val!r}\n"
            f"  umask = {umask_val!r}\n"
            f"  ACTION: amend gray-area decision #1 to a writable path."
        )

    print(h.report())
    h.dump_json()
```

#### 1.5.B — Indexing readiness

```python
async def test_indexing_readiness(live_sweevo_env):
    h = TimingHarness(phase=1, test_name="indexing_readiness")
    env = live_sweevo_env

    with h.step("index_build_in_sandbox"):
        with mock.patch.dict(os.environ, {"EOS_CI_IN_SANDBOX": "1"}):
            svc = env.make_ci_service()  # constructs DaemonBackend
            svc.ensure_initialized(wait=True)
    h.record("index_build_in_sandbox",
             count=svc._impl._cached_file_count,
             bytes_=svc._impl._snapshot_bytes)

    assert svc._impl._cached_symbol_count == expected_symbol_count

    with h.step("query_symbols_first"):
        results = svc.query_symbols("Bag")
    h.record("query_symbols_first", count=len(results))
    assert results

    print(h.report())
    h.dump_json()
```

#### 1.5.C — Corruption recovery

```python
async def test_corruption_recovery(live_sweevo_env):
    h = TimingHarness(phase=1, test_name="corruption_recovery")
    env = live_sweevo_env

    # First build to establish snapshot
    with mock.patch.dict(os.environ, {"EOS_CI_IN_SANDBOX": "1"}):
        svc = env.make_ci_service()
        svc.ensure_initialized(wait=True)
        baseline_count = svc._impl._cached_symbol_count

        # Resolve <wh> and snapshot path
        home_code, home = env.exec("echo $HOME")
        wh = workspace_root_hash(env.repo_dir)
        snapshot_path = f"{home.strip()}/.cache/eos-ci/{wh}/v1/index.snapshot"

        with h.step("corruption_inject"):
            env.exec(f"echo 'GARBAGE' > {snapshot_path}")

        with h.step("corruption_recovery"):
            svc2 = env.make_ci_service()
            svc2.ensure_initialized(wait=True)  # must rebuild from scratch, not crash

        assert svc2._impl._cached_symbol_count == baseline_count

    print(h.report())
    h.dump_json()
```

#### 1.5.D — Path-confinement guard (storage boundary)

```python
def test_storage_path_confinement(tmp_path):
    """Unit-style test (no live sandbox needed): storage._confine rejects path traversal."""
    state = tmp_path / "state"
    state.mkdir()

    # Legal name
    write_snapshot(state, "ok.bin", {"k": "v"})
    assert (state / "ok.bin").exists()

    # Path traversal attempt MUST raise
    with pytest.raises(StoragePathEscape):
        write_snapshot(state, "../escape.bin", {"k": "v"})

    with pytest.raises(StoragePathEscape):
        write_snapshot(state, "/etc/passwd", {"k": "v"})
```

#### 1.5.E — Compatibility probe (NEW, dep matrix)

```python
def test_compatibility_probe_dep_matrix(live_sweevo_env):
    """One-shot survey of every dep the daemon needs.
    Surfaces the matrix so a new sandbox image can be qualified in one test run."""
    h = TimingHarness(phase=1, test_name="compatibility_probe")
    env = live_sweevo_env

    checks = {
        "python_version":      "python3 --version",
        "python_310_plus":     "python3 -c 'import sys; assert sys.version_info >= (3,10)'",
        "sqlite3":             "python3 -c 'import sqlite3'",
        "msgpack_native":      "python3 -c 'import msgpack'",  # OK if missing — bundle vendors it
        "jedi":                "python3 -c 'import jedi'",     # OK if missing — degrades to no-LSP
        "git":                 "git --version",
        "unshare_userns":      "unshare -Urm true",
        "setsid":              "command -v setsid",
        "nohup":               "command -v nohup",
        "tar":                 "command -v tar",
        "base64":              "command -v base64",
        "kill":                "command -v kill",
        "ps":                  "command -v ps",
        "home_writable":       "test -w \"$HOME\"",
        "tmp_writable":        "test -w /tmp && touch /tmp/_eos_probe && rm /tmp/_eos_probe",
        "af_unix_sockets":     "python3 -c 'import socket; s=socket.socket(socket.AF_UNIX); s.close()'",
        "proc_pid_status":     "test -r /proc/self/status",  # for RSS sampling in Phase 3.5
    }

    matrix = {}
    for name, cmd in checks.items():
        with h.step(f"probe_{name}"):
            code, out = env.exec(cmd)
        matrix[name] = {"ok": code == 0, "exit_code": code, "output": out.strip()[:200]}

    print(f"\n=== Compatibility matrix for sandbox {env.sandbox_id} ===")
    for name, result in matrix.items():
        status = "✓" if result["ok"] else "✗"
        print(f"  {status} {name:20s} exit={result['exit_code']:3d} {result['output']!r}")

    # Hard requirements — fail loud
    required = ["python_310_plus", "sqlite3", "git", "unshare_userns",
                "setsid", "nohup", "tar", "base64", "kill", "ps",
                "home_writable", "tmp_writable", "af_unix_sockets"]
    missing = [r for r in required if not matrix[r]["ok"]]
    if missing:
        pytest.fail(f"Sandbox image missing required deps: {missing}\nFull matrix: {matrix}")

    # Soft requirements — warn (msgpack OK to be missing because we vendor it)
    soft = ["msgpack_native", "jedi", "proc_pid_status"]
    soft_missing = [s for s in soft if not matrix[s]["ok"]]
    if soft_missing:
        print(f"WARNING: soft deps missing: {soft_missing}")
        print("  msgpack_native missing: OK — bundle vendors msgpack")
        print("  jedi missing: LSP queries will degrade")
        print("  proc_pid_status missing: Phase 3.5 RSS sampling skipped")

    h.dump_json()
```

#### 1.5.F — Eager bootstrap timing

```python
async def test_eager_bootstrap_timing(live_sweevo_env_factory):
    """Measure create_sandbox cold-start vs warm-restart with eager CI."""
    h = TimingHarness(phase=1, test_name="eager_bootstrap_timing")

    with mock.patch.dict(os.environ, {"EOS_CI_IN_SANDBOX": "1"}):
        with h.step("create_sandbox_cold_with_ci_bootstrap"):
            env = live_sweevo_env_factory()

        with h.step("verify_index_built"):
            svc = env.make_ci_service()
            results = svc.query_symbols("Bag")
        assert len(results) > 0, "index not ready immediately after create_sandbox"

        # Restart simulation (Daytona pause/resume)
        env.pause_sandbox()
        with h.step("start_sandbox_warm_with_ci_bootstrap"):
            env.resume_sandbox()  # also triggers eager bootstrap

        with h.step("verify_index_still_ready"):
            results = svc.query_symbols("Bag")
        assert len(results) > 0

    cold_cost = h.steps["create_sandbox_cold_with_ci_bootstrap"]
    warm_cost = h.steps["start_sandbox_warm_with_ci_bootstrap"]

    # Hard SLO from overview success criteria
    assert cold_cost < 3.0, f"create_sandbox cold > 3s ({cold_cost:.2f}s) — investigate bundle upload"
    assert warm_cost < 0.5, f"start_sandbox warm > 500ms ({warm_cost:.2f}s) — bundle/daemon should be cached"

    print(h.report())
    h.dump_json()
```

#### 1.5.G — Overlay live mount probe (LOAD-BEARING — stronger than `unshare -Urm true`)

**Why this is needed:** the matrix probe in 1.5.E only checks `unshare -Urm true` exits 0. That proves user namespace creation works, but says **nothing** about whether the production overlay mount stack actually functions. On some kernels (4.x with overlayfs userxattr support gaps, hardened LSM profiles, missing whiteout-in-userns support), `unshare -Urm true` succeeds but the production `mount -t overlay -o lowerdir=...,upperdir=...,workdir=...,userxattr` either fails outright or silently returns wrong results when the workload tries to delete a file (whiteout creation).

This probe **exercises every primitive the production overlay (`overlay/runtime/namespace.py:setup_mounts`) depends on**, so a broken sandbox image fails Phase 1 instead of failing in Phase 4 when `svc.cmd` first tries to commit a deletion through the OCC pipeline.

**What it covers:**
- ✅ tmpfs mount inside an unprivileged user namespace
- ✅ bind mount of lowerdir (matches `namespace.py:48` pattern)
- ✅ overlay mount with full production opts (`lowerdir=...,upperdir=...,workdir=...,userxattr`)
- ✅ Write through merged → upperdir copy-up (gitinclude case)
- ✅ Modify through merged → existing-file copy-up (the hot path for `svc.cmd` edits)
- ✅ Delete through merged → whiteout marker (handles BOTH char-device(0,0) and userxattr-style `user.overlay.whiteout` xattr)
- ✅ user.* xattr round-trip (matches the production `userxattr` mount opt requirement)

```python
def test_overlay_live_mount_probe(live_sweevo_env):
    """End-to-end probe of the production overlay stack: tmpfs + bind lower
    + overlay (userxattr) + write/modify/delete + whiteout + xattr.

    Mirrors namespace.py:setup_mounts exactly. If this fails, svc.cmd will
    not work on this image, regardless of what the basic dep matrix says."""
    h = TimingHarness(phase=1, test_name="overlay_live_mount_probe")
    env = live_sweevo_env

    probe_script = r'''
set -e
tmpdir=$(mktemp -d)
lower=$tmpdir/lower
merged=$tmpdir/merged
tmpfs_root=$tmpdir/tmpfs

mkdir -p "$lower" "$merged" "$tmpfs_root"
echo "lower-keep"   > "$lower/keep.txt"
echo "lower-modify" > "$lower/modify.txt"
echo "lower-delete" > "$lower/delete.txt"

# Mirrors namespace.py:setup_mounts exactly
unshare -Urm bash -c "
  set -e

  # Step 1: tmpfs upper (size-capped, matches production)
  mount -t tmpfs -o size=10m tmpfs '$tmpfs_root'
  mkdir -p '$tmpfs_root/upper' '$tmpfs_root/work'

  # Step 2: bind-mount lowerdir (matches namespace.py:48)
  mount --bind '$lower' '$lower'

  # Step 3: overlay with the production userxattr opts
  mount -t overlay overlay -o 'lowerdir=$lower,upperdir=$tmpfs_root/upper,workdir=$tmpfs_root/work,userxattr' '$merged'

  # Step 4: WRITE — new file copies up to upperdir (gitinclude case)
  echo 'new-content' > '$merged/new.txt'
  test -f '$tmpfs_root/upper/new.txt' || { echo 'FAIL: new file not copied up'; exit 51; }
  test \"\$(cat '$merged/new.txt')\" = 'new-content' || { echo 'FAIL: merged view of new file wrong'; exit 51; }

  # Step 5: MODIFY — existing lowerdir file copies up on first write
  echo 'modified-content' > '$merged/modify.txt'
  test -f '$tmpfs_root/upper/modify.txt' || { echo 'FAIL: modify did not copy up'; exit 52; }
  test \"\$(cat '$tmpfs_root/upper/modify.txt')\" = 'modified-content' || { echo 'FAIL: upperdir copy-up content wrong'; exit 52; }

  # Step 6: DELETE — must produce a whiteout tombstone in upperdir
  rm '$merged/delete.txt'
  if [ -e '$merged/delete.txt' ]; then echo 'FAIL: delete still visible through merged'; exit 53; fi

  # Step 7: validate whiteout marker exists in upperdir
  upper_delete='$tmpfs_root/upper/delete.txt'
  if [ ! -e \"\$upper_delete\" ]; then
    echo 'FAIL: whiteout tombstone missing in upperdir'
    ls -la '$tmpfs_root/upper'
    exit 54
  fi

  # Step 8: validate the whiteout is a recognized form. Two valid representations:
  #   (a) Privileged-style: character device with major/minor 0/0
  #   (b) Userxattr-style:  regular file with user.overlay.whiteout xattr
  # Production uses 'userxattr' so we expect (b), but kernels vary — accept either.
  if stat -c '%t,%T' \"\$upper_delete\" 2>/dev/null | grep -q '^0,0\$'; then
    : # privileged-style char-device whiteout — OK
  elif getfattr -n user.overlay.whiteout \"\$upper_delete\" 2>/dev/null | grep -q whiteout; then
    : # userxattr-style whiteout (the production path) — OK
  else
    echo 'FAIL: tombstone is neither char(0,0) nor user.overlay.whiteout xattr'
    stat \"\$upper_delete\"
    getfattr -d \"\$upper_delete\" 2>/dev/null || true
    exit 55
  fi

  # Step 9: user.* xattr round-trip on a copied-up file (production needs userxattr)
  setfattr -n user.eos_probe -v 'probe_value' '$merged/modify.txt'
  got=\$(getfattr -n user.eos_probe --only-values '$merged/modify.txt' 2>/dev/null)
  if [ \"\$got\" != 'probe_value' ]; then
    echo \"FAIL: user.* xattr round-trip — got '\$got' expected 'probe_value'\"
    exit 56
  fi

  echo 'OK: overlay live probe passed'
"
rc=$?
rm -rf "$tmpdir"
exit $rc
'''

    with h.step("overlay_live_probe"):
        code, out = env.exec(probe_script, timeout=60)

    if code != 0:
        pytest.fail(
            f"OVERLAY LIVE PROBE FAILED (exit_code={code}):\n"
            f"{out}\n"
            f"This is the production overlay mount stack from namespace.py:setup_mounts.\n"
            f"Failure here means svc.cmd will not work on this image.\n"
            f"Investigate: kernel overlayfs userxattr support (≥5.11), unprivileged userns config,\n"
            f"LSM profiles (AppArmor/SELinux), and `xattr` userspace tools "
            f"(getfattr/setfattr from `attr` on Debian/Ubuntu, `attr` package on RHEL)."
        )

    print(h.report())
    h.dump_json()
```

**Common failure modes the probe surfaces (and what they mean):**

| Exit code | Meaning | Likely cause |
|---|---|---|
| 1 (early) | `mount -t tmpfs` failed | tmpfs not mountable in user namespace (unusual; very old kernel) |
| 1 (later) | `mount -t overlay` failed | kernel lacks overlayfs in userns OR `userxattr` opt unsupported (pre-5.11) |
| 51 | new file not in upperdir | overlay mount succeeded but copy-up broken |
| 52 | modify did not copy up | overlay copy-up-on-write broken or upperdir not writable |
| 53 | delete still visible through merged | whiteout creation failed, delete not honored by overlay |
| 54 | whiteout tombstone missing | overlay accepted `unlink()` but didn't record it |
| 55 | tombstone is unrecognized form | kernel uses some other whiteout representation — investigate |
| 56 | user.* xattr round-trip failed | `userxattr` opt accepted but xattrs not actually plumbed — masked failure |

**Run command:** `uv run pytest backend/tests/test_e2e/test_live_ci_phase1_indexing.py -m live -v -s`

### Task 1.6 — Storage unit tests

**File:** `backend/tests/test_sandbox/test_code_intelligence/test_storage.py`

**Cases:**
- `state_dir("/workspace")` creates `$HOME/.cache/eos-ci/<wh>/v1/` and the path exists.
- `state_dir(...)` raises `StorageUnavailable(errno=EACCES, path=...)` when `$HOME` is unwritable.
- `write_snapshot()` is atomic — `os.replace` semantics on POSIX.
- `read_snapshot()` returns `None` for a corrupt pickle AND unlinks the corrupt file.
- `read_snapshot()` returns `None` for a missing file (no exception).
- `workspace_root_hash` is deterministic and resolves through `realpath`.
- `_confine` rejects `..`, absolute paths, symlink-traversal attempts.

### Task 1.7 — Indexing unit tests

**File:** `backend/tests/test_sandbox/test_code_intelligence/test_ci_index_runner.py`

**Cases:**
- Run `python -m sandbox.code_intelligence.daemon.ci_index --workspace-root <fixture>` against a 5-file Python fixture; assert snapshot pickle has expected symbol counts.
- `--file <single>` mode patches a single file's entry without rebuilding all.
- The CLI returns exit code 13 on `StorageUnavailable` (privilege failure) with a structured JSON error on stdout.

### Task 1.8 — Regression check

- `.venv/bin/pytest backend/tests/test_sandbox/ backend/tests/test_tools/ -q` — green with flag off.
- `EOS_CI_IN_SANDBOX=1 uv run pytest backend/tests/test_e2e/test_live_ci_phase1_indexing.py -m live -v -s` — Phase 1 E2E still green with the daemon path enabled.

## Definition of done

- [ ] `storage.py` exists with documented API + `_confine` guard; unit tests pass.
- [ ] `daemon indexer` instantiates `CodeIntelligenceService` with `sandbox=None, transport=None` and produces a valid snapshot pickle.
- [ ] `_runtime_bundle_bytes()` produces a tar.gz containing the entire `sandbox/code_intelligence/` tree + `sandbox/client/async_bridge.py` + `sandbox/__init__.py` + **vendored `msgpack/`**.
- [ ] Bundle size verified: < 1 MB total; reported in PR description.
- [ ] Bundle hash idempotency: subsequent `ensure_runtime_uploaded` calls no-op when the marker matches.
- [ ] `DaemonBackend.ensure_initialized()` works end-to-end against a real `dask` swe-evo sandbox.
- [ ] **Eager bootstrap hook wired into `SandboxService.create_sandbox` and `start_sandbox`; flag-off and missing-workspace no-op paths verified.**
- [ ] **Phase 1 live E2E privilege probe (1.5.A) passes — `mkdir -p $HOME/.cache/eos-ci/...` succeeds without sudo on the sandbox image.**
- [ ] **Phase 1 live E2E compatibility matrix probe (1.5.E) — all required deps green on `dask__dask_2023.3.2_2023.4.0`.**
- [ ] **Phase 1 live E2E eager bootstrap timing (1.5.F) — `create_sandbox` cold < 3s; `start_sandbox` warm < 500ms.**
- [ ] **Phase 1 live E2E overlay live mount probe (1.5.G) — production `tmpfs + bind lower + overlay (userxattr)` stack works end-to-end on the sandbox image, including write/modify/delete + whiteout marker (char(0,0) OR `user.overlay.whiteout` xattr) + user.* xattr round-trip.**
- [ ] Phase 1 E2E corruption recovery (1.5.C) works — daemon rebuilds from scratch when snapshot is corrupted.
- [ ] Phase 1 E2E indexing readiness (1.5.B) returns non-empty symbol results.
- [ ] Phase 1 E2E path-confinement guard (1.5.D) rejects path-traversal attempts.
- [ ] Phase 1 timing report shows `index_build_in_sandbox` and `query_symbols_first` durations.
- [ ] Regression check: Phase 1 E2E + full unit suite still green.
- [ ] PR description includes: privilege-probe output (`$HOME`, `whoami`, `umask`), compatibility matrix output, **overlay live probe output** (whiteout style detected: char(0,0) vs userxattr xattr; `kernel uname -r`; xattr round-trip result), Phase 1 timing report, bundle size in KB, eager-bootstrap cold/warm timings.

## Risk callouts (Phase 1 specific)

| Severity | Risk | Mitigation |
|---|---|---|
| **HIGH** | Privilege failure on `$HOME/.cache/eos-ci/` (sandbox runs as a user without `$HOME` write) | Loud failure in 1.5.A with errno + `$HOME` + `whoami`; choose between `/tmp/eos-ci-$USER/` fallback (documented) or amending the storage layout decision before Phase 2 |
| **HIGH** | Production overlay mount stack (`tmpfs + bind + overlay -o ...,userxattr`) silently broken on the sandbox image despite `unshare -Urm true` succeeding | Task 1.5.G overlay live probe exercises the full stack including write/modify/delete + whiteout + xattr round-trip; fails Phase 1 with structured exit codes (51-56) so the failure mode is identifiable before Phase 4 ships svc.cmd through the same mounts |
| **MEDIUM** | Bundle size spikes because `code_intelligence/` grows over time | `_runtime_bundle_bytes()` reports its size on first call; alert when bundle > 5 MB (current ~200 KB; would take a major addition to hit the alert) |
| **MEDIUM** | Bundle hash check (1.3 idempotency) miscomputes → repeated full uploads | Test the marker file mechanism explicitly; include bundle bytes-length in the marker as a sanity check |
| **MEDIUM** | `read_bytes` on a multi-MB pickle is slow over the transport | Acceptable for Phase 1 (one-shot); Phase 2+ moves the cache into the daemon, so `query_symbols` no longer requires a snapshot transfer; Phase 3.5 swaps to SQLite for incremental updates |
| **MEDIUM** | Pickle `index.snapshot` fully rewritten on every `refresh(file_path)` | Phase 3.5 migrates to SQLite-backed storage with per-file rows |
| **LOW** | Workspace has files with non-utf8 names | Keep today's `Path` semantics; don't add unicode handling until a real bug surfaces |
| **LOW** | `$HOME` differs across sandbox image revisions | Resolve at runtime via `os.path.expanduser`; cache the value on the backend instance |

## Hand-off to Phase 2

Phase 2 picks up with:
- A working orchestrator-side `DaemonBackend.ensure_initialized()` that ships a payload and runs a script in-sandbox.
- Storage layout proven on real Daytona (`$HOME/.cache/eos-ci/<wh>/v1/`).
- Bundle helper (`_runtime_bundle_bytes()`) and `ensure_runtime_uploaded()` ready to be reused for the daemon binary (Phase 2 just adds `__main__.py` to the same bundle).
- Path-confinement guard already in `storage` — Phase 3 builds a daemon-level workspace-write bypass guard on top.
- A baseline for `index_build_in_sandbox` cost — Phase 2 must not regress it when daemon-mediated.
