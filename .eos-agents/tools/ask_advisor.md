---
intent: read_only
terminal: false
hooks: [block_in_isolated_mode]
---
Ask the advisor for a blocking, read-only audit of the terminal submission
you're about to make.

Use this when:
- You're about to call a terminal tool (e.g., `submit_worker_outcome`,
  `submit_plan_outcome`, `submit_root_task_outcome`) and you want a
  second pair of eyes on (1) tool selection and (2) whether the work you've
  done actually supports the payload.
- The submission is high-stakes (closes a goal, marks an attempt
  complete).

Do NOT use for:
- Trivial submissions where the right terminal is unambiguous and the
  work is obvious (e.g., a short summary acknowledging an already-passed
  eval).
- Fixing problems — the advisor only audits and cannot edit. Worker
  agents may apply trivial inline fixes themselves via
  `edit_file`/`write_file` (typo, wrong variable name, single-line
  obvious bug); the advisor's job is to confirm those fixes do not exceed
  that scope before approving a success terminal.

Capabilities and constraints:
- Read-only. The advisor cannot mutate files.
- NOT callable inside an isolated workspace. Because every terminal
  requires a prior advisor approval, terminal submission is impossible
  while isolated — call `exit_isolated_workspace` first, then
  `ask_advisor` and submit your terminal.
- The advisor sees your original task and contract, a filtered version
  of your transcript, the terminal-tool catalog (with each terminal's
  review focus), and the submission you're about to make.
- Lenient approve bar: the advisor approves when your tool choice is
  right and your payload is plausibly supported, even if the work isn't
  pristine. It rejects only on real quality problems (wrong terminal,
  stubs, TODOs, unsupported claims).
- You get back `approve` / `reject` plus a summary covering: tool
  selection, payload-vs-work support, residual risks.

Input shape:
- `tool_name`: the terminal you intend to call.
- `tool_payload`: the exact arguments you'd pass.

Output shape:
- The advisor's summary text, with verdict in metadata.

Common pitfalls:
- Calling `ask_advisor` AFTER submitting the terminal — too late. Call
  BEFORE.
- Ignoring a prior `reject` and re-asking with the same payload — a
  caller that ignores prior feedback warrants a sharper second reject.
