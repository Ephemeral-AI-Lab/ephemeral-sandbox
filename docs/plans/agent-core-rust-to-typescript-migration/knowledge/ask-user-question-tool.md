# Claude Code AskUserQuestion Tool (Question/Answer via the Permission Layer)

Status: Observed
Date: 2026-06-11
Source path: `/Users/yifanxu/machine_learning/LoVC/c c/src`
Migration context: `eos-agent-core/` TypeScript migration reference

## The Design Trick: There Is No Answer Tool

The question→answer round-trip rides entirely on the permission system. The
tool itself is almost a no-op; the dialog does the work by mutating the tool
call's own input before execution.

```
model calls AskUserQuestion({questions})
  ├─ checkPermissions() always returns behavior: 'ask'
  │      (AskUserQuestionTool.tsx ~:182 — the dialog IS the question UI)
  ├─ AskUserQuestionPermissionRequest.tsx renders options/previews/multiselect
  │      user picks answers (+ free-text "Other", notes, pasted images)
  ├─ onAllow(updatedInput) where updatedInput = {questions, answers, annotations}
  │      (AskUserQuestionPermissionRequest.tsx:398-407 — answers written INTO
  │       the tool input; `answers`/`annotations` exist on the INPUT schema
  │       precisely for this injection)
  ├─ call() is a pure echo: returns {questions, answers, annotations}
  └─ mapToolResultToToolResultBlockParam → tool_result text:
       'User has answered your questions: "Q"="A" selected preview:… user notes:…'
```

There is no `AnswerQuestion`/`answer_question` tool anywhere in the tree.
Rejection renders "User declined to answer questions". SDK/headless consumers
get the same contract through the `canUseTool` permission callback returning
`updatedInput` with `answers` filled in.

## Input/Output Schema (`tools/AskUserQuestionTool/AskUserQuestionTool.tsx`)

| Field | Shape | Notes |
| --- | --- | --- |
| `questions` | 1–4 of `{question, header, options, multiSelect}` | `header` is a chip, max 12 chars (`ASK_USER_QUESTION_TOOL_CHIP_WIDTH`) |
| `options` | 2–4 of `{label, description, preview?}` | no "Other" option — UI adds it automatically |
| `multiSelect` | boolean, default false | multi-select answers join as comma-separated string |
| `answers` | `Record<question, answer>` (input + output) | filled by the permission component, not the model |
| `annotations` | per-question `{preview?, notes?}` | selected preview + user free-text notes |
| `metadata.source` | optional string | analytics only (e.g. `"remember"`) |

A Zod `.refine` (`UNIQUENESS_REFINE`, `:32-54`) enforces unique question texts
and unique option labels per question — answers are keyed by question text, so
duplicates would collide.

Runtime metadata: `isReadOnly: true`, `isConcurrencySafe: true`,
`shouldDefer: true`, and `requiresUserInteraction: true` — the last is the
load-bearing flag the permission layer uses for "must have a human at the
keyboard" routing (`utils/permissions/permissions.ts:549,1232`).

## Preview Formats

`prompt()` appends format-specific guidance based on
`getQuestionPreviewFormat()` (`prompt.ts:10-30`):

- `markdown` — TUI monospace box; side-by-side layout when any option has a
  preview; single-select only.
- `html` — for SDK hosts that `innerHTML` the preview. `validateInput`
  (`:158-176` + `validateHtmlPreview`) rejects `<html>/<body>/<!DOCTYPE>`
  (fragment only), `<script>/<style>` (inline styles only), and non-HTML text.
  Comment notes this is an intent check, not a parser — HTML5 parsers accept
  anything.
- `undefined` — SDK consumer that hasn't opted in; preview guidance omitted
  entirely so the model never emits the field.

## Who Can Ask Questions: Main Thread Only

Enforcement is `ALL_AGENT_DISALLOWED_TOOLS` (`constants/tools.ts:37-47`),
applied in `filterToolsForAgent` (`tools/AgentTool/agentToolUtils.ts:94`)
*before* the in-process-teammate carve-outs (`:101-110`, which only rescue
tools from the async filter). The only bypass in that function is
ExitPlanModeV2 in plan mode; AskUserQuestion has none.

| Context | AskUserQuestion? | Mechanism |
| --- | --- | --- |
| Main thread | yes | `resolveAgentTools(isMainThread: true)` skips filtering entirely (`agentToolUtils.ts:137-142`) |
| Built-in subagents | no | `ALL_AGENT_DISALLOWED_TOOLS` |
| Custom agents | no | `CUSTOM_AGENT_DISALLOWED_TOOLS` ⊇ same set |
| Async/background agents | no | disallow list + absent from `ASYNC_AGENT_ALLOWED_TOOLS` |
| In-process teammates | no | absent from `IN_PROCESS_TEAMMATE_ALLOWED_TOOLS`; disallow check fires first |
| Coordinator mode | no | `COORDINATOR_MODE_ALLOWED_TOOLS` = Agent, TaskStop, SendMessage, SyntheticOutput only |
| Main thread with `--channels` | no | `isEnabled()` returns false (`AskUserQuestionTool.tsx:135-145`) — user is on Telegram/Discord, dialog would hang; channel permission relay already skips `requiresUserInteraction()` tools |

Substitute paths for non-main agents: teammates `SendMessage` the lead (the
only agent allowed to turn around and ask the human); teammate *permission*
prompts relay to the leader's dialog via `leaderPermissionBridge.ts`; teammate
plan approval relays via ExitPlanModeV2 + `plan_approval_response`.

## Plan-Mode Prompt Coupling

The tool prompt (`prompt.ts:43`) and plan-mode reminders
(`utils/messages.ts:3282-3392`) hard-code a division of labor: AskUserQuestion
is for clarifying requirements/choosing approaches; ExitPlanModeV2 is the only
legal way to ask for plan approval (the user cannot see the plan until that
call). Plan-mode turns are told to end ONLY in one of those two tools.

## EOS Migration Takeaways

- The "answer is injected into the tool's own input by the approval layer"
  pattern collapses what would otherwise be a two-tool ask/answer protocol
  plus a pending-question state machine into one round-trip. If EOS adds an
  interactive question op, reuse the permission/approval channel and an
  `updatedInput` contract rather than inventing a reply op.
- Keeping `answers` on the *input* schema (documented as "collected by the
  permission component") makes the injection contract explicit and keeps the
  tool pure/replayable — `call()` never blocks on UI.
- Gate interactivity on one runtime predicate (`requiresUserInteraction()`)
  instead of per-tool special cases; the headless/channels guard then becomes
  a generic "disable interactive tools when nobody is at the keyboard" rule.
- Question-text-as-answer-key requires the uniqueness refine; typed IDs per
  question would be the EOS-native alternative.
