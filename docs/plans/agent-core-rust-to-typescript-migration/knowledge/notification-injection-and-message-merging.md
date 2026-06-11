# Claude Code Notification Injection and Message Merging

Status: Observed
Date: 2026-06-11
Source path: `/Users/yifanxu/machine_learning/LoVC/c c/src`
Migration context: `eos-agent-core/` TypeScript migration reference —
Phase 04.9 notification trigger engine (does a multi-notification boundary
become one message or many?)

## The Question This Answers

When several machine-injected notices (reminders, file freshness, hook
context, memory) land at the same turn boundary, Claude Code does NOT send
them as separate user messages. It aggregates in two escalating passes, and
the second pass exists because of a measured model failure, not taste.

## Pass 0: Every Notice Is Born a `<system-reminder>` User Message

- `normalizeAttachmentForAPI` (`utils/messages.ts:3453`) maps each
  attachment (their notification analog: teammate mailbox, team context,
  skill discovery, directory listings, memory, hook `additionalContext`) to
  one or more standalone `UserMessage`s.
- `wrapInSystemReminder` (`messages.ts:3098`) produces the
  `<system-reminder>\n...\n</system-reminder>` envelope;
  `ensureSystemReminderWrap` (`messages.ts:1796`) re-wraps any text block
  that escaped, because the literal prefix `startsWith('<system-reminder>')`
  is the discriminator every later pass keys on. The EOS analog is the
  `<system_notification>{json}</system_notification>` envelope rendered at
  publish time by `systemNotificationMessage`.

## Pass 1: Adjacent User Messages Merge Into One API Message

- At reassembly, an attachment landing after an existing user message is
  folded into it immediately (`messages.ts:2269-2291`, reduce over
  `mergeUserMessagesAndToolResults`), and a final
  `mergeAdjacentUserMessages` sweep (`messages.ts:2451`) guarantees the API
  payload never carries two consecutive user messages.
- Merging creates shape obligations the pass then sands off:
  - `hoistToolResults` (`messages.ts:2470`) moves `tool_result` blocks to
    the front of the merged content — the API rejects a `tool_result` that
    does not directly answer the preceding `tool_use`.
  - `joinTextAtSeam` (`messages.ts:2505`) appends `\n` to the left block at
    a text-text seam: the API concatenates adjacent text blocks without a
    separator, so two queued prompts `"2 + 2"` + `"3 + 3"` once reached the
    model as `"2 + 23 + 3"`. The newline goes on the LEFT side so the right
    block's `startsWith('<system-reminder>')` classification survives.

## Pass 2: The Smoosh — Reminder Text Folded INSIDE `tool_result`

- `smooshSystemReminderSiblings` (`messages.ts:1837`, gated
  `tengu_chair_sermon`): in any user message that contains a `tool_result`,
  every `<system-reminder>`-prefixed text sibling is removed and folded into
  the LAST `tool_result`'s content via `smooshIntoToolResult`
  (`messages.ts:2534`).
- Why: issue #21049 — bare reminder text adjacent to tool results created an
  "anomalous two-consecutive-human-turns pattern that teaches the model to
  emit the stop sequence after tool results". An A/B
  (`sai-20260310-161901`, Arm B) measured the smoosh driving that failure
  mode to 0%.
- Real user input is deliberately exempt: "a Human: boundary before actual
  user input is semantically correct" (`messages.ts:1827-1832`). Only
  machine-injected reminders lose their message identity; steering never
  does.
- Constraints the fold respects (`messages.ts:2522-2545` region):
  `tool_reference` blocks cannot mix with other content (server error →
  bail, leave the sibling alone), and `is_error` tool_results accept only
  text blocks — `sanitizeErrorToolResultContent` (`messages.ts:1883`) is the
  read-side guard for transcripts persisted before the fold learned that,
  without which a resumed session 400s forever.

## Pipeline Order (and Its Cost)

```
attachments → normalizeAttachmentForAPI → ensureSystemReminderWrap
  → merge-at-append (2269) → relocateToolReferenceSiblings (gated)
  → orphan-thinking / whitespace / non-empty filters
  → mergeAdjacentUserMessages → smooshSystemReminderSiblings (gated)
  → sanitizeErrorToolResultContent → ID tagging
```

The file's own comment (`messages.ts:~2320`): "These multi-pass
normalizations are inherently fragile — each pass can create conditions a
prior pass was meant to handle. Consider unifying into a single pass." This
is the complexity bill aggregation paid.

## EOS Comparison and Takeaway

| | EOS (Phase 04.9, current) | Claude Code |
| --- | --- | --- |
| Unit of delivery | One user message per notification, appended at the drain in `runAgentLoop` | One user message per boundary; reminder text folded inside an adjacent `tool_result` when one exists |
| Ordering | Steers drain first, then notifications in publish order | Real user input keeps its own Human boundary; reminders lose message identity |
| Dedup | Inbox `publish(message, {key})` replaces a pending same-key entry (used by `session_settled`) | Smoosh + merge collapse shape, not content |
| Evidence | Live codex (gpt-5.5) e2e green with consecutive user messages, incl. two same-boundary reminders | A/B-proven necessary on Claude models at scale |

- EOS's post-tool-turn shape — `user(tool_results)` followed by
  `user(notification)` — is exactly the consecutive-human-turns pattern
  #21049 measured as harmful on Claude models. It is demonstrably fine on
  the codex stack (live suites pass; the model acts on the reminders), so
  EOS keeps the simple shape for now.
- Pinned EOS behavior: `packages/agent-runtime/tests/runtime.test.ts`
  ("reminds once per consecutive bare-text turn and drains same-boundary
  rule answers as separate messages") asserts two same-boundary rule
  answers arrive as two separate user messages in one provider request.
- Revisit trigger: an EOS run on the anthropic-messages wire showing
  premature stops after tool turns. The known fix is small and local
  because the inbox stores already-rendered messages: aggregate at the
  drain site in `runAgentLoop` (fold drained notifications into the
  preceding tool-result user message, or one merged user message per
  boundary) — one location, no publisher changes. Do not import the
  multi-pass normalizer; EOS controls its message assembly at exactly one
  seam, which is the structural advantage to preserve.
