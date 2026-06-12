let input = "";
process.stdin.on("data", (c) => (input += c));
process.stdin.on("end", () => {
  const ctx = JSON.parse(input);
  const text = "pursuit goal: " + ctx.pursuit_context.pursuit.goal;
  process.stdout.write(
    JSON.stringify({
      initial_messages: [{ role: "user", content: [{ type: "text", text }] }],
    }),
  );
});
