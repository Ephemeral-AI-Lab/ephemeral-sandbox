# CodeAct Runtime Examples

Use this reference before the first `daytona_codeact` verification or reproduction command on a benchmark lane.

## Rules

- Must execute repo commands through `shell("...")` inside `daytona_codeact`.
- The first benchmark reproduction or verify call should usually be a minimal `shell("...")` snippet, not a Python mini-program.
- Must default to repo-root-relative commands inside the shell string.
- Must not prepend guessed repo roots like `cd /testbed &&` when the sandbox cwd is already injected; only `cd` into a real repo subdirectory when the command truly needs it.
- Must treat missing tools, missing modules, or unsupported flags as runtime evidence first.
- Must not start pip-install loops or ad hoc environment mutation unless repo bootstrap evidence proves the lane owns that setup.
- After one existing-environment probe, must either use the working command form or switch to source-and-test reading on the owned repo surface.
- Never turn an install rejection into a `pip` -> `pip3` -> `conda` -> `uv` retry ladder.
- Must treat the object returned by `shell("...")` as a mapping with keys like `stdout`, `stderr`, and `exit_code`, and judge pass/fail from that mapping, not a successful `daytona_codeact` wrapper.
- Must not use `git status`, `git log`, `git diff`, `git show`, `git blame`, `git stash`, `git checkout`, `git restore`, or temporary revert probes to prove whether a sibling failure was "already there".
- If the live source file looks different from what you expected or a `daytona_edit_file` search misses, treat the live file plus payload evidence as authoritative, rebuild the edit from current text, and keep repo writes on edit/write tools instead of `daytona_codeact`.
- If `shell("...")` output is sparse, truncated, or piped through `head`/`tail`, must use `set -o pipefail` or the exact verify command before treating `exit_code` as success; sampled reproductions are preview-only.
- Must not broaden from a named failing id or bounded payload command to a much larger suite unless the payload itself assigns that broader command; if that assigned broader command fails first in a shared upstream file outside `owned_files`, confirm that path once and either widen one step for the same chain or surface it as shared-blocker evidence.
- If pytest says a named node is missing, report that exact node mismatch or hand the file surface back to replanning; do not run collect-only or hunt similar names unless the payload already owns the full file.
- If the failure happens during import, warning-filter parsing, or collection, the first runtime verify after an edit must prove that exact import chain is healthy before any broader sweep; if a private symbol still has internal importers, prove that internal path stays quiet before chasing the public warning contract.
- If a shared-module edit creates a new import or collection crash, fix that crash on the same chain before resuming unrelated diagnosis.
- If a broader verify first crashes in a shared non-owned file and the live text looks half-edited or contradictory, confirm that exact traceback once, then widen one step on the same chain or surface a blocker; do not use git/history probes to decide whether the breakage was older than your lane.
- When the owned assertion is `pytest.warns(...)` or `pytest.raises(..., match=...)`, verify the live import object model or regex behavior before changing public warning paths or error strings.

## Few-shot examples
- Example: payload verify command is `python -m pytest dask/tests/test_cli.py -x`.
  Call `daytona_codeact` with code like `shell("python -m pytest dask/tests/test_cli.py -x", timeout=120)`.
  Do not wrap that same command in Python `subprocess.run(...)`.
- Example: payload verify command is `pytest pkg/tests/test_hdf.py -x`, and you feel like writing a helper script first.
  Wrong: `import subprocess` plus `subprocess.run(["pytest", "pkg/tests/test_hdf.py", "-x"])`.
  Right: `result = shell("pytest pkg/tests/test_hdf.py -x", timeout=120)`.
  Keep the first runtime probe in the shell helper form unless you truly need to parse a prior shell result.
- Example: payload reproduction is `pytest pkg/tests/test_json.py -q 2>&1 | head -40`.
  Treat that as output sampling, not proof the tests passed; rerun the exact verify command without the pipe or use `set -o pipefail` before trusting `result["exit_code"]`.
  Do not call the lane green just because the pipeline exit code is `0` and the `daytona_codeact` manifest says `ok`.
- Example: the exact verify command dies during import because a module like `yaml` or `tlz` is missing before the named test loads.
  Treat that as still-red runtime evidence on the current lane, then inspect the owned test file, owned production file, and nearby repo import path before guessing about the environment.
  If the repository clearly owns the import path, keep diagnosing repo code; if the miss is purely ambient and outside repo ownership, surface runtime mismatch or replan evidence after that one probe.
  Do not start a generic `pip install ...` loop just because `ModuleNotFoundError` appeared first.
- Example: payload owns `pkg/tests/test_compat.py::test_deprecation`, and a private symbol now warns during `pkg/__init__.py` import so pytest dies while parsing warning filters.
  Verify the exact node plus one narrow import-smoke command after the edit; if `pkg/__init__.py` or `pkg/base.py` still imports that private symbol, switch that caller to the public replacement when available instead of adding a new quiet alias for the deprecated name.
  Do not bypass startup with `--override-ini`, `-p no:warnings`, or similar flags, and do not call that live repo traceback pre-existing.
- Example: your assigned verify now dies in `pkg/config.py`, and the live file shows impossible self-assignment or parameter/global conflicts after another worker touched it.
  Read the exact lines with structured tools, keep the failing command visible, and route the lane through one-step widening or replanning on that shared chain.
  Do not run `git diff`, `git show`, or a Python subprocess wrapper to argue that the syntax break was "already there".
- Example: payload owns `pkg/tests/test_json.py::test_engine_error`, and the assertion is `pytest.raises(ValueError, match="Pandas>=2.0 is required")`.
  Reproduce the exact regex match before editing product strings; a longer live error message does not prove failure when the pattern is still satisfied.
  Do not rewrite the production message or patch the test on the assumption that `match=` is plain substring text.
