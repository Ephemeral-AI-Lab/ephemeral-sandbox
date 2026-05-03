# Code Intelligence — In-Sandbox Daemon Migration

**Status:** Phase 6 daemon-local overlay fold implemented and live-benchmarked
**Date approved:** 2026-05-02
**Last amended:** 2026-05-03 (removed the retired Phase 0 plan and live baseline artifacts)
**Predecessor:** [`code-intelligence-merged-into-sandbox.md`](../code-intelligence-merged-into-sandbox.md) — CI moved into the `sandbox/` package; still ran orchestrator-side
**Estimated effort:** ~42-50 engineering days (≈8.5-10 weeks)

## Why this migration

Today `sandbox/code_intelligence/` runs in the orchestrator process and drives the sandbox over `process.exec`. Every symbol query, LSP call, mutation, and `svc.cmd` round-trip pays for:

- **Network ship cost** — base64-encoded scripts uploaded per call, source files downloaded per index build (`memory/codeact_overlay_cost_breakdown.md` shows `_commit_changes ~0.65s + overlay_run ~0.43s` dominate `svc.cmd`).
- **No cache survival** — the symbol index, LSP cache, and edit ledger live in orchestrator RAM and evaporate on every restart.
- **`argv` overflow** — large batch payloads hit `python3 -c` E2BIG limits (`memory/checked_batch_apply_argv_limit.md`).

Moving the engine into the sandbox eliminates the network for hot paths, lets indices survive orchestrator restart (sandbox-lifetime persistence), and removes the argv ceiling. The orchestrator becomes a thin daemon backend.

## Phase index

| Phase | Title | Engineering | E2E + harness | Total |
|---|---|---|---|---|
| [1](./phase-01-indexing-and-storage.md) | In-sandbox indexing + storage skeleton + **eager bootstrap hook** + compatibility probe | 3 days | 2-3 days | 5-6 days |
| [2](./phase-02-daemon-lifecycle.md) | Daemon process + lifecycle (eager spawn from `create_sandbox` / `start_sandbox`) | 4 days | 2-3 days | 6-7 days |
| [3](./phase-03-overlay-mutations-lsp.md) | Move OCC/overlay/LSP via package reuse + SQLite ledger + socket-first daemon startup | 3 days | 2-3 days | 5-6 days |
| [3.5](./phase-03-5-concurrency-perf-and-sqlite-index.md) | Concurrency/perf E2E suite + SQLite-backed index storage | 3 days | 2 days | 5 days |
| [3.6](./phase-03-6-lsp-server-upgrade.md) | LSP backend experiment — qualify basedpyright (alternative pyright); rewire `LspClient` to chosen backend; benchmark vs jedi.Script | 2-3 days (1d spike + 2d eng) | 2-3 days | 5-6 days |
| [4](./phase-04-svc-cmd-hot-path.md) | `svc.cmd` hot path through the daemon (superseded by 3.5 / 3.6 closure pass) | 2 days | 2-3 days | 4-5 days |
| [5](./phase-05-process-exec-daemon-default.md) | process.exec-backed daemon default + dead-code cleanup | 3 days | 2-3 days | 5-6 days |
| [6](./phase-06-fold-daemon-overlay-stages.md) | Fold daemon-side overlay stages into one in-namespace process | 0.5-1 day | 0.5-1 day | 1.5-2.5 days |

Phase order is **strict** (1 → 6, including 3.5 and 3.6). During the migration, each phase was independently mergeable because the flag-off path kept working; after the Phase 5 cleanup, transport-backed sandboxes are daemon-default.

**Why Phase 3.5 between 3 and 4:** Phase 3 lands the OCC engine inside the daemon. Before pushing the highest-volume hot path (`svc.cmd`, Phase 4) through that engine, we want a perf safety net that proves the daemon survives sustained load without memory leak, FD leak, or contention pathologies. Phase 3.5 also migrates the index from pickle to SQLite so per-file `refresh()` doesn't rewrite the world.

**Why Phase 3.6 between 3.5 and 4:** Phase 3 routed `find_definitions/find_references/hover/diagnostics` through the daemon, but kept today's per-call `python3 -c "import jedi"` shim. That's a 200-500ms cold-start per query — the obvious next bottleneck after Phase 3.5 stabilizes the daemon. Phase 3.6 is a focused experiment: qualify basedpyright as a persistent LSP child of the daemon; if it can't run on our sandbox image, qualify pyright instead; pick ONE; rewire `LspClient`; remove jedi.Script. **No runtime fallback chain** — silent degradation in the LSP path would mask whether the upgrade actually landed in production. Done before Phase 4 so the headline `svc.cmd` perf measurement isn't tangled with LSP latency improvements.

## Architectural endpoint

```
┌──────────────────────────────────────┐         ┌─────────────────────────────────────────────┐
│ Orchestrator                         │         │ Sandbox                                     │
│                                      │         │                                             │
│  SandboxService.create_sandbox(...)  │         │                                             │
│   ├─ provision Daytona               │         │                                             │
│   ├─ bootstrap_in_sandbox_ci_runtime │ ──────────► EAGER: bundle upload + daemon spawn       │
│   │   (eager bootstrap, blocking)    │         │            + index build kicked off bg     │
│   └─ return sandbox handle           │         │                                             │
│                                      │         │                                             │
│  CodeIntelligenceService             │         │  python -m sandbox.code_intelligence        │
│  (thin facade, unchanged public API) │         │             .daemon                         │
│           │                          │         │   ┌─────────────────────────────────────┐   │
│           ▼                          │         │   │ asyncio event loop                  │   │
│  CodeIntelligenceBackend Protocol    │         │   │  socket bound IMMEDIATELY            │   │
│   ├─ InProcessBackend                │         │   │  index builds in background thread  │   │
│   └─ DaemonBackend ──────────────────────────────────►   ↓                                 │   │
│           │   (DaemonBackend)          │         │   │  CodeIntelligenceService            │   │
│           ▼                          │         │   │   (sandbox=None, transport=None)    │   │
│  process.exec-backed daemon command (Phase 5)               │         │   │   — same package, local-FS branches │   │
│   or python socket shim              │         │   │  Owns: SymbolIndex, LspClient,      │   │
│   on stable run_sync loop            │         │   │        Arbiter, WriteCoordinator,   │   │
│           ▼                          │         │   │        OverlayAuditor, …            │   │
│  Unix socket on sandbox FS  ◄────────────────────────┤   ↓                                │   │
│                                      │         │   │  storage (sandbox-only adapter)  │   │
└──────────────────────────────────────┘         │   │   ↓                                 │   │
                                                 │   │  $HOME/.cache/eos-ci/<wh>/v1/       │   │
                                                 │   │   ├─ daemon.sock                    │   │
                                                 │   │   ├─ daemon.pid                     │   │
                                                 │   │   ├─ index.sqlite3 (Phase 3.5)      │   │
                                                 │   │   ├─ ledger.sqlite3 (WAL, Phase 3)  │   │
                                                 │   │   ├─ daemon.log                     │   │
                                                 │   │   └─ lsp_cache/                     │   │
                                                 │   └─────────────────────────────────────┘   │
                                                 └─────────────────────────────────────────────┘
```

**Key change vs original draft:** the daemon does NOT reimplement `ci_overlay.py`, `ci_mutations.py`, `ci_lsp.py`, etc. It ships the entire existing `sandbox.code_intelligence` package as its bundle and instantiates the existing `CodeIntelligenceService` with `sandbox=None, transport=None` so the local-FS branches activate. Zero drift risk by construction.

## Eager bootstrap contract (LOAD-BEARING — reverses predecessor migration)

**The CI daemon MUST be ready by the time `SandboxService.create_sandbox` returns and on every sandbox restart.** Lazy first-call bootstrap is forbidden. This intentionally **reverses** the predecessor migration's contract ("`create_sandbox` does NOT trigger CI bootstrap"); the daemon model makes the cost amortizable in a way the orchestrator-resident service couldn't.

### Hook locations

| Hook | File | What it does |
|---|---|---|
| `SandboxService.create_sandbox(...)` | `backend/src/sandbox/lifecycle/service.py:115` | After Daytona provisions the sandbox, calls `bootstrap_in_sandbox_ci_runtime(sandbox_id, workspace_root, transport=...)` which uploads the bundle + spawns the daemon + waits for socket readiness BEFORE returning |
| `SandboxService.start_sandbox(...)` | `backend/src/sandbox/lifecycle/service.py:174` | Same hook — covers Daytona auto-paused sandboxes coming back up |
| Restart recovery path | `backend/src/sandbox/lifecycle/service.py:~211` | Existing "probe failed → targeted restart" path also calls `bootstrap_in_sandbox_ci_runtime` after restart |
| `SandboxService.code_intelligence_for(...)` | (existing) | Returns the per-sandbox service; daemon auto-respawn is handled at the daemon backend layer once Phase 2 lands |
| Auto-respawn (Phase 2) | `DaemonBackend._call_daemon_command → ensure_daemon` | Last-resort safety net for daemon crash between calls — should rarely trigger if eager bootstrap works |

### Existing integration point

`backend/src/sandbox/lifecycle/workspace.py` keeps `ensure_code_intelligence_runtime(...)` for orchestrator-side context preparation and exposes `bootstrap_in_sandbox_ci_runtime(...)` for lifecycle-time eager bootstrap. Keeping the two paths separate avoids overloading the context-prep helper with sandbox create/start policy.

### Blocking contract

`create_sandbox` MUST return only after:
1. Bundle uploaded (~500ms-1s, idempotent — usually a no-op cached marker check)
2. Daemon spawned and socket bound (~500ms-1s for cold; ~50ms when bundle/daemon already up from a prior session)
3. **First `ping` succeeds** — proves the daemon is reachable

Symbol index build runs in the daemon's **background thread** (Phase 3.3 fix), so `create_sandbox` does NOT block on the index. `query_symbols` returns empty (or partial) until the build finishes. Callers that need full results call `svc.warmup()` or check `svc.is_initialized`.

**Net cost added to `create_sandbox`:** ~1-2s (cold), ~100ms (warm, bundle cached, daemon already alive from same sandbox session).

### Why eager (rationale to revisit if reversed)

- **Predictable latency.** Today's "first CI call is slow" pattern is hard to debug because it shows up only in the first user-visible op.
- **Production guarantees.** A user starting a CodeAct session expects the agent to be productive immediately, not after a 1.5-3s mystery stall.
- **Sandbox restart visibility.** Daytona may auto-pause/resume sandboxes; without eager bootstrap, the first call after resume hits the same cold-start cost. Eager hook on `start_sandbox` covers this.
- **Trade-off:** transport-only tests pay the eager-bootstrap cost when the lifecycle hook is enabled. Acceptable; this keeps lifecycle behavior single-path.

## Storage boundary (load-bearing)

There are **two distinct storage classes** in this design. Mixing them would be a bug.

| Class | What lives there | Path | Routed through |
|---|---|---|---|
| **Workspace files** | User code, edits, mutations, shell-cmd outputs | `workspace_root` (e.g. `/testbed`, `/workspace`) | `WriteCoordinator` (OCC) and `OverlayAuditor` (overlay) — **always** |
| **CI internal state** | Symbol index, LSP cache, edit ledger, daemon socket/PID/log | `$HOME/.cache/eos-ci/<wh>/v1/` (XDG) | `storage` adapter only — **never** through OCC/overlay |

**Invariants:**
- The CI internal state path is **outside `workspace_root`** so it never appears in `git status`, never gets versioned, never appears in the overlay lowerdir snapshot, and isn't user work.
- Workspace writes from daemon command handlers must go through `WriteCoordinator`. **No daemon command handler is allowed to write directly under `workspace_root`.** Phase 3 adds a guard test that attempts a bypass and asserts the daemon refuses it.
- `storage` writes are restricted to `$HOME/.cache/eos-ci/<wh>/v1/`. Any path outside that subtree raises.

## Post-cutover role of `SandboxTransport`

After Phase 5 lands and transport-backed sandboxes become daemon-default:

| Phase | `SandboxTransport.exec/read/write` role |
|---|---|
| Pre-daemon | Primary mutation/query channel; every CI op rides on it |
| Phases 1-4 (flag-off default) | Same as today |
| Phases 1-4 (flag-on opt-in) | Bootstrap (upload bundle, spawn daemon at `create_sandbox`) + every daemon command via shim |
| Phase 5 (daemon-default) | Bootstrap + recovery only — daemon ops ride the process.exec-backed daemon command |
| Post-Phase 6 (out of scope) | Bootstrap + recovery only; shim removed entirely |

**Implication:** `_apply_remote_*`, `_read_remote*`, `_write_remote`, `_delete_remote`, `_stage_remote_payload`, `_collect_via_search`, `_collect_via_list`, `_read_text_via_exec`, `_batch_read_text_via_exec` became dead code by Phase 5 and were removed in the post-canary cleanup.

## Sandbox image compatibility (hard contract)

The daemon depends on a specific set of OS primitives. **Phase 1 ships a compatibility probe** (Task 1.5.E) that runs once at sandbox bootstrap and produces a structured matrix so a new sandbox image can be qualified in one test run.

### Required (daemon won't start without these)

| Dep | Why | Today? |
|---|---|---|
| Python ≥ 3.10 | Codebase uses `match`, modern type syntax | ✓ already required |
| `sqlite3` (stdlib) | `LedgerStore` (Phase 3), `IndexStore` (Phase 3.5) | NEW |
| `os.path.expanduser("~")` returns writable path | `storage.state_dir` | NEW — Phase 1 privilege probe catches |
| AF_UNIX sockets | Daemon daemon command | NEW |
| `setsid`, `nohup`, `kill`, `ps` | Daemon spawn + lifecycle | NEW |
| `tar`, `base64`, `bash` | Bundle extraction | NEW |
| `git` | `git check-ignore` routes overlay upperdir paths between OCC and direct merge | ✓ |
| `unshare -Urm` (unprivileged user namespaces) | Overlay shell auditing | ✓ — most fragile dep |
| Writable `/tmp` (not `noexec`) | Bundle extraction + overlay tmpfs | ✓ |
| `msgpack` (Python pkg) | Daemon wire format | NEW — vendored into bundle (~50KB) so offline images work without `pip install` |

### Optional (degrade gracefully)

| Dep | Why | Fallback |
|---|---|---|
| ~~`jedi` (Python pkg)~~ | ~~LSP queries~~ | **Removed in Phase 3.6.** jedi.Script per-call mode replaced by a single qualified persistent LSP backend. The chosen backend's deps (basedpyright OR pyright) move to HARD REQUIRED in Phase 3.6 — see "Phase 3.6 hard contract" below |
| `/proc/<pid>/status` readable | RSS/FD sampling in Phase 3.5 perf E2E | Tests skip resource assertions if unavailable |

### Phase 3.6 hard contract — chosen LSP backend

After Phase 3.6 ships, the **single** qualified backend's deps become required (no fallback):

- **If basedpyright qualifies** (Stage A spike result, primary candidate): `python3 -c "import basedpyright"` MUST succeed on the production image (pre-baked, OR `pip install basedpyright` works at first-query time). Phase 1 compatibility probe extended to enforce this.
- **If pyright qualifies instead** (alternative when basedpyright can't run): `command -v node` AND `command -v pyright-langserver` MUST both succeed.

The qualification spike (Phase 3.6 Task 3.6.A) decides which one. The compatibility probe extension (Phase 3.6 Task 3.6.G) treats the chosen backend's deps as hard requirements thereafter — a new sandbox image lacking them fails Phase 1, not Phase 4. **No silent fallback to jedi.** If the chosen backend isn't runnable, LSP queries surface `LspUnavailable` and the caller sees the failure.

### Where it actually breaks

1. **Hardened distros with `kernel.unprivileged_userns_clone=0`** — no `unshare -Urm`. Today's overlay already breaks here; daemon migration neither helps nor hurts.
2. **`$HOME` not writable** — Phase 1 privilege probe catches with errno + `whoami` + `umask`.
3. **Alpine/musl images** — Python `subprocess.wait`/`os.fork` semantics differ subtly. Untested. Daytona is glibc-based.
4. **Read-only `/tmp`** — bundle upload fails. Surface clear error; don't fall back silently.
5. **Python < 3.10** — daemon syntax error at import.
6. **AppArmor/SELinux blocking AF_UNIX in `$HOME/.cache/`** — Phase 2 E2E catches with `test -S` post-spawn.

For Daytona swe-evo (`dask__dask_2023.3.2_2023.4.0`), Phase 1 compatibility probe must show **all required deps green**.

## The nine gray-area decisions (locked)

These are the choices that were surfaced before approval. Re-reading them is the fastest way to absorb the design.

| # | Decision | Choice |
|---|---|---|
| 1 | **Storage location** | `$HOME/.cache/eos-ci/<workspace_root_hash>/v1/` (XDG-compliant, no privilege requirement, outside `workspace_root` so `git status` stays clean) |
| 2 | **Persistence semantics** | Sandbox-lifetime only. Survives orchestrator restart + daemon crash. Wiped by `dispose_sandbox` and image rotation. Daemon performs startup integrity check; corrupt state triggers rebuild-from-scratch, never crash |
| 3 | **Workspace identity** | `<workspace_root_hash>` = `sha256(realpath(workspace_root))[:16]`. Multiple workspaces in one sandbox each get their own subtree; v1 schema directory allows future migration |
| 4 | **Bootstrap timing** | **EAGER ALWAYS.** Daemon spawns synchronously on `SandboxService.create_sandbox` and on every sandbox restart (`start_sandbox`, restart recovery, `code_intelligence_for` defensive path). First-call latency disappears; `create_sandbox` cost rises by ~1-2s cold (~100ms warm). Auto-respawn (Phase 2) still handles daemon-crash between calls |
| 5 | **Daemon transport** | Unix domain socket at `$HOME/.cache/eos-ci/<wh>/v1/daemon.sock`, length-prefixed msgpack frames. No TCP, no HTTP |
| 6 | **Process model** | Single-process daemon, asyncio event loop, one worker thread per language server. The five HARD INVARIANTS are enforced by the same async locks used today, just resident in the daemon. **Socket binds BEFORE index build starts** — `query_symbols` returns empty until index ready |
| 7 | **Failure model** | Any daemon command may raise `DaemonUnavailable`; `CodeIntelligenceService` retries once after respawn, then surfaces a structured error. Edit-path failures (OCC abort, merge conflict) surface as today |
| 8 | **Backend selection** | Transport-backed sandboxes select `DaemonBackend`; sandboxless/local flows select `InProcessBackend`. The old `EOS_CI_IN_SANDBOX=0` backend-selection rollback path is retired after the Phase 5 cleanup. |
| 9 | **Wire format** | msgpack with explicit schema versioning (`{"v": 1, "op": "...", ...}`); unknown fields rejected, unknown op = `UnsupportedOp`. **msgpack vendored into bundle** (~50KB) for offline-image compatibility |

## The five HARD INVARIANTS (must NOT regress)

These survive untouched through the migration. Phase 3 is where they get their live-E2E proof.

1. **Sorted-path locks** — `Arbiter` acquires per-file locks in sorted path order to prevent deadlock.
2. **Strict-base OCC + `aborted_version`** — base hash check; any drift aborts with `aborted_version`.
3. **Non-overlapping merge fallback** — non-strict modify changes attempt non-overlap merge before aborting.
4. **TimeMachine rollback** — partial-apply failures roll back via TimeMachine; ledger entries appended (never rewound).
5. **Symbol index + LSP cache invalidation on commit** — every committed change invalidates the relevant cache slots before reply.

## `svc.cmd` result preservation (FULL FIELD SET)

Every field of today's `OverlayAuditor.execute()` `SimpleNamespace` return must round-trip through the daemon byte-for-byte. The full set:

```python
SimpleNamespace(
    result: str,                              # stdout from the user command
    exit_code: int,
    changed_paths: list[str],                 # gitinclude paths committed via OCC
    ambient_changed_paths: list[str],         # paths not committed because OCC aborted or policy rejected
    files_written: int,
    git_commit_status: str | None,            # "committed" | "noop" | "aborted_version" | "rejected" | None
    git_conflict_file: str | None,
    git_conflict_reason: str | None,
    gitinclude_changed_paths: list[str],
    gitignore_direct_merged_paths: list[str],
    gitignore_direct_merged_count: int,
    mixed_gitinclude_gitignore: bool,
    mixed_partial_apply: bool,
    warnings: list[str],
)
```

Phase 4's result-shape parity test (Task 4.3) verifies the durable workflow fields. Downstream callers in `backend/src/sandbox/lifecycle/commit.py` rely on attribution, conflict reporting, and the gitignore direct-merge fields.

## Compatibility & rollout

- **Public API stable.** `SandboxService.code_intelligence_for(...)` returns the same `CodeIntelligenceService`. All toolkit callers (`tools/sandbox_toolkit/*`) work unchanged.
- **Flag-off = byte-identical to today** for in-process behavior. **`create_sandbox` cost differs** even with flag off because the eager bootstrap hook is wired in Phase 1 (it just no-ops when flag is off — no daemon spawned).
- **No root/sudo at any phase.** Daemon runs as the sandbox's default user. Phase 1 E2E asserts this with an explicit privilege probe.
- **Tests that bind no sandbox** (the in-process path) keep using `InProcessBackend`; daemon-daemon command mode activates only when `transport` and `sandbox_id` are both bound.
- **`pyproject.toml` includes `msgpack`** as a runtime dependency.

## Cross-phase success criteria

- [ ] Flag off: byte-identical behavior to today (full existing test suite green in every phase).
- [ ] Flag on: all unit + live E2E tests pass on `dask__dask_2023.3.2_2023.4.0`.
- [ ] All five HARD INVARIANTS preserved (proven by Phase 3 E2E live against real edits).
- [ ] Daemon survives orchestrator restart and `kill -9` (proven by Phases 2 and 3 E2E).
- [ ] **`create_sandbox` returns with daemon ready (`ping` succeeds) — no first-call cold start.**
- [ ] **`start_sandbox` (resume after pause) re-bootstraps daemon eagerly.**
- [ ] **`create_sandbox` cold-start cost < 3s; warm (bundle cached) < 500ms.**
- [ ] **Compatibility probe matrix shows all required deps green on `dask__dask_2023.3.2_2023.4.0` and any future image.**
- [ ] `$HOME/.cache/eos-ci/<wh>/v1/` cleared by `dispose_sandbox` (proven by Phase 2 E2E).
- [ ] Daemon survives sustained load: ≥200 mixed concurrent ops with bounded memory/FD growth (Phase 3.5 E2E).
- [ ] `svc.cmd` warm-path latency strictly lower than today (proven by Phase 4 E2E timing report).
- [ ] No root/sudo required in any phase (proven by Phase 1 E2E privilege assertion).
- [ ] **Per-phase live E2E timing report shows expected deltas against the relevant prior benchmark artifacts.**
- [ ] Per-op p50/p95/p99 latency reported across N=200 sustained calls (Phase 3.5 E2E).
- [ ] No direct workspace writes from daemon command handlers (Phase 3 bypass-attempt guard test).
- [ ] process.exec bridge floor faster than Phase 2 shim path (proven by Phase 5 E2E).
- [ ] All cross-phase regression checks green (each phase reruns prior phases' E2Es).
- [ ] Full `svc.cmd` `SimpleNamespace` field set preserved (Phase 4 result-shape parity test).
- [ ] `msgpack` vendored into bundle; daemon starts on offline image without `pip install`.
- [ ] **Phase 1 overlay live mount probe (Task 1.5.G) passes** — production tmpfs+bind+overlay+userxattr stack works end-to-end on the sandbox image, including write/modify/delete + whiteout marker (char(0,0) OR `user.overlay.whiteout` xattr) + user.* xattr round-trip. Stronger than `unshare -Urm true`.
- [ ] **Phase 3.6 LSP backend qualified** — basedpyright OR pyright runs as a persistent LSP child of the daemon on `dask__dask_2023.3.2_2023.4.0`; qualification report committed at `lsp-qualification-spike-result.md`.
- [ ] **Phase 3.6 LSP benchmark passes hard SLOs** — chosen backend `find_definitions` p50 ≥ 5x faster than pre-rewire jedi.Script baseline; p99 < 100ms warm; `hover` p50 ≥ 10x faster.
- [ ] **Phase 3.6 jedi.Script removed from production code** — `python_backend.py` deleted, `jedi` removed from `pyproject.toml`, no runtime fallback chain.
- [ ] **HARD INVARIANT 5 (LSP cache invalidation on commit) preserved against the chosen backend** (Phase 3.6 regression of Phase 3 Task 3.7.E).

## Cross-cutting risks

| Severity | Risk | Mitigation |
|---|---|---|
| **HIGH** | **Eager bootstrap inflates `create_sandbox` cost beyond user tolerance** (cold ~1-2s, warm ~100ms) | Phase 1 E2E times `create_sandbox` end-to-end vs baseline; if cold > 3s investigate (likely bundle upload) |
| **HIGH** | Privilege failure on `$HOME/.cache/eos-ci/` | Phase 1 E2E tests this explicitly; on `mkdir` failure, fail loud with errno; documented fallback (`/tmp/eos-ci-$USER/`) NOT silently applied |
| **HIGH** | Daemon process leak across `dispose_sandbox` | PID file + `kill -TERM` before registry pop; Phase 2 E2E checks `ps aux` post-dispose |
| **HIGH** | OCC invariants regress when relocated into daemon | Phase 3 E2E reproduces all five HARD INVARIANTS live with real edits — but with the bundle-the-package approach drift risk is eliminated by construction |
| **HIGH** | Daemon resource leak under sustained load (memory, FDs, open SQLite handles) | Phase 3.5 dedicated perf+stability E2E with explicit RSS/FD ceilings |
| **HIGH** | daemon command handler bypasses `WriteCoordinator` and writes `workspace_root` directly | Phase 3 bypass-attempt guard test; daemon dispatch wrapper rejects raw FS writes to the workspace |
| **HIGH** | `svc.cmd` `SimpleNamespace` shape drift drops fields like `gitinclude_changed_paths` or `mixed_partial_apply` | Phase 4 result-shape parity test exercises every field; full field set documented above |
| **HIGH** | Sandbox image lacks a required dep (msgpack, sqlite3, unshare) — daemon fails to start | Phase 1 compatibility probe runs first; surfaces full matrix; msgpack vendored to remove most-likely failure mode |
| **MEDIUM** | Snapshot/ledger corruption | Write-temp-then-rename, SQLite WAL, integrity check on startup, rebuild-from-scratch (tested in Phase 1 with intentional corruption) |
| **MEDIUM** | `process.exec` shim latency in Phases 2-4 | Phase 5 keeps direct process.exec-backed daemon command; live E2E must compare any proposed replacement before new API surface is added |
| **MEDIUM** | Wire-format drift across versions | Explicit `{"v": 1}` schema, unknown-field reject, `UnsupportedOp` for unknown verbs |
| **MEDIUM** | Sandbox image variance (`$HOME` differs across images) | Resolve `$HOME` at runtime via `os.path.expanduser`; never hardcode `/home/daytona` |
| **DEFERRED** | `memory/git_workspace_gitignored_deps_blocker.md` (gitignored deps invisible to overlay snapshot) | Out of scope; needs its own ADR. Migration neither helps nor hurts this blocker |

## Live E2E pattern (used by every phase)

Every phase ships at least one live E2E test against a real Daytona sandbox provisioned via the existing swe-evo harness. Conventions inherited from `backend/tests/test_e2e/test_live_ci_diagnostics.py`:

- Marks: `pytestmark = [pytest.mark.e2e, pytest.mark.live]`
- Skip gate: `EvalAgent.has_daytona()` — tests skip when no Daytona credentials in `.env`
- Sandbox: `dask__dask_2023.3.2_2023.4.0` from swe-evo, repo at `/testbed`
- Provisioning: `sandbox.testing.create_test_sandbox` / `delete_test_sandbox`
- Run command: `uv run pytest backend/tests/test_e2e/<file>.py -m live -v -s`
- Timing instrumentation: every test uses the shared `TimingHarness` from `backend/tests/test_e2e/_timing_harness.py`. Output JSON lands at `backend/tests/test_e2e/_timings/phase_N_<test>_<timestamp>.json`; tests that need comparisons choose their own relevant benchmark artifact.
- Phase 3.5 extends the harness with `step_repeat(name, n)` for collecting p50/p95/p99 distributions.

## File layout (full)

### Bundle shipped to the sandbox (Phase 1+)
The orchestrator ships the entire `backend/src/sandbox/code_intelligence/` tree as `/tmp/eos-ci-runtime/sandbox/code_intelligence/` plus the transitive sandbox modules needed by the in-sandbox runner, plus **vendored msgpack**:

```
/tmp/eos-ci-runtime/
  msgpack/                                             (vendored; ~50 KB; no pip install needed)
  sandbox/__init__.py                                  (empty marker)
  sandbox/errors.py
  sandbox/api/
  sandbox/client/async_bridge.py                       (existing — promoted in predecessor migration)
  sandbox/lifecycle/commit.py
  sandbox/code_intelligence/                           (FULL existing package)
    __init__.py
    service.py                                          (public engine facade)
    registry.py
    telemetry.py
    backends/
      __init__.py
      protocol.py                                      (CodeIntelligenceBackend Protocol)
      in_process.py                                    (local/sandboxless backend)
      daemon.py                                        (daemon backend composition)
    daemon/
      __main__.py
      client.py                                        (orchestrator command client)
      launcher.py                                      (bundle upload + daemon spawn)
      server.py                                        (asyncio server + dispatch)
      handlers.py                                      (daemon command handlers)
      guard.py                                         (workspace write guard)
      protocol.py                                      (frame codec)
      state.py                                         (process-local service state)
      storage.py                                       (state-dir facade)
      index_store.py                                   (SQLite symbol index)
      ledger_store.py                                  (SQLite edit ledger)
      paths.py                                         (state path helpers)
      wire.py                                          (DTO serialization)
    core/
    indexing/
    language_server/
      daemon_queries.py                                (LSP query adapter over daemon commands)
    mutations/
    overlay/
```

**Note on what's NOT in `daemon/`:** no copied overlay, mutation, LSP, or extracted engine implementation. The daemon constructs `CodeIntelligenceService` from the shipped package and the existing `sandbox=None, transport=None` paths activate the local-FS branches.

### Current source map
- `backend/src/server/routers/code_intelligence.py` — HTTP router for public code-intelligence requests.
- `backend/src/sandbox/api/code_intelligence_api.py` — tool-facing `CodeIntelligenceApi` Protocol.
- `backend/src/sandbox/api/code_intelligence_impl.py` — adapter from `CodeIntelligenceApi` to `CodeIntelligenceService`.
- `backend/src/sandbox/code_intelligence/service.py` — per-sandbox facade that exposes public engine methods and selects a backend.
- `backend/src/sandbox/code_intelligence/backends/protocol.py` — `CodeIntelligenceBackend` Protocol used by the facade.
- `backend/src/sandbox/code_intelligence/backends/in_process.py` — local/sandboxless backend implementation.
- `backend/src/sandbox/code_intelligence/backends/daemon.py` — small composition class for the daemon-backed implementation.
- `backend/src/sandbox/code_intelligence/daemon/client.py` — orchestrator-side daemon command client, retry, and `DaemonCommandError`.
- `backend/src/sandbox/code_intelligence/daemon/launcher.py` — uploads payload, spawns daemon, waits for socket.
- `backend/src/sandbox/code_intelligence/language_server/daemon_queries.py` — LSP query methods over the daemon command client.
- `backend/src/sandbox/code_intelligence/daemon/server.py` — sandbox-local asyncio daemon and dispatch table.
- `backend/src/sandbox/code_intelligence/daemon/handlers.py` — command handlers that call the sandbox-local `CodeIntelligenceService`.
- `backend/src/sandbox/code_intelligence/daemon/storage.py`, `index_store.py`, `ledger_store.py`, `paths.py` — daemon state, index, ledger, and path helpers.
- `backend/src/sandbox/api/transport.py` — process.exec-backed daemon command (Phase 5)

### Modified (orchestrator)
- `backend/src/sandbox/code_intelligence/registry.py` — selects `CodeIntelligenceBackend` based on flag
- `backend/src/sandbox/code_intelligence/service.py` — delegates to backend; accepts optional `edit_history` kwarg (Phase 3)
- `backend/src/sandbox/lifecycle/service.py` — `create_sandbox` and `start_sandbox` call the eager bootstrap hook (Phase 1)
- `backend/src/sandbox/lifecycle/workspace.py` — `bootstrap_in_sandbox_ci_runtime` is the eager-bootstrap entry (Phase 1)
- `pyproject.toml` — includes `msgpack` as runtime dependency
- (Phase 5 cleanup) `mutations/content_manager.py`, `indexing/file_discovery.py`, `language_server/transport.py` — deleted dead remote branches after canary stabilization

### Shared test infrastructure
- `backend/tests/test_e2e/_timing_harness.py` — `TimingHarness` context manager, `step()` decorator, JSON dumper, `compare_to()` baseline differ; Phase 3.5 extends with distribution collection (`step_repeat`)
- `backend/tests/test_e2e/_timings/` — directory holding `phase_N_<test>_<timestamp>.json`

### New live E2E tests (one per phase + the compatibility probe)
- `backend/tests/test_e2e/test_live_ci_phase1_indexing.py` (privilege probe + compatibility matrix probe + **overlay live mount probe** Task 1.5.G)
- `backend/tests/test_e2e/test_live_ci_phase2_daemon_lifecycle.py` (asserts daemon ready after `create_sandbox`)
- `backend/tests/test_e2e/test_live_ci_phase3_invariants.py`
- `backend/tests/test_e2e/test_live_ci_phase3_5_concurrent_perf.py`
- `backend/tests/test_e2e/test_live_ci_phase3_6_lsp_benchmark.py` (chosen LSP backend vs pre-rewire jedi.Script baseline)
- `backend/tests/test_e2e/test_live_ci_phase4_svc_cmd.py`
- `backend/tests/test_e2e/test_live_ci_phase5_default_on.py`

### Phase 3.6 throwaway script (committed for reproducibility)
- `scripts/lsp_qualification_spike.py` — Stage A one-shot experiment that picks basedpyright OR pyright; produces `docs/architecture/code-intelligence-in-sandbox-daemon/lsp-qualification-spike-result.md`

## Pre-existing context to read before starting any phase

- This overview file
- The phase file you're about to start
- `docs/architecture/code-intelligence-merged-into-sandbox.md` — predecessor migration (note: this migration REVERSES the predecessor's "no CI on create" contract)
- `backend/src/sandbox/code_intelligence/service.py` — current public API surface
- `backend/src/sandbox/code_intelligence/registry.py` — current `_SERVICES` dict
- `backend/src/sandbox/code_intelligence/overlay/auditor.py` — full `SimpleNamespace` result shape (Task 4.3 reference)
- `backend/src/sandbox/lifecycle/service.py:115` — `create_sandbox` entry point (Phase 1 hook target)
- `backend/src/sandbox/lifecycle/service.py:174` — `start_sandbox` entry point
- `backend/src/sandbox/lifecycle/workspace.py` — `bootstrap_in_sandbox_ci_runtime` eager lifecycle hook plus existing `ensure_code_intelligence_runtime` context-prep helper
- `backend/tests/test_e2e/test_live_ci_diagnostics.py` — canonical live-E2E pattern
- `backend/src/benchmarks/sweevo/sandbox.py` — swe-evo sandbox provisioning helpers
- `memory/codeact_overlay_cost_breakdown.md` — current cost profile of `svc.cmd`
- `memory/checked_batch_apply_argv_limit.md` — argv overflow context
- `memory/feedback_use_venv_pytest.md` — always use `.venv/bin/pytest`, never global pytest
- `memory/feedback_parallel_user_commits.md` — stage with explicit file paths only

## Dependencies

- `msgpack` (runtime dep; vendored into bundle)
- Python `sqlite3` (stdlib)
- `setsid`, `nohup`, `kill`, `ps` available in sandbox image (standard Linux)
- swe-evo harness (`backend/src/benchmarks/sweevo/sandbox.py`, `create_sweevo_test_sandbox`)
- Daytona credentials in `.env` for live E2E (`EvalAgent.has_daytona()` skip gate)

## Amendment log

- **2026-05-02 (initial):** approved with phases 0-5
- **2026-05-02 (audit response):**
  - Switched from "copy hand-picked modules into daemon-local extracted files" approach to "ship the entire `sandbox.code_intelligence` package and instantiate the existing `CodeIntelligenceService` with `sandbox=None`" — eliminates drift risk by construction
  - Added Phase 3.5 (concurrency/perf E2E + SQLite-backed index storage)
  - Tightened the storage-boundary invariants (workspace files vs CI internal state) into a load-bearing section
  - Added "post-cutover `SandboxTransport` is bootstrap/recovery only" as an explicit table
  - Documented full `svc.cmd` `SimpleNamespace` field set inline so future readers don't reduce it to `(stdout, stderr, exit_code)`
  - Added "no direct workspace writes from daemon command handlers" guard test in Phase 3
- **2026-05-02 (eager bootstrap + portability):**
  - **Reversed gray-area #4 from lazy to EAGER ALWAYS.** Daemon spawns on `create_sandbox` and every restart. Reverses predecessor migration's "no CI on create" contract.
  - Phase 1 wires the hook into `SandboxService.create_sandbox`, `start_sandbox`, and restart recovery. `sandbox/lifecycle/workspace.py:bootstrap_in_sandbox_ci_runtime` owns the eager bootstrap path.
  - Phase 3 amended: daemon binds socket FIRST, starts index build in background thread (was: blocked startup on `ensure_initialized(wait=True)`).
  - Added "Sandbox image compatibility" hard contract section with full dep matrix.
  - Phase 1 adds Task 1.5.E compatibility probe live E2E.
  - Added msgpack as runtime dep in `pyproject.toml` and vendored msgpack into bundle (Phase 1, ~50KB) for offline-image compatibility.
  - Cross-cutting risks: cold-start latency risk migrates from "first call after `create_sandbox`" (lazy model) to "`create_sandbox` cost rises ~1-2s cold" (eager model). Test asserts `create_sandbox` < 3s.
- **2026-05-02 (LSP upgrade experiment + stronger overlay probe):**
  - Added **Phase 3.6 — LSP backend experiment**. Qualifies basedpyright as the persistent LSP child of the daemon; if it can't run on `dask__dask_2023.3.2_2023.4.0`, qualifies pyright instead. Picks ONE; rewires `LspClient` to use only the qualified backend; deletes today's `python_backend.py` (jedi.Script per-call shim) and removes `jedi` from `pyproject.toml`. **No runtime fallback** — if the chosen backend can't start, `LspUnavailable` propagates; no silent degradation to a worse path. Three stages: (A) qualification spike, (B) implementation, (C) benchmark + regression.
  - Hard SLOs for the chosen backend vs pre-rewire jedi baseline: `find_definitions` p50 ≥ 5x faster, p99 < 100ms warm; `hover` p50 ≥ 10x faster.
  - Phase 1 Task 1.5.E (compatibility probe) extended in Phase 3.6 Task 3.6.G to treat the chosen backend's deps as HARD REQUIRED.
  - Bumped total estimated effort from 37-44 days to 42-50 days (≈8.5-10 weeks) for Phase 3.6.
  - Added **Phase 1 Task 1.5.G — overlay live mount probe**. Strengthens compatibility probe by exercising the production tmpfs + bind-mount lower + `mount -t overlay -o lowerdir=...,upperdir=...,workdir=...,userxattr` + write/modify/delete + whiteout marker validation (accepts both privileged char(0,0) and userxattr `user.overlay.whiteout` xattr) + user.* xattr round-trip. Mirrors `overlay/runtime/namespace.py:setup_mounts` exactly. Stronger than `unshare -Urm true` — fails Phase 1 instead of failing in Phase 4 if the production overlay mount stack doesn't work on a new sandbox image.
