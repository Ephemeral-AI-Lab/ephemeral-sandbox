#!/usr/bin/env node

// TurnCompleted trigger: when the turn count hits 80% of max_turns, remind
// the model to wrap up and submit. Stateless once-per-run via equality with
// the threshold turn, not `>=`.

const fs = require("node:fs");
const p = JSON.parse(fs.readFileSync(0, "utf8"));
const threshold = Math.ceil(p.facts.max_turns * 0.8);
if (p.event === "TurnCompleted" && p.facts.turn === threshold) {
  process.stdout.write(
    JSON.stringify({
      notification:
        `Turn ${p.facts.turn} of ${p.facts.max_turns} (80% of budget). ` +
        `Wrap up and submit via ${p.terminal_tool}.`,
    }),
  );
}
