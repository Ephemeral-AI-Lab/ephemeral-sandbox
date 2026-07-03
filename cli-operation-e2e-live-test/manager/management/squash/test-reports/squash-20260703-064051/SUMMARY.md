# LayerStack Squash Live-Docker Summary

- Run id: `squash-20260703-064051`
- Generated: `2026-07-03T06:41:15+08:00`
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
| `T_e2e` | 11 | 1298.457 | 2076.860 | 2514.826 |
| `T_quiesce` | 2 | 0.000 | 0.000 | 0.000 |
| `T_remount` | 2 | 1.000 | 1.000 | 1.000 |
| `T_squash` | 10 | 32.632 | 42.924 | 43.078 |
| `T_squash_invocation_1` | 8 | 33.359 | 42.958 | 43.078 |
| `T_squash_invocation_2` | 4 | 30.227 | 34.406 | 34.847 |
| `T_squash_invocation_3` | 1 | 25.642 | 25.642 | 25.642 |
