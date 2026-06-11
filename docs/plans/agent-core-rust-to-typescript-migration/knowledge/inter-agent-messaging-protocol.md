# Claude Code Inter-Agent Messaging Protocol (SendMessage, Mailboxes, Team Tools)

Status: Observed
Date: 2026-06-11
Source path: `/Users/yifanxu/machine_learning/LoVC/c c/src`
Migration context: `eos-agent-core/` TypeScript migration reference

Companion docs: `subagent-state-management.md` (inbox *state* table and drain
points), `task-list-tools-and-notifications.md` (task tool storage and idle
loops), `message-steering.md` (queue drains). This doc covers the protocol
surface: SendMessage routing, the wire shapes, and the per-role tool matrix.

## The One Rule

An agent's plain text output is invisible to every other agent. The only
inter-agent channels are the `SendMessage` tool and the shared task list. The
SendMessage prompt states this verbatim and adds: messages from teammates are
delivered automatically (no inbox-checking op), refer to teammates by name
never UUID, and broadcast is "expensive (linear in team size)"
(`tools/SendMessageTool/prompt.ts:34-36`).

## SendMessage Signature (`SendMessageTool.ts:68-90`)

```
{
  to:      string   // teammate name | "*" broadcast | agent name/agentId
                    // | "uds:/path.sock" | "bridge:session_…"  (UDS_INBOX flag)
  summary?: string  // 5-10 word UI preview — required when message is a string
  message: string | StructuredMessage
}
StructuredMessage (discriminated union, cannot be broadcast):
  {type:'shutdown_request', reason?}
  {type:'shutdown_response', request_id, approve, reason?}
  {type:'plan_approval_response', request_id, approve, feedback?}
```

## Routing Decision Tree (`SendMessageTool.call()`, `:741-905`)

```
string message?
  ├─ UDS_INBOX && to="bridge:…" → postInterClaudeMessage (Remote Control WS)
  │     re-checks bridge handle AFTER permission wait (can be minutes stale)
  ├─ UDS_INBOX && to="uds:…"    → sendToUdsSocket (local peer session)
  ├─ to resolves in appState.agentNameRegistry or parses as agentId:
  │     ├─ task running  → queuePendingMessage → task.pendingMessages[]
  │     │     drained at the agent's next tool round as a queued_command
  │     │     attachment, origin {kind:'coordinator'} (attachments.ts:1085)
  │     ├─ task stopped  → resumeAgentBackground with message as new prompt
  │     └─ task evicted  → resume from disk transcript (or error)
  ├─ to="*" → handleBroadcast → writeToMailbox per teammate
  └─ else  → handleMessage → writeToMailbox (teammate by name)
structured message? → shutdown / plan-approval handlers
```

Subagent and teammate addressing share one tool: the in-process subagent
registry is tried first, and teammate names never collide because `toAgentId`
validates the `createAgentId` format. "Messaging" a stopped subagent and
"resuming" it are the same mechanism.

## Mailbox Wire Shape (`utils/teammateMailbox.ts`)

Per-teammate inbox file: `~/.claude/teams/{team}/inboxes/{agent}.json`,
keyed by agent *name* within a team. Writes serialize through a lockfile with
retry/backoff (10 retries, 5–100 ms — async API needed explicit retries after
the sync API blocked the event loop, `:31-41`).

```
TeammateMessage = { from, text, timestamp, read, color?, summary? }
```

Delivery is poll-based everywhere — there is no push:

| Receiver | Poll | Behavior |
| --- | --- | --- |
| Interactive session | `useInboxPoller`, 1 s | idle → submit as a new turn; busy → queue in `AppState.inbox`, deliver when turn ends |
| In-process teammate between turns | `waitForNextPromptOrShutdown`, 500 ms (`inProcessRunner.ts:689`) | checks `pendingUserMessages` then mailbox; shutdown requests scanned first |
| Any agent at turn start | attachment generation (`attachments.ts:3590`) | unread non-protocol messages bundled as `<teammate-message>` attachments |

Race fixed in source: attachment generation filters out structured protocol
messages so the poller routes them to UI handlers — whichever reader wins
marks messages read, and protocol JSON must not be smooshed into LLM context
(comment at `attachments.ts:3580-3589`). In-process teammates skip
`AppState.inbox` entirely (it holds the *leader's* queue; reading it would
self-echo broadcasts).

## Protocol Messages Riding the Same Transport

| Message | Direction | Semantics |
| --- | --- | --- |
| `idle_notification` | teammate → lead | written to lead's inbox when a teammate finishes; carries `idleReason`, `summary`, `completedTaskId`, resolved/blocked/failed status (`teammateMailbox.ts:410`) |
| `shutdown_request` / `shutdown_response` | lead ↔ teammate | approval terminates the teammate's process; prompt says don't originate requests unasked |
| plan approval request / `plan_approval_response` | teammate → lead → teammate | teammate's ExitPlanModeV2 relays the plan; poller honors approvals only when `msg.from === 'team-lead'` — explicit anti-forgery so teammates can't approve each other (`useInboxPoller.ts:160-165`) |
| permission request/response (+ sandbox variants) | teammate → lead → teammate | for in-process teammates, `leaderPermissionBridge.ts` renders the request in the leader's own dialog with a worker badge; bash tries classifier auto-approve first (`inProcessRunner.ts:120-156`) |

Status updates are deliberately NOT chat: "Don't send structured JSON status
messages — use TaskUpdate" (`prompt.ts:47`).

## Per-Role Tool Matrix

From `IN_PROCESS_TEAMMATE_ALLOWED_TOOLS` / `ALL_AGENT_DISALLOWED_TOOLS`
(`constants/tools.ts`) and `filterToolsForAgent`:

| Tool | Lead/main | Teammate | Plain subagent |
| --- | --- | --- | --- |
| SendMessage | yes | yes | yes (sync; not in async allowlist) |
| TaskCreate/Get/List/Update | yes | yes (the coordination channel) | async: no; sync: yes |
| Agent (spawn) | yes | yes — sync helpers only; background/teammate spawn blocked in `call()` | no (unless `USER_TYPE=ant`) |
| TaskOutput / TaskStop | yes | no (recursion/main-thread state) | no |
| TeamCreate / TeamDelete | yes | no | no |
| AskUserQuestion | yes | no — relay via lead | no |
| CronCreate/Delete/List | yes | behind `AGENT_TRIGGERS` flag; teammate crons route to that teammate's `pendingUserMessages` | no |

Team tool signatures: `TeamCreate{team_name, description?, agent_type?}` →
`{team_name, team_file_path, lead_agent_id}`; `TeamDelete{}` (disband after
shutdown handshakes). Agent tool's multi-agent params
(`name`, `team_name`, `mode`) live in `fullInputSchema`
(`AgentTool.tsx:91-101`); `name` makes a subagent addressable via
`SendMessage({to: name})`.

## Teammate Colors

Palette `AGENT_COLORS` (`tools/AgentTool/agentColorManager.ts:14`): red, blue,
green, yellow, purple, orange, pink, cyan — 8 colors, assigned round-robin and
sticky per teammate ID (`teammateLayoutManager.ts:22-33`). Each mailbox
message carries the sender's color for UI rendering; theme keys are namespaced
(`red_FOR_SUBAGENTS_ONLY`) so agent colors can't collide with semantic UI
colors; tmux pane borders map purple→magenta, orange→colour208, pink→colour205
(`TmuxBackend.ts:61`). `general-purpose` subagents are deliberately uncolored
(`getAgentColor` returns undefined).

## Cross-Session Addressing (behind `UDS_INBOX`)

`ListPeers` discovers targets; `to:"uds:/tmp/cc-socks/1234.sock"` reaches a
local session over a Unix socket, `to:"bridge:session_…"` reaches a Remote
Control peer cross-machine. Inbound messages arrive wrapped as
`<cross-session-message from="…">`; the reply convention is copy `from` into
`to`. A listed peer is alive by definition — no busy state; messages enqueue
and drain at the receiver's next tool round.

## EOS Migration Takeaways

- One messaging tool with scheme-prefixed addresses (`name`, `*`, `uds:`,
  `bridge:`) beats per-transport tools; routing is a decision tree inside one
  `call()`, and new transports are new schemes.
- Auto-resume-on-message (running → queue, stopped → resume, evicted →
  resume-from-transcript) makes "send" total over agent lifecycle states —
  callers never need a liveness check before messaging.
- Everything is poll + file-lock, no push: acceptable at 1 s/500 ms cadence
  and trivially crash-safe, but the protocol/attachment read race shows the
  cost — two readers of one mailbox needed an explicit message-class filter.
  EOS already has typed channels; keep protocol and chat traffic on separate
  ops rather than one mailbox with filtering.
- Control-plane messages (shutdown, plan approval, permissions) ride the data
  channel as a discriminated union with `request_id` correlation and
  sender-identity checks (`from === 'team-lead'`). The anti-forgery check is
  string-based; EOS should use authenticated/typed sender IDs.
- Chat carries a required `summary` for UI preview — cheap, and avoids
  rendering full payloads in lists.
