# File Operation Live E2E Tests

Implements the 110-case live e2e matrix from
`docs/obsidian/ephemeral-os/implementation_plan/file_operations/test-case.md`
(`## Live E2E Matrix`). Every module maps 1:1 to one checklist group; keep the
doc and this suite in sync — a case added to the doc gets a test here, and
vice versa.

## Layout

| Module | Checklist group | Cases |
| --- | --- | --- |
| `smoke/test_read_smoke.py` | Read Smoke | 5 |
| `smoke/test_write_smoke.py` | Write Smoke | 5 |
| `smoke/test_edit_smoke.py` | Edit Smoke | 5 |
| `smoke/test_session_only_linux.py` | Session-Only Cases (Docker/Linux sandbox) | 5 |
| `concurrent/test_concurrent_sessionless.py` | Concurrent Operations — Sessionless | 17 |
| `concurrent/test_concurrent_session.py` | Concurrent Operations — Session | 9 |
| `correctness/test_correctness_sessionless.py` | Correctness: Layerstack, Mount, Conflict — Sessionless | 18 |
| `correctness/test_correctness_session.py` | Correctness: Layerstack, Mount, Conflict — Session | 9 |
| `file_exec/test_file_exec_sessionless.py` | File Ops + Exec Ops — Sessionless | 19 |
| `file_exec/test_file_exec_session.py` | File Ops + Exec Ops — Session | 8 |
| `blame/test_blame_sessionless.py` | File Blame — Sessionless | 6 |
| `blame/test_blame_session.py` | File Blame — Session | 4 |

`helpers.py` holds the file-op CLI wrappers (`file_read`, `file_write`,
`file_edit`, `file_blame`) and shared assertion helpers. Import it as
`runtime.file.helpers` (the suite root is on `pythonpath`, see `pytest.ini`).

## Conventions

- Standing correctness rule: any test whose operations publish content
  (sessionless write/edit, one-shot exec capture, or session capture) must
  assert `file_blame` ownership of the touched lines in addition to content
  and layerstack checks.
- Session lifecycle (create/destroy) goes through the shared fixtures in the
  suite-root `conftest.py`; never leak sandboxes or sessions.
- Mark larger-volume `[complex]` cases with `@pytest.mark.slow`.
- One test function per checklist case, named after the case, with the
  checklist text as its docstring.

## Running

```sh
cd cli-operation-e2e-live-test
pytest runtime/file                # whole matrix
pytest runtime/file/blame          # one group
pytest runtime/file -m "not slow"  # skip [complex] cases
```

The gateway starts automatically via the `gateway_up` fixture (reused if one
is already answering) and sandboxes default to `ubuntu:24.04` — override with
`E2E_IMAGE`. See `RUNNING.md` at the suite root for all knobs. The CLI emits
one JSON line per operation: stdout on success, stderr + exit 1 on an error
response, so assert on `error.kind` (e.g. `not_found`), never on exit codes.
Command shapes were smoke-verified live on ubuntu:24.04 (2026-07-02); see
the Test Runner Instructions section of the test-case doc.

Every live run also writes the latest per-operation timing summary to
`docs/obsidian/ephemeral-os/testing/file-operation/operation-timing/`. These are
client-side `sandbox-cli` wall-time metrics grouped by operation, including
count/min/p50/p95/max plus sub-50ms, sub-100ms, and sub-200ms percentages; they
are not pass/fail assertions.
