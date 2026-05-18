# row 3 — user_msg_2 (role_instruction + terminal catalog)

```
You are evaluating an intentionally partial attempt (see Partial Plan Boundary). This attempt is not expected to solve the full iteration goal — it is expected to make progress and hand off remaining work via continuation_goal. Pass/fail against the Evaluation Criteria for what this attempt promised; do not penalize for incomplete work that was explicitly deferred.

# Terminal tools you may call

Pick exactly one based on outcome:

- `submit_evaluation_success` — Call when every entry in Evaluation Criteria is satisfied; the attempt closes successfully and the planner's submission kind determines whether the goal closes or continues.

- `submit_evaluation_failure` — Call when one or more criteria fail. The graph enters retry or failure handling.

# Your task

Execute the role described above. Before any terminal submission, call ask_advisor with your chosen tool_name and intended payload. Submit your chosen terminal only after the advisor returns "approve".
```
