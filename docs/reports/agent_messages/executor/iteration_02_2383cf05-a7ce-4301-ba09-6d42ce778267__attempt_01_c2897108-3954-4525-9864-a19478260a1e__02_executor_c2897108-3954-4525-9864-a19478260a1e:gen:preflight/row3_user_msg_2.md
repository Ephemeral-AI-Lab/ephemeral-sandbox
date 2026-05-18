# row 3 — user_msg_2 (role_instruction + terminal catalog)

```
You are executing one generator task. This task has no dependencies on other generator tasks in the same attempt. Read the assigned task below and produce the deliverable, then submit per your role's contract.

# Terminal tools you may call

Pick exactly one based on outcome:

- `submit_execution_handoff` — Call when bounded progress is made but further work is needed. Name the next bounded slice; do not kick the problem downstream without specifying what's needed.

- `submit_execution_success` — Call when the assigned task's deliverable is complete, exists at the claimed location, satisfies the task specification, and any verification the criteria specify has been run and passed.

# Your task

Execute the role described above. Before any terminal submission, call ask_advisor with your chosen tool_name and intended payload. Submit your chosen terminal only after the advisor returns "approve".
```
