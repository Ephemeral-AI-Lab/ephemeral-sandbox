# Phase 3T Mixed Non-Plugin CP-4/AV-4 Report

- run_id: `local-c8cb1b3c1a18`
- gate_pass: `False`
- cp4_gate: `False`
- av4_gate: `False`
- audit_events: `69`
- audit_drop_free: `True`

## Operation Cells

- read_heavy c=1: 1/1 ok, p95=1.9424579804763198 ms, conflicts=0
- write_heavy c=1: 1/1 ok, p95=5.046874983236194 ms, conflicts=0
- edit_heavy c=1: 1/1 ok, p95=5.949791986495256 ms, conflicts=0
- conflict_heavy c=1: 1/1 ok, p95=4.560250032227486 ms, conflicts=0
- exec_tty_false c=1: 1/1 ok, p95=53.76762500964105 ms, conflicts=0
- exec_tty_true c=1: 1/1 ok, p95=51.259208004921675 ms, conflicts=0
- glob c=1: 1/1 ok, p95=16.443624976091087 ms, conflicts=0
- grep c=1: 1/1 ok, p95=29.765333980321884 ms, conflicts=0
- pty_input c=1: 1/1 ok, p95=160.89262499008328 ms, conflicts=0
- pty_long_session c=1: 1/1 ok, p95=111.65074998280033 ms, conflicts=0
- mixed_shared c=1: 1/1 ok, p95=1.5334159834310412 ms, conflicts=0

## Audit Event Types

{
  "background_tool.cancelled": 1,
  "background_tool.completed": 1,
  "background_tool.input": 1,
  "background_tool.started": 2,
  "layer_stack.lease_released": 5,
  "layer_stack.maintenance": 2,
  "occ.publish": 7,
  "overlay_workspace.cleanup": 5,
  "tool_call.completed": 15,
  "tool_call.finished": 15,
  "tool_call.started": 15
}
