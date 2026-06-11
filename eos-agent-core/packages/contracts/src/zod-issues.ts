import type { z } from "zod";

/** Human-readable one-line summary of a Zod validation failure. */
export function zodIssueSummary(error: z.ZodError): string {
  return error.issues
    .map((issue) =>
      issue.path.length > 0
        ? `${issue.path.map(String).join(".")}: ${issue.message}`
        : issue.message,
    )
    .join("; ");
}
