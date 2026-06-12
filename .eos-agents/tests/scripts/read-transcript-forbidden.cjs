const fs = require("node:fs");
let input = "";
process.stdin.on("data", (chunk) => (input += chunk));
process.stdin.on("end", () => {
  const payload = JSON.parse(input);
  const transcript = fs.readFileSync(payload.run.transcript_path, "utf8");
  if (transcript.includes("FORBIDDEN")) {
    process.stderr.write("transcript said no");
    process.exit(2);
  }
  process.exit(0);
});
