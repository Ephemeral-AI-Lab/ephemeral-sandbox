# row 3 — user_msg_2 (role_instruction + terminal catalog)

```
You are planning the first attempt for this iteration's goal. No prior attempts exist in this iteration. Propose a plan that decomposes the iteration goal into generator tasks with a clear evaluation contract. If you cannot solve the iteration in one attempt, submit a partial plan with a continuation_goal so the next iteration can pick up where this one ends. When the iteration goal is a list of independent items (for example a PR-description changelog of features and fixes), prefer a wide parallel DAG with one sibling generator task per item and one criterion per item; coalescing into a single 'all items done' criterion turns partial progress into total failure. If one attempt cannot fit every item, bind a tighter set of items here. If you defer work via continuation_goal, make that continuation_goal the next bounded slice only; do not dump the entire remaining backlog into it.

# Terminal tools you may call

Pick exactly one based on outcome:

- `submit_plan_closes_goal` — Call when this attempt's tasks fully cover Current Iteration. On evaluator PASS, the iteration closes terminally and the goal can succeed.

- `submit_plan_continues_goal` — Call when this attempt delivers a complete, coherent, bounded slice of Current Iteration and a clear remainder exists. The continuation_goal is the next iteration's whole scope, not a backlog dump.

# Your task

Execute the role described above. Before any terminal submission, call ask_advisor with your chosen tool_name and intended payload. Submit your chosen terminal only after the advisor returns "approve".
```
