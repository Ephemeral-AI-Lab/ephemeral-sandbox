import { useCallback, useEffect, useRef, useState } from 'react'
import {
  connectionMonitor,
  fetchState,
  respondPermission,
  respondQuestion,
  streamChat,
} from './api'
import type {
  AppState,
  BackendEvent,
  BridgeSessionSnapshot,
  McpServerSnapshot,
  ModalRequest,
  SelectOption,
  TaskSnapshot,
  ToolkitSnapshot,
  TranscriptItem,
} from './types'

/** Subscribe to backend connection state (polled via /api/health) */
export function useConnected(): boolean {
  const [connected, setConnected] = useState(connectionMonitor.connected)
  useEffect(() => {
    const unsub = connectionMonitor.onChange(setConnected)
    connectionMonitor.start()
    return () => {
      unsub()
      connectionMonitor.stop()
    }
  }, [])
  return connected
}

// ---------------------------------------------------------------------------
// Shared event dispatcher — SSE events from /api/chat are dispatched here
// ---------------------------------------------------------------------------
type EventCallback = (event: BackendEvent) => void
const _eventListeners = new Map<string, Set<EventCallback>>()

export function _dispatchEvent(event: BackendEvent) {
  const listeners = _eventListeners.get(event.type)
  if (listeners) {
    for (const cb of listeners) cb(event)
  }
  const wildcardListeners = _eventListeners.get('*')
  if (wildcardListeners) {
    for (const cb of wildcardListeners) cb(event)
  }
}

function _onEvent(type: string, cb: EventCallback): () => void {
  if (!_eventListeners.has(type)) _eventListeners.set(type, new Set())
  _eventListeners.get(type)!.add(cb)
  return () => _eventListeners.get(type)?.delete(cb)
}

/** Full harness state — fetched on connect, updated from SSE events */
export function useAppState(): AppState | null {
  const [state, setState] = useState<AppState | null>(null)
  const connected = useConnected()

  useEffect(() => {
    if (!connected) return
    fetchState()
      .then((event) => {
        if (event.state) setState(event.state as AppState)
      })
      .catch(() => {})
  }, [connected])

  useEffect(() => {
    return _onEvent('*', (e) => {
      if ((e.type === 'state_snapshot' || e.type === 'ready') && e.state) {
        setState(e.state as AppState)
      }
    })
  }, [])

  return state
}

/** Task list from state and SSE events */
export function useTasks(): TaskSnapshot[] {
  const [tasks, setTasks] = useState<TaskSnapshot[]>([])
  useEffect(() => {
    fetchState()
      .then((e) => setTasks(e.tasks ?? []))
      .catch(() => {})
    return _onEvent('*', (e) => {
      if (e.tasks) setTasks(e.tasks)
    })
  }, [])
  return tasks
}

/** MCP servers from state snapshots */
export function useMcpServers(): McpServerSnapshot[] {
  const [servers, setServers] = useState<McpServerSnapshot[]>([])
  useEffect(() => {
    return _onEvent('*', (e) => {
      if ((e.type === 'ready' || e.type === 'state_snapshot') && e.mcp_servers) {
        setServers(e.mcp_servers as McpServerSnapshot[])
      }
    })
  }, [])
  return servers
}

/** Bridge sessions from state snapshots */
export function useBridgeSessions(): BridgeSessionSnapshot[] {
  const [sessions, setSessions] = useState<BridgeSessionSnapshot[]>([])
  useEffect(() => {
    return _onEvent('*', (e) => {
      if ((e.type === 'ready' || e.type === 'state_snapshot') && e.bridge_sessions) {
        setSessions(e.bridge_sessions as BridgeSessionSnapshot[])
      }
    })
  }, [])
  return sessions
}

/** Toolkits from state snapshots */
export function useToolkits(): ToolkitSnapshot[] {
  const [toolkits, setToolkits] = useState<ToolkitSnapshot[]>([])
  useEffect(() => {
    fetchState()
      .then((e) => setToolkits((e.toolkits ?? []) as ToolkitSnapshot[]))
      .catch(() => {})
    return _onEvent('*', (e) => {
      if ((e.type === 'ready' || e.type === 'state_snapshot') && e.toolkits) {
        setToolkits(e.toolkits as ToolkitSnapshot[])
      }
    })
  }, [])
  return toolkits
}

/** Available slash commands */
export function useCommands(): string[] {
  const [commands, setCommands] = useState<string[]>([])
  useEffect(() => {
    fetchState()
      .then((e) => setCommands(e.commands ?? []))
      .catch(() => {})
  }, [])
  return commands
}

/** Transcript accumulator with SSE chat streaming */
export function useTranscript() {
  const [items, setItems] = useState<TranscriptItem[]>([])
  const [streamingText, setStreamingText] = useState('')
  const [busy, setBusy] = useState(false)
  const abortRef = useRef<AbortController | null>(null)

  useEffect(() => {
    const unsubs = [
      _onEvent('transcript_item', (e) => {
        if (e.item && e.item.role !== 'user') setItems((prev) => [...prev, e.item!])
      }),
      _onEvent('tool_started', (e) => {
        if (e.item) {
          const item: TranscriptItem = {
            ...e.item,
            tool_name: e.item.tool_name ?? e.tool_name ?? undefined,
            tool_input: e.item.tool_input ?? e.tool_input ?? undefined,
          }
          setItems((prev) => [...prev, item])
        }
      }),
      _onEvent('tool_completed', (e) => {
        if (e.item) {
          const item: TranscriptItem = {
            ...e.item,
            tool_name: e.item.tool_name ?? e.tool_name ?? undefined,
            is_error: e.item.is_error ?? e.is_error ?? undefined,
          }
          setItems((prev) => [...prev, item])
        }
      }),
      _onEvent('assistant_delta', (e) => {
        setStreamingText((prev) => prev + (e.message ?? ''))
      }),
      _onEvent('assistant_complete', (e) => {
        const text = e.message ?? ''
        setItems((prev) => [...prev, { role: 'assistant', text }])
        setStreamingText('')
      }),
      _onEvent('line_complete', () => {
        setBusy(false)
      }),
      _onEvent('clear_transcript', () => {
        setItems([])
        setStreamingText('')
      }),
      _onEvent('error', (e) => {
        setItems((prev) => [
          ...prev,
          { role: 'system', text: `Error: ${e.message ?? 'unknown'}` },
        ])
        setBusy(false)
      }),
    ]
    return () => unsubs.forEach((u) => u())
  }, [])

  const submitLine = useCallback((line: string, options?: { agent_name?: string; sandbox_id?: string }) => {
    setItems((prev) => [...prev, { role: 'user', text: line }])
    setBusy(true)

    abortRef.current = streamChat(
      line,
      (event) => _dispatchEvent(event),
      () => setBusy(false),
      (error) => {
        setItems((prev) => [
          ...prev,
          { role: 'system', text: `Error: ${error.message}` },
        ])
        setBusy(false)
      },
      options,
    )
  }, [])

  const clear = useCallback(() => {
    setItems([])
    setStreamingText('')
  }, [])

  return { items, streamingText, busy, submitLine, clear }
}

/** Modal state (permission / question dialogs) */
export function useModal() {
  const [modal, setModal] = useState<ModalRequest | null>(null)
  const [selectRequest, setSelectRequest] = useState<{
    title: string
    submitPrefix: string
    options: SelectOption[]
  } | null>(null)

  useEffect(() => {
    const unsubs = [
      _onEvent('modal_request', (e) => {
        setModal((e.modal as ModalRequest) ?? null)
      }),
      _onEvent('select_request', (e) => {
        const m = e.modal as Record<string, unknown> | undefined
        setSelectRequest({
          title: String(m?.title ?? 'Select'),
          submitPrefix: String(m?.submit_prefix ?? ''),
          options: (e.select_options ?? []) as SelectOption[],
        })
      }),
    ]
    return () => unsubs.forEach((u) => u())
  }, [])

  const handleRespondPermission = useCallback(
    (requestId: string, allowed: boolean) => {
      respondPermission(requestId, allowed).catch(() => {})
      setModal(null)
    },
    [],
  )

  const handleRespondQuestion = useCallback(
    (requestId: string, answer: string) => {
      respondQuestion(requestId, answer).catch(() => {})
      setModal(null)
    },
    [],
  )

  const dismissSelect = useCallback(() => {
    setSelectRequest(null)
  }, [])

  return {
    modal,
    selectRequest,
    respondPermission: handleRespondPermission,
    respondQuestion: handleRespondQuestion,
    dismissSelect,
  }
}
