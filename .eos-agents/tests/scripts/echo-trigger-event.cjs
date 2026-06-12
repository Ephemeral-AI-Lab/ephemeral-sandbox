let input = "";
process.stdin.on("data", (chunk) => (input += chunk));
process.stdin.on("end", () => {
  const payload = JSON.parse(input);
  console.log(
    JSON.stringify({ notification: payload.event + ":" + payload.terminal_tool }),
  );
});
