# LayerStack Squash Live-Docker Summary

- Run id: `squash-20260703-055850`
- Generated: `2026-07-03T05:59:08+08:00`
- Pytest exit status: `atexit`
- Cases: `11` run · `11` pass · `0` slow · `0` fail · `0` skipped

| Case | Tier | Status | Correctness | Space | Time | Notes |
| --- | --- | --- | --- | --- | --- | --- |
| `PRECONDITIONS` | preconditions | PASS | pass | pass | pass |  |
| `SMK-01` | smoke | PASS | pass | pass | pass |  |
| `SMK-02` | smoke | PASS | pass | pass | pass |  |
| `SMK-03` | smoke | PASS | pass | pass | pass |  |
| `SMK-04` | smoke | PASS | pass | pass | pass |  |
| `SMK-05` | smoke | PASS | pass | n/a | pass |  |
| `SMK-06` | smoke | PASS | pass | pass | pass |  |
| `SMK-07` | smoke | PASS | pass | pass | pass |  |
| `SMK-08` | smoke | PASS | pass | pass | pass |  |
| `SMK-09` | smoke | PASS | pass | pass | pass |  |
| `SMK-10` | smoke | PASS | pass | pass | pass |  |

## Timing Distributions

| Timer | Count | P50 ms | P95 ms | Max ms |
| --- | ---: | ---: | ---: | ---: |
| `T_e2e` | 11 | 1271.419 | 2016.791 | 2471.974 |
| `T_quiesce` | 2 | 0.000 | 0.000 | 0.000 |
| `T_remount` | 2 | 1.000 | 1.000 | 1.000 |
| `T_squash` | 10 | 31.385 | 38.409 | 38.795 |
| `T_squash_invocation_1` | 8 | 30.230 | 38.495 | 38.795 |
| `T_squash_invocation_2` | 4 | 29.693 | 34.370 | 34.716 |
| `T_squash_invocation_3` | 1 | 23.839 | 23.839 | 23.839 |
