# Task Center Runner Test Suites

## Layout

- `mock/` — deterministic mocked-agent scenario tests. These may use the
  SWE-EVO Docker image as an environment fixture, but they do not run a real
  agent. `contracts/`, `environments/`, `sandbox/`, and `task_center/` are
  subcategories of this mocked-agent boundary.
  - `mock/sandbox/` — sandbox connection, stability, load, and
    sandbox-heavy TaskCenter workflow tests.
  - `mock/task_center/` — basic TaskCenter workflow correctness
    tests that are not load tests.
- `real_agent/` — tests that run real LLM agents through the task-center
  runner.

The full SWE-EVO benchmark lifecycle is not a pytest suite. Run it through the
benchmark CLI:

```bash
uv run python -m task_center_runner.benchmarks.sweevo \
  --instance-id dask__dask_2023.3.2_2023.4.0
```

## Commands

```bash
uv run pytest -q backend/src/task_center_runner/tests/mock
uv run pytest -q backend/src/task_center_runner/tests/mock/sandbox
uv run pytest -q backend/src/task_center_runner/tests/mock/task_center
uv run pytest -q backend/src/task_center_runner/tests/real_agent
uv run python -m task_center_runner.benchmarks.sweevo \
  --instance-id dask__dask_2023.3.2_2023.4.0
```
