/**
 * HTTP + SSE API client for EphemeralOS backend.
 * Replaces the WebSocket-based communication with:
 * - fetch() for REST endpoints (config, permission, question, state)
 * - POST /api/chat with SSE streaming for chat responses
 */

import type {
  BackendEvent,
  ConfigUpdate,
  DbHealthStatus,
  ModelRegistration,
  SessionSummary,
  SessionDetail,
  AgentRunSummary,
  SessionUsage,
  ModelUsage,
} from './types'

type EventHandler = (event: BackendEvent) => void

const API_BASE = '/api'

// ---------------------------------------------------------------------------
// REST endpoints
// ---------------------------------------------------------------------------

export async function fetchState(): Promise<BackendEvent> {
  const res = await fetch(`${API_BASE}/state`)
  return res.json()
}

export async function updateConfig(config: ConfigUpdate): Promise<{
  changed: boolean
  model?: string
  provider?: string
  auth_status?: string
  base_url?: string
}> {
  const res = await fetch(`${API_BASE}/config`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(config),
  })
  return res.json()
}

export async function respondPermission(requestId: string, allowed: boolean): Promise<void> {
  await fetch(`${API_BASE}/permission`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ request_id: requestId, allowed }),
  })
}

export async function respondQuestion(requestId: string, answer: string): Promise<void> {
  await fetch(`${API_BASE}/question`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ request_id: requestId, answer }),
  })
}

export async function fetchSessions(): Promise<{ sessions: Array<{ value: string; label: string }> }> {
  const res = await fetch(`${API_BASE}/sessions`)
  return res.json()
}

export async function healthCheck(): Promise<boolean> {
  try {
    const res = await fetch(`${API_BASE}/health`)
    return res.ok
  } catch {
    return false
  }
}

// ---------------------------------------------------------------------------
// SSE streaming for chat
// ---------------------------------------------------------------------------

/**
 * Submit a chat line and stream back events via SSE.
 * Returns an AbortController to cancel the stream.
 */
export function streamChat(
  line: string,
  onEvent: EventHandler,
  onDone: () => void,
  onError: (error: Error) => void,
  options?: { agent_name?: string; sandbox_id?: string },
): AbortController {
  const controller = new AbortController()
  let finished = false

  const finish = () => {
    if (!finished) {
      finished = true
      onDone()
    }
  }

  ;(async () => {
    try {
      const body: Record<string, string> = { line }
      if (options?.agent_name) body.agent_name = options.agent_name
      if (options?.sandbox_id) body.sandbox_id = options.sandbox_id

      const res = await fetch(`${API_BASE}/chat`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
        signal: controller.signal,
      })

      if (!res.ok) {
        const body = await res.json().catch(() => ({ error: res.statusText }))
        onError(new Error(body.error || `HTTP ${res.status}`))
        finish()
        return
      }

      if (!res.body) {
        onError(new Error('No response body'))
        finish()
        return
      }

      const reader = res.body.getReader()
      const decoder = new TextDecoder()
      let buffer = ''

      while (true) {
        const { done, value } = await reader.read()
        if (done) break

        buffer += decoder.decode(value, { stream: true })
        const lines = buffer.split('\n')
        buffer = lines.pop()!

        for (const line of lines) {
          if (line.startsWith('data: ')) {
            const data = line.slice(6)
            if (data === '[DONE]') {
              finish()
              return
            }
            try {
              const event = JSON.parse(data) as BackendEvent
              onEvent(event)
            } catch {
              // skip malformed events
            }
          }
        }
      }
      finish()
    } catch (err) {
      if (err instanceof DOMException && err.name === 'AbortError') {
        finish()
        return
      }
      onError(err instanceof Error ? err : new Error(String(err)))
      finish()
    }
  })()

  return controller
}

// ---------------------------------------------------------------------------
// Connection polling (replaces WebSocket connect/disconnect)
// ---------------------------------------------------------------------------

type ConnectionListener = (connected: boolean) => void

class ConnectionMonitor {
  private _connected = false
  private _listeners = new Set<ConnectionListener>()
  private _interval: ReturnType<typeof setInterval> | null = null
  private _refCount = 0

  get connected() {
    return this._connected
  }

  start() {
    this._refCount++
    if (this._refCount === 1) {
      this._poll()
      this._interval = setInterval(() => this._poll(), 3000)
    }
  }

  stop() {
    this._refCount = Math.max(0, this._refCount - 1)
    if (this._refCount === 0 && this._interval) {
      clearInterval(this._interval)
      this._interval = null
      this._setConnected(false)
    }
  }

  onChange(listener: ConnectionListener): () => void {
    this._listeners.add(listener)
    return () => this._listeners.delete(listener)
  }

  private async _poll() {
    const ok = await healthCheck()
    this._setConnected(ok)
  }

  private _setConnected(value: boolean) {
    if (value !== this._connected) {
      this._connected = value
      for (const listener of this._listeners) listener(value)
    }
  }
}

export const connectionMonitor = new ConnectionMonitor()

// ---------------------------------------------------------------------------
// Database persistence API (/api/db/*)
// ---------------------------------------------------------------------------

const DB_BASE = '/api/db'

export async function fetchDbHealth(): Promise<DbHealthStatus> {
  const res = await fetch(`${DB_BASE}/health`)
  return res.json()
}

export async function fetchModels(): Promise<{ models: ModelRegistration[]; active: string | null }> {
  const res = await fetch(`${DB_BASE}/models`)
  if (!res.ok) return { models: [], active: null }
  return res.json()
}

export async function registerModel(params: {
  key: string
  label: string
  class_path: string
  kwargs: Record<string, unknown>
  activate?: boolean
}): Promise<{ ok: boolean; model: ModelRegistration }> {
  const res = await fetch(`${DB_BASE}/models/register`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(params),
  })
  return res.json()
}

export async function selectModel(key: string): Promise<{ ok: boolean; model: ModelRegistration }> {
  const res = await fetch(`${DB_BASE}/models/select`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ key }),
  })
  return res.json()
}

export async function deleteModel(key: string): Promise<{ ok: boolean }> {
  const res = await fetch(`${DB_BASE}/models/${key}`, { method: 'DELETE' })
  return res.json()
}

// ---------------------------------------------------------------------------
// Sessions & Agent Runs
// ---------------------------------------------------------------------------

export async function fetchDbSessions(limit = 50): Promise<SessionSummary[]> {
  const res = await fetch(`${DB_BASE}/sessions?limit=${limit}`)
  if (!res.ok) return []
  const data = await res.json()
  return data.sessions ?? []
}

export async function fetchDbSession(sessionId: string): Promise<SessionDetail | null> {
  const res = await fetch(`${DB_BASE}/sessions/${sessionId}`)
  if (!res.ok) return null
  return res.json()
}

export async function fetchSessionRuns(sessionId: string, limit = 100): Promise<AgentRunSummary[]> {
  const res = await fetch(`${DB_BASE}/sessions/${sessionId}/runs?limit=${limit}`)
  if (!res.ok) return []
  const data = await res.json()
  return data.runs ?? []
}

export async function fetchSessionUsage(sessionId: string): Promise<SessionUsage | null> {
  const res = await fetch(`${DB_BASE}/usage/${sessionId}`)
  if (!res.ok) return null
  return res.json()
}

export async function fetchGlobalUsage(): Promise<ModelUsage[]> {
  const res = await fetch(`${DB_BASE}/usage`)
  if (!res.ok) return []
  const data = await res.json()
  return data.by_model ?? []
}
