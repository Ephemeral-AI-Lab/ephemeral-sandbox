# CodeAct Runtime Examples

Use this reference as a required benchmark-lane preflight. Load it with `load_skill_reference(skill_name="team-developer-playbook", reference_name="codeact-runtime-examples")` before the first `daytona_codeact` reproduction or verification command.

## Task/Goal

- You are about to run the first benchmark-lane reproduction or verification command.
- CodeAct is a runtime tool, not a shell-output wrapper.

## Avoid

- Must not start pip-install loops or ad hoc environment mutation. Missing packages are evidence; edit dependency metadata only when that file is in scope, otherwise request replanning with the missing package and command output.
- Must not inspect benchmark test files with `daytona_read_file(...)` before the first exact `daytona_codeact(command="...", timeout=N)` repro; use the named node, scout note, and runtime traceback first. After repro, bounded read-only test snippets are allowed when they explain expected behavior, imports, fixtures, or parametrization.

## Workflow

- Gate: before every benchmark CodeAct call, inspect the exact `command` string. If it contains the literal character `|` or `>` anywhere, the tool call is a workflow failure even if the shell command would succeed; rewrite to a direct repo-root command first.
- The benchmark-lane repo-command form is direct `daytona_codeact(command="...", timeout=N)`.
- Good: `daytona_codeact(command="python -m pytest dask/tests/test_config.py::test_update_defaults -q")`
- Bad: `daytona_codeact(command="cd /testbed && python -m pytest ... 2>&1 | head -100")`
- Bad: `daytona_codeact(command="python -m pytest dask/tests/test_cli.py -v 2>&1 | tail -60")`
- Rewrite any planned command containing `2>&1`, `2>/dev/null`, `>`, `>>`, `| head`, or `| tail` before the tool call. CodeAct already captures stdout and stderr; use pytest flags such as `--tb=short -q`, narrower nodes, background execution, or tool truncation for volume control.
- If you think you need `head` or `tail`, the preflight is not complete. Keep the same pytest target, remove the pipe or redirect, and add `--tb=short -q` or a narrower node instead.
- Must not append shell capture plumbing to CodeAct commands.
- Must not write or move files through CodeAct. Avoid `sed -i`, `tee file`, output redirects, `touch`/`cp`/`mv`, inline Python writes, `shutil.move`, `os.rename`, `git rm`, or `git mv`. Pure removals such as `rm`, `unlink`, `os.remove`, `os.unlink`, `Path.unlink`, and `shutil.rmtree` are allowed through CodeAct because the overlay audit path converts tracked removals into OCC-gated deletes and rejects unsupported removal shapes. Use `daytona_edit_file`, `daytona_write_file`, `daytona_rename_symbol`, `daytona_delete_file`, or `daytona_move_file` for explicit repo file operations.
- Must not inspect source through CodeAct. Avoid `cat`, `sed -n`, `grep`/`rg`, `head`/`tail`/`nl`, `git diff`, Python file reads, and `inspect.getsource`; use notes and CI first, then `daytona_read_file` or `daytona_grep`.
- Code mode is not an escape hatch. Do not import or call `subprocess`, `os.system`, `os.popen`, or `Popen` to run pytest, git, grep, shell, or repo commands; use direct `command="..."` for allowed runtime commands and dedicated Daytona/CI tools for reads and edits.
- If you truly need multi-step Python mode, keep it to in-process Python helper logic; do not use it to wrap repo shell commands.
- Must keep repo commands repo-root-relative; do not prefix commands with `cd /testbed &&`, `cd /workspace &&`, or another repo-root `cd`.
- Must not call unprefixed tools like `write_file`, `edit_file`, `read_file`, `bash`, or `grep`; the valid tools are the exact names in the tool list, such as `daytona_write_file`, `daytona_rename_symbol`, `daytona_delete_file`, and `daytona_move_file`.
- If `Unknown tool` appears, treat it as your own Daytona tool-name defect and retry once with the exact tool name before continuing.
- Must judge pass/fail from `shell(...)["exit_code"]`, not wrapper metadata.
- A success summary may cite only commands actually run after the final edit, with their observed exit code or failing ids.
- If a probe returns manifest `status: error`, traceback text, or no trustworthy exit code, simplify the next probe instead of broadening.
- If pytest says a named node is missing, exits `4`, or collects `0` items, report that exact control failure or hand the file surface back to replanning.

## Expected Outcome

- Wrong first probe: `subprocess.run(...)` or a helper that shells out to pytest. Right first probe: `daytona_codeact(command="pytest pkg/tests/test_hdf.py -x", timeout=120)`.
- If a permission test runs as root and `chmod` still leaves the file readable, treat that as still-red runtime evidence and do not skip or rewrite the verify file.
