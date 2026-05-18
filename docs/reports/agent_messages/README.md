# Agent message captures

Source run: `.sweevo_runs/scenario_logs/pipeline.first_three_messages_capture/20260517T205735Z_a4b65ba5c1ae`

Per-agent initial rows extracted from `message.jsonl`. Launch shapes (Round 3):

* **planner** — 4 rows (system + context + role_instruction + skill).
* **executor / evaluator** — 3 rows; no skill in v1.
* **entry_executor** — 2 rows; single-user-message launch.

## Captures

| Agent | Iteration | Attempt | Rows |
|---|---|---|---|
| entry_executor | — | — | 2 |
| planner | iteration_01_fa9d8064-8500-4ae8-a142-f37a8a77fef5 | attempt_01_ef10a2e0-73c2-4b91-99b5-646d3d4e4a20 | 4 |
| planner | iteration_01_fa9d8064-8500-4ae8-a142-f37a8a77fef5 | attempt_02_42d053a1-e813-414a-992a-fb27b4ed3fd1 | 4 |
| executor | iteration_01_fa9d8064-8500-4ae8-a142-f37a8a77fef5 | attempt_02_42d053a1-e813-414a-992a-fb27b4ed3fd1 | 3 |
| evaluator | iteration_01_fa9d8064-8500-4ae8-a142-f37a8a77fef5 | attempt_02_42d053a1-e813-414a-992a-fb27b4ed3fd1 | 3 |
| planner | iteration_02_2383cf05-a7ce-4301-ba09-6d42ce778267 | attempt_01_c2897108-3954-4525-9864-a19478260a1e | 4 |
| executor | iteration_02_2383cf05-a7ce-4301-ba09-6d42ce778267 | attempt_01_c2897108-3954-4525-9864-a19478260a1e | 3 |
| evaluator | iteration_02_2383cf05-a7ce-4301-ba09-6d42ce778267 | attempt_01_c2897108-3954-4525-9864-a19478260a1e | 3 |
