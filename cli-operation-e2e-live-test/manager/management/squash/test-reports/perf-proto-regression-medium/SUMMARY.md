# LayerStack Squash Live-Docker Summary

- Run id: `perf-proto-regression-medium`
- Generated: `2026-07-03T06:18:05+08:00`
- Pytest exit status: `0`
- Cases: `9` run · `9` pass · `0` slow · `0` fail · `0` skipped

| Case | Tier | Status | Correctness | Space | Time | Notes |
| --- | --- | --- | --- | --- | --- | --- |
| `MED-08` | medium | PASS | pass | pass | pass |  |
| `MED-09` | medium | PASS | pass | pass | pass |  |
| `MED-10` | medium | PASS | pass | pass | pass |  |
| `MED-11` | medium | PASS | pass | pass | pass |  |
| `MED-13` | medium | PASS | pass | pass | pass |  |
| `MED-17` | medium | PASS | pass | pass | pass |  |
| `MED-19` | medium | PASS | pass | pass | pass |  |
| `MED-20` | medium | PASS | pass | pass | pass |  |
| `PRECONDITIONS` | preconditions | PASS | pass | pass | pass |  |

## Timing Distributions

| Timer | Count | P50 ms | P95 ms | Max ms |
| --- | ---: | ---: | ---: | ---: |
| `T_e2e` | 9 | 1467.065 | 2215.253 | 2572.063 |
| `T_quiesce` | 3 | 0.000 | 0.900 | 1.000 |
| `T_remount` | 2 | 1.000 | 1.000 | 1.000 |
| `T_squash` | 8 | 40.758 | 65.941 | 66.921 |
| `T_squash_invocation_1` | 8 | 40.758 | 65.941 | 66.921 |
| `T_squash_invocation_2` | 2 | 33.808 | 34.704 | 34.803 |
