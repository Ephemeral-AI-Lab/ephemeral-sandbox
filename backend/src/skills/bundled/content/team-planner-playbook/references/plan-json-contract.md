# Plan JSON Contract
Use this reference as an optional final helper immediately before calling `submit_plan(...)`. It is intentionally short; it is not a planning guide.

STOP READING AFTER THE CHECKLIST AND CALL THE TOOL. After this reference loads, your next assistant turn must contain one `submit_plan(...)` tool call and no text block. Do not write a recap, checklist, JSON preview, task list, or "let me call submit_plan now" sentence. That sentence is a known failure pattern because it ends the turn without the terminal tool call. If the payload is incomplete, still call `submit_plan(...)` with the best valid payload you can defend now.

## Task/Goal

- You already have the owner ledger, deps, and task prose. Use the `submit_plan` tool schema directly if you do not need this final helper.
- Your only remaining work is putting the already-decided tasks into the tool input.

## Avoid

- Do not summarize what you will submit.
- Do not list task ids in text.
- Do not say "the plan is ready", "let me submit", or "let me call submit_plan now".
- Do not make another tool call except `submit_plan(...)`.
- Do not include `task_note`, `background`, `parent_id`, `rationale`, or `output: null`.

## Workflow

Call `submit_plan(new_tasks=[...])` now.

Tool input checklist:

- Top-level keys: `new_tasks` and optional string `output` only.
- Each task item keys: `id`, `description`, `name`, `spec`, `deps`, `scope_paths`.
- `name` is an exact registered agent name such as `developer`, `validator`, or `team_planner`.
- `description` is a short label under about 10 words.
- `deps` is a top-level task field and every `id` is unique.
- For a `validator` task, `deps` must be non-empty and contain every same-layer non-validator sibling id in this `submit_plan` payload, including `developer` lanes and child `team_planner` lanes. Mentioning dependencies inside `spec` does not set task deps.
- `spec` is the briefing and uses numbered colon labels in this exact order: `1. Goal:`, `2. Environment:`, `3. Scope:`, `4. Context:`, `5. Acceptance Criteria:`. Do not use Markdown headings.
- `scope_paths` uses live-confirmed production owner paths, or a broader production boundary on `team_planner` when exact ownership is still uncertain. Keep verification-only test targets in `spec` context or acceptance criteria unless the task explicitly owns a test-only bug.
- At most one terminal validator is present. Never submit it with `deps: []` when the plan has non-validator siblings.
- If the plan includes child planners like `plan-parquet` or `plan-groupby`, the terminal validator's `deps` must include those ids as well as direct developer ids.

## Expected Outcome

- The next assistant response is the terminal `submit_plan(...)` tool call, not prose.
- No visible or hidden text block says what you are about to submit.
- The payload passes the checklist above.
