# LayerStack Squash Live-Docker Summary

- Run id: `perf-20260703-052525-proto-s200`
- Generated: `2026-07-03T06:15:00+08:00`
- Pytest exit status: `0`
- Cases: `2` run · `2` pass · `0` slow · `0` fail · `0` skipped

| Case | Tier | Status | Correctness | Space | Time | Notes |
| --- | --- | --- | --- | --- | --- | --- |
| `LOAD-COMBO-HTTP` | hard | PASS | pass | pass | pass |  |
| `PRECONDITIONS` | preconditions | PASS | pass | pass | pass |  |

## Timing Distributions

| Timer | Count | P50 ms | P95 ms | Max ms |
| --- | ---: | ---: | ---: | ---: |
| `T_e2e` | 2 | 22125.833 | 40952.034 | 43043.834 |
| `T_http_disconnect` | 1 | 31.058 | 31.058 | 31.058 |
| `T_squash` | 1 | 311.389 | 311.389 | 311.389 |
| `T_squash_invocation_1` | 1 | 210.164 | 210.164 | 210.164 |
| `T_squash_invocation_2` | 1 | 236.336 | 236.336 | 236.336 |
| `T_squash_invocation_3` | 1 | 311.389 | 311.389 | 311.389 |
| `T_squash_invocation_4` | 1 | 37.058 | 37.058 | 37.058 |
