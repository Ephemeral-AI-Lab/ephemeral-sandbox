let input = "";
process.stdin.on("data", (chunk) => (input += chunk));
process.stdin.on("end", () => {
  const payload = JSON.parse(input);
  process.stderr.write(
    [
      payload.event,
      payload.tool_name,
      payload.tool_use_id,
      payload.run.kind,
      String(payload.run.workspace.is_isolated),
    ].join("|"),
  );
  process.exit(2);
});
