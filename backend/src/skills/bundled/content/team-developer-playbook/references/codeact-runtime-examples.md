# CodeAct Runtime Examples

Use this reference before the first `daytona_codeact` verification or reproduction command on a benchmark lane.

## Rules

- Must execute repo commands through `shell("...")` inside `daytona_codeact`.
- Must keep the command repo-root-relative inside the shell string.
- Must not replace a simple repo command with raw Python `subprocess.run(...)` boilerplate.
- Must treat missing tools, missing modules, or unsupported flags as runtime evidence first.
- Must not start pip-install loops or ad hoc environment mutation unless repo bootstrap evidence proves the lane owns that setup.
- Never treat ambient package installation as the default next step just because the first verify command failed.

## Few-shot examples

- Example: payload verify command is `python -m pytest dask/tests/test_cli.py -x`.
  Call `daytona_codeact` with code like `shell("cd /testbed && python -m pytest dask/tests/test_cli.py -x", timeout=120)`.
  Do not wrap that same command in Python `subprocess.run(...)`.
- Example: `pytest` is missing from `PATH`.
  Probe with `shell("cd /testbed && which pytest || python -m pytest --version", timeout=30)`, then retry with the working command form.
  Do not pivot into `subprocess`, and do not assume the benchmark fix is to install pytest.
- Example: the exact verify command dies during import because a module like `yaml` or `tlz` is missing before the named test loads.
  Treat that as runtime or ambient evidence on the current lane first, then decide whether the repository owns the missing import path.
  Do not start a generic `pip install ...` loop unless repo bootstrap files or the owned production surface prove that setup belongs to the fix.
