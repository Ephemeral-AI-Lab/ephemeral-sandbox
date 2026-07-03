# Workspace-Session Finalize-Policy Live-Docker Test Spec

Source of truth:
`docs/obsidian/ephemeral-os/implementation_plan/finalize-policy/spec.md`
(draft v2), implemented in commit `66f3921a8`
(`feat: workspace-session finalize-policy redesign`).

This spec covers two things:

1. **Compatibility** — how the existing live e2e suites must be updated for the
   new `create_workspace_session` / `exec_command` semantics.
2. **New coverage** — a case catalog under `runtime/workspace_session/`
   targeting `create_workspace_session`, `exec_command`, and the finalize
   policy, mirroring the verdict conventions of
   `manager/management/squash/test_spec.md`
   (`test-reports/<RUN_ID>/<CASE_ID>/verdict.json` per executed case).

## 1. What changed at the CLI surface

Flags are unchanged on all three operations. Responses and semantics changed:

| Operation | Change | Kind |
| --- | --- | --- |
| `create_workspace_session` | response gains `finalize_policy: "no_op"` (always `no_op` for CLI-created sessions; there is **no** `--finalize-policy` flag) | additive |
| `exec_command` | response gains `workspace_session_id` on every yield (running and terminal, all drain paths) | additive |
| `exec_command` | terminal response gains `publish_rejected: true` + `publish_reject_class` when this command's completion ran a finalize whose publish was rejected; the unpublished changes are discarded and the destroy still happens | additive |
| `exec_command` (bare) | still implicitly creates a session, now named policy `publish_then_destroy`; the session id **escapes** in the response, so progress-check riders (`exec_command --workspace-session-id <id>` while the first command runs) are a supported pattern; a rider defers finalization until the last running command completes | semantic |
| `destroy_workspace_session` | refusal contract unchanged (`error.details.active_command_session_ids`), but the check is now the session's own command ledger; sessions whose finalization failed (`finalize_failed` / stuck-`finalizing`) are **destroyable** through this op — it is the documented recovery path | semantic |
| file ops / remounts | never extend or trigger the session lifecycle; one racing the last command completion or a destroy now loses cleanly with `operation_failed` (“workspace session not found”) instead of running against a torn-down session | semantic |
| command drains | completed commands are retained up to 512 terminal entries per daemon; a drain (`read_command_lines` / `write_command_stdin`) against an evicted id returns `command not found` | semantic |
| observability snapshot | each workspace in the daemon snapshot JSON gains `finalize_policy` | additive |
| vocabulary | “one-shot”, “exec-owned”, “caller-owned”, “user-owned” are gone from op descriptions; say “implicit session (`publish_then_destroy`)” and “session (`no_op`)” | doc |

Everything the suite already asserts by **field lookup** (`result["..."]`)
keeps working — no existing assertion reads a key that was removed. Only
exact-shape assertions would break, and the audit below found none on these
operations.

## 2. Compatibility audit of the existing suites

Verified against the current tree (grep for `exec_command`,
`create_workspace_session`, `destroy_workspace_session`):

| File | Verdict | Required update |
| --- | --- | --- |
| `runtime/file/helpers.py` | compatible | `create_workspace_session()` returns only the id today; extend it to also assert `finalize_policy == "no_op"` (one line). Add an `exec_command` docstring note that the response now carries `workspace_session_id`. |
| `runtime/file/**/test_*.py` (smoke, correctness, file_exec, blame, concurrent) | compatible | none — they assert by field lookup. Optional hardening: sessionless `file_exec` tests may assert the implicit exec response's `workspace_session_id` is present and no longer resolvable after terminal status (see EX-03). |
| `runtime/command/test_exec_command_layer_depth_benchmark.py` | compatible | none — it never drains more than 512 completed commands per daemon. Add a comment noting the retention cap so future depth extensions know drains of old command ids expire. |
| `runtime/test_squash_remount.py` | compatible | `_publish` uses a bare exec to publish a layer — that is exactly the implicit `publish_then_destroy` path and keeps working. No change. |
| `runtime/daemon_http/test_daemon_http.py` | compatible | none; optional: assert `finalize_policy` is present on snapshot workspaces (additive field). |
| `runtime/network_isolation/test_network_isolation.py` | compatible | none — network profile axis is orthogonal to finalize policy. |
| `manager/management/squash/helpers.py` | compatible | `_destroy_session` already handles the `active_command_session_ids` rejection loop — contract unchanged. Rename the two “one-shot exec capture …” comments (lines ~2051, ~2084) and the HRD-09 title “one-shot finalize mid-switch” to “implicit-session finalize …” — vocabulary only, behavior identical (HRD-09 exercises the same completion-edge finalize, now the policy runner). |
| `manager/management/*` (sandbox lifecycle) | unaffected | none. |

One **behavioral** re-check is required, not just vocabulary: any test that
races a session file op against `destroy_workspace_session` (today only the
in-repo Rust suites do) must expect the file op to fail `not found` rather
than run after the gate releases. No current Python test does this; new cases
WS-04/EX-06 below pin the behavior at the e2e level.

## 3. New coverage catalog — `runtime/workspace_session/`

Layout per the suite README: `runtime/workspace_session/{__init__.py,
helpers.py, test_workspace_session.py, test_exec_finalize.py}` plus this spec.
Helpers wrap only `sandbox-cli runtime …` operations and return parsed JSON;
every case writes `test-reports/<RUN_ID>/<CASE_ID>/verdict.json` with
`correctness` / `teardown` axes (timing axis only where noted).

Policy availability constraint: the CLI cannot create a
`publish_then_destroy` session (**no** `--finalize-policy` flag by design), so
the e2e policy matrix is: explicit create ⇒ `no_op`; bare `exec_command` ⇒
implicit `publish_then_destroy`. Both rows are covered below.

### create_workspace_session (WS)

| Case | Tier | Title | Assertions |
| --- | --- | --- | --- |
| WS-01 | smoke | create response contract | `workspace_session_id` non-empty, `network_profile: "shared"` by default (`"isolated"` with the flag), `finalize_policy: "no_op"`; observability snapshot lists the workspace with `finalize_policy: "no_op"`. |
| WS-02 | smoke | no_op session survives command completion | create → `exec_command --workspace-session-id … 'echo hi'` to terminal → session still usable (second exec + `file_read` succeed) → explicit destroy succeeds. |
| WS-03 | smoke | destroy refuses while a command runs | create → start `sleep 30` with `--yield-time-ms 0` → destroy returns `operation_failed` with `error.details.active_command_session_ids == [<command id>]` → Ctrl-C via `write_command_stdin` → destroy succeeds. |
| WS-04 | medium | destroy always discards; sync op racing destroy loses cleanly | create → `file_write` a change → destroy → new implicit exec `cat` shows the change is **absent** (no publish on explicit destroy); a `file_read` issued immediately after destroy returns `operation_failed` not-found, never stale content. |
| WS-05 | medium | no `--finalize-policy` flag exists | `sandbox-cli runtime create_workspace_session --finalize-policy no_op` fails argument parsing; CLI catalog (`sandbox-cli help`) shows no such flag. |
| WS-06 | medium | destroyed id stays dead | after WS-02's destroy, `exec_command --workspace-session-id <id>`, `file_read`, and a second destroy all return `operation_failed` not-found (and the daemon does not wedge — a fresh create still works). |

### exec_command (EX)

| Case | Tier | Title | Assertions |
| --- | --- | --- | --- |
| EX-01 | smoke | implicit exec response contract | bare `exec_command 'echo hi'` terminal response has `status: "ok"`, `workspace_session_id` present, no `publish_rejected` key; the id is **not** resolvable afterwards (follow-up exec into it → `operation_failed`). |
| EX-02 | smoke | implicit exec publishes then destroys | bare `exec_command 'echo v1 > /workspace/e2e-implicit.txt'` → a **new** bare exec `cat /workspace/e2e-implicit.txt` reads `v1` (publish landed in the layerstack); observability snapshot no longer lists the first session. |
| EX-03 | smoke | session exec carries the session id | `exec_command --workspace-session-id <no_op id>` running and terminal responses both carry that `workspace_session_id`. |
| EX-04 | medium | rider defers finalization | bare `exec_command 'sleep 8; echo done > /workspace/rider.txt'` with `--yield-time-ms 0` → grab `workspace_session_id` → rider `exec_command --workspace-session-id <id> 'ls /workspace'` completes → session still alive (second rider works) → wait for the long command to finish → session finalizes: follow-up exec into the id fails, and a new bare exec `cat /workspace/rider.txt` reads `done`. |
| EX-05 | medium | publish rejection surfaces on the terminal response | bare `exec_command 'mkdir -p /workspace/layers && echo x > /workspace/layers/evil.txt'` (a layerstack-internal path) → terminal response `status: "ok"` **and** `publish_rejected: true` with `publish_reject_class: "protected_path"` → session is destroyed anyway (id unresolvable) → a new bare exec proves the mutation was discarded. The load-bearing assertions are `publish_rejected: true`, destroy-still-happens, and discard. |
| EX-06 | medium | file op racing the last completion gets not-found | bare `exec_command 'sleep 2'` with `--yield-time-ms 0` → tight loop of `file_read --workspace-session-id <id>` until it flips to `operation_failed` not-found; assert no read ever returns partial/torn state and the loop terminates ≤ 30 s (finalize completed). |
| EX-07 | medium | interrupt/timeout paths still finalize | bare exec `sleep 60` → Ctrl-C via `write_command_stdin` → cancelled terminal response carries `workspace_session_id`; session finalizes (id unresolvable). Repeat with `--timeout-ms 1000` and a timed-out status. |
| EX-08 | hard | drain retention cap | env-gated (`E2E_RETENTION=1`): one `no_op` session; run 520 fast `exec_command`s to terminal in it; drain the **first** command id → `operation_failed` command-not-found; drain the newest → still readable; session destroy succeeds (ledger is empty — retention never touches the ledger). |

### finalize policy cross-checks (FP)

| Case | Tier | Title | Assertions |
| --- | --- | --- | --- |
| FP-01 | medium | remount sweep cannot finalize an idle implicit session | start a bare long-running exec (its session is `publish_then_destroy`, ledger non-empty) and an idle `no_op` session; run `manager checkpoint_squash` (post-squash sweep remounts every live session) → both sessions survive the sweep (exec still drains; `no_op` session still resolves); then finish the command → only the implicit session finalizes. |
| FP-02 | medium | empty capture skips publish | snapshot the manifest version (observability layerstack view) → bare `exec_command 'true'` (no writes) → manifest version unchanged (no empty layer, no no-op publish), session destroyed. |
| FP-03 | medium | back-to-back implicit execs are independent sessions | two sequential bare execs return different `workspace_session_id`s; writes from the first are visible to the second (published layer), not via a shared live session. |
| FP-04 | hard | finalize-vs-destroy interleave storm | N=8 threads alternating bare execs and explicit create/exec/destroy cycles for 60 s; verdict: zero daemon faults, zero leaked sessions in the observability snapshot at the end, every explicit destroy either succeeds or reports `active_command_session_ids`. |

Timing axis: EX-04, EX-06, FP-01, FP-04 record wall-clock bounds in the
verdict (finalize observed within 30 s of terminal status); the others are
correctness/teardown only.

Teardown evidence for every case: after the family run, the observability
snapshot lists no workspace created by the case, and `destroy_sandbox`
succeeds (fixture-owned, mirroring `conftest.py`).

## 4. Harness work items

1. `runtime/workspace_session/helpers.py`: wrappers `create_session()`
   (returns the full JSON, asserts `finalize_policy`), `exec_bare()`,
   `exec_in()`, `destroy_session()` (returns raw result for refusal checks),
   `wait_finalized(sandbox_id, workspace_session_id, timeout_s)` — polls
   `exec_command --workspace-session-id <id> 'true' --yield-time-ms 0` until
   `operation_failed` not-found (the black-box finalize signal), and
   `interrupt(command_session_id)` (Ctrl-C via `write_command_stdin`).
2. `runtime/file/helpers.py`: assert `finalize_policy == "no_op"` in
   `create_workspace_session()`; no other change.
3. `manager/management/squash/helpers.py`: vocabulary rename only (§2).
4. Reuse the squash suite's verdict writer (or copy its minimal shape) for
   `test-reports/<RUN_ID>/<CASE_ID>/verdict.json` + a family `SUMMARY.md`.

## 5. Runbook

```sh
cd cli-operation-e2e-live-test
pytest runtime/workspace_session -m smoke      # WS-01..03, EX-01..03
pytest runtime/workspace_session               # everything except env-gated
E2E_RETENTION=1 pytest runtime/workspace_session -k EX_08
```

Smoke tier must stay under ~60 s wall; medium under ~5 min; EX-08 and FP-04
are opt-in. All cases go through `sandbox-cli` structured JSON only — no log
scraping, per the suite charter.

## Current Proof

Final verification date: 2026-07-03.

Prerequisites and Phase 0 gates:

| Gate | Result |
| --- | --- |
| Docker prerequisite | Docker `29.5.2` available. |
| Requirements | Installed in `/tmp/eos-cli-operation-e2e-venv` because system Python rejected global install with PEP 668. |
| Gateway cold start | `bin/start-sandbox-docker-gateway --rebuild-binary` rebuilt `sandbox-daemon-linux-arm64` and restarted the gateway. |
| Rebuild smoke | `E2E_REBUILD_BINARY=1 pytest -m smoke`: `11 passed, 170 deselected in 18.45s`. |
| Baseline runtime before new family | `pytest runtime`: `120 passed, 1 skipped in 239.13s`. |
| Baseline manager | `pytest manager`: `58 passed in 218.76s`. |
| Phase 0 transient audit | One post-edit `pytest runtime` hit a runtime/file/correctness cluster; `pytest --lf` passed `12 passed, 1 deselected in 19.09s`, then both full reruns below passed. No assertion was weakened. |
| Phase 0 runtime gate 1 | `pytest runtime --log-cli-level=WARNING`: `120 passed, 1 skipped in 230.79s`. |
| Phase 0 runtime gate 2 | `pytest runtime --log-cli-level=WARNING`: `120 passed, 1 skipped in 234.15s`. |

Workspace-session proof runs:

| Run id | Command | Pytest result |
| --- | --- | --- |
| `workspace-session-20260703-063439` | `pytest runtime/workspace_session -m smoke --log-cli-level=WARNING` | `6 passed, 12 deselected in 7.37s` |
| `workspace-session-20260703-063450` | `pytest runtime/workspace_session --log-cli-level=WARNING` | `16 passed, 2 skipped in 35.46s` |
| `workspace-session-20260703-063529` | `pytest runtime/workspace_session --log-cli-level=WARNING` | `16 passed, 2 skipped in 36.18s` |
| `workspace-session-20260703-063228` | `E2E_RETENTION=1 pytest runtime/workspace_session -k EX_08 --log-cli-level=WARNING` | `1 passed, 17 deselected in 47.07s` |
| `workspace-session-20260703-063323` | `E2E_STORM=1 pytest runtime/workspace_session -k FP_04 --log-cli-level=WARNING` | `1 passed, 17 deselected in 61.94s` |

Post-family gates:

| Gate | Result |
| --- | --- |
| Full runtime tree | `pytest runtime --log-cli-level=WARNING`: `136 passed, 3 skipped in 273.46s (0:04:33)`. |
| Global smoke | `pytest -m smoke --log-cli-level=WARNING`: `17 passed, 182 deselected in 24.44s`. |
| Gateway-log grep | No daemon log file path references under `runtime/workspace_session`. |

Allowed skips:

| Scope | Case | Reason |
| --- | --- | --- |
| Normal workspace-session family | EX-08 | `E2E_RETENTION=1` not set. |
| Normal workspace-session family | FP-04 | `E2E_STORM=1` not set. |
| Full runtime tree | layer-depth benchmark | `E2E_EXEC_BENCH=1` not set. |

Per-case evidence:

| Case | Status | Pytest node id | Verdict |
| --- | --- | --- | --- |
| WS-01 | PASS | `runtime/workspace_session/test_workspace_session.py::test_WS_01_create_response_contract` | `runtime/workspace_session/test-reports/workspace-session-20260703-063529/WS-01/verdict.json` |
| WS-02 | PASS | `runtime/workspace_session/test_workspace_session.py::test_WS_02_no_op_session_survives_command_completion` | `runtime/workspace_session/test-reports/workspace-session-20260703-063529/WS-02/verdict.json` |
| WS-03 | PASS | `runtime/workspace_session/test_workspace_session.py::test_WS_03_destroy_refuses_while_command_runs` | `runtime/workspace_session/test-reports/workspace-session-20260703-063529/WS-03/verdict.json` |
| WS-04 | PASS | `runtime/workspace_session/test_workspace_session.py::test_WS_04_destroy_discards_and_sync_op_loses_cleanly` | `runtime/workspace_session/test-reports/workspace-session-20260703-063529/WS-04/verdict.json` |
| WS-05 | PASS | `runtime/workspace_session/test_workspace_session.py::test_WS_05_no_finalize_policy_flag_exists` | `runtime/workspace_session/test-reports/workspace-session-20260703-063529/WS-05/verdict.json` |
| WS-06 | PASS | `runtime/workspace_session/test_workspace_session.py::test_WS_06_destroyed_id_stays_dead` | `runtime/workspace_session/test-reports/workspace-session-20260703-063529/WS-06/verdict.json` |
| EX-01 | PASS | `runtime/workspace_session/test_exec_finalize.py::test_EX_01_implicit_exec_response_contract` | `runtime/workspace_session/test-reports/workspace-session-20260703-063529/EX-01/verdict.json` |
| EX-02 | PASS | `runtime/workspace_session/test_exec_finalize.py::test_EX_02_implicit_exec_publishes_then_destroys` | `runtime/workspace_session/test-reports/workspace-session-20260703-063529/EX-02/verdict.json` |
| EX-03 | PASS | `runtime/workspace_session/test_exec_finalize.py::test_EX_03_session_exec_carries_the_session_id` | `runtime/workspace_session/test-reports/workspace-session-20260703-063529/EX-03/verdict.json` |
| EX-04 | PASS | `runtime/workspace_session/test_exec_finalize.py::test_EX_04_rider_defers_finalization` | `runtime/workspace_session/test-reports/workspace-session-20260703-063529/EX-04/verdict.json` |
| EX-05 | PASS | `runtime/workspace_session/test_exec_finalize.py::test_EX_05_publish_rejection_surfaces_on_terminal_response` | `runtime/workspace_session/test-reports/workspace-session-20260703-063529/EX-05/verdict.json` |
| EX-06 | PASS | `runtime/workspace_session/test_exec_finalize.py::test_EX_06_file_op_racing_last_completion_gets_not_found` | `runtime/workspace_session/test-reports/workspace-session-20260703-063529/EX-06/verdict.json` |
| EX-07 | PASS | `runtime/workspace_session/test_exec_finalize.py::test_EX_07_interrupt_and_timeout_paths_still_finalize` | `runtime/workspace_session/test-reports/workspace-session-20260703-063529/EX-07/verdict.json` |
| EX-08 | PASS | `runtime/workspace_session/test_exec_finalize.py::test_EX_08_drain_retention_cap` | `runtime/workspace_session/test-reports/workspace-session-20260703-063228/EX-08/verdict.json` |
| FP-01 | PASS | `runtime/workspace_session/test_exec_finalize.py::test_FP_01_remount_sweep_cannot_finalize_idle_implicit_session` | `runtime/workspace_session/test-reports/workspace-session-20260703-063529/FP-01/verdict.json` |
| FP-02 | PASS | `runtime/workspace_session/test_exec_finalize.py::test_FP_02_empty_capture_skips_publish` | `runtime/workspace_session/test-reports/workspace-session-20260703-063529/FP-02/verdict.json` |
| FP-03 | PASS | `runtime/workspace_session/test_exec_finalize.py::test_FP_03_back_to_back_implicit_execs_are_independent` | `runtime/workspace_session/test-reports/workspace-session-20260703-063529/FP-03/verdict.json` |
| FP-04 | PASS | `runtime/workspace_session/test_exec_finalize.py::test_FP_04_finalize_vs_destroy_interleave_storm` | `runtime/workspace_session/test-reports/workspace-session-20260703-063323/FP-04/verdict.json` |
