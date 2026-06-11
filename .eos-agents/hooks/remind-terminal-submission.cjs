#!/usr/bin/env node

// TurnCompleted trigger: the spin rescue. A no-tool-call turn with no
// background sessions and no pending steers gets a reminder naming the
// run's terminal tool. The `live_sessions === 0` check is load-bearing: it
// is the same fact the engine's park gate reads, so script and engine
// classify the turn identically (sessions live: the engine parks and
// idle-wake owns it; none: this script speaks).

const fs = require("node:fs");
const p = JSON.parse(fs.readFileSync(0, "utf8"));
if (
  p.event === "TurnCompleted" &&
  p.facts.tool_calls === 0 &&
  p.facts.live_sessions === 0 &&
  !p.facts.has_pending_steers
) {
  process.stdout.write(
    JSON.stringify({
      notification:
        "You produced no tool call and have no background work. " +
        `To finish this run you must call your terminal tool ${p.terminal_tool}.`,
    }),
  );
}
