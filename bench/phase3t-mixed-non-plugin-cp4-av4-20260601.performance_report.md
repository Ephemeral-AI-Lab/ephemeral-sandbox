# Phase 3T Mixed Non-Plugin CP-4/AV-4 Report

- run_id: `local-1536290df7dd`
- gate_pass: `True`
- cp4_gate: `True`
- av4_gate: `True`
- audit_events: `2406`
- audit_drop_free: `True`

## Operation Cells

- read_heavy c=1: 2/2 ok, p95=1.9684580038301647 ms, conflicts=0
- read_heavy c=3: 6/6 ok, p95=6.242583971470594 ms, conflicts=0
- read_heavy c=5: 10/10 ok, p95=7.925375015474856 ms, conflicts=0
- read_heavy c=10: 20/20 ok, p95=19.58329201443121 ms, conflicts=0
- write_heavy c=1: 2/2 ok, p95=6.962499988730997 ms, conflicts=0
- write_heavy c=3: 6/6 ok, p95=11.119040951598436 ms, conflicts=0
- write_heavy c=5: 10/10 ok, p95=16.051916987635195 ms, conflicts=0
- write_heavy c=10: 20/20 ok, p95=45.74887495255098 ms, conflicts=0
- edit_heavy c=1: 2/2 ok, p95=6.112959003075957 ms, conflicts=0
- edit_heavy c=3: 6/6 ok, p95=9.902374993544072 ms, conflicts=2
- edit_heavy c=5: 10/10 ok, p95=14.317959023173898 ms, conflicts=6
- edit_heavy c=10: 20/20 ok, p95=43.81466697668657 ms, conflicts=10
- conflict_heavy c=1: 2/2 ok, p95=1.4007919817231596 ms, conflicts=1
- conflict_heavy c=3: 6/6 ok, p95=3.7183749955147505 ms, conflicts=6
- conflict_heavy c=5: 10/10 ok, p95=5.932874977588654 ms, conflicts=10
- conflict_heavy c=10: 20/20 ok, p95=18.029250029940158 ms, conflicts=20
- exec_tty_false c=1: 2/2 ok, p95=46.74416600028053 ms, conflicts=0
- exec_tty_false c=3: 6/6 ok, p95=52.57508304202929 ms, conflicts=0
- exec_tty_false c=5: 10/10 ok, p95=94.93370802374557 ms, conflicts=0
- exec_tty_false c=10: 20/20 ok, p95=192.4367090105079 ms, conflicts=0
- exec_tty_true c=1: 2/2 ok, p95=48.48391702398658 ms, conflicts=0
- exec_tty_true c=3: 6/6 ok, p95=64.4066659733653 ms, conflicts=0
- exec_tty_true c=5: 10/10 ok, p95=98.94279204308987 ms, conflicts=0
- exec_tty_true c=10: 20/20 ok, p95=207.52583298599347 ms, conflicts=0
- glob c=1: 2/2 ok, p95=16.684832982718945 ms, conflicts=0
- glob c=3: 6/6 ok, p95=20.133791025727987 ms, conflicts=0
- glob c=5: 10/10 ok, p95=36.53170797042549 ms, conflicts=0
- glob c=10: 20/20 ok, p95=66.78354198811576 ms, conflicts=0
- grep c=1: 2/2 ok, p95=31.62633301690221 ms, conflicts=0
- grep c=3: 6/6 ok, p95=37.2976660146378 ms, conflicts=0
- grep c=5: 10/10 ok, p95=72.54625001223758 ms, conflicts=0
- grep c=10: 20/20 ok, p95=129.11791598889977 ms, conflicts=0
- pty_input c=1: 2/2 ok, p95=161.58383298898116 ms, conflicts=0
- pty_input c=3: 6/6 ok, p95=166.37912497390062 ms, conflicts=0
- pty_input c=5: 10/10 ok, p95=261.96495903423056 ms, conflicts=0
- pty_input c=10: 20/20 ok, p95=497.4853750318289 ms, conflicts=0
- pty_long_session c=1: 2/2 ok, p95=110.2064159931615 ms, conflicts=0
- pty_long_session c=3: 6/6 ok, p95=114.48220897000283 ms, conflicts=0
- pty_long_session c=5: 10/10 ok, p95=164.15170801337808 ms, conflicts=0
- pty_long_session c=10: 20/20 ok, p95=285.3005829965696 ms, conflicts=0
- mixed_shared c=1: 2/2 ok, p95=2.157083014026284 ms, conflicts=0
- mixed_shared c=3: 6/6 ok, p95=52.39787499886006 ms, conflicts=1
- mixed_shared c=5: 10/10 ok, p95=58.485708956141025 ms, conflicts=2
- mixed_shared c=10: 20/20 ok, p95=85.82595805637538 ms, conflicts=3

## Audit Event Types

{
  "background_tool.cancelled": 38,
  "background_tool.completed": 43,
  "background_tool.input": 38,
  "background_tool.progress": 4,
  "background_tool.started": 76,
  "layer_stack.lease_released": 208,
  "layer_stack.maintenance": 2,
  "occ.conflict": 61,
  "occ.publish": 228,
  "overlay_workspace.cleanup": 208,
  "tool_call.completed": 500,
  "tool_call.finished": 500,
  "tool_call.started": 500
}
