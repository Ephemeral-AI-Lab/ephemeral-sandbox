# Query Engine

The core loop that streams LLM responses, executes tools mid-stream, manages background tasks, and compacts conversation history for API submissions.

## Overall Architecture

The query engine consists of three layers: the `EphemeralAgent` runtime wrapper, the `QueryContext` configuration container, and the `_run_query_loop` state machine that drives message cycles. Each cycle streams from the LLM, executes tools (either immediately or deferred to background), collects results, and feeds them back into the next turn until the agent has no more tool calls.

```
┌──────────────────────────────────┐
│  EphemeralAgent.run(prompt)      │
└────────────┬─────────────────────┘
             │ appends user message        │ passes to
             ▼                             ▼
┌────────────────────────┐    ┌────────────────────────┐
│   display_messages     │    │      run_query()        │
│   (mutable list)       │    └───────────┬────────────┘
└────────────────────────┘                │ creates stamped event iterator
             ▲                            ▼
             │               ┌────────────────────────┐
             │               │   _run_query_loop()     │
             │               │   (infinite while True) │
             │               └───────────┬────────────┘
             │                           │ compacts history
             │                           ▼
             │               ┌────────────────────────┐
             │               │   compact_for_api()    │
             │               └───────────┬────────────┘
             │                           │ builds api_messages
             │                           ▼
             │               ┌────────────────────────┐
             │               │   ApiMessageRequest    │
             │               └───────────┬────────────┘
             │                           │ streams from
             │                           ▼
             │               ┌────────────────────────┐
             │               │  api_client            │
             │               │  .stream_message()     │
             │               └───────────┬────────────┘
             │                           │ yields events
             │                           ▼
             │         ┌─────────────────────────────────┐
             │         │  Event Handler (switch on type)  │
             │         └──────┬──────────┬───────────┬───┘
             │    TextDelta   │  ToolUse │  Message  │
             │                │  Delta   │  Complete │
             │                ▼          ▼           ▼
             │  ┌─────────────────┐  ┌───────────────────────┐
             │  │ yield Assistant │  │ capture final_message │
             │  │  TextDelta      │  │ + usage               │
             │  └─────────────────┘  └───────────┬───────────┘
             │                                   │
             │              ┌────────────────────┴──────────────────────┐
             │              │ has tool_uses?                             │
             │              ▼                                            ▼
             │   ┌─────────────────────┐                  ┌──────────────────────┐
             │   │ Dispatch Tool       │                  │ Check background     │
             │   │ Results             │                  │ task status          │
             │   └──────────┬──────────┘                  └──────┬───────────────┘
             │              │                           has       │ none pending
             │              │ validate_tool_batch()   pending     ▼
             │     budget   │                            │   ┌──────────────┐
             │    exceeded  ▼                            │   │ return (EXIT)│
             │   ┌──────────────────┐                   ▼   └──────────────┘
             │   │ yield error      │          ┌─────────────────┐
             │   │ return           │          │ wait_any(30s)   │
             │   └──────────────────┘          └────────┬────────┘
             │                                          │
             │         foreground tools                 │ background tools
             │              ▼                           ▼
             │   ┌──────────────────────┐   ┌───────────────────────┐
             │   │ single: await        │   │ launch_background_    │
             │   │  execute_tool_call() │   │ tool()                │
             │   │ multi:               │   └──────────┬────────────┘
             │   │  asyncio.gather()    │              │ creates task
             │   └──────────┬───────────┘              ▼
             │              │              ┌───────────────────────┐
             │              │              │ BackgroundTaskManager │
             │              │              │ TrackedBackgroundTask │
             │              │              └───────────────────────┘
             │              │
             └──────────────┘  (append results → loop continues)
```

## Message and Tool Result Flow

Each turn produces a sequence of `StreamEvent` objects flowing from the LLM stream through execution, collection, and finally into `display_messages` for the next cycle. Tools yield intermediate progress, completion, or cancellation events that structure the conversation state.

```
  Query Loop        API Stream      StreamingToolExecutor   Tool Execution   display_messages
      │                 │                    │                    │                │
      │─stream_message(ApiMessageRequest)───▶│                    │                │
      │                 │                    │                    │                │
      │◀──ApiTextDeltaEvent/ApiThinkingDeltaEvent─────────────────│                │
      │                 │                    │                    │                │
      │──add_tool(ApiToolUseDeltaEvent)──────▶│                    │                │
      │                 │                    │──_start_tool()─────▶│                │
      │                 │                    │                    │                │
      │◀────────────────│────ToolExecutionProgress (optional)─────│                │
      │──get_progress()─▶│                    │                    │                │
      │◀──[ToolExecutionProgress, ...]────────│                    │                │
      │                 │                    │                    │                │
      │◀──ApiMessageCompleteEvent────────────│                    │                │
      │                 │                    │                    │                │
      │──await executor.get_remaining()──────▶│                    │                │
      │◀──[ToolExecutionCompleted | ToolExecutionCancelled, ...]───│                │
      │                 │                    │                    │                │
      │──append ConversationMessage(role=assistant)───────────────────────────────▶│
      │──append ConversationMessage(role=user, [ToolResultBlock,...])──────────────▶│
      │                 │                    │                    │   ┌────────────┤
      │                 │                    │                    │   │grows with  │
      │                 │                    │                    │   │tool results│
      │                 │                    │                    │   └────────────┤
      │──compact_for_api(display_messages)──▶│                    │                │
      │◀──[api_messages...]─────────────────│                    │                │
      │                 │                    │                    │                │
      │  (next iteration uses compacted history)                  │                │
```

## Tool Execution Dispatch

When the LLM sends tool calls, the loop must decide: execute immediately in the foreground, defer to background, or reject due to batch validation or budget. The `StreamingToolExecutor` handles mid-stream tool starts; deferred tools bypass it and go through the background path.

```
┌─────────────────────────────┐
│  Tool Use Block arrives     │
│  in LLM stream              │
└──────────────┬──────────────┘
               │ executor.add_tool()
               ▼
     ┌─────────────────────┐
     │ should_defer         │
     │ returns true?        │
     └────┬────────────┬────┘
         yes           no
          ▼             ▼
┌──────────────┐  ┌─────────────────────┐
│ Mark in      │  │ Input complete      │
│ _deferred    │  │ and valid?          │
│ set; return  │  └────┬───────────┬────┘
│ None         │      no           yes
└──────┬───────┘       ▼            ▼
       │     ┌──────────────┐  ┌────────────────────────┐
       │     │ Track as     │  │ _start_tool()          │
       │     │ queued       │  │ creates asyncio.Task   │
       │     └──────────────┘  └────────┬───────────────┘
       │                    concurrent  │   not safe
       │                    safe        ▼
       │                    │  ┌──────────────────────┐
       │                    │  │ Track as executing   │
       │                    │  │ (sequential)         │
       │                    │  └──────────────────────┘
       │                    ▼
       │         ┌─────────────────────┐
       │         │ await _execute_     │
       │         │ tool() via          │
       │         │ run_tool_safely     │
       │         └──────────┬──────────┘
       │                    │
       │           success  │  error
       │              ▼     │    ▼
       │  ┌────────────────┐│┌────────────────┐
       │  │ ToolResult     │││ ToolResult     │
       │  │ is_error=False │││ is_error=True  │
       │  └───────┬────────┘│└───────┬────────┘
       │          └─────────┴────────┘
       │                    │ after stream ends
       │                    ▼
       │         ┌──────────────────────┐
       │         │ executor             │
       │         │ .get_remaining()     │
       │         └──────────┬───────────┘
       │                    │ iterate _tools
       │                    ▼
       │         ┌──────────────────────────────┐
       │         │ yield ToolExecutionCompleted │
       │         │ or ToolExecutionCancelled    │
       │         └──────────┬───────────────────┘
       │                    │ append to tool_results
       │                    ▼
       │         ┌──────────────────────┐
       │         │ accumulate           │
       │         │ ToolResultBlock      │
       │         └──────────────────────┘
       │
       │ after stream ends (Background Dispatch Path)
       ▼
┌─────────────────────────────┐
│ background_preflight check  │
└────────┬────────────────────┘
  fails  │  passes
    ▼    │     ▼
┌──────┐ │  ┌────────────────────────────┐
│Yield │ │  │ manager.launch()           │
│error │ │  │ creates TrackedBackground  │
│result│ │  │ Task → asyncio task runs   │
└──┬───┘ │  │ async in parallel          │
   │     │  └──────────────┬─────────────┘
   │     │                 │ yield BackgroundTaskStarted
   ▼     │                 ▼
┌──────────────────────────────┐
│ accumulate ToolResultBlock / │
│ agent sees task started      │
└──────────────────────────────┘
```

## Background Task Lifecycle

Background tools launch async tasks tracked by `BackgroundTaskManager`. Tasks can run concurrently with the next LLM turn. The loop polls via `collect_completed()` at the start of each iteration to deliver finished tasks back to the agent.

```
                         manager.launch()
                               │
                               ▼
                        ┌─────────────┐
                        │   RUNNING   │◀── asyncio_task is active
                        │             │    progress_lines append on demand
                        │             │    queryable via check_background_progress
                        └──┬───┬───┬──┘
    task completes         │   │   │ cancel() called
    successfully           │   │   │ or task.cancel()
            ▼              │   │   ▼
     ┌───────────┐         │   │ ┌───────────┐
     │ COMPLETED │         │   │ │ CANCELLED │
     │           │         │   │ │           │
     │ Terminal  │         │   │ └─────┬─────┘
     │ state;    │ task    │   │       │
     │ result    │ raises  │   │       │
     │ captured; │ exception│  │       │
     │ waiting   │    ▼    │   │       │
     │ for engine│ ┌──────┐│   │       │
     └─────┬─────┘ │FAILED││   │       │
           │       └──┬───┘│   │       │
           │          │    │   │       │
           └──────────┴────┘   └───────┘
                      │ collect_completed() returns task
                      ▼
               ┌─────────────┐
               │  DELIVERED  │◀── collect_completed() marks this
               │             │    agent sees BackgroundTaskCompleted
               │             │    removed from polling next iteration
               └──────┬──────┘
                      │
                     [*]
```

## Loop Termination and Exit Conditions

The query loop exits when: (1) the LLM sends no tool calls and no background tasks are pending, (2) the tool call budget is exhausted, or (3) a fatal error occurs. Background tasks waiting to complete can extend the loop past a no-tool turn.

```
┌──────────────────────────────────┐
│  final_message captured          │
│  from stream                     │
└──────────┬───────────────────────┘
           │
   ┌───────┴──────────┐
   │ has tool_uses?   │
   └──┬────────────┬──┘
      yes          no
      ▼             ▼
┌──────────────┐  ┌───────────────────────────┐
│ Process tool │  │ Check background           │
│ results and  │  │ task status                │
│ append to    │  └────────────┬───────────────┘
│ display_msg  │    None/no    │  has pending
└──────┬───────┘    pending    │  background
       │               ▼       ▼
  tool_call     ┌────────────┐ ┌────────────────────┐
  _limit        │return(EXIT)│ │ await wait_any()   │
  exceeded      │no more work│ │ timeout=30s        │
       │        └────────────┘ └────────┬───────────┘
       ▼                     completed  │  timeout
┌─────────────────┐             ▼       │    ▼
│ yield budget    │  ┌──────────────┐   │  ┌──────────────────────┐
│ error           │  │ deliver_     │   │  │ append_and_emit_     │
│ return (EXIT)   │  │ completed_   │   │  │ reminder()           │
└──────┬──────────┘  │ background_  │   │  │ agent sees progress  │
       │             │ task()       │   │  └──────────┬───────────┘
       │             │ append to    │   │             │
       │             │ display_msg  │   │             │
       │             └──────┬───────┘   │             │
       │                    │           │             │
       │                    └─────┬─────┘             │
       │                          │ loop continues    │
       │                          └─────────┬─────────┘
       │                                    ▼
       │                          ┌─────────────────┐
       │                          │  loop continues │
       │                          │  next iteration │
       │                          └─────────────────┘
       │ cancel all pending
       ▼
┌──────────────────────┐
│ background_manager   │
│ .cancel_all()        │
└──────────┬───────────┘
           ▼
         (END)
```

## Integration Points: Platform Hooks, External Triggers, and Snapshots

The query loop integrates with external systems via platform-owned tool hooks, `on_turn` callbacks for live progress, and `api_messages_snapshot` for compaction state inspection. Pre-hooks run after input validation and before `ToolExecutionStarted`; they may mutate parsed args, emit user-visible `SystemNotification` advisories, or return a failed tool result. Post-hooks run after the tool body and can emit advisories or replace the final tool result with an error. Terminal submission (for example `submit_task_success` or `request_replan`) is now a regular in-loop tool governed by `QueryContext.terminal_tools`; the legacy post-run submission phase has been removed.

```
                        ┌─────────────────────────────┐
                        │  Query Loop                 │
                        │  _run_query_loop()          │
                        └──────┬──────┬──────┬────────┘
                               │      │      │
           on_turn callback    │      │      │  agent metadata
                  ▼            │      │      ▼
     ┌────────────────────┐    │      │  ┌────────────────────────┐
     │ on_turn(display_   │    │      │  │ agent_name, work_id    │
     │ messages)          │    │      │  │ stamped on events      │
     └────────┬───────────┘    │      │  └──────────┬─────────────┘
              │                │      │             │ multiplexing
              ▼                │      │             ▼
 ┌────────────────────────┐    │      │  ┌────────────────────────┐
 │ Live progress tracking │    │      │  │ Session relay /        │
 │ e.g. UI streaming      │    │      │  │ agent pools            │
 └────────────────────────┘    │      │  │ multi-agent coord.     │
                                │      │  └────────────────────────┘
              platform hooks    │      │ snapshots
                    ▼           │      ▼
         ┌──────────────────┐   │  ┌──────────────────────────────┐
         │ tools.core.hooks │   │  │ context.api_messages_        │
         │ pre/post chain   │   │  │ snapshot                     │
         │ per tool call    │   │  │ (before each LLM call)       │
         └────────┬─────────┘   │  └───────────────┬──────────────┘
                  │             │                   │ compact_for_api
                  ▼             │                   ▼
    ┌─────────────────────────┐ │  ┌──────────────────────────────┐
    │ Post-run submission     │ │  │ SessionState tracking        │
    │ phase (outside query    │ │  │ (compaction module)          │
    │ loop)                   │ │  └───────────────┬──────────────┘
    └────────────┬────────────┘ │                  │ auditing
                 │              │                  ▼
                 ▼              │  ┌──────────────────────────────┐
    ┌────────────────────────┐  │  │ Message compaction history   │
    │ External tool backends │  │  │ token usage tracking         │
    │ e.g. file operations   │  │  └──────────────────────────────┘
    └────────────────────────┘  │
```

## Conversation State and Streaming

The `display_messages` list is the source of truth for conversation history. Each `ConversationMessage` contains a role (assistant or user) and content blocks: text, tool uses (from LLM), or tool results (execution feedback). Streaming compaction happens before each API call to manage token budgets.

```
 display_messages (Append-Only)
┌────────────────────────────────────────────────────────────┐
│                                                            │
│  ┌──────────────────────────┐                             │
│  │ ConversationMessage      │                             │
│  │ role: user               │                             │
│  │ content: [TextBlock]     │                             │
│  └──────────────┬───────────┘                             │
│                 │                                          │
│                 ▼                                          │
│  ┌──────────────────────────┐                             │
│  │ ConversationMessage      │                             │
│  │ role: assistant          │                             │
│  │ content: [TextBlock,     │                             │
│  │           ToolUseBlock]  │                             │
│  └──────────────┬───────────┘                             │
│                 │                                          │
│                 ▼                                          │
│  ┌──────────────────────────┐                             │
│  │ ConversationMessage      │                             │
│  │ role: user               │                             │
│  │ content: [ToolResult,    │                             │
│  │           ToolResult]    │                             │
│  └──────────────┬───────────┘                             │
│                 │                                          │
│                 ▼                                          │
│  ┌──────────────────────────┐ ◀── final_message appended │
│  │ ConversationMessage      │                             │
│  │ role: assistant          │                             │
│  │ ...                      │                             │
│  └──────────────────────────┘                             │
│                                                            │
└──────────────────────────┬─────────────────────────────────┘
                           │ passed to
                           ▼
              ┌────────────────────────────┐
              │ compact_for_api()          │
              │ applies SessionState       │
              │ summarization rules        │
              └─────────────┬──────────────┘
                            │
                            ▼
              ┌────────────────────────────┐
              │ api_messages               │
              │ (compacted copy)           │
              └─────────────┬──────────────┘
                            │ sent to
                            ▼
              ┌────────────────────────────┐
              │ LLM API                    │
              └─────────────┬──────────────┘
                            │ returns
                            ▼
              ┌────────────────────────────┐
              │ final_message              │
              └────────────────────────────┘
                (appended back to display_messages)
```

## Agent Runtime Wrapper

The `EphemeralAgent` is spawned per request by `spawn_agent()`, wrapping the query loop with agent-specific config: model, toolkits, system prompt, and budget. It owns the mutable `display_messages` list and exposes a read-only `display_messages` property to callers.

```
┌──────────────────────────────────────────────────────────┐
│  spawn_agent(config, messages, agent_def,                │
│              session_state, sandbox_id)                  │
└──────┬───────────────┬──────────────────┬────────────────┘
       │               │                  │
       ▼               ▼                  ▼
┌─────────────┐  ┌──────────────┐  ┌──────────────────────┐
│ _resolve_   │  │ _build_agent │  │ _build_agent_system_ │
│ agent_      │  │ _tool_       │  │ prompt()             │
│ identity()  │  │ registry()   │  └──────────┬───────────┘
└──────┬──────┘  └──────┬───────┘             │ finalize with
       │                │                     │ awareness
       ▼                ▼                     ▼
┌─────────────┐  ┌──────────────┐  ┌──────────────────────┐
│ resolved_   │  │skill_registry│  │ finalize_tool_       │
│ model       │  │daytona_tool  │  │ registry_and_prompt()│
│ api_client  │  │kit           │  │ inject capability    │
└──────┬──────┘  │background_  │  │ text                 │
       │         │toolkit      │  └──────────┬───────────┘
       │         └──────┬───────┘             │
       │                │                     ▼
       │                │          ┌──────────────────────┐
       │                │          │ system_prompt        │
       │                │          └──────────┬───────────┘
       └────────────────┴──────────────────────┘
                        │ create context
                        ▼
           ┌────────────────────────────────┐
           │ QueryContext                   │
           │ (api_client, tool_registry,    │
           │  system_prompt, model, ...)    │
           └──────────────┬────────────────┘
                          │ wrap
                          ▼
           ┌────────────────────────────────┐
           │ EphemeralAgent                 │
           │ (agent_name, query_context,    │
           │  _display_messages=[...])      │
           └──────────┬───────────┬─────────┘
                      │           │
         expose       ▼           ▼  async method
         property  ┌──────┐   ┌────────────────────────┐
                   │ dis- │   │ run(prompt)            │
                   │ play_│   │ -> AsyncIterator       │
                   │ mess-│   │    [StreamEvent]       │
                   │ ages │   └──────────┬─────────────┘
                   │ (r/o)│             │ calls
                   └──────┘             ▼
                              ┌──────────────────────────┐
                              │ run_query(context,       │
                              │ display_messages)        │
                              └──────────┬───────────────┘
                                         │ stamped events
                                         ▼
                              ┌──────────────────────────┐
                              │ stamped_event_iterator   │
                              └──────────┬───────────────┘
                                         │ yields
                                         ▼
                              ┌──────────────────────────┐
                              │ caller receives events   │
                              │ (with agent_name,        │
                              │  work_id)                │
                              └──────────────────────────┘
```
