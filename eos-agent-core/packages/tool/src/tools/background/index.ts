import type { BackgroundSessionSupervisor } from "@eos/background";

import type { ToolDefinition } from "../../contract.js";
import { cancelBackgroundSessionTool } from "./cancel-background-session.js";
import { listBackgroundSessionsTool } from "./list-background-sessions.js";

/** The family's name universe: static, so profile validation needs no service. */
export const BACKGROUND_TOOL_NAMES = [
  "list_background_sessions",
  "cancel_background_session",
] as const;

/** The background family: list + cancel, closed over the supervisor. */
export function backgroundTools(supervisor: BackgroundSessionSupervisor): ToolDefinition[] {
  return [
    listBackgroundSessionsTool(supervisor),
    cancelBackgroundSessionTool(supervisor),
  ];
}
