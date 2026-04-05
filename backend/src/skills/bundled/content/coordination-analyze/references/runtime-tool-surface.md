# Analyze Runtime Tool Surface

Use only these runtime planning tools in `analyze`:

- `query_ci_status`
- `query_workspace_structure`
- `query_symbols`
- `query_symbol_references`
- `read_sandbox_file`
- `query_edit_hotspots`

Meta tools allowed only for loading the skill contract:

- `get_skill_instructions`
- `get_skill_reference`

Do not invent helper names.
Do not call submit tools in this phase.
Prefer the smallest goal-relevant region set that fits within the 6-region budget.
