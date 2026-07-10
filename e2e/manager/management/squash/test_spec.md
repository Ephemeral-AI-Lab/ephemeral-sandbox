# LayerStack Squash Live-Docker Catalog Mirror

Source of truth: `docs/obsidian/ephemeral-os/implementation_plan/squash/test-case.md`.

This suite mirrors all catalog IDs under `manager/management/squash/` and writes
`test-reports/<RUN_ID>/<CASE_ID>/verdict.json` for every executed case.

| Case | Tier | Title |
| --- | --- | --- |
| SMK-01 | smoke | idle block reclaims at commit (B1) |
| SMK-02 | smoke | nothing to squash is a clean no-op |
| SMK-03 | smoke | singleton run below a boundary is never touched |
| SMK-04 | smoke | result contract shape, success and fault |
| SMK-05 | smoke | CLI catalog placement |
| SMK-06 | smoke | idle session migrates via plain staged switch (B2) |
| SMK-07 | smoke | interactive PTY shell blocks cleanly (leased) |
| SMK-08 | smoke | the three observability records, and only those |
| SMK-09 | smoke | immediate idempotence |
| SMK-10 | smoke | daemon restart: boot reap-then-sweep on a healthy stack is a no-op sweep |
| MED-01 | medium | whiteout winners: re-emitted only when masking, dropped when net-nothing |
| MED-02 | medium | opaque dir is dual-encoded and never resurrects |
| MED-03 | medium | witness matrix: dir-created-then-emptied, modes, shadowed subtrees |
| MED-04 | medium | hardlink flatten is metadata-bound and byte-neutral at peak |
| MED-05 | medium | racing publishes: run-presence recheck, tail preserved, no starvation |
| MED-06 | medium | singleflight per root under concurrent invocations |
| MED-07 | medium | lease acquired between plan and commit keeps sources |
| MED-08 | medium | live migration under a running batch command (E5) |
| MED-09 | medium | escaped-pgid child with an fd pin blocks, then converges (E1) |
| MED-10 | medium | child mount pins even after its creator exited |
| MED-11 | medium | nested mount namespace blocks remount (E2) |
| MED-12 | medium | strict-unmount EBUSY parks with both leases |
| MED-13 | medium | post-PONR runner death is faulty |
| MED-14 | medium | daemon crash between promote and manifest rename |
| MED-15 | medium | boot reap-then-sweep with planted orphans and live sessions |
| MED-16 | medium | sidecar hygiene |
| MED-17 | medium | persist failure still migrates |
| MED-18 | medium | OVL_MAX_STACK creation boundary |
| MED-19 | medium | masks never observable |
| MED-20 | medium | quiesce at 100 tasks |
| HTTP-01 | medium | running HTTP server migrates live |
| HTTP-02 | medium | workspace-cwd HTTP server resumes leased |
| LOAD-499 | hard | 499-layer stack squashes in-cap |
| LOAD-LARGE | hard | large file squash |
| LOAD-499-HTTP | hard | 499-layer stack with HTTP disconnect |
| LOAD-LARGE-HTTP | hard | large file squash with HTTP disconnect |
| LOAD-COMBO-HTTP | hard | multi-block active workspace HTTP load |
| HRD-01 | hard | B3 replay: multi-block plan, mixed classification, reclaim cascade |
| HRD-02 | hard | B4 replay: two generations, re-squash of S |
| HRD-03 | hard | B5 replay: every hard path in one sweep |
| HRD-04 | hard | E4 full pin matrix |
| HRD-05 | hard | E8 PONR boundary |
| HRD-06 | hard | E10 crash matrix |
| HRD-07 | hard | EBUSY park convergence |
| HRD-08 | hard | admission-gate storm |
| HRD-09 | hard | one-shot finalize/timeout hook firing mid-switch |
| HRD-10 | hard | dense-pinning adversarial floor |
| HRD-11 | hard | deep chain: 200-layer churn collapses to 3 lowerdirs live |
| HRD-12 | hard | E9: over-cap chains fail closed at the mount syscall |
| HRD-13 | hard | commit durability cost |
| HRD-14 | hard | re-squash across 5 generations |
| HRD-15 | hard | sweep at k=8 |
| HRD-16 | hard | ENOSPC on both sides of the commit boundary |
| HRD-17 | hard | G1 kernel gate |
| HRD-18 | hard | G2 parity with the negative control |
| HRD-19 | hard | mid-sweep daemon kill at k=6 |
| HRD-20 | hard | soak marathon: 20 randomized iterations |

Allowed skips, matching section 5.3:

| Case | Allowed reason |
| --- | --- |
| HRD-04 | subcases-9-11 |
| HRD-12 | leg-b:not_constructible_at_ci_scale |
| HRD-17 | failure-leg:gate_green_env |

## Measurement Kit

Each case creates one live Docker sandbox, drives `sandbox-manager-cli
squash_layerstacks` through structured JSON, records S0-S3 disk snapshots where
applicable, captures `T_squash`, `T_quiesce`, `T_remount`, and `T_e2e` timers,
checks correctness/space/time axes, and writes teardown evidence for an empty
lease registry, no `.remount-*` residue, empty `staging/`, and strict unmount
cleanup. The suite writes `SUMMARY.md`, `timing-distribution.json`, and the
HRD-20 `soak-baseline.json` under `test-reports/<RUN_ID>/`.

## Current Proof

Final consecutive full runs on 2026-07-03:

| Run id | Pytest result | Summary |
| --- | --- | --- |
| `squash-20260703-031940` | `50 passed in 129.14s` | `51` run, `51` pass, `0` slow, `0` fail, `0` skipped |
| `squash-20260703-032157` | `50 passed in 128.64s` | `51` run, `51` pass, `0` slow, `0` fail, `0` skipped |

Final verdict audit: 51 `verdict.json` files, no failed correctness, space,
time, or teardown axes. Allowed partial notes only: HRD-12 leg b and HRD-17
failure leg, both as catalog section 5.3 permits.

Speed note: the post-rebuild baseline full run was
`squash-20260703-030503` at `50 passed in 246.54s`; the optimized harness
finishes at `128.64s` by using structured `file_write` for small publishes,
concurrent synthetic layer publishing, and one-shot command log artifact
writes.

## Extra HTTP Service Cases

`HTTP-01` and `HTTP-02` are additional medium-tier probes outside the original
50-case source catalog. Both start the service and client through
`sandbox-cli runtime exec_command`. `T_http_disconnect` is the largest observed
silent gap on a persistent `/ticks` HTTP stream that emits numbered lines every
1ms. The cases enforce the same correctness, space, time, and teardown verdict
axes.

## Extra Load Cases

`LOAD-499` publishes 499 tiny layers, proves the stack can be mounted, squashes
it, and verifies the first and last files remain readable. `LOAD-LARGE`
publishes one configurable large blob (`SQUASH_LARGE_FILE_KIB`, default 8MiB)
between small layers and verifies the byte count after squash.

`LOAD-499-HTTP` keeps the 499 tiny data-layer fixture, runs the HTTP helper
from `/run` during squash, and records `T_http_disconnect` as the maximum
observed stream silence. `LOAD-LARGE-HTTP` does the same around the large-file
fixture.

`LOAD-COMBO-HTTP` combines repeated small-file publishes, small overwrites,
real 2MiB large overwrites written with `dd`, 200 active workspace sessions,
background commands, four HTTP servers, and three squash rounds. Environment
knobs with the `SQUASH_COMBO_` prefix can scale those counts up to the bounded
fixture limits.
