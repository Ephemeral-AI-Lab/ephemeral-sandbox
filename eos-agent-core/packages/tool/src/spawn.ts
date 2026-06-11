import { spawn } from "node:child_process";

const DEFAULT_COMMAND_TIMEOUT_MS = 60_000;

/** How one spawned command settled; mapping to a protocol is the caller's. */
export type SpawnedCommandResult =
  | { kind: "spawn_error"; message: string }
  | { kind: "aborted" }
  | { kind: "exited"; code: number | null; stdout: string; stderr: string };

/**
 * The command mechanics shared by tool hooks and notification rules: spawn with
 * `shell: true`, payload JSON + newline on stdin, lifetime bounded by the
 * optional caller signal plus the per-command timeout. First settle wins;
 * a synchronous `spawn()` fault rejects and is the caller's to map.
 */
export function spawnJsonCommand(
  command: { command: string; cwd?: string; timeout_ms?: number },
  payload: unknown,
  signal?: AbortSignal,
): Promise<SpawnedCommandResult> {
  const timeout = AbortSignal.timeout(command.timeout_ms ?? DEFAULT_COMMAND_TIMEOUT_MS);
  const commandSignal = signal ? AbortSignal.any([signal, timeout]) : timeout;
  return new Promise((resolve) => {
    const child = spawn(command.command, {
      shell: true,
      signal: commandSignal,
      ...(command.cwd !== undefined && { cwd: command.cwd }),
    });
    let stdout = "";
    let stderr = "";
    child.stdout.setEncoding("utf8");
    child.stderr.setEncoding("utf8");
    child.stdout.on("data", (chunk: string) => (stdout += chunk));
    child.stderr.on("data", (chunk: string) => (stderr += chunk));
    child.on("error", (error) => {
      // An aborted signal kills the child through this same event (Node
      // raises AbortError here, before "close"), so classify by the signal:
      // a timeout or caller abort is "aborted", never a spawn fault.
      resolve(
        commandSignal.aborted
          ? { kind: "aborted" }
          : { kind: "spawn_error", message: error.message },
      );
    });
    child.on("close", (code) => {
      resolve(
        commandSignal.aborted
          ? { kind: "aborted" }
          : { kind: "exited", code, stdout, stderr },
      );
    });
    // EPIPE from a command that exits without reading stdin is not an error.
    child.stdin.on("error", () => undefined);
    child.stdin.write(`${JSON.stringify(payload)}\n`);
    child.stdin.end();
  });
}
