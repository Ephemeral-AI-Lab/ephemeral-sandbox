# Plan: React Web Frontend for EphemeralOS

## Summary

Build a React web frontend for EphemeralOS, porting the coordination UI patterns from synthetic-os while adapting to EphemeralOS's existing backend protocol. The current terminal UI (React Ink) communicates via child-process stdio with `OHJSON:` JSON-line protocol. We need:

1. A thin WebSocket/HTTP bridge layer in the Python backend
2. A React web app with coordination dashboard, task DAG, and session views

## Task Type
- [x] Frontend (React web app)
- [x] Backend (WebSocket bridge)
- [x] Fullstack

## Architecture Decision

### Option A: Add WebSocket endpoint to backend (Recommended)
- Add a FastAPI/Starlette WebSocket server alongside existing stdio host
- Reuse the same `BackendEvent` / `FrontendRequest` protocol — just transport over WS instead of stdio
- Minimal backend changes (~200 lines)

### Option B: Proxy stdio over WebSocket
- Spawn backend as child process from a WS bridge
- More complex, less direct

**Decision: Option A** — cleaner, direct, and the protocol is already well-defined.

---

## Implementation Steps

### Step 1: Backend WebSocket Bridge (~200 LOC)

**File:** `src/ephemeralos/ui/web_host.py` (NEW)

- Add a lightweight WebSocket server (using `websockets` or `starlette`)
- Translate the existing `OHJSON:` protocol to native WebSocket frames
- Reuse `BackendHost` event handling logic from `backend_host.py`
- Expose at `ws://localhost:PORT/ws`

**File:** `src/ephemeralos/ui/web_server.py` (NEW)

- Static file server for the React build output
- Health check endpoint `GET /api/health`
- Session info endpoint `GET /api/state`
- Serve on configurable port (default 8420)

**Pseudo-code:**
```python
# web_host.py
class WebBackendHost:
    """WebSocket version of BackendHost."""
    
    async def handle_ws(self, ws):
        # On connect: send "ready" event with state snapshot
        await ws.send(json.dumps(ready_event()))
        
        # Bidirectional: 
        # - Receive FrontendRequest JSON from WS
        # - Route to same handlers as stdio backend
        # - Emit BackendEvent JSON back over WS
        async for raw in ws:
            request = json.loads(raw)
            await self.handle_request(request)
    
    async def emit_event(self, event: BackendEvent):
        await self.ws.send(json.dumps(asdict(event)))
```

**Key files to reference:**
- `src/ephemeralos/ui/backend_host.py:1-312` — existing stdio protocol
- `src/ephemeralos/ui/protocol.py:1-197` — event/request models

### Step 2: Frontend Scaffold (~500 LOC)

**Directory:** `frontend/web/` (NEW)

```
frontend/web/
├── package.json          # React 19, Vite, TailwindCSS 4, React Router, TanStack Query
├── vite.config.ts        # Dev server with WS proxy to backend
├── tsconfig.json
├── index.html
├── tailwind.config.ts
└── src/
    ├── main.tsx          # Entry: QueryClientProvider + RouterProvider
    ├── App.tsx           # Routes: /, /tasks, /session/:id
    ├── lib/
    │   ├── types.ts      # Port from terminal/src/types.ts + extend
    │   ├── ws.ts         # WebSocket client (singleton, reconnect, event dispatch)
    │   └── api.ts        # React Query hooks wrapping WS events
    ├── components/
    │   └── layout/
    │       ├── Header.tsx
    │       ├── Sidebar.tsx
    │       └── Layout.tsx
    └── pages/
        └── (see Step 3-5)
```

**Dependencies:**
```json
{
  "react": "^19.0.0",
  "react-dom": "^19.0.0",
  "react-router": "^7.0.0",
  "tailwindcss": "^4.0.0",
  "@tanstack/react-query": "^5.0.0",
  "vite": "^6.0.0",
  "@vitejs/plugin-react": "^4.0.0"
}
```

### Step 3: WebSocket Client & State Layer (~300 LOC)

**File:** `frontend/web/src/lib/ws.ts`

```typescript
// Singleton WebSocket with auto-reconnect
class HarnessSocket {
  private ws: WebSocket | null = null;
  private listeners = new Map<string, Set<(event: BackendEvent) => void>>();
  
  connect(url: string): void { /* reconnect logic */ }
  send(request: FrontendRequest): void { /* JSON.stringify + send */ }
  on(eventType: string, cb: (e: BackendEvent) => void): () => void { /* subscribe */ }
}
export const socket = new HarnessSocket();
```

**File:** `frontend/web/src/lib/api.ts`

```typescript
// React hooks wrapping WebSocket state
export function useAppState(): AppState { /* listen to state_snapshot */ }
export function useTasks(): TaskSnapshot[] { /* listen to tasks_snapshot */ }
export function useTranscript(): TranscriptItem[] { /* accumulate transcript_item events */ }
export function useAssistantStream(): string { /* assistant_delta → buffer → assistant_complete */ }
export function useMcpServers(): McpServerSnapshot[] { /* from state events */ }
export function useBridgeSessions(): BridgeSessionSnapshot[] { /* from state events */ }
```

### Step 4: Dashboard Page (~400 LOC)

**File:** `frontend/web/src/pages/DashboardPage.tsx`

Port from synthetic-os `CoordinationPage.tsx` patterns:
- **Metrics bar**: Model name, token count, task stats, MCP server count
- **Task list**: Cards showing background tasks with status badges
- **Session info**: Current model, permission mode, CWD
- **Quick actions**: New prompt submission, permission mode toggle

Components:
- `StatusCard` — metric display (reuse synthetic-os `MetricsSummaryBar` pattern)
- `TaskCard` — individual task with status badge + progress
- `SessionPanel` — active session info

### Step 5: Conversation Page (~600 LOC)

**File:** `frontend/web/src/pages/ConversationPage.tsx`

Port from synthetic-os `CoordinationSessionDetailPage.tsx` + terminal `ConversationView.tsx`:
- **Transcript view**: Scrollable message list with role-based styling
- **Streaming display**: Real-time assistant text assembly
- **Tool call display**: Collapsible tool invocations with input/output
- **Input composer**: Text area with submit, command autocomplete
- **Permission modal**: Approve/deny tool execution
- **Question modal**: Answer backend questions

Components:
- `MessageBubble` — role-colored message display
- `ToolCallBlock` — expandable tool invocation (name, input summary, output)
- `StreamingText` — live text with cursor animation
- `PromptComposer` — input area with `/command` autocomplete
- `PermissionDialog` — modal for tool approval
- `QuestionDialog` — modal for backend questions

### Step 6: Task Detail Page (~300 LOC)

**File:** `frontend/web/src/pages/TaskDetailPage.tsx`

- Task metadata display (type, status, timing, CWD)
- Output log viewer (fetch from output_file path)
- Status timeline
- Action buttons (kill for running tasks)

### Step 7: Integration & CLI Flag (~100 LOC)

**File:** `src/ephemeralos/commands/registry.py` (MODIFY)

- Add `--web` / `--ui web` CLI flag
- When set, start `WebBackendHost` instead of terminal UI
- Open browser to `http://localhost:8420`

**File:** `pyproject.toml` (MODIFY)

- Add optional `[web]` dependency group: `starlette`, `uvicorn`, `websockets`

---

## Key Files

| File | Operation | Description |
|------|-----------|-------------|
| `src/ephemeralos/ui/web_host.py` | Create | WebSocket backend host |
| `src/ephemeralos/ui/web_server.py` | Create | Static server + health API |
| `src/ephemeralos/ui/backend_host.py` | Reference | Existing stdio protocol to mirror |
| `src/ephemeralos/ui/protocol.py` | Reference | Event/request models |
| `src/ephemeralos/commands/registry.py` | Modify | Add --web CLI flag |
| `pyproject.toml` | Modify | Add web dependencies |
| `frontend/web/` | Create | Entire React web app |
| `frontend/web/src/lib/ws.ts` | Create | WebSocket client |
| `frontend/web/src/lib/api.ts` | Create | React Query hooks |
| `frontend/web/src/pages/DashboardPage.tsx` | Create | Main dashboard |
| `frontend/web/src/pages/ConversationPage.tsx` | Create | Chat/transcript view |
| `frontend/web/src/pages/TaskDetailPage.tsx` | Create | Task detail view |

## Estimated Size

| Layer | LOC |
|-------|-----|
| Backend WebSocket bridge | ~300 |
| Frontend scaffold + config | ~200 |
| WebSocket client + state hooks | ~300 |
| Dashboard page | ~400 |
| Conversation page | ~600 |
| Task detail page | ~300 |
| CLI integration | ~100 |
| **Total** | **~2,200** |

## Risks and Mitigation

| Risk | Mitigation |
|------|------------|
| Backend stdio protocol assumes single client | WebSocket host manages one active session; reject additional connections |
| Permission modals need synchronous response | Use modal state in React + WS reply; backend already uses request_id for async responses |
| Streaming assistant text can arrive rapidly | Buffer deltas, render on requestAnimationFrame |
| Vite dev server needs WS proxy | Configure vite.config.ts proxy to backend port |
| React 19 + TanStack Query v5 API changes | Pin versions, follow latest docs |

## What We Port from Synthetic-OS

| Synthetic-OS Pattern | EphemeralOS Adaptation |
|----------------------|------------------------|
| `CoordinationPage` metrics bar | `DashboardPage` with task/MCP stats |
| `CoordinationDetailPage` 3-pane layout | Future: DAG view when coordinator mode matures |
| `CoordinationSessionDetailPage` transcript | `ConversationPage` with streaming + tool calls |
| `useCoordinationWs` WebSocket hook | `ws.ts` singleton + React hooks |
| `StatusBadge` component | `StatusBadge` for task/MCP status |
| ReactFlow DAG canvas | Future phase (when EphemeralOS swarm/coordinator has task graph) |
| Adaptive polling (TanStack Query) | Same pattern for fallback when WS disconnects |

## What We DON'T Port

- PostgreSQL/SQLAlchemy persistence (EphemeralOS is file-based)
- Daytona sandbox UI (EphemeralOS uses worktrees)
- Team/agent roster management (EphemeralOS defines agents in code)
- Code intelligence pages (not in EphemeralOS scope yet)
- Multi-run hierarchy clustering (single session model)

## SESSION_ID
- CODEX_SESSION: N/A (codeagent-wrapper not available)
- GEMINI_SESSION: N/A (codeagent-wrapper not available)
