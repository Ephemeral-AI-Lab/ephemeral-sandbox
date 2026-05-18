# row 3 — user_msg_2 (role_instruction + terminal catalog)

```
You are planning the first attempt for a later iteration. The prior iteration produced concrete results (see Previous Iteration Results). Your decomposition should continue from where the prior iteration ended — build on prior outputs, do not redo their work. The Current Iteration text is the authoritative scope for this planner; use the original Goal only for orientation and do not add backlog items that Current Iteration did not explicitly name. When the iteration goal is a list of independent items, consult Previous Iteration Results for which items already passed and plan only the remaining items, keeping one criterion per item so the evaluator can report per-item pass/fail rather than a single coarse verdict.

# Terminal tools you may call

Pick exactly one based on outcome:

- `submit_plan_closes_goal` — Call when this attempt's tasks fully cover Current Iteration. On evaluator PASS, the iteration closes terminally and the goal can succeed.

- `submit_plan_continues_goal` — Call when this attempt delivers a complete, coherent, bounded slice of Current Iteration and a clear remainder exists. The continuation_goal is the next iteration's whole scope, not a backlog dump.

# Your task

Execute the role described above. Before any terminal submission, call ask_advisor with your chosen tool_name and intended payload. Submit your chosen terminal only after the advisor returns "approve".
```
