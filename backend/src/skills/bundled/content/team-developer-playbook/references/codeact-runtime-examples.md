# CodeAct Runtime Examples

Use this reference before the first `daytona_codeact` verification or reproduction command on a benchmark lane.

## Task/Goal

- You are about to run the first benchmark-lane reproduction or verification command.

## Avoid

- Must not start pip-install loops or ad hoc environment mutation unless repo bootstrap evidence proves the lane owns that setup.
- Must not inspect benchmark test files with `daytona_read_file(...)` before the first exact `daytona_codeact(command="...", timeout=N)` repro; use the named node, scout note, and runtime traceback first.

## Workflow

- The preferred benchmark-lane repo-command form is direct `daytona_codeact(command="...", timeout=N)`.
- Must not append shell capture plumbing such as `2>&1`, `2>/dev/null`, or `1>/tmp/out`; `daytona_codeact` already captures stdout and stderr.
- Must not edit files through CodeAct. Avoid `sed -i`, `tee file`, output redirects, `touch`/`cp`/`mv`/`rm`, and inline Python writes; use `daytona_edit_file`, `daytona_write_file`, `daytona_rename_symbol`, `daytona_delete_file`, or `daytona_move_file`. The shell policy blocks `rm`/`mv` precisely because those two tools are the sanctioned OCC-gated path for deletes and moves — bypassing them via shell would skip the base-hash check.
- Must not inspect source through CodeAct. Avoid `cat`, `sed -n`, `grep`/`rg`, `head`/`tail`/`nl`, Python file reads, and `inspect.getsource`; use notes and CI first, then `daytona_read_file` or `daytona_grep`.
- If you truly need multi-step Python mode, keep repo commands inside `shell("...")` and still avoid `subprocess`.
- Must keep repo commands repo-root-relative; do not prefix commands with `cd /testbed &&`, `cd /workspace &&`, or another repo-root `cd`.
- Must not call unprefixed tools like `write_file`, `edit_file`, `read_file`, `bash`, or `grep`; the valid tools are the exact names in the tool list, such as `daytona_write_file`, `daytona_rename_symbol`, `daytona_delete_file`, and `daytona_move_file`.
- If `Unknown tool` appears, treat it as your own Daytona tool-name defect and retry once with the exact tool name before continuing.
- Must judge pass/fail from `shell(...)["exit_code"]`, not wrapper metadata.
- If a probe returns manifest `status: error`, traceback text, or no trustworthy exit code, simplify the next probe instead of broadening.
- If pytest says a named node is missing, exits `4`, or collects `0` items, report that exact control failure or hand the file surface back to replanning.

## Expected Outcome

- Wrong first probe: `subprocess.run(...)` or a helper that shells out to pytest. Right first probe: `daytona_codeact(command="pytest pkg/tests/test_hdf.py -x", timeout=120)`.
- If a permission test runs as root and `chmod` still leaves the file readable, treat that as still-red runtime evidence and do not skip or rewrite the verify file.
