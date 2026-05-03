# Phase 3.6 — LSP backend experiment: qualify basedpyright, fall back to pyright if it can't run on our sandbox

**Estimated effort:** 5-6 days (1 day qualification spike + 2 days engineering + 2-3 days E2E + benchmark)
**Risk profile:** MEDIUM — new external dep; LSP results may differ from jedi (better, but not byte-identical)
**Status:** Not started
**Blocks on:** Phase 3.5 complete

## Framing — this is an experiment, not a multi-backend selector

The shape of this phase is **not** "ship a runtime selector that picks basedpyright/pyright/jedi based on what's available." That would be silent degradation, and silent degradation in an LSP path means we'd never notice the perf upgrade isn't actually landing in production.

The shape is:

1. **Qualify basedpyright on our sandbox image** (`dask__dask_2023.3.2_2023.4.0`). If it runs as a persistent LSP child, that's the answer.
2. **If basedpyright cannot run on our image, qualify pyright instead.** (Pyright requires `node` in the image; check during qualification.)
3. **Benchmark the qualified backend against today's per-call `jedi.Script`** to confirm the perf claim.
4. **Rewire `LspClient` to use ONLY the qualified backend.** No runtime selection. If the chosen backend fails in production, the daemon errors loudly — we want to know, not silently fall back to a worse path.

Today's `jedi.Script` per-call code path stays in the codebase as the **benchmark baseline** and as the active production path until Phase 3.6 ships its rewire commit. Once the rewire lands, the jedi.Script fallback is removed (Phase 5 cleanup pass also covers this).

## Why between 3.5 and 4

Three reasons:

1. **Phase 3.5 proved daemon stability under load.** Adding a child subprocess to an unstable daemon would compound risk; doing it after the perf safety net is the right order.
2. **Independent perf signal.** Phase 4's headline `svc.cmd` win shouldn't be tangled with LSP latency improvements. Phase 3.6 isolates and measures the LSP delta against Phase 0's jedi-baseline.
3. **HARD INVARIANT 5 (LSP cache invalidation on commit) was proven in Phase 3 (Task 3.7.E).** Phase 3.6 swaps the LSP *backend implementation*; the contract is unchanged, the invariant gets re-proven against the new backend in Task 3.6.6.

## What we're picking between

| Backend | Distribution | Sandbox dep | Why a candidate |
|---|---|---|---|
| **basedpyright** (primary candidate) | Python wheel (`pip install basedpyright`) | Python ≥ 3.10 (already required) | Best quality (full type inference, narrowing, generics), Python-only (no `node`), maintained fork of pyright with active community |
| **pyright** (alternative) | npm package (`pyright-langserver`) | `node` runtime | Same engine as basedpyright; only viable if our sandbox image bundles `node`. The official Microsoft path |
| ~~jedi-language-server~~ | Not considered | — | Same engine as today's jedi.Script — would only marginally beat the persistent jedi cost; not worth experiment time when basedpyright/pyright both offer step-change quality + speed |

**Today's `jedi.Script` (per-call)** stays as the benchmark baseline; it is **not** a candidate.

## Phase structure (three sequential stages)

### Stage A — Qualification spike (1 day, manual)

A focused experiment that answers a single question: **does basedpyright work as a persistent LSP server inside `dask__dask_2023.3.2_2023.4.0`?**

The spike is a single throwaway script (`scripts/lsp_qualification_spike.py`) that:

1. Provisions a real Daytona sandbox.
2. Tries `python3 -c "import basedpyright"` inside the sandbox.
3. If missing, tries `pip install basedpyright` inside the sandbox.
4. Spawns `python3 -m basedpyright.langserver --stdio` as a subprocess; sends `initialize` over JSON-RPC; waits for the response.
5. Sends one `textDocument/definition` query against `/testbed/dask/__init__.py`.
6. Reports: install OK? launch OK? handshake OK? first-query OK? round-trip latency?

**Decision rule:**

- If **all four checks pass for basedpyright**: basedpyright is the chosen backend. Skip to Stage B.
- If **any check fails for basedpyright**: re-run the spike for pyright (requires `command -v node` and `command -v pyright-langserver`). If pyright passes, pyright is the chosen backend. If both fail, **stop the migration and reconsider** — Phase 3.6 doesn't ship.

**Output:** a 1-page qualification report committed at `docs/architecture/code-intelligence-in-sandbox-daemon/lsp-qualification-spike-result.md`. Includes: which backend won, why, install command used, raw timing for the spike's single query, and any sandbox-image gotchas surfaced.

### Stage B — Implementation (2 days)

Rewire the daemon's LSP path to use the **single** qualified backend. No runtime selector, no fallback.

### Stage C — Benchmark + regression (2-3 days)

Head-to-head benchmark vs today's `jedi.Script`. HARD INVARIANT 5 regression. Phases 0-3.5 E2Es re-run.

## What ships (Stages B + C)

| Artifact | File | Purpose |
|---|---|---|
| Qualification spike script | `scripts/lsp_qualification_spike.py` (new, throwaway after Stage A) | One-shot experiment (Stage A); committed for reproducibility but never imported |
| Qualification report | `docs/architecture/code-intelligence-in-sandbox-daemon/lsp-qualification-spike-result.md` (new) | The decision document — which backend, why, evidence |
| LSP child manager | `backend/src/sandbox/code_intelligence/language_server/lsp_child.py` (new) | `LspBackendChild`: spawn the **chosen** backend, JSON-RPC framing, request/response correlation, graceful shutdown. **No backend selection logic** — `kind` is hardcoded based on Stage A outcome |
| JSON-RPC stdio adapter | `backend/src/sandbox/code_intelligence/language_server/jsonrpc.py` (new) | Content-Length framing, `send_request`/`send_notification`, response queue keyed by request id |
| `LspClient` rewire | `backend/src/sandbox/code_intelligence/language_server/client.py` (modified) | Routes ALL queries to `LspBackendChild`. Today's `python_backend.py` (jedi.Script) is removed in the same commit |
| Daemon lifecycle integration | `backend/src/sandbox/code_intelligence/daemon/server.py` (modified) | `_DAEMON_STATE.lsp_child` lazy-spawned on first query; torn down on graceful shutdown. Crash → daemon restart (no in-process fallback) |
| Compatibility probe extension | `backend/tests/test_e2e/test_live_ci_phase1_indexing.py` (extends Task 1.5.E) | New keys for the **chosen** backend's deps only |
| Phase 3.6 live E2E benchmark | `backend/tests/test_e2e/test_live_ci_phase3_6_lsp_benchmark.py` (new) | Chosen backend vs `jedi.Script` baseline, p50/p95/p99 distributions, hard SLO assertion |
| Unit tests | `backend/tests/test_sandbox/test_code_intelligence/test_lsp_child.py` | JSON-RPC framing round-trip; concurrent-request multiplexing; child crash → typed exception |

## Detailed task list

### Task 3.6.A — Qualification spike (Stage A)

**File:** `scripts/lsp_qualification_spike.py` (throwaway after the report is written; committed for reproducibility)

**Acceptance:** the spike runs against a real `dask__dask_2023.3.2_2023.4.0` sandbox and prints a structured report:

```
=== LSP qualification spike ===
sandbox: <id>  image: dask__dask_2023.3.2_2023.4.0  user: <whoami>

--- basedpyright ---
import basedpyright:                FAIL  (ModuleNotFoundError)
pip install basedpyright:           OK    (12.4s, 78.2 MB)
import basedpyright (retry):        OK
launch python3 -m basedpyright.langserver --stdio:  OK  (proc started, 0.21s)
LSP initialize handshake:           OK    (0.18s)
textDocument/definition (first):    OK    (0.034s, 1 result)
VERDICT: basedpyright qualified ✓

(stopping — basedpyright won; pyright not attempted)
```

If basedpyright fails any step, the spike continues to pyright:

```
--- basedpyright ---
import basedpyright:                FAIL  (ModuleNotFoundError)
pip install basedpyright:           FAIL  (network policy: DNS blocked for pypi.org)
VERDICT: basedpyright DISQUALIFIED ✗

--- pyright ---
command -v node:                    OK    (/usr/bin/node, v20.11.1)
command -v pyright-langserver:      OK    (/usr/local/bin/pyright-langserver)
launch pyright-langserver --stdio:  OK    (proc started, 0.31s)
LSP initialize handshake:           OK    (0.42s)
textDocument/definition (first):    OK    (0.052s, 1 result)
VERDICT: pyright qualified ✓
```

**The spike commits its findings to `lsp-qualification-spike-result.md`.** The decision in that document drives every subsequent task in this phase.

**Stage A output (the only thing Stages B/C consume):** a single literal — `LSP_BACKEND_CHOSEN ∈ {"basedpyright", "pyright"}`. Hardcoded in `lsp_child.py` from then on.

### Task 3.6.B — JSON-RPC stdio adapter (Stage B)

**File:** `backend/src/sandbox/code_intelligence/language_server/jsonrpc.py`

LSP frame format: `Content-Length: <N>\r\n\r\n<json-body>` (UTF-8). One frame per message. Reader parses headers, reads exactly N bytes, decodes JSON.

```python
def encode_request(req_id: int, method: str, params: dict) -> bytes:
    body = json.dumps({"jsonrpc": "2.0", "id": req_id, "method": method, "params": params}).encode()
    return f"Content-Length: {len(body)}\r\n\r\n".encode() + body

def encode_notification(method: str, params: dict) -> bytes:
    body = json.dumps({"jsonrpc": "2.0", "method": method, "params": params}).encode()
    return f"Content-Length: {len(body)}\r\n\r\n".encode() + body

async def read_frame(reader: asyncio.StreamReader) -> dict:
    """Read one Content-Length-prefixed JSON-RPC frame. Raises asyncio.IncompleteReadError on EOF."""
    length: int | None = None
    while True:
        line = await reader.readline()
        if not line:
            raise asyncio.IncompleteReadError(b"", None)
        line = line.rstrip(b"\r\n")
        if not line:
            break  # End of headers
        if line.startswith(b"Content-Length:"):
            length = int(line.split(b":", 1)[1].strip())
    if length is None:
        raise FrameError("missing Content-Length header")
    body = await reader.readexactly(length)
    return json.loads(body)
```

### Task 3.6.C — Persistent LSP child process manager (Stage B)

**File:** `backend/src/sandbox/code_intelligence/language_server/lsp_child.py`

```python
# Set ONCE at the top of this file from the Stage A spike result.
# No runtime selection.
LSP_BACKEND_CHOSEN: Literal["basedpyright", "pyright"] = "basedpyright"  # or "pyright"

_LAUNCH_CMD = {
    "basedpyright": ["python3", "-m", "basedpyright.langserver", "--stdio"],
    "pyright":      ["pyright-langserver", "--stdio"],
}[LSP_BACKEND_CHOSEN]


class LspChildUnavailable(Exception):
    """The chosen LSP backend isn't installed/runnable on this sandbox.
    Surfaces loudly — never silently swapped for a different backend."""


class LspChildCrashed(Exception):
    """The child process died mid-request. Daemon restarts the child once;
    a second crash escalates to LspChildUnavailable so the operator sees it."""


class LspBackendChild:
    """Persistent LSP server child process owned by the daemon.

    Lifecycle:
        start()       → spawn, exchange initialize/initialized handshake
        find_definitions / find_references / hover / diagnostics
                      → JSON-RPC request, await response by id, return
        did_change()  → notification on commit (HARD INVARIANT 5)
        shutdown()    → graceful: shutdown request → exit notification → wait
        restart()     → if proc dies between calls, respawn ONCE; second crash → LspChildUnavailable
    """

    def __init__(self, workspace_root: str) -> None:
        self.workspace_root = workspace_root
        self._proc: asyncio.subprocess.Process | None = None
        self._next_id = itertools.count(1)
        self._pending: dict[int, asyncio.Future[Any]] = {}
        self._reader_task: asyncio.Task | None = None
        self._write_lock = asyncio.Lock()       # serialize writes to stdin
        self._respawn_used = False              # bounded restart counter

    async def start(self) -> None:
        """Spawn the chosen backend and exchange the initialize/initialized handshake.

        Raises LspChildUnavailable if the backend isn't installed/runnable. Caller
        (daemon) treats this as a hard error — no silent swap."""
        try:
            self._proc = await asyncio.create_subprocess_exec(
                *_LAUNCH_CMD,
                stdin=asyncio.subprocess.PIPE,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE,
            )
        except FileNotFoundError as exc:
            raise LspChildUnavailable(
                f"chosen backend {LSP_BACKEND_CHOSEN} not found on PATH: {exc}. "
                f"Install it on the sandbox image or re-qualify backends in lsp-qualification-spike-result.md"
            ) from exc

        self._reader_task = asyncio.create_task(self._read_loop())
        try:
            await asyncio.wait_for(self._send_request("initialize", {
                "processId": os.getpid(),
                "rootUri": f"file://{self.workspace_root}",
                "capabilities": _DEFAULT_LSP_CAPABILITIES,
                "initializationOptions": _backend_init_options(LSP_BACKEND_CHOSEN),
            }), timeout=20.0)
            await self._send_notification("initialized", {})
        except (asyncio.TimeoutError, LspChildCrashed) as exc:
            self._proc.terminate()
            raise LspChildUnavailable(
                f"chosen backend {LSP_BACKEND_CHOSEN} failed initialize handshake: {exc}"
            ) from exc

    async def find_definitions(self, file_path: str, line: int, character: int) -> list[SymbolInfo]: ...
    async def find_references(self, file_path: str, line: int, character: int) -> list[ReferenceInfo]: ...
    async def hover(self, file_path: str, line: int, character: int) -> HoverResult | None: ...
    async def diagnostics(self, file_path: str) -> list[Diagnostic]: ...
    async def did_change(self, file_path: str, content: str) -> None: ...
    async def shutdown(self, *, timeout_s: float = 2.0) -> None: ...
```

**Critical contracts:**
- **One backend, hardcoded.** `LSP_BACKEND_CHOSEN` is a module-level constant set from Stage A. There is no runtime selector. Changing the backend means re-running the qualification spike, updating the constant, and re-running the benchmark.
- **Single child per daemon.** Daemon holds one `LspBackendChild` per `_DAEMON_STATE.svc`. Concurrent LSP queries multiplex over the same child via JSON-RPC id correlation.
- **Restart-on-crash bounded to 1.** First crash → respawn once. Second crash → `LspChildUnavailable` propagates to the orchestrator; daemon does NOT silently swap to another backend. Operator sees the failure.
- **Graceful shutdown order** (LSP spec): `shutdown` request → wait for response → `exit` notification → close stdin → wait up to 2s → `terminate()`.

### Task 3.6.D — `LspClient` rewire (Stage B)

**File:** `backend/src/sandbox/code_intelligence/language_server/client.py` (modified)

Today's `LspClient` constructs `python_backend.PythonBackend` per call (which spawns `python3 -c "import jedi; ..."`). Phase 3.6 swaps to:

```python
class LspClient:
    def __init__(self, *, workspace_root: str, ...) -> None:
        self._workspace_root = workspace_root
        self._child: LspBackendChild | None = None
        self._lock = asyncio.Lock()

    async def _ensure_child(self) -> LspBackendChild:
        async with self._lock:
            if self._child is None:
                self._child = LspBackendChild(self._workspace_root)
                await self._child.start()  # raises LspChildUnavailable on hard fail
            return self._child

    async def find_definitions(self, file_path, symbol, line=0, character=0):
        try:
            child = await self._ensure_child()
            return await child.find_definitions(file_path, line, character)
        except LspChildCrashed:
            # Bounded restart-on-crash: try once
            self._child = None
            child = await self._ensure_child()
            return await child.find_definitions(file_path, line, character)

    # find_references / hover / diagnostics — same shape
```

**No fallback.** If `LspChildUnavailable` propagates out, the request fails. The orchestrator surfaces a structured error envelope (`kind="LspUnavailable"`) so the caller knows the LSP path is down — they can retry, escalate, or run without LSP. They cannot get silently-degraded results from jedi.Script behind their back.

**Removal in the same commit:**
- `backend/src/sandbox/code_intelligence/language_server/python_backend.py` — DELETED.
- `backend/src/sandbox/code_intelligence/language_server/transport.py` — `_run_python_script`, the jedi-via-`python3 -c` shim, and the `pip install jedi` lazy-install code path — DELETED.
- `pyproject.toml` — `jedi>=0.19.0` runtime dep removed.

The benchmark (Task 3.6.F) compares against today's jedi.Script using a saved baseline JSON from a pre-rewire run, NOT against the live jedi path (which no longer exists post-rewire).

### Task 3.6.E — Daemon lifecycle integration (Stage B)

**File:** `backend/src/sandbox/code_intelligence/daemon/server.py` (modified)

```python
@dataclass
class _DaemonState:
    svc: CodeIntelligenceService
    workspace_root: str
    started_at: float
    guard_enabled: bool = True
    guard_strict: bool = False
    lsp_child: LspBackendChild | None = None    # NEW: lazy-spawned

async def run_daemon(workspace_root: str) -> None:
    ...
    # LSP child is NOT spawned here — lazy init in LspClient on first query.
    # Reason: daemon ping/index ops should not pay LSP startup cost on cold sandbox.
    ...

async def _shutdown() -> None:
    """Graceful shutdown order: stop accepting → drain → close LSP child → unlink files."""
    server.close()
    await server.wait_closed()
    if _DAEMON_STATE.lsp_child is not None:
        try:
            await asyncio.wait_for(_DAEMON_STATE.lsp_child.shutdown(), timeout=3.0)
        except asyncio.TimeoutError:
            logger.warning("lsp child did not shutdown in 3s; terminating")
            _DAEMON_STATE.lsp_child._proc.terminate()
    state_path = state_dir(_DAEMON_STATE.workspace_root)
    (state_path / "daemon.sock").unlink(missing_ok=True)
    (state_path / "daemon.pid").unlink(missing_ok=True)
```

### Task 3.6.F — Phase 3.6 live E2E benchmark (Stage C, HEADLINE DELIVERABLE)

**File:** `backend/tests/test_e2e/test_live_ci_phase3_6_lsp_benchmark.py`

The benchmark compares the **chosen** backend (post-rewire, the only LSP path that exists) against a saved pre-rewire `jedi.Script` benchmark when one is present. Pre-rewire jedi.Script no longer exists in the codebase, so a missing historical benchmark downgrades the speedup assertion to a warning instead of reintroducing the old Phase 0 live test.

**Phase 3.6 benchmark (post-rewire):**

```python
async def test_lsp_chosen_backend_benchmark(live_sweevo_env):
    """Measure the chosen backend (basedpyright OR pyright — set in lsp_child.py)
    against the pre-3.6 jedi.Script baseline.

    Hard SLO:
        - chosen backend find_definitions p50 ≤ jedi baseline p50 / 5 (5x speedup)
        - chosen backend find_definitions p99 ≤ 100ms warm
        - chosen backend hover p50 ≤ jedi baseline p50 / 10 (hover is hot in editors)
    """
    h = TimingHarness(phase=3.6, test_name="chosen_lsp_backend_benchmark")
    env = live_sweevo_env
    svc = env.make_ci_service_flag_on()
    svc.ensure_initialized(wait=True)

    target_file = "/testbed/dask/__init__.py"
    target_symbol = "compute"
    target_line, target_char = _find_symbol_location(env, target_file, target_symbol)

    # Cold: first query (LSP child startup + initialize). Reported separately so
    # cold-start cost is visible, not silently amortized into warm samples.
    with h.step("cold_first_query"):
        svc.find_definitions(target_file, target_symbol, line=target_line, character=target_char)

    # Warm-up to fill backend internal caches
    for _ in range(5):
        svc.find_definitions(target_file, target_symbol, line=target_line, character=target_char)

    # Distributions (50 samples each)
    ops = [
        ("find_definitions", lambda: svc.find_definitions(
            target_file, target_symbol, line=target_line, character=target_char)),
        ("find_references",  lambda: svc.find_references(
            target_file, target_symbol, line=target_line, character=target_char)),
        ("hover",            lambda: svc.hover(target_file, target_line, target_char)),
        ("diagnostics",      lambda: svc.diagnostics(target_file)),
    ]
    for op_name, op_fn in ops:
        for step in h.step_repeat(op_name, n=50):
            with step:
                op_fn()

    # ---- Hard SLO assertions vs pre-rewire jedi baseline ----
    jedi_baseline = _load_pre_rewire_jedi_baseline()
    chosen_p50 = h.distributions["find_definitions"]["p50"]
    jedi_p50   = jedi_baseline["find_definitions"]["p50"]
    assert chosen_p50 * 5 <= jedi_p50, (
        f"{LSP_BACKEND_CHOSEN} find_definitions p50 ({chosen_p50*1000:.1f}ms) "
        f"NOT ≥5x faster than jedi baseline ({jedi_p50*1000:.1f}ms) — "
        f"the entire point of Phase 3.6 has failed. Re-evaluate backend choice."
    )

    chosen_p99 = h.distributions["find_definitions"]["p99"]
    assert chosen_p99 < 0.1, (
        f"{LSP_BACKEND_CHOSEN} find_definitions p99 ({chosen_p99*1000:.1f}ms) > 100ms warm — investigate"
    )

    hover_p50 = h.distributions["hover"]["p50"]
    jedi_hover_p50 = jedi_baseline["hover"]["p50"]
    assert hover_p50 * 10 <= jedi_hover_p50, (
        f"{LSP_BACKEND_CHOSEN} hover p50 ({hover_p50*1000:.1f}ms) "
        f"NOT ≥10x faster than jedi baseline ({jedi_hover_p50*1000:.1f}ms)"
    )

    # ---- Print headline comparison table ----
    _print_lsp_benchmark_table(LSP_BACKEND_CHOSEN, h.distributions, jedi_baseline)
    print(h.report())
    h.dump_json()


def _print_lsp_benchmark_table(chosen: str, current: dict, baseline: dict) -> None:
    print(f"\n=== Phase 3.6 LSP benchmark: {chosen} vs jedi.Script baseline ===")
    header = f"{'op':<20} {'jedi (ms)':>20} {chosen+' (ms)':>22} {'speedup':>10}"
    print(header)
    print("-" * len(header))
    for op in ["find_definitions", "find_references", "hover", "diagnostics"]:
        b = baseline[op]
        c = current[op]
        b_str = f"{b['p50']*1000:>5.1f}/{b['p95']*1000:>5.1f}/{b['p99']*1000:>5.1f}"
        c_str = f"{c['p50']*1000:>5.1f}/{c['p95']*1000:>5.1f}/{c['p99']*1000:>5.1f}"
        speedup = b['p50'] / max(c['p50'], 1e-6)
        print(f"{op:<20} {b_str:>20} {c_str:>22} {speedup:>9.1f}x  (p50/p95/p99 ms)")
```

**Sample expected output (illustrative — actual numbers depend on chosen backend and image):**

```
=== Phase 3.6 LSP benchmark: basedpyright vs jedi.Script baseline ===
op                            jedi (ms)        basedpyright (ms)    speedup
--------------------------------------------------------------------------------
find_definitions       240.5/310.2/415.8        8.3/14.1/22.7         29.0x  (p50/p95/p99 ms)
find_references        450.3/580.9/720.4       18.4/28.1/41.2         24.5x  (p50/p95/p99 ms)
hover                  220.1/275.3/390.5        6.9/11.0/18.5         31.9x  (p50/p95/p99 ms)
diagnostics            380.7/470.2/610.0       12.5/19.8/30.3         30.5x  (p50/p95/p99 ms)
```

### Task 3.6.G — Compatibility probe extension (Stage C)

**File:** `backend/tests/test_e2e/test_live_ci_phase1_indexing.py` (extends Task 1.5.E)

After Stage A picks the backend, the matrix probe gets keys for **the chosen backend's deps only**:

If `LSP_BACKEND_CHOSEN == "basedpyright"`:
```python
checks.update({
    "basedpyright_native":  "python3 -c 'import basedpyright'",
})
required.append("basedpyright_native")  # HARD REQUIREMENT — no fallback exists
```

If `LSP_BACKEND_CHOSEN == "pyright"`:
```python
checks.update({
    "node":                "command -v node",
    "pyright_langserver":  "command -v pyright-langserver",
})
required.extend(["node", "pyright_langserver"])
```

**The chosen backend's deps move from "soft" to "required" in 1.5.E** because there's no fallback. A new sandbox image that lacks the chosen backend fails Phase 1 — clearly, not silently in production.

### Task 3.6.H — Unit tests (Stage C)

**`test_lsp_child.py`:**
- Frame round-trip: `encode_request` → `read_frame` produces the original dict.
- `LspBackendChild` against a mock subprocess that echoes responses — `find_definitions` resolves the future correctly.
- Concurrent requests get distinct ids; responses route to the correct future.
- Subprocess EOF mid-request → outstanding futures fail with `LspChildCrashed`.
- `start()` against a `FileNotFoundError` (binary missing) → raises `LspChildUnavailable` with a clear message.
- `shutdown()` sends `shutdown` then `exit` then closes stdin in correct order; terminates on timeout.
- Restart-on-crash: first crash respawns and succeeds; second consecutive crash escalates `LspChildUnavailable`.

### Task 3.6.I — Regression check (Stage C)

- `.venv/bin/pytest backend/tests/test_sandbox/ backend/tests/test_tools/ -q` — green with flag off and on.
- **HARD INVARIANT 5 (LSP cache invalidation on commit) regression** — Phase 3 Task 3.7.E re-run with the chosen backend. `apply_edit` followed by `find_definitions` on the same symbol must return the post-edit definition, never stale.
- Re-run Phases 0, 1, 2, 3, 3.5 live E2Es — all green.
- `grep -r "import jedi\|from jedi\|python_backend" backend/src` returns only test fixtures (if any); production code is jedi-free.

## Definition of done

### Stage A — Qualification
- [ ] `scripts/lsp_qualification_spike.py` runs end-to-end against `dask__dask_2023.3.2_2023.4.0`.
- [ ] `lsp-qualification-spike-result.md` committed; chosen backend declared (basedpyright OR pyright); evidence attached.

### Stage B — Implementation
- [ ] `LSP_BACKEND_CHOSEN` constant set in `lsp_child.py` from Stage A outcome — single value, no runtime selection.
- [ ] `LspBackendChild` ships with start/shutdown/restart-on-crash (bounded to 1).
- [ ] JSON-RPC adapter handles Content-Length framing; round-trip unit tests pass.
- [ ] `LspClient.find_definitions/find_references/hover/diagnostics` route to `LspBackendChild`. **No fallback path.**
- [ ] `python_backend.py` deleted; `transport.py`'s jedi shim deleted; `jedi` removed from `pyproject.toml`.
- [ ] Daemon lifecycle: child lazy-spawned on first query; torn down on graceful shutdown.

### Stage C — Benchmark + regression
- [ ] Pre-rewire jedi benchmark artifact captured before merging Stage B, when speedup assertions are required.
- [ ] **Phase 3.6 live E2E benchmark passes against `dask__dask_2023.3.2_2023.4.0`.**
- [ ] **HARD SLO 1: chosen backend `find_definitions` p50 ≥ 5x faster than jedi baseline p50.**
- [ ] **HARD SLO 2: chosen backend `find_definitions` p99 < 100ms warm.**
- [ ] **HARD SLO 3: chosen backend `hover` p50 ≥ 10x faster than jedi baseline p50.**
- [ ] HARD INVARIANT 5 regression test passes against the chosen backend.
- [ ] Compatibility probe extended: chosen backend's deps moved from soft to required.
- [ ] Regression check: Phases 0, 1, 2, 3, 3.5 E2Es + full unit suite green.
- [ ] PR description includes: chosen backend + qualification rationale + benchmark table with the headline speedup in big bold letters.

## Risk callouts (Phase 3.6 specific)

| Severity | Risk | Mitigation |
|---|---|---|
| **HIGH** | Stage A finds NEITHER basedpyright nor pyright works on `dask__dask_2023.3.2_2023.4.0` | Phase 3.6 does not ship; reopen design — options: pre-bake basedpyright into the image, switch to a different sandbox base, or accept jedi.Script as the long-term LSP path |
| **HIGH** | Chosen backend produces materially different results from jedi (e.g., narrows or broadens definition results) | Parity test in Stage C — assert results are a strict superset of jedi's output for a fixed symbol corpus (basedpyright/pyright may know more, never less). If false negatives appear, document and ship a known-divergence list |
| **HIGH** | Chosen backend crashes mid-request in production → `LspChildUnavailable` propagates → user-facing LSP query fails | This is the intended behavior — surfaces loud, not silently degraded. Daemon log includes the chosen backend's stderr; orchestrator alerts on `LspUnavailable` error envelopes |
| **MEDIUM** | basedpyright bundled typeshed disagrees with the workspace's actual Python version | `initializationOptions.pythonVersion` set from sandbox `python3 --version`; verify in Stage A spike with a stdlib hover |
| **MEDIUM** | LSP client cache invalidation race — `did_change` notification arrives at child after a concurrent query already returned stale results | Existing OCC commit serialization preserved (Phase 3); `did_change` sent BEFORE the commit reply per HARD INVARIANT 5; regression test in Task 3.6.I |
| **MEDIUM** | `pip install basedpyright` requires network access → Stage A spike fails on offline images even though basedpyright would work if pre-installed | Stage A spike reports install method; if it required network, the qualification report flags "REQUIRES PRE-BAKED" so the production image build is amended |
| **LOW** | Process zombie on daemon kill -9 (graceful shutdown skipped) | Daemon parent-PID = 1 via `setsid`; on `dispose_sandbox` the entire process tree is reaped (Daytona full-VM model) |
| **LOW** | LSP child's stderr fills daemon.log over a long-lived sandbox | Same `RotatingFileHandler` (10 MB × 3 backups) as Phase 2 daemon log |

## Hand-off to Phase 4

Phase 4 picks up with:
- ONE LSP backend integrated into the daemon (whichever Stage A qualified).
- `find_definitions / find_references / hover / diagnostics` routing through that single backend; no fallback, no runtime selection.
- jedi.Script removed from the codebase; `jedi` removed from `pyproject.toml`.
- Benchmark JSON in `_timings/phase_3_6_lsp_backend_benchmark_<ts>.json` documenting the speedup vs the pre-rewire jedi baseline.
- HARD INVARIANT 5 still holds against the new backend.
- Phase 4's `svc.cmd` measurement is now isolated from LSP improvements — apples-to-apples comparable to Phase 0 baseline.
- The compatibility probe (extended in Task 3.6.G) treats the chosen backend's deps as HARD REQUIREMENTS — a new sandbox image that lacks them fails Phase 1, not Phase 4.
