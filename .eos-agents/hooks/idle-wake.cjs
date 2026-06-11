#!/usr/bin/env node

// IdleParked trigger: a park that outlives timeout_ms gets a reminder
// listing the running sessions; the publish itself wakes the park.
// Unconditional output is correct: being invoked at all proves the run is
// parked past the timeout.

const fs = require("node:fs");
const p = JSON.parse(fs.readFileSync(0, "utf8"));
const running = p.background_sessions.filter((s) => s.status === "running");
process.stdout.write(
  JSON.stringify({
    notification:
      `You have been waiting ${Math.round(p.facts.idle_elapsed_ms / 1000)}s ` +
      `for background work: ${running.map((s) => `${s.type}:${s.id}`).join(", ")}. ` +
      "Choose one: keep waiting (reply without tool calls), inspect with " +
      "list_background_sessions, or cancel with cancel_background_session and proceed.",
  }),
);
