# Phase 8 Implementation Report - Lowerdir as Snapshot

## Summary

Phase 8 replaced the git-tree snapshot mechanism with the overlay lowerdir as the command-start base.

The shipped model is:

- `setup_mounts(...)` binds the live workspace to `_NS_LOWER`.
- The user command writes only through the merged overlay, so file updates land in `_NS_UPPER`.
- The classifier reads base bytes from `_NS_LOWER/<rel>` and final bytes from `_NS_UPPER/<rel>`.
- Gitignore remains only a routing rule for upperdir entries: gitinclude paths go through OCC, gitignored paths direct-merge.
- The daemon carries base bytes in `diff.ndjson`; it does not need to read `_NS_LOWER` after the unshare process exits.

## Code Changes

| Area | Change |
|---|---|
| Snapshot construction | Deleted `backend/src/sandbox/code_intelligence/overlay/git_snapshot.py` and removed the auditor-side `git_snapshot` stage. |
| Runner CLI | Removed `--snap`; the runner no longer accepts or emits a snapshot identifier. |
| Base reads | Replaced `git show <snap>:path` with `lowerdir_base_factory(lower_root=_NS_LOWER)`, which reads base bytes from the filesystem. |
| Module cleanup | Removed the legacy `git_adapters.py` boundary; lowerdir reads now live in `lowerdir.py`, and gitignore routing lives in `gitignore.py`. |
| Envelope cleanup | Removed the legacy `snap`, `snapshot_timings`, and `git_snapshot_timings` fields from the active overlay/daemon command result path. |
| Classifier | Renamed the base-read dependency from `git_show_base` to `read_base`. |
| Gitignored base visibility | Lowerdir reads include gitignored content such as `.venv/pyvenv.cfg`; the snapshot layer no longer filters by `.gitignore`. |
| Git routing preflight | Added a cheap lowerdir `.git` metadata check before the user command runs, preserving fail-closed behavior for non-git workspaces without rebuilding a snapshot. |
| Freshness guard | Added a daemon-local idle-window fingerprint guard over `workspace_root`, `.git/index`, and `.git/HEAD`. It fails closed if the workspace shifts between overlay runs outside the daemon path. |
| Docs | Updated the daemon overview so `git` is described as the `git check-ignore` dependency, not a snapshot construction dependency. |

## Phase 0-8 Code Review Follow-up

The post-phase review found one implementation gap and one cleanup seam:

- `SymbolIndex` mirrored writes into `IndexStore` but did not hydrate from
  `index.sqlite3` on daemon restart. The restart-survival guarantee now holds
  without a full rebuild: a new `SymbolIndex(..., persistence=IndexStore(...))`
  loads persisted rows into memory and marks the index ready.
- The active daemon path no longer needs the Phase 1 standalone
  index runner, the orchestrator-side remote snapshot downloader, or
  the public pickle `write_snapshot/read_snapshot` API. Those paths were
  removed; only a private pickle reader remains for the one-shot
  `index.snapshot` to `index.sqlite3` migration.
- Phase 1 live E2E assertions were updated from retired `_cached_*` /
  `index.snapshot` fields to current daemon status and `index.sqlite3`
  recovery.
- The legacy `attribute_changes=False` ambient-write bypass was removed
  from the active overlay commit path. The argument remains accepted for
  compatibility, but gitinclude writes now always go through OCC and
  gitignored writes keep using direct merge.

## Probe Conclusions

Task 8.0.B/C resolved to the in-namespace mechanism:

- OCC base reads happen in `overlay_runtime/runner.py` after the user command and before `diff.ndjson` is written.
- Those base reads run inside the same unshare process that can see `_NS_LOWER`.
- The daemon-side commit step consumes serialized `base_content` from `diff.ndjson`.
- Therefore Phase 8 does not require a daemon-side persistent bind mount or CoW filesystem snapshot for the current per-command OCC contract.

Task 8.0.A did not block implementation because the selected mechanism does not rely on reflinks, btrfs subvolumes, or another kernel CoW primitive.

## Verification

Commands run:

```bash
.venv/bin/python -m compileall -q backend/src/sandbox/code_intelligence/overlay backend/tests/test_sandbox/test_code_intelligence/test_overlay_run.py backend/tests/test_sandbox/test_code_intelligence/test_overlay_auditor.py backend/tests/test_sandbox/test_code_intelligence/test_service_cmd.py
.venv/bin/pytest backend/tests/test_sandbox/test_code_intelligence/test_overlay_run.py backend/tests/test_sandbox/test_code_intelligence/test_overlay_auditor.py backend/tests/test_sandbox/test_code_intelligence/test_overlay_daemon_local_parity.py backend/tests/test_sandbox/test_code_intelligence/test_service_cmd.py -q
.venv/bin/ruff check backend/src/sandbox/code_intelligence/overlay backend/tests/test_sandbox/test_code_intelligence/test_overlay_run.py backend/tests/test_sandbox/test_code_intelligence/test_overlay_auditor.py backend/tests/test_sandbox/test_code_intelligence/test_overlay_daemon_local_parity.py backend/tests/test_sandbox/test_code_intelligence/test_service_cmd.py backend/tests/test_e2e/test_live_daytona_opaque_dir_overlay.py
.venv/bin/pytest backend/tests/test_e2e/test_live_ci_phase6_svc_cmd_fold.py -m live -v -s
```

Results:

| Check | Result |
|---|---|
| Compile | PASS |
| Focused unit tests | PASS, 84 passed |
| Ruff | PASS |
| Live Phase 6 performance E2E | PASS, 1 passed |

## Performance Evidence

Live timing artifact:

`backend/tests/test_e2e/_timings/phase_6_svc_cmd_fold_concurrency_1_5_10_2026-05-02T20-58-46Z.json`

Key results:

| Metric | Result |
|---|---:|
| `daemon_local_unshare_subprocess_count` | 16 |
| `daemon_local_git_snapshot_stage_count` | 0 |
| 1x `svc_cmd` latency p50 | 6.257s |
| 5x `svc_cmd` latency p50 | 1.390s |
| 10x `svc_cmd` latency p50 | 1.472s |
| 10x `svc_cmd` latency p95 | 1.506s |
| 10x `svc_cmd` latency p99 | 1.506s |
| 10x committed ops | 10 |
| 10x errors | 0 |
| RSS growth | 0.75 MB |
| FD growth | 0 |

Compared with the existing Phase 6 timing artifact
`phase_6_svc_cmd_fold_concurrency_1_5_10_2026-05-02T20-36-34Z.json`, the 10x p50 moved from 1.926s to 1.472s. The old `svc_cmd_10x_git_snapshot_total` p50 was 0.246s; the new run has no `svc_cmd_10x_git_snapshot_total` distribution because the snapshot stage is gone.

## Residual Risks

- The freshness guard is intentionally cheap: it detects idle-window changes to `workspace_root`, `.git/index`, and `.git/HEAD`, but it is not a full tree hash.
- Historical replay of older lowerdir states is still out of scope. Phase 8 only fixes the current command's base-read semantics.
- Historical timing artifacts and older phase docs still contain git snapshot terminology; the active overlay/daemon command result path no longer emits snapshot timing fields.
