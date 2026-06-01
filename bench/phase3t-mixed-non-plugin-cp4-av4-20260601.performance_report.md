# Phase 3T Mixed Non-Plugin CP-4/AV-4 Report

- run_id: `local-5db2ff80c11e`
- gate_pass: `True`
- cp4_gate: `True`
- av4_gate: `True`
- audit_events: `2446`
- audit_drop_free: `True`

## Operation Cells

- read_heavy c=1: 2/2 ok, p95=1.943790994118899 ms, conflicts=0
- read_heavy c=3: 6/6 ok, p95=5.142874957527965 ms, conflicts=0
- read_heavy c=5: 10/10 ok, p95=9.233582997694612 ms, conflicts=0
- read_heavy c=10: 20/20 ok, p95=19.356958044227213 ms, conflicts=0
- write_heavy c=1: 2/2 ok, p95=5.939832946751267 ms, conflicts=0
- write_heavy c=3: 6/6 ok, p95=9.499542007688433 ms, conflicts=0
- write_heavy c=5: 10/10 ok, p95=18.63012503599748 ms, conflicts=0
- write_heavy c=10: 20/20 ok, p95=52.24116699537262 ms, conflicts=0
- edit_heavy c=1: 2/2 ok, p95=6.345416011754423 ms, conflicts=0
- edit_heavy c=3: 6/6 ok, p95=10.466624982655048 ms, conflicts=2
- edit_heavy c=5: 10/10 ok, p95=16.637207998428494 ms, conflicts=6
- edit_heavy c=10: 20/20 ok, p95=52.1095409640111 ms, conflicts=10
- conflict_heavy c=1: 2/2 ok, p95=1.6828749794512987 ms, conflicts=1
- conflict_heavy c=3: 6/6 ok, p95=4.3662499519996345 ms, conflicts=6
- conflict_heavy c=5: 10/10 ok, p95=5.2639159839600325 ms, conflicts=10
- conflict_heavy c=10: 20/20 ok, p95=19.63549997890368 ms, conflicts=20
- exec_tty_false c=1: 2/2 ok, p95=46.14820901770145 ms, conflicts=0
- exec_tty_false c=3: 6/6 ok, p95=55.44554098742083 ms, conflicts=0
- exec_tty_false c=5: 10/10 ok, p95=100.1781250233762 ms, conflicts=0
- exec_tty_false c=10: 20/20 ok, p95=177.46487498516217 ms, conflicts=0
- exec_tty_true c=1: 2/2 ok, p95=53.711042040959 ms, conflicts=0
- exec_tty_true c=3: 6/6 ok, p95=69.43779200082645 ms, conflicts=0
- exec_tty_true c=5: 10/10 ok, p95=104.14637497160584 ms, conflicts=0
- exec_tty_true c=10: 20/20 ok, p95=204.25745798274875 ms, conflicts=0
- glob c=1: 2/2 ok, p95=16.62204199237749 ms, conflicts=0
- glob c=3: 6/6 ok, p95=24.104125041048974 ms, conflicts=0
- glob c=5: 10/10 ok, p95=36.35395801393315 ms, conflicts=0
- glob c=10: 20/20 ok, p95=69.77158400695771 ms, conflicts=0
- grep c=1: 2/2 ok, p95=31.03095799451694 ms, conflicts=0
- grep c=3: 6/6 ok, p95=36.58495802665129 ms, conflicts=0
- grep c=5: 10/10 ok, p95=70.19570795819163 ms, conflicts=0
- grep c=10: 20/20 ok, p95=128.49812500644475 ms, conflicts=0
- pty_input c=1: 2/2 ok, p95=159.97550002066419 ms, conflicts=0
- pty_input c=3: 6/6 ok, p95=164.52699998626485 ms, conflicts=0
- pty_input c=5: 10/10 ok, p95=266.14629197865725 ms, conflicts=0
- pty_input c=10: 20/20 ok, p95=444.54916700487956 ms, conflicts=0
- pty_long_session c=1: 2/2 ok, p95=108.78974996739998 ms, conflicts=0
- pty_long_session c=3: 6/6 ok, p95=113.43037500046194 ms, conflicts=0
- pty_long_session c=5: 10/10 ok, p95=171.39470798429102 ms, conflicts=0
- pty_long_session c=10: 20/20 ok, p95=353.3458330202848 ms, conflicts=0
- mixed_shared c=1: 2/2 ok, p95=1.8633329891599715 ms, conflicts=0
- mixed_shared c=3: 6/6 ok, p95=58.60929097980261 ms, conflicts=1
- mixed_shared c=5: 10/10 ok, p95=61.50712497765198 ms, conflicts=2
- mixed_shared c=10: 20/20 ok, p95=110.64416699809954 ms, conflicts=3

## Audit Event Types

{
  "background_tool.cancelled": 38,
  "background_tool.completed": 45,
  "background_tool.input": 38,
  "background_tool.progress": 11,
  "background_tool.started": 76,
  "layer_stack.lease_released": 210,
  "layer_stack.maintenance": 2,
  "occ.conflict": 61,
  "occ.publish": 228,
  "overlay_workspace.cleanup": 210,
  "tool_call.completed": 509,
  "tool_call.finished": 509,
  "tool_call.started": 509
}
