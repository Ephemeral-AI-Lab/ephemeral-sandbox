const fs = require("node:fs");
let input = "";
process.stdin.on("data", (chunk) => (input += chunk));
process.stdin.on("end", () => {
  const payload = JSON.parse(input);
  const transcript = fs.readFileSync(payload.run.transcript_path, "utf8");
  if (transcript.includes('"read_note"')) {
    process.stdout.write(
      JSON.stringify({ decision: "allow", additionalContext: "note was read before writing" }),
    );
    process.exit(0);
  }
  process.stderr.write("write_note requires reading the note first");
  process.exit(2);
});
