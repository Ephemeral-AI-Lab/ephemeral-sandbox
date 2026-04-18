# Pre-Completion Validation

Use this reference before signaling completion when you have made source edits.

## Task/Goal

- You edited one or more source files and are preparing the final verification and summary.

## Avoid

- Do not skip this step even if your narrow verification command passed. A passing narrow test does not prove that your edits left no import or name errors in files outside the test's import chain.
- Do not treat `ci_diagnostics` as a substitute for runtime verification. You still must run the assigned verification command. This is a pre-flight check, not the verdict.
- If `ci_diagnostics` reports errors you cannot fix without widening scope, call `submit_task_summary(type='fail')` with the exact diagnostic output instead of leaving the errors in place.

## Workflow

- Before your final message, run `ci_diagnostics(file_path)` on **every file you edited** during this task. This catches import errors, undefined names, syntax errors, and type mismatches before the validator or any parallel sibling sees your changes.

1. Collect the list of files you edited (from your `daytona_edit_file`, `daytona_write_file`, `daytona_rename_symbol`, `daytona_delete_file`, and `daytona_move_file` calls). A deleted file has no diagnostics to run; a moved file should be checked at its destination path.
2. For each file, call `ci_diagnostics(file_path)`.
3. If any diagnostic has severity `error`:
   - Fix the error immediately with `daytona_edit_file`.
   - Re-run `ci_diagnostics(file_path)` on the fixed file.
   - Repeat until the file is clean.
4. Only after all edited files pass diagnostics, proceed to your final verification command.

## Expected Outcome

- Every edited file is diagnostics-clean before the final runtime verification and summary.
