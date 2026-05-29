# Task Center Runner Run Commands

Run commands from the repository root:

```bash
cd /Users/yifanxu/machine_learning/LoVC/EphemeralOS
```

The pytest suites use the SWE-EVO fixture instance from `EOS_SWEEVO_INSTANCE`.
If it is not set, the default fixture instance is used.

```bash
export EOS_SWEEVO_INSTANCE=dask__dask_2023.3.2_2023.4.0
```

The configured local database comes from `ephemeralos.yaml` and can be
overridden with `EPHEMERALOS_DATABASE_URL`.

## Sandbox Mock Tests

Sandbox tests cover sandbox connection, stability, load-oriented behavior, and
sandbox-heavy TaskCenter workflows.

```bash
EOS_SWEEVO_INSTANCE=dask__dask_2023.3.2_2023.4.0 \
uv run pytest -q backend/src/task_center_runner/tests/mock/sandbox
```

Run one sandbox test file:

```bash
EOS_SWEEVO_INSTANCE=dask__dask_2023.3.2_2023.4.0 \
uv run pytest -q \
  backend/src/task_center_runner/tests/mock/sandbox/project_build/test_complex_project_build_smoke.py
```

Run one sandbox test function:

```bash
EOS_SWEEVO_INSTANCE=dask__dask_2023.3.2_2023.4.0 \
uv run pytest -q \
  backend/src/task_center_runner/tests/mock/sandbox/project_build/test_complex_project_build_smoke.py::test_complex_project_build_smoke
```

Capacity and heavy sandbox tests are gated by `runner.live_e2e.capacity_enabled`
and `runner.live_e2e.heavy_enabled` in `ephemeralos.yaml`. Sandbox reuse is
controlled by `runner.sandbox_reuse_mode` in the same file; use `reuse` for the
normal shared-sandbox capacity workflow and `force_fresh` when debugging
provisioning.

## TaskCenter Mock Tests

TaskCenter mock tests focus on workflow correctness rather than load testing.

```bash
EOS_SWEEVO_INSTANCE=dask__dask_2023.3.2_2023.4.0 \
uv run pytest -q backend/src/task_center_runner/tests/mock/task_center
```

Run one TaskCenter test file:

```bash
EOS_SWEEVO_INSTANCE=dask__dask_2023.3.2_2023.4.0 \
uv run pytest -q backend/src/task_center_runner/tests/mock/task_center/test_focused_scenarios.py
```

Run one parametrized TaskCenter case:

```bash
EOS_SWEEVO_INSTANCE=dask__dask_2023.3.2_2023.4.0 \
uv run pytest -q \
  'backend/src/task_center_runner/tests/mock/task_center/test_focused_scenarios.py::test_focused_reference_scenario_runs[pipeline.initial_workflow]'
```

## Real-Agent Tests

Real-agent tests run live LLM agents through the TaskCenter runner. They require
working provider credentials and a working sandbox provider.

```bash
EOS_SWEEVO_INSTANCE=dask__dask_2023.3.2_2023.4.0 \
uv run pytest -q backend/src/task_center_runner/tests/real_agent
```

Run one real-agent test:

```bash
EOS_SWEEVO_INSTANCE=dask__dask_2023.3.2_2023.4.0 \
uv run pytest -q \
  backend/src/task_center_runner/tests/real_agent/test_real_agent.py::test_real_agent_resolves_canonical_instance
```

The real-agent timeout comes from
`runner.live_e2e.real_agent_max_duration_s` in `ephemeralos.yaml`.

## Full SWE-EVO Benchmark

The full SWE-EVO benchmark is not a pytest suite. Run it through the benchmark
CLI and pass the instance id with `--instance-id`.

```bash
uv run python -m task_center_runner.benchmarks.sweevo \
  --instance-id dask__dask_2023.3.2_2023.4.0
```

Useful options:

```bash
uv run python -m task_center_runner.benchmarks.sweevo \
  --instance-id dask__dask_2023.3.2_2023.4.0 \
  --max-duration-s 10800 \
  --audit-dir .sweevo_runs
```

The TaskCenter-side benchmark implementation lives in:

- `backend/src/task_center_runner/benchmarks/sweevo/run.py`
- `backend/src/task_center_runner/benchmarks/sweevo/eval.py`
- `backend/src/task_center_runner/benchmarks/sweevo/run.py`

The CLI entrypoint is:

- `backend/src/task_center_runner/benchmarks/sweevo/__main__.py`

## Single-Test Pattern

Use pytest node ids to avoid running a whole suite:

```bash
uv run pytest -q path/to/test_file.py
uv run pytest -q path/to/test_file.py::test_function_name
uv run pytest -q 'path/to/test_file.py::test_parametrized_case[param-id]'
```

Use `--collect-only` first when you need the exact node id:

```bash
uv run pytest --collect-only -q backend/src/task_center_runner/tests/mock/sandbox
```
