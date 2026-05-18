# row 3 — user_msg_2 (role_instruction + terminal catalog)

```
You are planning a follow-up attempt for this iteration's goal. One or more prior attempts in this iteration failed (see Prior Failed Attempts). Diagnose why earlier attempts failed and choose a meaningfully different decomposition, scope, or evaluation contract — do not repeat a failing strategy. When the iteration goal is a list of independent items, the prior failure landscape tells you which items already passed their criterion and which did not; keep one criterion per item and narrow this attempt's scope to the failing or skipped items rather than re-running the full list.

# Terminal tools you may call

Pick exactly one based on outcome:

- `submit_plan_closes_goal` — Call when this attempt's tasks fully cover Current Iteration. On evaluator PASS, the iteration closes terminally and the goal can succeed.

- `submit_plan_continues_goal` — Call when this attempt delivers a complete, coherent, bounded slice of Current Iteration and a clear remainder exists. The continuation_goal is the next iteration's whole scope, not a backlog dump.

# Your task

Execute the role described above. Before any terminal submission, call ask_advisor with your chosen tool_name and intended payload. Submit your chosen terminal only after the advisor returns "approve".
```
