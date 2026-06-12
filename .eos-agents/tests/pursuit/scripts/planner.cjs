const { create_variable_reference_map } = require("./variable_reference_map.cjs");

function get_initial_messages(vars) {
  const user = (text) => ({ role: "user", content: [{ type: "text", text }] });
  const messages = [user("# Pursuit goal\n" + vars.pursuit_goal)];
  if (vars.current_leg_goal === null) {
    messages.push(user("Plan work items for the current leg goal."));
  } else {
    messages.push(user("# Current leg goal\n" + vars.current_leg_goal));
    if (vars.previous_attempt_outcome !== null) {
      messages.push(user("# Previous attempt\n" + JSON.stringify(vars.previous_attempt_outcome)));
    }
    messages.push(user("Submit planner outcome with work items for this leg goal."));
  }
  return messages;
}

let input = "";
process.stdin.on("data", (c) => (input += c));
process.stdin.on("end", () => {
  const ctx = JSON.parse(input);
  const vars = create_variable_reference_map(ctx);
  const initial_messages = get_initial_messages(vars);
  process.stdout.write(JSON.stringify({ initial_messages }));
});
