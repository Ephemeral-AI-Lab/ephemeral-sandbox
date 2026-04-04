import { useState, useEffect } from 'react'
import { Link } from 'react-router'
import { useAppState, useTasks, useToolkits, useMcpServers, useBridgeSessions } from '@/lib/hooks'
import { fetchDbHealth } from '@/lib/api'
import type { TaskSnapshot, ToolkitSnapshot, McpServerSnapshot, BridgeSessionSnapshot, DbHealthStatus } from '@/lib/types'

// ── MetricCard ────────────────────────────────────────────────────────────────

interface MetricCardProps {
  label: string
  value: string
  accent?: string
}

function MetricCard({ label, value, accent }: MetricCardProps) {
  return (
    <div className="flex flex-col gap-1 rounded-lg bg-zinc-900 border border-zinc-800 px-4 py-3 min-w-0">
      <span className="text-xs text-zinc-500 uppercase tracking-wide font-medium">{label}</span>
      <span className={`text-base font-semibold truncate ${accent ?? 'text-zinc-100'}`}>{value}</span>
    </div>
  )
}

// ── StatusBadge ───────────────────────────────────────────────────────────────

const STATUS_COLORS: Record<string, string> = {
  running: 'bg-green-900 text-green-300 border-green-800',
  completed: 'bg-blue-900 text-blue-300 border-blue-800',
  failed: 'bg-red-900 text-red-300 border-red-800',
  pending: 'bg-yellow-900 text-yellow-300 border-yellow-800',
  killed: 'bg-zinc-800 text-zinc-400 border-zinc-700',
  connected: 'bg-green-900 text-green-300 border-green-800',
  disconnected: 'bg-red-900 text-red-300 border-red-800',
  connecting: 'bg-yellow-900 text-yellow-300 border-yellow-800',
}

function StatusBadge({ status }: { status: string }) {
  const color = STATUS_COLORS[status.toLowerCase()] ?? 'bg-zinc-800 text-zinc-400 border-zinc-700'
  return (
    <span className={`inline-flex items-center rounded-full border px-2 py-0.5 text-xs font-medium ${color}`}>
      {status}
    </span>
  )
}

// ── TaskCard ──────────────────────────────────────────────────────────────────

function TaskCard({ task }: { task: TaskSnapshot }) {
  return (
    <Link
      to={`/tasks/${task.id}`}
      className="block rounded-lg bg-zinc-900 border border-zinc-800 px-4 py-3 hover:border-zinc-600 hover:bg-zinc-800 transition-colors"
    >
      <div className="flex items-start justify-between gap-2">
        <p className="text-sm text-zinc-100 line-clamp-2 flex-1">{task.description || '(no description)'}</p>
        <StatusBadge status={task.status} />
      </div>
      <div className="mt-2 flex items-center gap-2">
        <span className="inline-flex items-center rounded border border-zinc-700 bg-zinc-800 px-1.5 py-0.5 text-xs text-zinc-400 font-mono">
          {task.type}
        </span>
        <span className="text-xs text-zinc-600 font-mono truncate">{task.id}</span>
      </div>
      {Object.keys(task.metadata).length > 0 && (
        <div className="mt-2 flex flex-wrap gap-1">
          {Object.entries(task.metadata).map(([k, v]) => (
            <span key={k} className="text-xs text-zinc-500">
              <span className="text-zinc-600">{k}:</span> {v}
            </span>
          ))}
        </div>
      )}
    </Link>
  )
}

// ── McpServerCard ─────────────────────────────────────────────────────────────

function McpServerCard({ server }: { server: McpServerSnapshot }) {
  return (
    <div className="rounded-lg bg-zinc-900 border border-zinc-800 px-4 py-3">
      <div className="flex items-center justify-between gap-2">
        <span className="text-sm font-medium text-zinc-100 truncate">{server.name}</span>
        <StatusBadge status={server.state} />
      </div>
      <div className="mt-2 flex gap-3 text-xs text-zinc-500">
        {server.tool_count !== undefined && (
          <span>{server.tool_count} tool{server.tool_count !== 1 ? 's' : ''}</span>
        )}
        {server.resource_count !== undefined && (
          <span>{server.resource_count} resource{server.resource_count !== 1 ? 's' : ''}</span>
        )}
        {server.transport && <span className="text-zinc-600">{server.transport}</span>}
      </div>
      {server.detail && (
        <p className="mt-1 text-xs text-zinc-600 truncate">{server.detail}</p>
      )}
    </div>
  )
}

// ── ToolkitCard ──────────────────────────────────────────────────────────────

function ToolkitCard({ toolkit }: { toolkit: ToolkitSnapshot }) {
  const [expanded, setExpanded] = useState(false)
  return (
    <div
      className="rounded-lg bg-zinc-900 border border-zinc-800 px-4 py-3 hover:border-zinc-600 transition-colors cursor-pointer"
      onClick={() => setExpanded(!expanded)}
    >
      <div className="flex items-center justify-between gap-2">
        <span className="text-sm font-medium text-zinc-100">{toolkit.name}</span>
        <span className="inline-flex items-center rounded-full border border-zinc-700 bg-zinc-800 px-2 py-0.5 text-xs font-medium text-zinc-400">
          {toolkit.tools.length} tool{toolkit.tools.length !== 1 ? 's' : ''}
        </span>
      </div>
      <p className="mt-1 text-xs text-zinc-500">{toolkit.description}</p>
      {expanded && (
        <div className="mt-2 flex flex-wrap gap-1">
          {toolkit.tools.map(tool => (
            <span
              key={tool}
              className="inline-flex items-center rounded border border-zinc-700 bg-zinc-800 px-1.5 py-0.5 text-xs text-zinc-400 font-mono"
            >
              {tool}
            </span>
          ))}
        </div>
      )}
    </div>
  )
}

// ── BridgeSessionCard ─────────────────────────────────────────────────────────

function BridgeSessionCard({ session }: { session: BridgeSessionSnapshot }) {
  return (
    <div className="rounded-lg bg-zinc-900 border border-zinc-800 px-4 py-3">
      <div className="flex items-center justify-between gap-2">
        <span className="text-sm font-mono text-zinc-100 truncate">{session.command}</span>
        <StatusBadge status={session.status} />
      </div>
      <div className="mt-2 flex gap-3 text-xs text-zinc-500">
        <span className="truncate">{session.cwd}</span>
        <span className="shrink-0">PID {session.pid}</span>
      </div>
    </div>
  )
}

// ── DashboardPage ─────────────────────────────────────────────────────────────

export default function DashboardPage() {
  const appState = useAppState()
  const tasks = useTasks()
  const toolkits = useToolkits()
  const servers = useMcpServers()
  const sessions = useBridgeSessions()
  const [dbHealth, setDbHealth] = useState<DbHealthStatus | null>(null)

  useEffect(() => {
    fetchDbHealth().then(setDbHealth).catch(() => {})
  }, [])

  if (!appState) {
    return (
      <div className="flex items-center justify-center h-full p-6">
        <span className="text-zinc-500 text-sm">Connecting...</span>
      </div>
    )
  }

  const runningTasks = tasks.filter(t => t.status === 'running').length
  const mcpTotal = appState.mcp_connected + appState.mcp_failed
  const totalTools = toolkits.reduce((sum, tk) => sum + tk.tools.length, 0)

  return (
    <div className="min-h-full bg-zinc-950 text-zinc-100 p-6 space-y-6">
      {/* Metrics Row */}
      <div className="grid grid-cols-2 sm:grid-cols-3 lg:grid-cols-6 gap-3">
        <MetricCard label="Model" value={appState.model} />
        <MetricCard label="Effort" value={appState.effort} />
        <MetricCard
          label="Toolkits"
          value={`${toolkits.length} (${totalTools} tools)`}
        />
        <MetricCard
          label="MCP"
          value={`${appState.mcp_connected} / ${mcpTotal}`}
          accent={appState.mcp_failed > 0 ? 'text-yellow-400' : 'text-green-400'}
        />
        <MetricCard
          label="Tasks"
          value={`${runningTasks} / ${tasks.length}`}
          accent={runningTasks > 0 ? 'text-green-400' : 'text-zinc-100'}
        />
        <MetricCard
          label="Database"
          value={dbHealth?.database === 'connected' ? 'Connected' : 'Off'}
          accent={dbHealth?.database === 'connected' ? 'text-green-400' : 'text-zinc-500'}
        />
      </div>

      {/* Toolkits */}
      <section>
        <h2 className="text-sm font-semibold text-zinc-400 uppercase tracking-wide mb-3">
          Toolkits
        </h2>
        {toolkits.length === 0 ? (
          <div className="rounded-lg border border-zinc-800 px-4 py-6 text-center text-sm text-zinc-600">
            No toolkits registered
          </div>
        ) : (
          <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-2">
            {toolkits.map(tk => (
              <ToolkitCard key={tk.name} toolkit={tk} />
            ))}
          </div>
        )}
      </section>

      {/* Tasks + MCP Servers */}
      <div className="grid grid-cols-1 lg:grid-cols-2 gap-6">
        {/* Tasks Section */}
        <section>
          <h2 className="text-sm font-semibold text-zinc-400 uppercase tracking-wide mb-3">
            Background Tasks
          </h2>
          {tasks.length === 0 ? (
            <div className="rounded-lg border border-zinc-800 px-4 py-6 text-center text-sm text-zinc-600">
              No background tasks
            </div>
          ) : (
            <div className="space-y-2">
              {tasks.map(task => (
                <TaskCard key={task.id} task={task} />
              ))}
            </div>
          )}
        </section>

        {/* MCP Servers Section */}
        <section>
          <h2 className="text-sm font-semibold text-zinc-400 uppercase tracking-wide mb-3">
            MCP Servers
          </h2>
          {servers.length === 0 ? (
            <div className="rounded-lg border border-zinc-800 px-4 py-6 text-center text-sm text-zinc-600">
              No MCP servers configured
            </div>
          ) : (
            <div className="space-y-2">
              {servers.map(server => (
                <McpServerCard key={server.name} server={server} />
              ))}
            </div>
          )}
        </section>
      </div>

      {/* Bridge Sessions */}
      {sessions.length > 0 && (
        <section>
          <h2 className="text-sm font-semibold text-zinc-400 uppercase tracking-wide mb-3">
            Bridge Sessions
          </h2>
          <div className="space-y-2">
            {sessions.map(session => (
              <BridgeSessionCard key={session.session_id} session={session} />
            ))}
          </div>
        </section>
      )}

      {sessions.length === 0 && (
        <section>
          <h2 className="text-sm font-semibold text-zinc-400 uppercase tracking-wide mb-3">
            Bridge Sessions
          </h2>
          <div className="rounded-lg border border-zinc-800 px-4 py-6 text-center text-sm text-zinc-600">
            No bridge sessions
          </div>
        </section>
      )}
    </div>
  )
}
