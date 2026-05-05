# Sandbox API Load-Test Performance Experimental Report

## Summary

This experiment evaluated the performance of the migrated sandbox API path after
wiring public `sandbox.api` methods to the per-call snapshot layer stack, overlay
runtime, and OCC commit pipeline. The initial load suite exposed a severe shell
latency problem: at 50 concurrent shell operations, the shell API spent multiple
seconds waiting for async executor continuations to resume, even though the
actual shell process execution was fast.

The optimized implementation removed direct `asyncio.to_thread` dispatch from the
sandbox hot path, collapsed shell overlay + OCC application into one executor-side
transaction, added an OCC serial merger for disjoint prepared changesets, and
reduced benchmark logging overhead. The full load suite improved from roughly
27.68 seconds to 5.01 seconds.

At 50 concurrent operations, the final p95 latencies are:

| Workload | Before p95 | After p95 | Result |
|---|---:|---:|---:|
| read | 690.4 ms | 84.7 ms | 8.2x faster |
| write | 572.4 ms | 54.1 ms | 10.6x faster |
| edit | 960.4 ms | 103.8 ms | 9.3x faster |
| shell | 7393.5 ms | 1576.2 ms | 4.7x faster |
| mixed edit/write/shell | 918.5 ms | 56.2 ms | 16.3x faster |
| serial prepared writes | 456.1 ms | 50.8 ms | 9.0x faster |

## Experiment Scope

The suite covers concurrency levels `1, 5, 10, 20, 50` for:

- `read`
- `write`
- `edit`
- `shell`
- mixed edit/write/shell, with total operation count equal to the concurrency
  level
- write conflict detection
- edit conflict detection
- shell concurrent-update conflict detection
- layer-stack squash verification
- OCC serial merger correctness for concurrent prepared writes

This report focuses on the c=50 performance because that is where the bottleneck
was visible.

## Test Commands

Optimized full run:

```bash
PYTHONUNBUFFERED=1 uv run pytest backend/tests/test_sandbox/test_api/test_load.py -q \
  --log-file=/tmp/eos_load_full_optimized_2026-05-04.log \
  --log-file-level=INFO \
  --log-file-format='%(message)s'
```

Result:

```text
10 passed in 5.01s
```

Additional verification:

```text
backend/tests/test_sandbox/test_api        30 passed in 5.17s
backend/tests/test_sandbox/test_async_bridge.py 13 passed in 0.19s
ruff check backend/src/sandbox ...         passed
git diff --check                           passed
```

Reference logs:

| Run | Path |
|---|---|
| before optimization | `/tmp/eos_load_full_quietlog_2026-05-04.log` |
| optimized full suite | `/tmp/eos_load_full_optimized_2026-05-04.log` |
| pure process baseline | ephemeral one-off terminal output from local process exec probe |

## Baseline Finding: Process Execution Was Not Slow

A pure local process execution baseline used the same command shape as the shell
load test: `mkdir + printf + cat`.

At 50 concurrent operations:

| Probe | Wall | op p50 | op p95 |
|---|---:|---:|---:|
| native async process exec | 103.8 ms | 50.3 ms | 96.9 ms |
| `subprocess.run` via executor | 83.4 ms | 57.7 ms | 77.4 ms |

This ruled out the user command itself as the primary source of shell latency.
The production shell path's `overlay.run_command_s` also stayed low:

| Run | c=50 `overlay.run_command_s` p95 |
|---|---:|
| before | 22.5 ms |
| after | 51.2 ms |

The shell command is fast. The bottleneck was orchestration around it.

## Root Cause

Before optimization, shell had two async executor boundaries:

1. API awaits overlay shell execution.
2. API resumes, converts captured changes, then awaits OCC apply.

At c=50, the inner work was small but executor future resumption was very slow:

| Timing | Before c=50 p95 |
|---|---:|
| `api.shell.overlay_s` | 5348.4 ms |
| `overlay.invoker.resume_wait_s` | 5289.6 ms |
| `overlay.invoker.worker_total_s` | 67.4 ms |
| `api.shell.occ_apply_s` | 4951.7 ms |
| `occ.apply.commit_resume_wait_s` | 4651.1 ms |
| `occ.apply.commit_worker_s` | 135.3 ms |

The observed shell latency was mostly async continuation delay, not command
execution, mount work, capture work, or layer publishing.

The benchmark itself also contributed measurable overhead because every op log
included full timing maps and layer-stack metrics, and layer-stack metrics walked
the storage tree. Live `-s` logging amplified this further.

## Implementation Changes

### 1. Removed direct `asyncio.to_thread` from the sandbox hot path

The sandbox hot path now dispatches sync work through
`sandbox.runtime.async_bridge.run_sync_in_executor`, backed by a dedicated
sandbox executor. This avoids direct use of `asyncio.to_thread` and avoids
default-executor contention in the benchmark.

Changed paths include:

- `backend/src/sandbox/runtime/async_bridge.py`
- `backend/src/sandbox/overlay/runner/runtime_invoker.py`
- `backend/src/sandbox/occ/service.py`
- `backend/src/sandbox/occ/orchestrator.py`
- `backend/src/sandbox/providers/daytona/client/async_.py`

### 2. Collapsed shell overlay + OCC into one shell transaction

The optimized shell path runs overlay capture and OCC apply inside one
executor-side transaction:

```text
API -> run_shell_transaction worker
       -> acquire snapshot lease
       -> execute overlay shell
       -> capture upperdir changes
       -> prepare/apply OCC
       -> read stdout/stderr refs
       -> return ShellResult
```

This removes the large mid-pipeline API resume gap between overlay and OCC.

New/changed paths:

- `backend/src/sandbox/runtime/overlay_shell/transaction.py`
- `backend/src/sandbox/api/shell.py`
- `backend/src/sandbox/overlay/runner/snapshot_overlay_runner.py`
- `backend/src/sandbox/overlay/runner/runtime_invoker.py`
- `backend/src/sandbox/overlay/client.py`

### 3. Added OCC serial merger for prepared changesets

OCC commit publication is serialized by the layer-stack lock, so 50 independent
workers publishing one layer each caused unnecessary lock churn. The new serial
merger batches disjoint prepared changesets and publishes them together.

New/changed paths:

- `backend/src/sandbox/occ/serial_merger.py`
- `backend/src/sandbox/occ/service.py`
- `backend/src/sandbox/occ/orchestrator.py`

At c=50, the serial merger batched many independent operations:

| Workload | c=50 `occ.serial.batch_size` p50/p95 |
|---|---:|
| write | 30 / 30 |
| edit | 26 / 26 |
| shell | 48 / 48 |
| mixed | 50 / 50 |
| serial prepared writes | 36 / 36 |

### 4. Reduced benchmark logging overhead

Per-op logs now emit timing keys by default instead of full timing maps. Per-op
layer-stack metrics are disabled by default; batch-level events still emit full
aggregate timing stats and full layer-stack metrics.

Verbose per-op logs can be re-enabled with:

```bash
EOS_SANDBOX_API_LOAD_VERBOSE_OPS=1
EOS_SANDBOX_API_LOAD_VERBOSE_STACK=1
```

This keeps in-flight progress logs without letting the log path dominate the
measured workload.

## Results

### c=50 summary

| Workload | Before wall | After wall | Before p95 | After p95 | After parallel factor |
|---|---:|---:|---:|---:|---:|
| read | 1.314 s | 0.087 s | 690.4 ms | 84.7 ms | 32.78 |
| write | 0.641 s | 0.059 s | 572.4 ms | 54.1 ms | 38.44 |
| edit | 1.377 s | 0.107 s | 960.4 ms | 103.8 ms | 41.46 |
| shell | 11.017 s | 1.578 s | 7393.5 ms | 1576.2 ms | 49.83 |
| mixed | 1.098 s | 0.060 s | 918.5 ms | 56.2 ms | 39.20 |
| serial prepared writes | 0.514 s | 0.053 s | 456.1 ms | 50.8 ms | 41.69 |

### c=50 shell split after optimization

`api.shell.total_s` is the shell command path containing overlay + OCC.

| Timing | p50 | p95 | max |
|---|---:|---:|---:|
| `api.shell.total_s` | 1574.8 ms | 1576.2 ms | 1576.5 ms |
| `api.shell.worker_total_s` | 1566.1 ms | 1568.9 ms | 1571.0 ms |
| `api.shell.transaction_dispatch_s` | 7.8 ms | 9.9 ms | 10.4 ms |
| `api.shell.overlay_s` | 1518.9 ms | 1526.5 ms | 1528.1 ms |
| `api.shell.occ_apply_s` | 45.4 ms | 53.6 ms | 145.5 ms |
| `overlay.mount_snapshot_s` | 749.6 ms | 910.3 ms | 937.9 ms |
| `overlay.run_command_s` | 38.1 ms | 51.2 ms | 56.1 ms |
| `overlay.capture_changes_s` | 611.6 ms | 718.9 ms | 733.2 ms |
| `occ.apply.commit_resume_wait_s` | 0.0 ms | 0.0 ms | 0.0 ms |

The old async-resume bottleneck is gone. The remaining shell cost is now real
overlay mount/capture fanout, which was explicitly left out of scope for this
optimization pass.

### c=50 write/edit split after optimization

| Workload | p95 | OCC resume p95 | serial queue p95 | publish p95 |
|---|---:|---:|---:|---:|
| write | 54.1 ms | 2.0 ms | 24.5 ms | 4.0 ms |
| edit | 103.8 ms | 1.6 ms | 67.7 ms | 4.7 ms |

The previous multi-hundred millisecond resume waits were removed. The remaining
cost is primarily serial-merge queueing and batch commit work.

## Correctness Coverage

The optimized load suite still verifies:

- all API operations succeed at concurrency levels `1, 5, 10, 20, 50`
- write conflict detection for concurrent current writes
- edit conflict detection for concurrent current edits
- shell concurrent update conflict detection
- layer-stack squash correctness
- OCC global serial merger correctness for concurrent prepared writes
- mixed workload semantics where total ops equal the concurrency level

Additional targeted API and async bridge tests passed after the optimization.

## Conclusions

1. The original shell p95 was not caused by the shell command. Pure process
   execution was under 100 ms p95 at c=50.
2. The original shell p95 was dominated by async executor continuation delays
   around overlay and OCC.
3. Collapsing shell overlay + OCC into one executor-side transaction removed the
   multi-second resume waits.
4. The OCC serial merger substantially improved write, edit, mixed, and prepared
   write throughput by batching disjoint commits into fewer layer publishes.
5. Benchmark logging was a measurable part of the problem. Keeping aggregate
   batch stats while slimming per-op logs made the load numbers more reliable.
6. The remaining c=50 shell cost is overlay mount/capture work under high fanout,
   not OCC and not command execution. Overlay mount/capture optimization is
   intentionally deferred.

## Follow-up Work

- Keep the current optimized shell/OCC path as the baseline for future per-call
  snapshot experiments.
- Add a separate overlay mount/capture experiment if shell p95 below 1.5 seconds
  at c=50 becomes a requirement.
- Consider adding a benchmark assertion for `occ.serial.batch_size > 1` at high
  concurrency to protect the serial merger behavior from regressions.
- Preserve the safe default: generic shell stays on overlay + OCC; read-only
  speedups should remain structured API fast paths, not inferred shell bypasses.
