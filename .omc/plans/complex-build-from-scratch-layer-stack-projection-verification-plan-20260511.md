# Complex build-from-scratch layer-stack projection verification plan
**Date:** 2026-05-11
**Status:** DRAFT v2 — open questions resolved (§13), updated with /ephemeral-os repo, LSP+Daytona saturation, edit-heavy bias, test-heavier-than-source ratio, perf-metrics deliverable
**Owner:** sandbox / live-e2e
**Pairs with:** `.omc/plans/occ-layer-stack-commit-resume-auto-squash-report-20260511.md` (depth-crossing probe), `.omc/plans/full-stack-adversarial-agent-tool-script-testing-plan-20260510.md` (subsystem matrix)

---

## 1. Goal

Inside a freshly-initialized `/ephemeral-os` git repository (built from scratch by the mock agent in the sandbox), build a small Python backend, then run its pytest suite via `shell` and assert it passes. The pytest pass is the binary signal that the **layer stack projects OCC layers and overlay capture into a filesystem the Python interpreter can `import` from**. The scenario also emits a **detailed, analyzable performance metrics artifact** covering tool use, layer-stack stats, overlay stats, and OCC stats so we can quantify the cost of each subsystem under realistic workload.

The scenario crosses `AUTO_SQUASH_MAX_DEPTH` ≥10 times, exercises all five Pyright LSP tools and the full Daytona sandbox toolkit (`read_file`, `write_file`, `edit_file`, `shell`) plus their direct `sandbox.api` counterparts, and biases heavily toward `edit_file` over `write_file` so the OCC apply path is exercised at incremental-edit granularity rather than file-replacement granularity.

## 2. What this scenario proves that existing scenarios do not

| Claim | Existing coverage | New coverage here |
|---|---|---|
| Layer stack squashes correctly past depth 32 once | `auto_squash_commit_resume` (synthetic 36-write probe) | repeats across **≥10 squash cycles** with semantically-meaningful files |
| LSP tools work after edits | `lsp_refresh_semantics` (5 files, 1 edit round) | **20+ files, 5+ edit rounds, all 5 LSP tools per round, post-squash refresh** |
| OCC + overlay + layerstack each work in isolation | `full_stack_adversarial` matrix cells | the four subsystems **integrated end-to-end through a real interpreter** |
| Projection serves what was written | implicit in `read_file` round trips | `python -m pytest` `import`s **every projected file** through the overlay |
| Quantitative cost of each subsystem under load | scattered timings in tool metadata | **single analyzable metrics artifact** with per-tool/per-subsystem percentiles |

Concrete projection failure modes this catches:
1. A file written 200 mutations ago survives ≥6 auto-squashes and is still `import`-able.
2. The **last** `edit_file` on a stale file wins at `import` time (no stale layer winning under squash).
3. Cross-module references after a rename refactor (`old_name` → `new_name`) resolve correctly under both Pyright (LSP) and CPython (`import`).
4. A test file edited then read-back via `shell cat` vs `read_file` agrees byte-for-byte with what `pytest` collects and runs.
5. After an `edit_file` that touches a file already 30+ layers deep, the projected content reflects the edit (no stale upperdir winning over the most-recent OCC layer).

## 3. Non-goals

- Not a microbenchmark of any single subsystem. Aggregate cost is reported; individual operations are not isolated for tuning.
- Not a real-world FastAPI/Pydantic exercise. The demo project uses **stdlib only** (no `pip install`) so the scenario is self-contained.
- Not a replacement for `auto_squash_commit_resume` or `full_stack_adversarial`. This sits **alongside** them as a higher-cost integration gate.

## 4. Project shape — what we build inside the sandbox

A small task scheduler / queue library. **Stdlib only** (`dataclasses`, `enum`, `heapq`, `json`, `pathlib`, `unittest.mock`, `pytest`).

### 4.1 Project root: `/ephemeral-os`

The repo lives at `/ephemeral-os` inside the sandbox — **not** under `/testbed/.ephemeralos/sweevo-mock/` like prior scenarios. The mock-agent's first acts in the scenario are sandbox bootstrap (Phase 0, §6.1):
1. `shell` — verify `git` is on `PATH`; if missing, install via `apt-get install -y git` (re-attempt with `apk add git` then `yum install -y git` as fallbacks; fail loudly if all fail).
2. `shell` — `mkdir -p /ephemeral-os && cd /ephemeral-os && git init -b main`.
3. `shell` — `git config user.email mock@ephemeral-os.test && git config user.name "Mock Agent"`.
4. `shell` — `printf '__pycache__/\n*.pyc\n.pytest_cache/\n' > /ephemeral-os/.gitignore`.
5. `read_file` `.gitignore` — round-trip proof.
6. `shell` — `git -C /ephemeral-os add .gitignore && git -C /ephemeral-os commit -m "init"`.

**Sandbox-prerequisite note:** the layer-stack and overlay must capture mutations under `/ephemeral-os`. Existing fixtures declare the workspace at `/testbed`. This plan requires the live_e2e fixture (§8) to be extended so the workspace contract includes `/ephemeral-os` (either as a replacement workspace or an additional captured root). If the scope of fixture-extension is non-trivial, the implementer escalates rather than silently fall back to `/testbed/ephemeral-os/`.

### 4.2 File tree (test LOC > source LOC, per §13.5)

```
/ephemeral-os/
├── .gitignore                         (~5 LOC)
├── pyproject.toml                     (~10 LOC) — minimal pytest config, no deps
├── conftest.py                         (~25 LOC) — top-level pytest fixtures
├── scheduler_demo/
│   ├── __init__.py                     (~10)
│   ├── config.py                       (~70)
│   ├── errors.py                       (~50)
│   ├── domain/
│   │   ├── __init__.py                 (~5)
│   │   ├── task.py                     (~110)
│   │   ├── schedule.py                 (~90)
│   │   └── priority.py                 (~70)
│   ├── services/
│   │   ├── __init__.py                 (~5)
│   │   ├── scheduler.py                (~130)
│   │   ├── executor.py                 (~100)
│   │   └── retry.py                    (~80)
│   ├── storage/
│   │   ├── __init__.py                 (~5)
│   │   ├── memory_store.py             (~100)
│   │   └── serializer.py               (~80)
│   ├── api/
│   │   ├── __init__.py                 (~5)
│   │   ├── routes.py                   (~90)
│   │   └── adapters.py                 (~70)
│   └── util/
│       ├── __init__.py                 (~5)
│       └── time_utils.py               (~50)
└── tests/
    ├── __init__.py                     (~5)
    ├── conftest.py                     (~50)
    ├── test_config.py                  (~80)
    ├── test_errors.py                  (~70)
    ├── test_task.py                    (~180)
    ├── test_schedule.py                (~150)
    ├── test_priority.py                (~110)
    ├── test_scheduler.py               (~200)
    ├── test_executor.py                (~150)
    ├── test_retry.py                   (~120)
    ├── test_memory_store.py            (~160)
    ├── test_serializer.py              (~120)
    ├── test_routes.py                  (~140)
    ├── test_adapters.py                (~100)
    ├── test_time_utils.py              (~80)
    └── test_integration.py             (~200)
```

| | files | LOC |
|---|---|---|
| Source (incl. `__init__.py`, `pyproject.toml`, root `conftest.py`) | 21 | **~1,090** |
| Tests (incl. `tests/__init__.py`, `tests/conftest.py`) | 16 | **~1,915** |
| **Total** | **37** | **~3,005** |

**Test/source ratio:** ~1.76× (tests > source, per §13.5). **File count:** 37 (≥20). **Test count:** ~80 unit tests + ~12 integration tests, all stdlib + pytest.

## 5. Where the source content lives — fixtures dir

The scenario logic does **not** carry source code as inline strings. Instead:

```
backend/src/live_e2e/scenarios/sandbox/_fixtures/scheduler_demo/
    <full project tree above, checked into the repo as plain .py files>
```

The scenario reads these from disk at scenario-build time, then issues the corresponding tool calls into the sandbox at `/ephemeral-os/`. To bias toward `edit_file` over `write_file` (per §13.6, user request 4), each non-trivial source/test file is split into **(a) a small skeleton stub** written via `write_file`, then **(b) a series of `edit_file` patches** that build the full file incrementally. The skeleton + patch pairs live alongside the final `.py`:

```
backend/src/live_e2e/scenarios/sandbox/_fixtures/scheduler_demo/
    domain/task.py                          (final form, used by tests + LSP fixture verification)
    domain/task.py.skeleton                 (initial write_file payload — ~15 LOC)
    domain/task.py.patches.json             (ordered list of edit_file (old, new, description) — produces final form)
```

A pre-merge CI step verifies `apply(skeleton, patches) == final` for every file (cheap host-side test, milliseconds).

Trade-off accepted: ~3K LOC of fixture code added to the repo, plus ~30 patch files. One-time cost; the patch files make the edit progression reviewable by humans.

## 6. Tool-call orchestration — phase plan

The new executor action `complex_project_build` is added to `MockSquadRunner._run_executor`. It dispatches to `_run_complex_project_build_probe`, a programmatic loop following the `_run_auto_squash_commit_resume_probe` pattern.

**Bias rule (§13.6):** `edit_file` count ≥ 4 × `write_file` count across the whole run. The scenario tracks the running ratio and asserts it in §7.

### 6.1 Phase 0 — Sandbox bootstrap (~30 calls)
- 6 shell calls to install/verify `git`, mkdir `/ephemeral-os`, init repo, write `.gitignore`, initial commit (§4.1).
- `read_file` `.gitignore` (1 read).
- LSP `diagnostics` on `/ephemeral-os/.gitignore` to confirm Pyright sees the new workspace root (1 LSP).
- `shell git -C /ephemeral-os status` (round-trip proof, 1 shell).
- `shell which git python3 python pytest` (provider-toolchain probe, ~4 shell calls — count each tool individually).
- Direct `sandbox.api.read_file` snapshot of `/ephemeral-os/.gitignore` to compare host-API vs tool-API outputs (1 sandbox.api).
- Daytona/sandbox-toolkit confidence probe: `sandbox.api.shell` invocation duplicating the `git status` from the toolkit, comparing exit code + stdout (~2 calls).
- **Subtotal:** **~30**

### 6.2 Phase A — Skeleton (~140 calls)
- Create directory structure via `shell mkdir -p` (~7 calls)
- Write 7 empty `__init__.py` files via `write_file`
- Write `pyproject.toml`, root `conftest.py` (~2)
- `read_file` each just-written file (~9 readbacks)
- `shell ls -R /ephemeral-os` to verify tree (~3)
- LSP `diagnostics` on each `__init__.py` (warm Pyright cache for new workspace root) (~7)
- LSP `query_symbols` empty-query against the workspace (~1, exercises symbol-index over empty package)
- `git -C /ephemeral-os add . && git commit -m "skeleton"` (~2 shell)
- Direct `sandbox.api.read_file` snapshot of `pyproject.toml` (1 sandbox.api)
- **Subtotal:** **~140**

### 6.3 Phase B — Core domain (~480 calls, edit-heavy)
- `write_file` 6 substantive skeleton stubs: `config.py`, `errors.py`, `domain/{task,schedule,priority}.py`, `util/time_utils.py` (~6 writes; each stub is ~15 LOC).
- For each file: **8–12 `edit_file` patches** to build the full file incrementally (~60 edits — ratio: 10:1 edit/write within phase).
- After every 3 edits: all 5 LSP tools (`hover`, `find_definitions`, `find_references`, `query_symbols`, `diagnostics`) on the just-edited symbol (~100 LSP).
- `read_file` after each edit batch (~30).
- `shell python -c "import scheduler_demo.<module>"` per module (~6 shell).
- Direct `sandbox.api.edit_file` (batch form, 2 search/replace edits at once) on one file per phase to exercise the batch-edit path (~6 sandbox.api).
- Cross-check: `sandbox.api.read_file` vs tool-`read_file` on same path returns identical bytes (~6 sandbox.api comparisons).
- `shell git add -A && git diff --stat HEAD~1` (~6 shell — git visibility into OCC layers).
- Conflict probe: 1 intentional missing-anchor `edit_file` per major file (~6 expected errors).
- **Subtotal:** **~480**

### 6.4 Phase C — Services + storage + api (~720 calls, edit-heavy)
- 7 substantive skeleton stubs (~7 writes).
- **12–16 `edit_file` patches per file** (~95 edits — ratio: 13.5:1 within phase).
- All 5 LSP tools after every 3 edits (~155 LSP).
- LSP `find_references` calls now span multiple files (real cross-module refs); explicitly verify `>=N` references for each public symbol where N comes from the fixture-known reference graph (~30 cross-file LSP).
- `read_file` after each edit batch (~30).
- `shell python -c "import scheduler_demo.services.scheduler; ..."` per module (~7).
- Direct `sandbox.api.edit_file` batch path (~7 sandbox.api).
- Direct `sandbox.api.shell` invocation of `python -c "import scheduler_demo"` (compare with tool-`shell` output) (~7).
- Conflict probes (~7 expected errors).
- Stale-edit probe: capture the file's content via `read_file`, mutate via `shell` (`echo >> file`), then attempt an `edit_file` against the now-stale content snapshot — assert OCC reports stale-content conflict (~7 staged probes).
- **Subtotal:** **~720**

### 6.5 Phase D — Refactor passes (~520 calls, edit-saturated)
Three rename refactors that exercise the layer stack across many files at once:
1. Rename `Task.status` → `Task.state` across model + 9 dependent files (~35 edits + 35 LSP refs).
2. Rename `MemoryStore.get` → `MemoryStore.fetch` across services + tests (~30 edits + 30 LSP refs).
3. Add a new `priority` field to `Task` and propagate to 12 dependent files (~50 edits + 50 LSP refs).

- After each refactor: full 5-LSP-tool sweep on the renamed symbol; assert reference count matches expected post-rename count (~3 × 5 = 15 LSP).
- After each refactor: `shell python -m pytest tests/ -k <touched_module>` (~3 shell pytest invocations as smoke).
- After each refactor: `git -C /ephemeral-os add -A && git commit -m "refactor: ..."` (~3 shell git).
- Direct `sandbox.api.read_file` against renamed-target paths to verify projection (~9 sandbox.api).
- **Subtotal:** **~520**

### 6.6 Phase E — Test suite (~580 calls, edit-heavy)
- Write all 14 substantive test-file skeletons (~14 writes).
- **8–10 `edit_file` patches per test file** (~125 edits — ratio: ~9:1 within phase).
- All 5 LSP tools on each test file at least once (~70 LSP).
- `read_file` round trips (~50).
- `shell python -m pytest tests/test_<X>.py -v --tb=short` per test file (~14).
- Direct `sandbox.api.read_file` on every test file (~16 sandbox.api).
- Direct `sandbox.api.shell` for one pytest run (compare exit-code/stdout with tool-`shell`) (~1).
- 2 intentional `xfail`-via-edit probes (introduce a wrong assertion via `edit_file`, run pytest, observe failure, fix via `edit_file`) — both exercise edit/projection coupling (~4).
- **Subtotal:** **~580**

### 6.7 Phase F — Final pytest gate + post-squash LSP saturation + metrics (~330 calls)
- **Headline:** `shell python -m pytest tests/ -v --tb=short --junit-xml=/ephemeral-os/.metrics/pytest.xml` (1 call).
- After full pytest pass, run all 5 LSP tools on every public symbol (~30 symbols × 5 tools = 150 LSP).
- `read_file` against every source/test file once more to capture final content via projection (~37).
- `shell cat` the same files (~37) — assert byte-for-byte equality with `read_file`.
- Direct `sandbox.api.read_file` snapshot on all 37 files (~37 sandbox.api) — assert equality with `read_file` and `cat`.
- `shell find /ephemeral-os -type f | sort | wc -l` — verify file count (~1).
- `shell git -C /ephemeral-os log --oneline | wc -l` — verify commit count (~1).
- Intentional missing-anchor `edit_file` to capture conflict shape (~1, expected error).
- Direct `sandbox.api.edit_file` against the same missing anchor (compare conflict payload shape) (~1, expected error).
- **Metrics emission** (~10 calls): `write_file` `/ephemeral-os/.metrics/perf.json` with the aggregated metrics (§9), then `read_file` it back, then `sandbox.api.read_file` it back, then `git add -A && git commit -m "metrics"`. The aggregation logic is performed inside the runner method `_run_complex_project_build_probe` (Python in-process — no `shell python` codegen needed).
- Final `shell git log --stat` (~1).
- **Subtotal:** **~330**

### 6.8 Tool-call budget total

| Phase | Calls | Cumulative |
|---|---|---|
| 0. Sandbox bootstrap | 30 | 30 |
| A. Skeleton | 140 | 170 |
| B. Core domain | 480 | 650 |
| C. Services/storage/api | 720 | 1,370 |
| D. Refactor passes | 520 | 1,890 |
| E. Test suite | 580 | 2,470 |
| F. Final gate + metrics | 330 | **~2,800** |

**Floor:** 2,000 (user requirement). **Realized estimate:** ~2,800. **Edit:write ratio (estimate):** total `edit_file` ≈ 460, total `write_file` ≈ 80 → ~5.75× — meets the §13.6 ≥4× requirement.

Auto-squash threshold (32) is crossed naturally: phases B+C+D produce ~250+ OCC mutations from `edit_file` alone, so squash fires ≥7× per phase, ≥10× across the full run (likely 15–20×).

## 7. Layer-stack + LSP + Daytona + perf assertions — what the paired test checks

Test file: `backend/src/live_e2e/tests/sweevo/test_complex_project_build.py`

Gating:
```python
@pytest.mark.skipif(not os.environ.get("EPHEMERALOS_DATABASE_URL"),
                    reason="EPHEMERALOS_DATABASE_URL not set - live_e2e requires PostgreSQL")
@pytest.mark.skipif(not os.environ.get("EPHEMERALOS_RUN_HEAVY_LIVE_E2E"),
                    reason="heavy live e2e — opt-in via EPHEMERALOS_RUN_HEAVY_LIVE_E2E=1")
@pytest.mark.timeout(2400)  # 40 minutes
async def test_complex_project_build_layer_stack_projection(...):
```

Assertions (mirrors `test_auto_squash_commit_resume.py` shape, extended):

**Core projection / squash claims:**
1. `report.task_center_status == "done"`.
2. `len(report.tool_calls) >= 2000`. Realized ~2,800.
3. `EventType.SANDBOX_LAYER_STACK_LAYERS_SQUASHED` count ≥ 10 in both in-memory events and `sandbox_events.jsonl`.
4. Required sandbox events present: `SANDBOX_OCC_CHANGESET_RECEIVED`, `SANDBOX_OCC_CHANGES_COMMITTED`, `SANDBOX_BATCH_EDIT_APPLIED`, `SANDBOX_CONFLICT_DETECTED` (×2 — tool + sandbox.api).
5. At least one tool-call metadata block contains `layer_stack.auto_squash.total_s`, and `max(depth_before)` across all tool calls > 32.
6. `shell python -m pytest tests/ -v` exit code == 0; stdout contains `passed` and zero `failed`.
7. All 37 files importable: per-module `python -c "import …"` calls all returned exit 0.
8. **Tri-source projection consistency:** for each of 37 files, `read_file` content == `shell cat` content == `sandbox.api.read_file` content (byte-identical).
9. Cross-module symbol references after rename refactors match expected counts from the fixture reference graph.
10. No unexpected tool errors: error count == intentional-conflict count (~16 — one per phase B file + one per phase C file + 2 phase E xfail probes + 2 phase F probes).

**Edit-bias claim (§13.6):**
11. `count(tool_name == "edit_file") / max(count(tool_name == "write_file"), 1) >= 4.0`.

**LSP saturation:**
12. Each of the 5 LSP tools (`hover`, `find_definitions`, `find_references`, `query_symbols`, `diagnostics`) was invoked **≥30 times** across the run (so coverage is wide, not just deep on one tool).
13. At least one `find_references` call returns `>= N_expected` references for the renamed `Task.state` symbol (where `N_expected` = number of import/use sites from the fixture reference graph).
14. At least one `query_symbols` call against `scheduler_demo.services.scheduler` returns the post-squash symbol list matching the expected set.

**Daytona / sandbox.api saturation:**
15. Direct `sandbox.api.read_file` was invoked **≥40 times**.
16. Direct `sandbox.api.edit_file` (batch) was invoked **≥10 times** with `applied_edits == 2`.
17. Direct `sandbox.api.shell` was invoked **≥3 times** and matched tool-shell output exactly.
18. The intentional missing-anchor probe via `sandbox.api.edit_file` reports `success == False` with non-empty `conflict_reason` and the same payload shape as the tool-driven conflict in §7.10.

**Metrics artifact (§9):**
19. `/ephemeral-os/.metrics/perf.json` exists, parses, and contains all four top-level keys: `tool_use`, `layer_stack`, `overlay`, `occ`.
20. `tool_use.total_calls` matches `len(report.tool_calls)` ±1 (the +/-1 allows for the metrics-write call itself).
21. `layer_stack.squash_count >= 10`, `layer_stack.max_depth_before > 32`, and `layer_stack.materialize_s_total >= 0`.
22. `occ.commit_count` matches the count of `SANDBOX_OCC_CHANGES_COMMITTED` events.
23. `overlay.capture_upperdir_s_total > 0` and `overlay.shell_calls == count(tool_name == "shell")`.
24. The pytest junit XML at `/ephemeral-os/.metrics/pytest.xml` shows `failures == 0`, `errors == 0`, `tests >= 80`.

**Repo bootstrap claims (§4.1):**
25. Final `git log --oneline` (captured in scenario summary) shows at least 5 commits (init, skeleton, refactor×3, metrics).
26. `git status` at end is clean (`nothing to commit, working tree clean`).

## 8. Implementation surface — files added/changed

**New (added):**
- `backend/src/live_e2e/scenarios/sandbox/complex_project_build.py` — `ComplexProjectBuild(ScenarioBase)` (~180 LOC, follows `AutoSquashCommitResume` pattern; smoke variant `ComplexProjectBuildSmoke` co-located).
- `backend/src/live_e2e/scenarios/sandbox/_fixtures/scheduler_demo/**/*.{py,toml,gitignore}` — final-form project source (~3,005 LOC across 37 files).
- `backend/src/live_e2e/scenarios/sandbox/_fixtures/scheduler_demo/**/*.skeleton` and `*.patches.json` — incremental edit progression (~30 patch files).
- `backend/src/live_e2e/scenarios/sandbox/_fixtures/refactor_passes.py` — Phase D rename plans (~150 LOC).
- `backend/src/live_e2e/scenarios/sandbox/_fixtures/lsp_reference_graph.json` — expected reference counts per public symbol post-refactor.
- `backend/src/live_e2e/scenarios/sandbox/_metrics.py` — perf-metrics aggregator (parses tool-call timings, emits perf.json) (~120 LOC).
- `backend/src/live_e2e/tests/sweevo/test_complex_project_build.py` — paired test (~350 LOC; smoke + full forms).
- `backend/src/live_e2e/tests/sweevo/test_complex_project_build_fixtures.py` — host-side test that `apply(skeleton, patches) == final` for each fixture file (~80 LOC; pre-merge gate).

**Modified (surgical):**
- `backend/src/live_e2e/scenarios/sandbox/__init__.py` — register `ComplexProjectBuild`, `ComplexProjectBuildSmoke`.
- `backend/src/live_e2e/scenarios/__init__.py` — register both in `SCENARIO_REGISTRY`.
- `backend/src/live_e2e/squad/runner.py` — add `complex_project_build` and `complex_project_build_smoke` action handlers next to `_run_auto_squash_commit_resume_probe`. **No refactor of the existing if/elif chain.**
- `backend/src/live_e2e/fixtures.py` (or equivalent) — extend the live_e2e workspace contract to declare `/ephemeral-os` as a captured workspace root in addition to `/testbed`. **This is the single deepest change.** If extending the workspace contract turns out to require sandbox-provider work (Daytona snapshot config, mount setup), the implementer escalates to a follow-up plan rather than scoping that here.

**Not changed:** `base.py`, `tool_scripts.py`, `full_stack_tool_scripts.py`, hooks registry, audit subsystem.

## 9. Performance metrics artifact — `/ephemeral-os/.metrics/perf.json`

Emitted at the end of Phase F. Schema (versioned `complex_project_build.perf.v1`):

```jsonc
{
  "schema": "complex_project_build.perf.v1",
  "run_id": "<task_center_run_id>",
  "scenario": "sandbox.complex_project_build",
  "wall_seconds_total": 612.4,

  "tool_use": {
    "total_calls": 2812,
    "by_tool": {
      "write_file":   {"count":  82, "errors":  0, "wall_s_total":  18.3, "wall_s_p50":  0.18, "wall_s_p95":  0.41, "wall_s_max":  0.83},
      "edit_file":    {"count": 471, "errors": 16, "wall_s_total":  98.1, "wall_s_p50":  0.16, "wall_s_p95":  0.55, "wall_s_max":  1.92},
      "read_file":    {"count": 318, "errors":  0, "wall_s_total":  12.4, "wall_s_p50":  0.03, "wall_s_p95":  0.08, "wall_s_max":  0.21},
      "shell":        {"count": 154, "errors":  3, "wall_s_total": 142.6, "wall_s_p50":  0.41, "wall_s_p95":  3.20, "wall_s_max":  9.81},
      "lsp.hover":    {"count":  52, "errors":  0, "wall_s_total":   8.1, "wall_s_p50":  0.12, "wall_s_p95":  0.34, "wall_s_max":  0.71},
      "lsp.find_definitions":  {"count":  43, "errors": 0, "wall_s_total":  6.5, ...},
      "lsp.find_references":   {"count": 117, "errors": 0, "wall_s_total": 22.4, ...},
      "lsp.query_symbols":     {"count":  31, "errors": 0, "wall_s_total":  4.8, ...},
      "lsp.diagnostics":       {"count":  74, "errors": 0, "wall_s_total": 12.1, ...},
      "sandbox.api.read_file": {"count":  44, "errors": 0, "wall_s_total":  1.6, ...},
      "sandbox.api.edit_file": {"count":  12, "errors": 1, "wall_s_total":  1.9, ...},
      "sandbox.api.shell":     {"count":   5, "errors": 0, "wall_s_total":  4.2, ...}
    },
    "edit_to_write_ratio": 5.74,
    "errors_total": 20,
    "expected_errors_total": 16,
    "unexpected_errors_total": 0
  },

  "layer_stack": {
    "squash_count": 17,
    "squash_total_s": 4.62,
    "squash_p50_s": 0.21,
    "squash_p95_s": 0.48,
    "squash_max_s": 0.91,
    "max_depth_before": 36.0,
    "depth_distribution_buckets": [{"max_depth": 8, "count": 24}, {"max_depth": 16, "count": 31}, ...],
    "materialize_s_total": 12.4,
    "materialize_count": 421,
    "materialize_p50_s": 0.012,
    "materialize_p95_s": 0.084
  },

  "overlay": {
    "capture_upperdir_s_total": 38.7,
    "capture_upperdir_count": 154,
    "capture_upperdir_p50_s": 0.18,
    "capture_upperdir_p95_s": 0.84,
    "capture_upperdir_max_s": 4.21,
    "shell_calls": 154,
    "shell_calls_with_capture": 154
  },

  "occ": {
    "changeset_count": 553,
    "commit_count": 553,
    "commit_total_s": 71.4,
    "commit_p50_s": 0.08,
    "commit_p95_s": 0.42,
    "commit_max_s": 1.83,
    "publish_layer_total_s": 33.2,
    "publish_layer_p50_s": 0.04,
    "commit_resume_wait_total_s": 9.6,
    "commit_resume_wait_p95_s": 0.31,
    "conflict_count": 18,
    "conflict_expected_count": 16,
    "conflict_unexpected_count": 0
  },

  "phases": [
    {"name": "0_bootstrap",   "start_s":   0.0, "end_s":  18.4, "tool_calls": 30,  "squashes": 0},
    {"name": "A_skeleton",    "start_s":  18.4, "end_s":  47.1, "tool_calls": 140, "squashes": 1},
    {"name": "B_core",        "start_s":  47.1, "end_s": 152.6, "tool_calls": 480, "squashes": 3},
    ...
  ]
}
```

All metrics are aggregated from `tool_call.metadata["timings"]` keys already populated by the runtime (`layer_stack.materialize_s`, `layer_stack.auto_squash.total_s`, `layer_stack.auto_squash.depth_before`, `command_exec.capture_upperdir_s`, `occ.apply.commit_resume_wait_s`, `occ.commit.total_s`, `occ.commit.publish_layer_s`) plus per-tool wall-time captured by the runner around `_call_tool`.

A small post-run analyzer in `_metrics.py` ingests the JSON and prints a one-screen summary table; that analyzer is also exposed as a CLI under `backend/scripts/analyze_complex_build_perf.py` so engineers can re-render reports from saved metrics without re-running the scenario.

## 10. Runtime / cost / CI placement

- **Expected wall time (full):** 12–25 minutes per run; smoke variant 1–2 minutes.
- **Gate:** double opt-in via env vars
  - `EPHEMERALOS_DATABASE_URL` (matches existing live_e2e gate)
  - `EPHEMERALOS_RUN_HEAVY_LIVE_E2E=1` (heavy-test gate, also gates other future >5min scenarios)
- **CI placement:**
  - Pre-merge: smoke variant (§11) + the host-side `test_complex_project_build_fixtures` (~ms).
  - Nightly: full ~2,800-call form.
- **Local runbook:** `EPHEMERALOS_RUN_HEAVY_LIVE_E2E=1 EPHEMERALOS_DATABASE_URL=… .venv/bin/pytest backend/src/live_e2e/tests/sweevo/test_complex_project_build.py -v -s`.

## 11. Smoke variant (for pre-merge gating)

`ComplexProjectBuildSmoke` reuses the same `_run_complex_project_build_probe` machinery but with:
- `/ephemeral-os` bootstrap unchanged (Phase 0 — git install + init).
- 6 source files instead of 21 (`config.py`, `errors.py`, `domain/task.py`, `services/scheduler.py`, `tests/test_task.py`, `tests/conftest.py`).
- 1 refactor pass instead of 3.
- 1 pytest run instead of 14.
- Edit:write ratio still ≥4×.
- Tool-call floor: ≥250.
- Same projection-consistency gate (§7.6, §7.8) and same metrics artifact (§9, possibly with smaller numbers).
- Runs in <2 min, no `EPHEMERALOS_RUN_HEAVY_LIVE_E2E` required, only `EPHEMERALOS_DATABASE_URL`.

## 12. Risks and mitigations

| Risk | Mitigation |
|---|---|
| `git` not installed in the sandbox image | Phase 0 attempts `apt-get`, `apk`, `yum` in order; fails fast with a clear message; runbook documents adding `git` to the base image as a follow-up |
| `/ephemeral-os` not captured by overlay/layer-stack | §8 calls out the workspace-contract extension as the deepest change; if non-trivial, escalate before starting implementation |
| Pytest unavailable in /ephemeral-os python environment | Phase 0 includes `python -m pytest --version` probe; if missing, scenario fails fast |
| Test runs flaky under squash race | Reuse the commit-resume-wait pin from `auto_squash_commit_resume`; no new race surface introduced |
| Fixture source drifts from real Python validity | Pre-merge `test_complex_project_build_fixtures` validates `apply(skeleton, patches) == final` and that `final` parses with `ast.parse` |
| 2,800-call run blows test timeout | `pytest.mark.timeout(2400)` (40 min) on the test; smoke variant has a separate 5-min timeout |
| LSP find_references count drifts as Pyright is upgraded | Test asserts `>= expected_min` not `==`, with the fixture `lsp_reference_graph.json` documenting the floor; floor is reviewed when Pyright pin moves |
| Metrics schema churn breaks downstream readers | Schema versioned (`complex_project_build.perf.v1`); analyzer accepts only matching schema |

## 13. Resolved decisions

User answered 2026-05-11:
1. **Fixture LOC trade-off:** **smaller** — target ~3K LOC across ≥20 files; accept LOC<5000.
2. **Smoke variant + nightly heavy:** **both** — pre-merge runs the smoke (§11), nightly runs the full ~2,800-call form.
3. **Heavy-gate env name:** **`EPHEMERALOS_RUN_HEAVY_LIVE_E2E`** confirmed.
4. **Refactor count in Phase D:** **3 passes** as proposed (§6.5).

User answered 2026-05-11 (round 2):
5. **Repo root + git setup:** project lives at **`/ephemeral-os`** (fresh `git init`); Phase 0 bootstrap installs `git` if absent (§4.1, §6.1).
6. **LSP and Daytona saturation:** all 5 LSP tools and direct `sandbox.api` (read_file/edit_file/shell) exercised ≥30/40/3 times respectively (§6, §7.12–18).
7. **Test LOC > source LOC:** test ~1,915 LOC vs source ~1,090 LOC, ratio 1.76× (§4.2).
8. **Edit > write:** edit:write ratio ≥4×, realized ~5.75× (§6 bias rule, §7.11).
9. **Detailed perf metrics:** `/ephemeral-os/.metrics/perf.json` per schema in §9; CLI analyzer at `backend/scripts/analyze_complex_build_perf.py` (§9).

## 14. Acceptance criteria (definition of done)

A merge ships when:
- [ ] `test_complex_project_build_smoke` passes pre-merge (CI-gated).
- [ ] `test_complex_project_build_fixtures` (host-side, apply-skeleton-and-patches) passes pre-merge.
- [ ] `EPHEMERALOS_RUN_HEAVY_LIVE_E2E=1 pytest test_complex_project_build` passes locally and in nightly CI.
- [ ] Realized tool-call count ≥2,000 (target band 2,500–3,000).
- [ ] Edit:write ratio ≥4× (target ~5×).
- [ ] Squash event count ≥10 across both event sources.
- [ ] All 26 paired-test assertions in §7 pass.
- [ ] `/ephemeral-os/.metrics/perf.json` validates against the v1 schema.
- [ ] `backend/scripts/analyze_complex_build_perf.py` renders a readable summary from a saved perf.json.
- [ ] No regressions in existing `test_auto_squash_commit_resume` or `test_full_stack_adversarial` runs.
- [ ] Plan + implementation reports filed under `.omc/plans/` matching existing convention.
