# Command Center — Design Plan

## Goal
A per-TeamRun service combining:
1. **Code intelligence** over the shared workspace + artifact store.
2. **Push notifications** from teammate updates (delivered to per-agent inboxes).
3. **Pull toolkit** so agents can query current state on demand.

## Restated design
- **Command Center (CC):** TeamRun-scoped service owning a code-intelligence index and an event bus.
- **Push:** posthook on agent B emits a structured event → CC fans out to subscribers' inboxes → next time agent A's worker turn starts, the inbox is drained into A's prompt context.
- **Pull:** a `command_center` toolkit (`query_state`, `find_symbol`, `who_touched`, `read_inbox`, `get_artifact`) callable mid-turn.

## Why it fits the existing codebase
- `execute_with_posthook` (`backend/src/hooks/agent_posthook.py`) is a **single chokepoint** where every agent's structured submission lands. Natural place to publish events — no per-tool instrumentation needed.
- `InMemoryArtifactStore` (`backend/src/team/artifact_store.py`) already has `snapshot()` / `restore()`, so CC's index can be **rebuilt deterministically from a checkpoint**.
- `Worker._run_one` (`backend/src/team/worker.py:64`) builds `query_ctx` per work item — clean injection point for the inbox + toolkit, no agent-loop changes.

## Strengths
1. **Posthook as publish boundary is correct.** Only place where output is validated (typed `Plan`, accepted submission). Mid-turn tool publishing would flood the bus with noise.
2. **Push + pull hybrid is right.** Push handles "you didn't know to ask"; pull handles "I need to verify before I act." Pure push spams context; pure pull misses time-critical updates.
3. **CC owning code intelligence is queryable across agents.** Today `team/context/siblings.py` is snapshot-into-prompt — every agent pays token cost up front. Pull tool inverts this: agents pay only when they ask. Big win as team size grows.
4. **TeamRun-scoped, not global.** Matches existing lifecycle (cancel_event, budgets, checkpoint). No cross-run leakage.

## Risks / things that will bite

### 1. Inbox draining is a context-budget problem in disguise
If 5 siblings each push between A's turns, A's next prompt grows by 5 × N tokens. Need either:
- (a) **Per-agent inbox cap with overflow** → "you have 12 unread, call `read_inbox` to see them", or
- (b) Relevance filtering at publish time (subscribe by topic).

**Decision:** start with (a). Simpler; pull tool already drains the rest.

### 2. Code intelligence over a moving workspace
Files change between turns. Options:
- **Snapshot-per-turn:** lazy index from artifact store + file context at start of each query. Cheap, always consistent, no cross-turn caching.
- **Incremental with invalidation:** posthook publishes "files touched" → CC invalidates index regions. Faster, but cache-coherence bug surface.
- **LSP-backed:** delegate to per-language LSP server. Most accurate, heaviest dep.

**Decision:** snapshot-per-turn for v1. Already paid in `team/context/files.py`; CC just exposes via tools instead of stuffing prompts.

### 3. Push events must be ordered and idempotent
Two siblings completing simultaneously can race. Use a monotonic event id per TeamRun (promote `agent_run_id` uuid → counter on dispatcher). Inboxes track `last_seen_event_id`. Makes checkpoint/restore trivial: snapshot counter + per-agent cursors.

### 4. The `subscribe` tool is a trap if mis-scoped
If agents subscribe to arbitrary topics, the LLM will invent topic names nobody publishes to. Options:
- (a) Enumerate fixed topics (`plan_submitted`, `file_changed`, `work_item_failed`).
- (b) **Implicit subscription** — agent A automatically receives events from any sibling whose work_item it depends on per the dispatcher graph.

**Decision:** (b). Dependency graph already encodes "who cares about whom"; no LLM footgun.

### 5. Posthook serializer no-skills contract extends to CC
`agent_posthook.py:78` enforces serializers must not carry skills. Extend that contract: posthook serializers must not get the `command_center` toolkit either. Generalize `_assert_serializer_has_no_skills`.

### 6. CC must not be imported from `hooks/`
`agent_posthook.py:21` notes "team/ is not imported here" — that genericity is load-bearing. CC publishing happens from the **worker** after `execute_with_posthook` returns, not from inside the hook. Worker has both result and team_run; hook stays pure.

## v1 module shape

```
backend/src/team/command_center/
  bus.py         # TeamBus: publish(event), subscribe_implicit(agent, deps)
  inbox.py       # per-agent ring buffer with cap + cursor
  index.py       # snapshot-per-turn code/artifact index
  tools.py       # command_center toolkit:
                 #   read_inbox, query_state, find_symbol,
                 #   who_touched, get_artifact
```

## Wiring
1. `TeamRun.__init__` constructs `TeamBus` and an empty inbox per registered agent.
2. `Worker._run_one`, **after** `execute_with_posthook` returns, calls `team_run.bus.publish(event_from(result, submitted))`. One line, post-validation.
3. `build_query_context` drains the agent's inbox (capped) into `query_ctx` and attaches the `command_center` toolkit. Overflow becomes a hint: "12 more unread; call read_inbox".
4. Checkpoint/restore: `TeamBus` serializes `(event_log_tail, per_agent_cursors)` alongside the artifact snapshot.

## Key decisions (pinned)
1. **Implicit subscription via the dependency graph**, not free-form topics.
2. **Snapshot-per-turn code intelligence** before attempting incremental indexing.
3. **Per-agent inbox cap with overflow → read_inbox**, not publish-time filtering.
4. **Publish from the worker, not the hook** — preserve `hooks/` genericity.
5. **Posthook serializers banned from CC toolkit**, same reasoning as the existing no-skills rule.

## Open questions
- Event schema: a single `TeamEvent` union, or one type per kind (`PlanSubmitted`, `WorkItemFailed`, …)?
- Does `find_symbol` need cross-language support in v1, or Python-only against the workspace?
- Should `read_inbox` mark events as read on call, or require an explicit ack?
