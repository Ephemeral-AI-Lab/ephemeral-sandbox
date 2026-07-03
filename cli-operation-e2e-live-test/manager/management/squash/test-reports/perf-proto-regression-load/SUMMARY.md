# LayerStack Squash Live-Docker Summary

- Run id: `perf-proto-regression-load`
- Generated: `2026-07-03T06:20:19+08:00`
- Pytest exit status: `0`
- Cases: `3` run · `3` pass · `0` slow · `0` fail · `0` skipped

| Case | Tier | Status | Correctness | Space | Time | Notes |
| --- | --- | --- | --- | --- | --- | --- |
| `LOAD-499-HTTP` | hard | PASS | pass | pass | pass |  |
| `LOAD-LARGE-HTTP` | hard | PASS | pass | pass | pass |  |
| `PRECONDITIONS` | preconditions | PASS | pass | pass | pass |  |

## Timing Distributions

| Timer | Count | P50 ms | P95 ms | Max ms |
| --- | ---: | ---: | ---: | ---: |
| `T_e2e` | 3 | 5494.814 | 16094.543 | 17272.291 |
| `T_http_disconnect` | 2 | 10.596 | 12.182 | 12.358 |
| `T_squash` | 2 | 122.081 | 190.749 | 198.379 |
| `T_squash_invocation_1` | 2 | 122.081 | 190.749 | 198.379 |
