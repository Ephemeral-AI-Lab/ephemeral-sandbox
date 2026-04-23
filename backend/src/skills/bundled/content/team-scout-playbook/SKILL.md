---
name: team-scout-playbook
description: Authoritative playbook for the scout subagent. Performs evidence-only exploration of assigned target paths and posts findings to Task Center with submit_file_notes.
---

# Team Scout Playbook

Read the following sections to scout the assigned `target_paths` and post a durable handoff, then finish with exactly one `submit_file_notes(...)` call.

## Conditional references

- Must load `completion-contract` before the first read when `target_paths` is a single file or short fixed file list and `load_skill_reference` is available.

## Tool rules

- Must inspect only and use CI/Task Center tools only.
- First tool phase after reading the assigned payload: call `read_file_note(file_path="...")` for each assigned target path, even when the result is empty. Do not call `ci_workspace_structure(...)`, `ci_query_symbol(...)`, `ci_diagnostics(...)`, or any source-read tool until every assigned target note has been read.
- The first assistant message that calls tools may contain only the required `read_file_note(...)` calls. Do not batch a CI/query/source tool in the same first tool message as those note reads.
- For a single-file or short fixed file-list scout, after the note reads use at most one file-path `ci_query_symbol(...)` per assigned path. If those queries return definitions for every assigned path, the next tool must be `submit_file_notes(...)`; do not call `ci_workspace_structure(...)` or extra symbol hunts unless a target stayed cold.
- Missing exact-target gate: if an exact-file bootstrap query returns no definitions, or if bootstrap/structure evidence shows the assigned exact file is missing or replaced by a package/directory boundary, the next tool must be `submit_file_notes(...)`. Do not call `ci_workspace_structure(...)`, run `ci_query_symbol(...)` on nearby helper names, or inspect adjacent files/directories to discover a replacement owner in the same scout.
- After the required file-note reads, prefer `ci_workspace_structure(...)`, `ci_query_symbol(...)`, and `ci_diagnostics(...)` before any raw source read.
- Must call exactly one `submit_file_notes(...)` after evidence collection and before any final response. The tool input must include one note item per assigned target path, with non-empty `content` in each item.
- If a prompt lists `final_response` because scout notes are prompt-mandated instead of runtime-terminal, treat it only as an optional post-note acknowledgment. Never use final prose instead of `submit_file_notes(...)`.
- Context may mention benchmark ids, hypotheses, or adjacent production files, but it does not widen scope. If `context` asks you to inspect `core.py` while `target_paths` contains only `groupby.py`, keep `core.py` as an unresolved adjacent hypothesis in the note and do not query or read it.
- Must keep benchmark tests read-only evidence unless the assignment explicitly makes tests the owner surface.
- May inspect bounded benchmark test snippets when needed to understand expected behavior, imports, fixtures, or parametrization; do not locate, correct, or modify the test path, and map the evidence back to production owners.
- Must not recommend skipping, xfail-marking, rewriting, or reconfiguring benchmark tests, benchmark harness files, or pytest configuration. If evidence points at a dependency, optional extra, or environment mismatch, report that as a hypothesis or gap for production/dependency RCA.
- Must keep missing targets missing in the note; mention nearby files only as unconfirmed adjacent evidence, not as replacements for `paths`.
- For a missing exact file, report zero coverage for that exact path in the note and stop. Do not hunt for nearby files, sibling modules, or package structure to "fix" the handed path inside the scout.
- Must state that a no-symbol exact file should not be used as `scope_paths` when structure shows a directory or nested files for the same owner family. List the live directory or nested files as adjacent evidence unless they were assigned.
- Never use sandbox tools, edit tools, or runtime execution tools.

## Workflow

1. Read the task payload before the first exploration tool call.
2. Read existing notes for every assigned `target_paths` entry. This is the required first tool phase.
3. Enumerate only the assigned `target_paths`.
4. For directories or packages, map boundaries with CI after notes are read; for exact files, use one file-path `ci_query_symbol(...)` per assigned path after notes are read, and if every exact path returns definitions stop and post the note instead of continuing to other CI exploration.
5. If a target is a benchmark test path and tests are not the explicit owner surface, inspect only the bounded snippet needed to understand failure semantics, then post the production-owner evidence or gap.
6. If a target is missing or an exact file is disproved by a directory/nested-file structure result, keep it missing, record zero coverage, and post the gap instead of suggesting or hunting for a nearby replacement as scope.
7. Stop as soon as a downstream worker could act without reopening the same scope.
8. Post durable batched notes with scope, mapped files, entry points, owner seam, subdivisions, and gaps via `submit_file_notes(...)`, using one note item per assigned target path.
9. If the tool result returns and a final response is required, reply only `Posted.` and do not include findings there.

## Hard rules

1. Must not edit files or run implementation commands.
2. Must post the durable handoff with exactly one `submit_file_notes(...)` call before finishing.
3. Must not end with only visible findings; the findings belong inside the `submit_file_notes` input.
4. Must keep any post-note final message short and non-authoritative.
5. Must report honest coverage.
6. Must keep missing targets missing.
7. Must not widen a single-file scout into package-wide exploration.
8. Must not inspect adjacent production files that appear only in `context`; only `target_paths` authorize file or directory exploration.
9. Must not treat benchmark tests as owner-surface or edit targets unless the task explicitly says so.
10. Must not use scouts to locate or correct benchmark test paths when the production owner is the real target.
11. Never claim code was created, fixed, patched, or refactored.
12. Never prescribe test skips, xfails, rewrites, pytest configuration changes, or benchmark harness edits as the fix for fail-to-pass work.
13. Never use raw source reads as the primary navigation tool when notes or CI evidence can answer the seam question.
