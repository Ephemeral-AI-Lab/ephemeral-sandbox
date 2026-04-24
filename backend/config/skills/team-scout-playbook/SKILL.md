---
name: team-scout-playbook
description: Authoritative playbook for the scout subagent. Performs evidence-only exploration of assigned target paths and posts findings to Task Center with submit_file_notes.
---

# Team Scout Playbook

Scout only assigned `target_paths`, post durable notes, then finish with exactly one `submit_file_notes(...)`.

```text
Caption: scout route. Notes first, then exploration, then exact-file completion when needed.

payload -> [1 Notes] -> [2 Explore] -> [3 Load completion-contract if exact-file] -> [4 Submit notes]
```

| Stage | Output |
| --- | --- |
| 1. Notes | `read_file_note(file_paths=[all assigned target_paths])` as the first tool phase. |
| 2. Explore | Evidence-only map of scope, entry points, owner seam, subdivisions, and gaps. |
| 3. Exact-file completion | Load `completion-contract` only after notes and exploration. |
| 4. Submit notes | One `submit_file_notes({ prompt, scoped_paths })` covering exactly assigned repo-relative keys, then stop. |

## 2. Explore

| Target shape | Exploration |
| --- | --- |
| Single file / short fixed file list | Use at most one file-path `ci_query_symbol(...)` per assigned path, then Stage 3. |
| Directory/package | Use CI tools to map subdivisions, entry points, owner seam, and gaps. |
| Benchmark/test path | Record expected behavior, then map production-owner evidence or gaps. |
| Missing exact target | Record zero coverage and do not hunt nearby replacements. |
| Adjacent files | Mention inside assigned-path notes, not `scoped_paths`. |

Keep `target_paths` as the exploration boundary. Prefer notes and CI before raw source reads. No sandbox, edit, command, pytest, or runtime execution tools.

If exact-path symbol queries returned definitions, submit notes next. If a target is missing, no-symbol, or replaced by a package boundary, submit notes with that gap instead of widening exploration.

```text
Caption: durable handoff sections.

Scope | Files mapped | Entry points | Owner seam | Suggested subdivisions | Gaps
```

| Check | Expected result |
| --- | --- |
| Coverage | Each assigned target appears once in `scoped_paths`; no discovered extras. |
| Multi-path prompt | Path-labeled findings that stand alone when read back. |
| Scope honesty | Missing/no-symbol/adjacent evidence stays explicit in assigned notes. |
| Terminal action | Successful `submit_file_notes(...)` is the last tool action. |

After a successful submit, reply only `Posted.` if asked for final text.
