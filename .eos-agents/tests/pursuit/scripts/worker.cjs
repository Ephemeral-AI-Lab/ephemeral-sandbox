const { create_variable_reference_map } = require("./variable_reference_map.cjs");

function get_initial_messages(vars) {
  const user = (text) => ({ role: "user", content: [{ type: "text", text }] });
  const messages = [user("# Pursuit goal\n" + vars.pursuit_goal)];
  messages.push(user("# Current leg goal\n" + (vars.current_leg_goal ?? "")));
  messages.push(user("# Work item title\n" + (vars.work_item_title ?? "")));
  messages.push(user("# Work item spec\n" + (vars.item_spec ?? "")));
  if (vars.dependency_outcomes.length > 0) {
    messages.push(user("# Dependencies\n" + JSON.stringify(vars.dependency_outcomes)));
  }
  messages.push(user("Submit worker outcome for this work item."));
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
