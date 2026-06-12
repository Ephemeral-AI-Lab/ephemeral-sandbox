import { z } from "zod";

import { MessageSchema } from "./messages.js";

// --- entity ids ---------------------------------------------------------------

export const PursuitIdSchema = z.string().min(1).brand<"PursuitId">();
export type PursuitId = z.infer<typeof PursuitIdSchema>;

export function mintPursuitId(): PursuitId {
  return PursuitIdSchema.parse(crypto.randomUUID());
}

export function pursuitIdFrom(raw: string): PursuitId {
  return PursuitIdSchema.parse(raw);
}

export const LegIdSchema = z.string().min(1).brand<"LegId">();
export type LegId = z.infer<typeof LegIdSchema>;

export function mintLegId(): LegId {
  return LegIdSchema.parse(crypto.randomUUID());
}

export function legIdFrom(raw: string): LegId {
  return LegIdSchema.parse(raw);
}

export const AttemptIdSchema = z.string().min(1).brand<"AttemptId">();
export type AttemptId = z.infer<typeof AttemptIdSchema>;

export function mintAttemptId(): AttemptId {
  return AttemptIdSchema.parse(crypto.randomUUID());
}

export function attemptIdFrom(raw: string): AttemptId {
  return AttemptIdSchema.parse(raw);
}

export const PlanIdSchema = z.string().min(1).brand<"PlanId">();
export type PlanId = z.infer<typeof PlanIdSchema>;

export function mintPlanId(): PlanId {
  return PlanIdSchema.parse(crypto.randomUUID());
}

export function planIdFrom(raw: string): PlanId {
  return PlanIdSchema.parse(raw);
}

export const WorkItemIdSchema = z.string().min(1).brand<"WorkItemId">();
export type WorkItemId = z.infer<typeof WorkItemIdSchema>;

export function workItemIdFrom(raw: string): WorkItemId {
  return WorkItemIdSchema.parse(raw);
}

// --- status -------------------------------------------------------------------

export const PursuitEntityRunStatusSchema = z.enum([
  "NotStarted",
  "Running",
  "Success",
  "Failed",
  "Cancelled",
]);
export type PursuitEntityRunStatus = z.infer<typeof PursuitEntityRunStatusSchema>;

export const WorkItemRunStatusSchema = z.enum([
  "NotStarted",
  "Running",
  "Success",
  "Failed",
  "Blocked",
  "Cancelled",
]);
export type WorkItemRunStatus = z.infer<typeof WorkItemRunStatusSchema>;

export type PursuitContextEntityStatus =
  | PursuitEntityRunStatus
  | WorkItemRunStatus;

export type PursuitTerminalStatus = Extract<
  PursuitEntityRunStatus,
  "Success" | "Failed" | "Cancelled"
>;

export function isPursuitEntityTerminal(
  status: PursuitEntityRunStatus,
): status is PursuitTerminalStatus {
  return status === "Success" || status === "Failed" || status === "Cancelled";
}

export function isWorkItemTerminal(status: WorkItemRunStatus): boolean {
  return (
    status === "Success" ||
    status === "Failed" ||
    status === "Blocked" ||
    status === "Cancelled"
  );
}

// --- creation and submission payloads -----------------------------------------

export const LegGoalModeSchema = z.enum(["dynamic", "predefined"]);
export type LegGoalMode = z.infer<typeof LegGoalModeSchema>;

const NonEmptyStringListSchema = z.tuple([z.string().min(1)], z.string().min(1));

export const CreatePursuitInputSchema = z
  .strictObject({
    pursuit_goal: z.string().min(1),
    leg_goal_mode: LegGoalModeSchema.optional(),
    leg_goals: NonEmptyStringListSchema.optional(),
  })
  .superRefine((payload, ctx) => {
    const derived = payload.leg_goals === undefined ? "dynamic" : "predefined";
    if (payload.leg_goal_mode !== undefined && payload.leg_goal_mode !== derived) {
      ctx.addIssue({
        code: "custom",
        path: ["leg_goal_mode"],
        message: `leg_goal_mode ${payload.leg_goal_mode} does not match ${derived} payload shape`,
      });
    }
  });
export type CreatePursuitInput = z.infer<typeof CreatePursuitInputSchema>;

export const DelegatePursuitInputSchema = CreatePursuitInputSchema.extend({
  max_attempts: z.number().int().positive().optional(),
});
export type DelegatePursuitInput = z.infer<typeof DelegatePursuitInputSchema>;

export const PlannerWorkItemSpecSchema = z.strictObject({
  id: z.string().min(1),
  agent_name: z.string().min(1),
  title: z.string().min(1),
  spec: z.string().min(1),
  depends_on: z.array(z.string().min(1)).default([]),
});
export type PlannerWorkItemSpec = z.infer<typeof PlannerWorkItemSpecSchema>;

export const PlannerOutcomePayloadSchema = z.strictObject({
  summary: z.string().min(1),
  leg_goal: z.string().min(1).optional(),
  next_leg_goal: z.string().min(1).optional(),
  work_items: z.array(PlannerWorkItemSpecSchema).min(1),
});
export type PlannerOutcomePayload = z.infer<typeof PlannerOutcomePayloadSchema>;

export const WorkerOutcomePayloadSchema = z.strictObject({
  summary: z.string().min(1),
  is_pass: z.boolean(),
  outcome: z.string().min(1),
});
export type WorkerOutcomePayload = z.infer<typeof WorkerOutcomePayloadSchema>;

// --- context read DTOs ---------------------------------------------------------

export interface ContextPage {
  path: string;
  status: PursuitContextEntityStatus;
  total_bytes: number;
  offset: number;
  content: string;
  next_offset?: number;
}

export interface ContextSearch {
  files: readonly { path: string; status: PursuitContextEntityStatus }[];
  matches: readonly {
    path: string;
    status: PursuitContextEntityStatus;
    field: string;
    snippet: string;
  }[];
  truncated?: string;
}

// --- context-script IO ---------------------------------------------------------

export const PursuitContextWorkItemSchema = z.strictObject({
  id: z.string(),
  agent_name: z.string(),
  title: z.string(),
  spec: z.string(),
  depends_on: z.array(z.string()),
  status: WorkItemRunStatusSchema,
  summary: z.string().nullable(),
  outcome: z.string().nullable(),
  agent_run_id: z.string().nullable(),
  context_path: z.string(),
  leg_goal_version: z.number().int().positive(),
});
export type PursuitContextWorkItem = z.infer<typeof PursuitContextWorkItemSchema>;

export const AttemptFailureReasonSchema = z.strictObject({
  work_item_id: z.string().nullable(),
  kind: z.enum([
    "planner_failed",
    "context_composition_failed",
    "failed",
    "blocked_by_failed_dependency",
  ]),
  message: z.string().nullable(),
  summary: z.string().nullable(),
  outcome: z.string().nullable(),
  blocked_by: z.array(z.string()).optional(),
});
export type AttemptFailureReason = z.infer<typeof AttemptFailureReasonSchema>;

export const PursuitContextPlanSchema = z.strictObject({
  id: z.string(),
  status: PursuitEntityRunStatusSchema,
  declared_leg_goal: z.string().nullable(),
  declared_next_leg_goal: z.string().nullable(),
  summary: z.string().nullable(),
  agent_run_id: z.string().nullable(),
  leg_goal_version: z.number().int().positive(),
});
export type PursuitContextPlan = z.infer<typeof PursuitContextPlanSchema>;

export const PursuitContextAttemptSchema = z.strictObject({
  id: z.string(),
  sequence: z.number().int(),
  status: PursuitEntityRunStatusSchema,
  failure_reasons: z.array(AttemptFailureReasonSchema),
  is_consistent_with_leg_goal: z.boolean(),
  context_path: z.string(),
  outcome: z.string().nullable(),
  leg_goal_version: z.number().int().positive(),
  plan: PursuitContextPlanSchema,
  work_items: z.array(PursuitContextWorkItemSchema),
});
export type PursuitContextAttempt = z.infer<typeof PursuitContextAttemptSchema>;

export const PursuitContextLegSchema = z.strictObject({
  id: z.string(),
  sequence: z.number().int(),
  origin: z.enum(["initial", "next_leg_goal", "predefined"]),
  status: PursuitEntityRunStatusSchema,
  leg_goal: z.string(),
  leg_goal_version: z.number().int().positive(),
  leg_goal_provenance: z.string(),
  is_leg_goal_mutatable: z.boolean(),
  next_leg_goal: z.string().nullable(),
  max_attempts: z.number().int(),
  context_path: z.string(),
  outcome: z.string().nullable(),
  attempts: z.array(PursuitContextAttemptSchema),
});
export type PursuitContextLeg = z.infer<typeof PursuitContextLegSchema>;

export const PursuitContextSnapshotSchema = z.strictObject({
  pursuit: z.strictObject({
    id: z.string(),
    goal: z.string(),
    leg_goal_mode: LegGoalModeSchema,
    predefined_leg_count: z.number().int().nonnegative().nullable(),
    status: PursuitEntityRunStatusSchema,
    context_path: z.string(),
    outcome: z.string().nullable(),
    legs: z.array(PursuitContextLegSchema),
  }),
});
export type PursuitContextSnapshot = z.infer<typeof PursuitContextSnapshotSchema>;

export const PlannerContextInputSchema = z.strictObject({
  kind: z.literal("planner"),
  pursuit_context: PursuitContextSnapshotSchema,
  current: z.strictObject({
    pursuit_id: z.string(),
    leg_id: z.string(),
    attempt_id: z.string(),
    plan_id: z.string(),
  }),
});
export type PlannerContextInput = z.infer<typeof PlannerContextInputSchema>;

export const WorkerContextInputSchema = z.strictObject({
  kind: z.literal("worker"),
  pursuit_context: PursuitContextSnapshotSchema,
  current: z.strictObject({
    pursuit_id: z.string(),
    leg_id: z.string(),
    attempt_id: z.string(),
    work_item_id: z.string(),
  }),
});
export type WorkerContextInput = z.infer<typeof WorkerContextInputSchema>;

export const InitialUserMessageSchema = MessageSchema.extend({
  role: z.literal("user"),
});
export type InitialUserMessage = z.infer<typeof InitialUserMessageSchema>;

export const ContextScriptOutputSchema = z.strictObject({
  initial_messages: z.array(InitialUserMessageSchema).min(1),
});
export type ContextScriptOutput = z.infer<typeof ContextScriptOutputSchema>;

// --- service-facing contracts --------------------------------------------------

export interface PursuitSettlement {
  status: PursuitTerminalStatus;
  summary: string;
}

export interface PursuitHandle {
  pursuit_id: PursuitId;
  cancel(reason?: string): Promise<void>;
  settle(): Promise<PursuitSettlement>;
}

export type SubmissionResult = { ok: true } | { ok: false; error: string };

export type PursuitAgentSubmissionBinding =
  | {
      kind: "planner";
      submit(payload: PlannerOutcomePayload): Promise<SubmissionResult>;
    }
  | {
      kind: "worker";
      submit(payload: WorkerOutcomePayload): Promise<SubmissionResult>;
    };
