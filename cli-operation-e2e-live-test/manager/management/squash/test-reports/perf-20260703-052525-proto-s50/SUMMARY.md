# LayerStack Squash Live-Docker Summary

- Run id: `perf-20260703-052525-proto-s50`
- Generated: `2026-07-03T06:11:10+08:00`
- Pytest exit status: `0`
- Cases: `2` run · `2` pass · `0` slow · `0` fail · `0` skipped

| Case | Tier | Status | Correctness | Space | Time | Notes |
| --- | --- | --- | --- | --- | --- | --- |
| `LOAD-COMBO-HTTP` | hard | PASS | pass | pass | pass |  |
| `PRECONDITIONS` | preconditions | PASS | pass | pass | pass |  |

## Timing Distributions

| Timer | Count | P50 ms | P95 ms | Max ms |
| --- | ---: | ---: | ---: | ---: |
| `T_e2e` | 2 | 15752.969 | 28907.741 | 30369.382 |
| `T_http_disconnect` | 1 | 12.025 | 12.025 | 12.025 |
| `T_squash` | 1 | 106.827 | 106.827 | 106.827 |
| `T_squash_invocation_1` | 1 | 106.827 | 106.827 | 106.827 |
| `T_squash_invocation_2` | 1 | 86.856 | 86.856 | 86.856 |
| `T_squash_invocation_3` | 1 | 104.121 | 104.121 | 104.121 |
| `T_squash_invocation_4` | 1 | 37.854 | 37.854 | 37.854 |
