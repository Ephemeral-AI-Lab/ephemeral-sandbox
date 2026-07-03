# LayerStack Squash Live-Docker Summary

- Run id: `squash-20260703-060441`
- Generated: `2026-07-03T06:08:19+08:00`
- Pytest exit status: `atexit`
- Cases: `58` run · `58` pass · `0` slow · `0` fail · `0` skipped

| Case | Tier | Status | Correctness | Space | Time | Notes |
| --- | --- | --- | --- | --- | --- | --- |
| `HRD-01` | hard | PASS | pass | pass | pass |  |
| `HRD-02` | hard | PASS | pass | pass | pass |  |
| `HRD-03` | hard | PASS | pass | pass | pass |  |
| `HRD-04` | hard | PASS | pass | pass | pass |  |
| `HRD-05` | hard | PASS | pass | pass | pass |  |
| `HRD-06` | hard | PASS | pass | pass | pass |  |
| `HRD-07` | hard | PASS | pass | pass | pass |  |
| `HRD-08` | hard | PASS | pass | pass | pass |  |
| `HRD-09` | hard | PASS | pass | pass | pass |  |
| `HRD-10` | hard | PASS | pass | pass | pass |  |
| `HRD-11` | hard | PASS | pass | pass | pass |  |
| `HRD-12` | hard | PASS | pass | pass | pass | `skipped:leg-b:not_constructible_at_ci_scale` |
| `HRD-13` | hard | PASS | pass | pass | pass |  |
| `HRD-14` | hard | PASS | pass | pass | pass |  |
| `HRD-15` | hard | PASS | pass | pass | pass |  |
| `HRD-16` | hard | PASS | pass | pass | pass |  |
| `HRD-17` | hard | PASS | pass | pass | pass | `skipped:failure-leg:gate_green_env is allowed by §5.3 and unit-gated` |
| `HRD-18` | hard | PASS | pass | pass | pass |  |
| `HRD-19` | hard | PASS | pass | pass | pass |  |
| `HRD-20` | hard | PASS | pass | pass | pass |  |
| `HTTP-01` | medium | PASS | pass | pass | pass |  |
| `HTTP-02` | medium | PASS | pass | pass | pass |  |
| `LOAD-499` | hard | PASS | pass | pass | pass |  |
| `LOAD-499-HTTP` | hard | PASS | pass | pass | pass |  |
| `LOAD-COMBO-HTTP` | hard | PASS | pass | pass | pass |  |
| `LOAD-LARGE` | hard | PASS | pass | pass | pass |  |
| `LOAD-LARGE-HTTP` | hard | PASS | pass | pass | pass |  |
| `MED-01` | medium | PASS | pass | pass | pass |  |
| `MED-02` | medium | PASS | pass | pass | pass |  |
| `MED-03` | medium | PASS | pass | pass | pass |  |
| `MED-04` | medium | PASS | pass | pass | pass |  |
| `MED-05` | medium | PASS | pass | pass | pass |  |
| `MED-06` | medium | PASS | pass | pass | pass |  |
| `MED-07` | medium | PASS | pass | pass | pass |  |
| `MED-08` | medium | PASS | pass | pass | pass |  |
| `MED-09` | medium | PASS | pass | pass | pass |  |
| `MED-10` | medium | PASS | pass | pass | pass |  |
| `MED-11` | medium | PASS | pass | pass | pass |  |
| `MED-12` | medium | PASS | pass | pass | pass |  |
| `MED-13` | medium | PASS | pass | pass | pass |  |
| `MED-14` | medium | PASS | pass | pass | pass |  |
| `MED-15` | medium | PASS | pass | pass | pass |  |
| `MED-16` | medium | PASS | pass | pass | pass |  |
| `MED-17` | medium | PASS | pass | pass | pass |  |
| `MED-18` | medium | PASS | pass | pass | pass |  |
| `MED-19` | medium | PASS | pass | pass | pass |  |
| `MED-20` | medium | PASS | pass | pass | pass |  |
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

## Allowed Partial Skips

- `HRD-12`: `skipped:leg-b:not_constructible_at_ci_scale`
- `HRD-17`: `skipped:failure-leg:gate_green_env is allowed by §5.3 and unit-gated`

## Timing Distributions

| Timer | Count | P50 ms | P95 ms | Max ms |
| --- | ---: | ---: | ---: | ---: |
| `T_e2e` | 58 | 1596.142 | 10424.598 | 44382.383 |
| `T_http_disconnect` | 5 | 17.896 | 18.978 | 19.053 |
| `T_quiesce` | 7 | 0.000 | 0.700 | 1.000 |
| `T_remount` | 6 | 1.000 | 1.000 | 1.000 |
| `T_squash` | 57 | 40.622 | 167.088 | 1335.972 |
| `T_squash_invocation_1` | 54 | 39.981 | 161.161 | 657.946 |
| `T_squash_invocation_10` | 1 | 38.957 | 38.957 | 38.957 |
| `T_squash_invocation_11` | 1 | 40.562 | 40.562 | 40.562 |
| `T_squash_invocation_12` | 1 | 30.972 | 30.972 | 30.972 |
| `T_squash_invocation_13` | 1 | 45.047 | 45.047 | 45.047 |
| `T_squash_invocation_14` | 1 | 35.439 | 35.439 | 35.439 |
| `T_squash_invocation_15` | 1 | 35.601 | 35.601 | 35.601 |
| `T_squash_invocation_16` | 1 | 48.509 | 48.509 | 48.509 |
| `T_squash_invocation_17` | 1 | 37.206 | 37.206 | 37.206 |
| `T_squash_invocation_18` | 1 | 32.056 | 32.056 | 32.056 |
| `T_squash_invocation_19` | 1 | 53.673 | 53.673 | 53.673 |
| `T_squash_invocation_2` | 19 | 34.247 | 119.270 | 787.777 |
| `T_squash_invocation_20` | 1 | 30.304 | 30.304 | 30.304 |
| `T_squash_invocation_21` | 1 | 28.190 | 28.190 | 28.190 |
| `T_squash_invocation_3` | 5 | 44.312 | 734.747 | 906.390 |
| `T_squash_invocation_4` | 4 | 40.007 | 42.179 | 42.347 |
| `T_squash_invocation_5` | 3 | 40.595 | 46.491 | 47.146 |
| `T_squash_invocation_6` | 3 | 32.943 | 37.565 | 38.079 |
| `T_squash_invocation_7` | 1 | 44.475 | 44.475 | 44.475 |
| `T_squash_invocation_8` | 1 | 32.863 | 32.863 | 32.863 |
| `T_squash_invocation_9` | 1 | 50.328 | 50.328 | 50.328 |
