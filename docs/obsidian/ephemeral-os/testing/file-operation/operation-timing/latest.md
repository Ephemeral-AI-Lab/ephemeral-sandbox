# Sandbox CLI Operation Timing

- Generated: `2026-07-02T08:23:57+08:00`
- Command: `/opt/homebrew/bin/pytest runtime/file/smoke --tb=short -q --log-cli-level=WARNING`
- Exit status: `0`
- CLI calls measured: `124`
- Durations are client-side `sandbox-cli` wall time.
- `sub50` is measurement only; the suite does not enforce a timing SLO.

| Operation | Count | Min ms | P50 ms | P95 ms | Max ms | Sub50 | CLI errors |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `manager.create_sandbox` | 20 | 226.6 | 256.8 | 522.0 | 523.8 | 0.0% | 0 |
| `manager.destroy_sandbox` | 20 | 297.6 | 349.5 | 368.3 | 376.7 | 0.0% | 0 |
| `manager.list_sandboxes` | 1 | 25.3 | 25.3 | 25.3 | 25.3 | 100.0% | 0 |
| `runtime.create_workspace_session` | 9 | 28.8 | 31.5 | 44.2 | 44.3 | 100.0% | 0 |
| `runtime.destroy_workspace_session` | 9 | 44.8 | 47.7 | 54.8 | 55.1 | 77.8% | 0 |
| `runtime.exec_command` | 7 | 50.5 | 52.8 | 87.5 | 100.9 | 0.0% | 0 |
| `runtime.file_blame` | 9 | 23.1 | 27.3 | 31.5 | 31.8 | 100.0% | 0 |
| `runtime.file_edit` | 7 | 30.2 | 33.5 | 52.4 | 53.2 | 71.4% | 3 |
| `runtime.file_read` | 24 | 23.9 | 30.2 | 50.8 | 52.1 | 91.7% | 4 |
| `runtime.file_write` | 18 | 32.4 | 46.9 | 57.0 | 58.5 | 55.6% | 4 |
