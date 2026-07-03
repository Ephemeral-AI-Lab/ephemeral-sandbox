# LayerStack Squash Live-Docker Summary

- Run id: `perf-proto-regression-smoke`
- Generated: `2026-07-03T06:16:53+08:00`
- Pytest exit status: `0`
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
| `T_e2e` | 11 | 1198.400 | 2061.079 | 2475.405 |
| `T_quiesce` | 2 | 0.000 | 0.000 | 0.000 |
| `T_remount` | 2 | 1.000 | 1.000 | 1.000 |
| `T_squash` | 10 | 32.783 | 49.850 | 56.947 |
| `T_squash_invocation_1` | 8 | 35.895 | 51.427 | 56.947 |
| `T_squash_invocation_2` | 4 | 28.444 | 32.771 | 33.406 |
| `T_squash_invocation_3` | 1 | 27.929 | 27.929 | 27.929 |
