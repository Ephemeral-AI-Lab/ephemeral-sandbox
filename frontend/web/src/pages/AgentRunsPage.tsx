import { Fragment, useCallback, useEffect, useState, type ReactNode } from 'react'
import { useParams, useNavigate } from 'react-router'
import { fetchDbSession, fetchSessionRuns, fetchSessionUsage, fetchRunDetail, fetchSessionMessages } from '../lib/api'
import { ErrorBox, EmptyState, StatusBadge } from '../lib/components'
import type {
  AgentRunSummary,
  AgentRunDetail,
  ConversationMessagePayload,
  RunUsageSummary,
  SessionDetail,
  SessionUsage,
  SubagentRunSummary,
} from '../lib/types'

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function formatTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`
  return String(n)
}

function formatTime(iso: string | null): string {
  if (!iso) return '\u2014'
  return new Date(iso).toLocaleString()
}

function durationStr(start: string | null, end: string | null): string {
  if (!start) return '\u2014'
  const s = new Date(start).getTime()
  const e = end ? new Date(end).getTime() : Date.now()
  const ms = e - s
  if (ms < 1000) return `${ms}ms`
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)}s`
  return `${(ms / 60_000).toFixed(1)}m`
}

function usageTotals(usage: RunUsageSummary | null) {
  return {
    prompt_tokens: usage?.prompt_tokens ?? 0,
    completion_tokens: usage?.completion_tokens ?? 0,
    total_tokens: usage?.total_tokens ?? 0,
  }
}

type MessageBlock = {
  type: string
  text?: string
  name?: string
  input?: Record<string, unknown>
  content?: string
}

const ROLE_COLORS: Record<string, string> = {
  user: 'border-blue-500/30 bg-blue-500/5',
  assistant: 'border-emerald-500/30 bg-emerald-500/5',
}

const ROLE_LABELS: Record<string, string> = {
  user: 'text-blue-400',
  assistant: 'text-emerald-400',
}

const PANEL_ACCENTS: Record<string, string> = {
  amber: 'border-amber-500/30 text-amber-400',
  emerald: 'border-emerald-500/30 text-emerald-400',
  sky: 'border-sky-500/30 text-sky-400',
  violet: 'border-violet-500/30 text-violet-400',
  zinc: 'border-zinc-700 text-zinc-400',
}

function renderMessageBlock(block: MessageBlock, key: number) {
  if (block.type === 'text' && block.text) {
    return (
      <pre key={key} className="whitespace-pre-wrap break-words text-xs leading-relaxed text-zinc-300 font-mono">
        {block.text.length > 2000 ? block.text.slice(0, 2000) + '...' : block.text}
      </pre>
    )
  }
  if (block.type === 'tool_use') {
    return (
      <div key={key} className="mt-1 rounded bg-zinc-800/60 px-3 py-2 text-xs">
        <span className="font-medium text-blue-400">{block.name}</span>
        <pre className="mt-1 max-h-24 overflow-auto text-[10px] text-zinc-500 font-mono">
          {JSON.stringify(block.input, null, 2)}
        </pre>
      </div>
    )
  }
  if (block.type === 'tool_result') {
    return (
      <div key={key} className="mt-1 rounded bg-zinc-800/60 px-3 py-2 text-xs">
        <span className="font-medium text-emerald-400">tool_result</span>
        <pre className="mt-1 max-h-24 overflow-auto text-[10px] text-zinc-500 font-mono">
          {typeof block.content === 'string'
            ? (block.content.length > 500 ? block.content.slice(0, 500) + '...' : block.content)
            : JSON.stringify(block.content, null, 2)}
        </pre>
      </div>
    )
  }
  return null
}

function MessageCard({
  role,
  content,
  text,
}: {
  role: string
  content?: MessageBlock[]
  text?: string | null
}) {
  return (
    <div className={`rounded-lg border px-4 py-3 ${ROLE_COLORS[role] ?? 'border-zinc-700 bg-zinc-800/50'}`}>
      <div className={`mb-1.5 text-[10px] font-semibold uppercase tracking-wider ${ROLE_LABELS[role] ?? 'text-zinc-500'}`}>
        {role}
      </div>
      {content?.map(renderMessageBlock)}
      {!content && text && (
        <pre className="whitespace-pre-wrap break-words text-xs leading-relaxed text-zinc-300 font-mono">
          {text.length > 2000 ? text.slice(0, 2000) + '...' : text}
        </pre>
      )}
    </div>
  )
}

// ---------------------------------------------------------------------------
// Conversation History Panel
// ---------------------------------------------------------------------------

function ConversationHistoryPanel({ sessionId }: { sessionId: string }) {
  const [expanded, setExpanded] = useState(false)
  const [messages, setMessages] = useState<ConversationMessagePayload[]>([])
  const [error, setError] = useState<string | null>(null)
  const [loading, setLoading] = useState(false)
  const [loaded, setLoaded] = useState(false)

  useEffect(() => {
    if (!expanded || loaded) return
    let cancelled = false
    setLoading(true)
    setError(null)
    fetchSessionMessages(sessionId)
      .then((data) => {
        if (!cancelled) {
          setMessages(data)
          setLoaded(true)
        }
      })
      .catch((err) => {
        if (!cancelled) {
          setError(err instanceof Error ? err.message : String(err))
        }
      })
      .finally(() => {
        if (!cancelled) {
          setLoading(false)
        }
      })
    return () => { cancelled = true }
  }, [expanded, loaded, sessionId])

  return (
    <div className="mb-6 rounded-lg border border-zinc-800 bg-zinc-900">
      <button
        onClick={() => setExpanded(!expanded)}
        className="flex w-full items-center justify-between px-4 py-3 text-left text-xs text-zinc-400 hover:text-zinc-200"
      >
        <span className="font-medium">
          Conversation History
          {loaded && <span className="ml-2 text-zinc-600">({messages.length} messages)</span>}
        </span>
        <span className="text-zinc-600">{expanded ? '\u25B2' : '\u25BC'}</span>
      </button>
      {expanded && (
        <div className="border-t border-zinc-800 px-4 py-3">
          {loading && <p className="text-xs text-zinc-500">Loading messages...</p>}
          {error && <p className="text-xs text-red-400">{error}</p>}
          {loaded && !error && messages.length === 0 && (
            <p className="text-xs text-zinc-500">No messages recorded.</p>
          )}
          {loaded && !error && messages.length > 0 && (
            <div className="max-h-[32rem] space-y-3 overflow-y-auto">
              {messages.map((msg, i) => (
                <MessageCard
                  key={i}
                  role={msg.role}
                  content={msg.content as MessageBlock[] | undefined}
                />
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  )
}

// ---------------------------------------------------------------------------
// Collapsible message list (shared by message_history / compacted_history / response)
// ---------------------------------------------------------------------------

function CollapsiblePanel({
  title,
  badge,
  accentColor = 'zinc',
  defaultOpen = false,
  children,
}: {
  title: string
  badge?: string
  accentColor?: string
  defaultOpen?: boolean
  children: ReactNode
}) {
  const [open, setOpen] = useState(defaultOpen)
  const accent = PANEL_ACCENTS[accentColor] ?? PANEL_ACCENTS.zinc

  return (
    <div className={`rounded-lg border ${accent.split(' ')[0]} bg-zinc-950/50`}>
      <button
        onClick={() => setOpen(!open)}
        className={`flex w-full items-center justify-between px-4 py-2.5 text-left text-xs hover:bg-zinc-800/30 ${accent.split(' ').slice(1).join(' ')}`}
      >
        <span className="font-medium">
          {title}
          {badge && <span className="ml-2 text-zinc-600">({badge})</span>}
        </span>
        <span className="text-zinc-600">{open ? '\u25B2' : '\u25BC'}</span>
      </button>
      {open && (
        <div className="border-t border-zinc-800 px-4 py-3">
          {children}
        </div>
      )}
    </div>
  )
}

function CollapsibleMessageList({
  title,
  messages,
  accentColor = 'zinc',
  defaultOpen = false,
}: {
  title: string
  messages: Record<string, unknown>[]
  accentColor?: string
  defaultOpen?: boolean
}) {
  return (
    <CollapsiblePanel
      title={title}
      badge={`${messages.length} message${messages.length !== 1 ? 's' : ''}`}
      accentColor={accentColor}
      defaultOpen={defaultOpen}
    >
      <div className="max-h-[32rem] space-y-3 overflow-y-auto">
        {messages.map((msg, i) => {
          const role = (msg.role as string) ?? 'unknown'
          const content = msg.content as MessageBlock[] | undefined
          const text = (msg.text as string) ?? null

          return (
            <MessageCard
              key={i}
              role={role}
              content={content}
              text={text}
            />
          )
        })}
      </div>
    </CollapsiblePanel>
  )
}

// ---------------------------------------------------------------------------
// Collapsible plain text (for reasoning)
// ---------------------------------------------------------------------------

function CollapsibleText({
  title,
  text,
  accentColor = 'zinc',
  defaultOpen = false,
}: {
  title: string
  text: string
  accentColor?: string
  defaultOpen?: boolean
}) {
  return (
    <CollapsiblePanel
      title={title}
      accentColor={accentColor}
      defaultOpen={defaultOpen}
    >
      <div className="max-h-[32rem] overflow-y-auto">
        <pre className="whitespace-pre-wrap break-words text-xs leading-relaxed text-zinc-300 font-mono">
          {text}
        </pre>
      </div>
    </CollapsiblePanel>
  )
}

// ---------------------------------------------------------------------------
// Run usage summary + subagent table
// ---------------------------------------------------------------------------

function RunUsageSummaryStrip({
  parentUsage,
  subagentRuns,
}: {
  parentUsage: RunUsageSummary | null
  subagentRuns: SubagentRunSummary[]
}) {
  const parent = usageTotals(parentUsage)
  const hasParentUsage = parentUsage !== null
  const subagents = subagentRuns.reduce(
    (sum, run) => {
      const usage = usageTotals(run.usage)
      return {
        prompt_tokens: sum.prompt_tokens + usage.prompt_tokens,
        completion_tokens: sum.completion_tokens + usage.completion_tokens,
        total_tokens: sum.total_tokens + usage.total_tokens,
      }
    },
    { prompt_tokens: 0, completion_tokens: 0, total_tokens: 0 },
  )
  const hasSubagentUsage = subagentRuns.some((run) => run.usage !== null)
  const combined = {
    prompt_tokens: parent.prompt_tokens + subagents.prompt_tokens,
    completion_tokens: parent.completion_tokens + subagents.completion_tokens,
    total_tokens: parent.total_tokens + subagents.total_tokens,
  }
  const hasCombinedUsage = hasParentUsage || hasSubagentUsage

  return (
    <div className="grid gap-3 md:grid-cols-3">
      <div className="rounded-lg border border-zinc-800 bg-zinc-950/50 p-4">
        <div className="text-[10px] font-medium uppercase tracking-wider text-zinc-500">Parent Run</div>
        <div className="mt-2 font-mono text-sm text-zinc-100">{hasParentUsage ? formatTokens(parent.total_tokens) : '\u2014'}</div>
        <div className="mt-1 text-[11px] text-zinc-500">
          {hasParentUsage ? `${formatTokens(parent.prompt_tokens)} in / ${formatTokens(parent.completion_tokens)} out` : '\u2014'}
        </div>
      </div>
      <div className="rounded-lg border border-zinc-800 bg-zinc-950/50 p-4">
        <div className="text-[10px] font-medium uppercase tracking-wider text-zinc-500">Subagents</div>
        <div className="mt-2 font-mono text-sm text-zinc-100">{hasSubagentUsage ? formatTokens(subagents.total_tokens) : '\u2014'}</div>
        <div className="mt-1 text-[11px] text-zinc-500">
          {hasSubagentUsage ? `${formatTokens(subagents.prompt_tokens)} in / ${formatTokens(subagents.completion_tokens)} out` : '\u2014'}
        </div>
      </div>
      <div className="rounded-lg border border-zinc-800 bg-zinc-950/50 p-4">
        <div className="text-[10px] font-medium uppercase tracking-wider text-zinc-500">Run Tree Total</div>
        <div className="mt-2 font-mono text-sm text-zinc-100">{hasCombinedUsage ? formatTokens(combined.total_tokens) : '\u2014'}</div>
        <div className="mt-1 text-[11px] text-zinc-500">
          {hasCombinedUsage ? `${formatTokens(combined.prompt_tokens)} in / ${formatTokens(combined.completion_tokens)} out` : '\u2014'}
        </div>
      </div>
    </div>
  )
}

function SubagentRunsTable({ runs }: { runs: SubagentRunSummary[] }) {
  if (runs.length === 0) return null

  return (
    <div className="rounded-lg border border-zinc-800 bg-zinc-950/50">
      <div className="border-b border-zinc-800 px-4 py-2 text-[10px] font-medium uppercase tracking-wider text-zinc-500">
        Subagent Runs ({runs.length})
      </div>
      <div className="overflow-x-auto">
        <table className="w-full text-xs">
          <thead className="bg-zinc-950 text-[10px] text-zinc-600">
            <tr>
              <th className="px-3 py-2 text-left">Task</th>
              <th className="px-3 py-2 text-left">Agent</th>
              <th className="px-3 py-2 text-left">Model</th>
              <th className="px-3 py-2 text-left">Status</th>
              <th className="px-3 py-2 text-left">Input</th>
              <th className="px-3 py-2 text-right">Prompt</th>
              <th className="px-3 py-2 text-right">Completion</th>
              <th className="px-3 py-2 text-right">Total</th>
              <th className="px-3 py-2 text-right">Events</th>
              <th className="px-3 py-2 text-left">Started</th>
            </tr>
          </thead>
          <tbody className="divide-y divide-zinc-800/30">
            {runs.map((run) => (
              <tr key={run.id} className="hover:bg-zinc-800/20">
                <td className="px-3 py-2 font-mono text-zinc-500">{run.parent_task_id || '\u2014'}</td>
                <td className="px-3 py-2 text-zinc-100">{run.agent_name}</td>
                <td className="px-3 py-2 font-mono text-zinc-500">{run.usage?.model_id || '\u2014'}</td>
                <td className="px-3 py-2">
                  <StatusBadge status={run.status} />
                </td>
                <td className="max-w-xs truncate px-3 py-2 text-zinc-400">{run.input_query || '\u2014'}</td>
                <td className="px-3 py-2 text-right font-mono text-zinc-400">
                  {run.usage ? formatTokens(run.usage.prompt_tokens) : '\u2014'}
                </td>
                <td className="px-3 py-2 text-right font-mono text-zinc-400">
                  {run.usage ? formatTokens(run.usage.completion_tokens) : '\u2014'}
                </td>
                <td className="px-3 py-2 text-right font-mono text-zinc-100">
                  {run.usage ? formatTokens(run.usage.total_tokens) : '\u2014'}
                </td>
                <td className="px-3 py-2 text-right font-mono text-zinc-500">{run.event_count}</td>
                <td className="whitespace-nowrap px-3 py-2 text-zinc-500">{formatTime(run.started_at)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  )
}

// ---------------------------------------------------------------------------
// Expandable Run Detail (message_history, compacted_history, reasoning, response)
// ---------------------------------------------------------------------------

function RunDetailPanel({ runId }: { runId: string }) {
  const [detail, setDetail] = useState<AgentRunDetail | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [loading, setLoading] = useState(true)

  useEffect(() => {
    let cancelled = false
    setLoading(true)
    setError(null)
    fetchRunDetail(runId)
      .then((d) => {
        if (!cancelled) {
          setDetail(d)
        }
      })
      .catch((err) => {
        if (!cancelled) {
          setError(err instanceof Error ? err.message : String(err))
        }
      })
      .finally(() => {
        if (!cancelled) {
          setLoading(false)
        }
      })
    return () => { cancelled = true }
  }, [runId])

  if (loading) {
    return (
      <tr>
        <td colSpan={8} className="px-6 py-4 text-xs text-zinc-500">
          Loading run details...
        </td>
      </tr>
    )
  }

  if (error) {
    return (
      <tr>
        <td colSpan={8} className="px-6 py-4 text-xs text-red-400">
          {error}
        </td>
      </tr>
    )
  }

  const messageHistory = detail?.message_history ?? null
  const compactedHistory = detail?.compacted_history ?? null
  const response = detail?.response ?? null
  const reasoning = detail?.reasoning ?? null
  const usage = detail?.usage ?? null
  const subagentRuns = detail?.subagent_runs ?? []
  const hasContent = Boolean(
    usage ||
    subagentRuns.length > 0 ||
    (messageHistory && messageHistory.length > 0) ||
    (compactedHistory && compactedHistory.length > 0) ||
    (response && response.length > 0) ||
    reasoning
  )

  if (!hasContent) {
    return (
      <tr>
        <td colSpan={8} className="px-6 py-4 text-xs text-zinc-500">
          No details recorded for this run.
        </td>
      </tr>
    )
  }

  return (
    <tr>
      <td colSpan={8} className="px-0 py-0">
        <div className="mx-4 my-3 space-y-3">
          <RunUsageSummaryStrip parentUsage={usage} subagentRuns={subagentRuns} />
          <SubagentRunsTable runs={subagentRuns} />
          {messageHistory && messageHistory.length > 0 && (
            <CollapsibleMessageList
              title="Message History"
              messages={messageHistory}
              accentColor="amber"
            />
          )}
          {compactedHistory && compactedHistory.length > 0 && (
            <CollapsibleMessageList
              title="Compacted History"
              messages={compactedHistory}
              accentColor="sky"
            />
          )}
          {reasoning && (
            <CollapsibleText
              title="Reasoning"
              text={reasoning}
              accentColor="violet"
            />
          )}
          {response && response.length > 0 && (
            <CollapsibleMessageList
              title="Response"
              messages={response}
              accentColor="emerald"
              defaultOpen={true}
            />
          )}
        </div>
      </td>
    </tr>
  )
}

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

export default function AgentRunsPage() {
  const { sessionId } = useParams<{ sessionId: string }>()
  const navigate = useNavigate()
  const [session, setSession] = useState<SessionDetail | null>(null)
  const [runs, setRuns] = useState<AgentRunSummary[]>([])
  const [usage, setUsage] = useState<SessionUsage | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [expandedRunId, setExpandedRunId] = useState<string | null>(null)

  const load = useCallback(async () => {
    if (!sessionId) return
    setLoading(true)
    setError(null)
    try {
      const [sess, runList, usg] = await Promise.all([
        fetchDbSession(sessionId),
        fetchSessionRuns(sessionId),
        fetchSessionUsage(sessionId),
      ])
      setSession(sess)
      setRuns(runList)
      setUsage(usg)
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err))
    } finally {
      setLoading(false)
    }
  }, [sessionId])

  useEffect(() => { load() }, [load])

  if (!sessionId) return <p className="p-6 text-sm text-zinc-500">No session selected.</p>

  // Distinct agent names in this session
  const uniqueAgents = new Set(runs.map((r) => r.agent_name))

  return (
    <div className="p-6">
      {/* Back nav + header */}
      <div className="mb-4">
        <button
          onClick={() => navigate('/sessions')}
          className="text-xs text-zinc-500 hover:text-zinc-300"
        >
          &larr; All Sessions
        </button>
      </div>

      <div className="mb-6 flex items-center justify-between">
        <div>
          <h1 className="text-lg font-semibold text-zinc-100">
            {session?.summary || `Session ${sessionId.slice(0, 8)}`}
          </h1>
          <div className="mt-1 flex items-center gap-3 text-xs text-zinc-500">
            {session && (
              <>
                <span>{session.message_count} messages</span>
                <span className="text-zinc-700">|</span>
                <span className="font-mono">{sessionId.slice(0, 12)}</span>
              </>
            )}
          </div>
        </div>
        <button
          onClick={load}
          className="rounded bg-zinc-800 px-3 py-1 text-xs text-zinc-300 hover:bg-zinc-700"
        >
          Refresh
        </button>
      </div>

      {/* Usage summary cards */}
      <div className="mb-6 grid grid-cols-5 gap-4">
        <div className="rounded-lg border border-zinc-800 bg-zinc-900 p-4">
          <div className="text-xs text-zinc-500">Ephemeral Agents</div>
          <div className="mt-1 text-xl font-semibold text-zinc-100">{runs.length}</div>
          <div className="mt-0.5 text-[10px] text-zinc-600">
            {uniqueAgents.size} distinct
          </div>
        </div>
        <div className="rounded-lg border border-zinc-800 bg-zinc-900 p-4">
          <div className="text-xs text-zinc-500">Prompt Tokens</div>
          <div className="mt-1 text-xl font-semibold text-zinc-100">
            {usage ? formatTokens(usage.prompt_tokens) : '\u2014'}
          </div>
        </div>
        <div className="rounded-lg border border-zinc-800 bg-zinc-900 p-4">
          <div className="text-xs text-zinc-500">Completion Tokens</div>
          <div className="mt-1 text-xl font-semibold text-zinc-100">
            {usage ? formatTokens(usage.completion_tokens) : '\u2014'}
          </div>
        </div>
        <div className="rounded-lg border border-zinc-800 bg-zinc-900 p-4">
          <div className="text-xs text-zinc-500">Total Tokens</div>
          <div className="mt-1 text-xl font-semibold text-zinc-100">
            {usage ? formatTokens(usage.total_tokens) : '\u2014'}
          </div>
        </div>
        <div className="rounded-lg border border-zinc-800 bg-zinc-900 p-4">
          <div className="text-xs text-zinc-500">Tracked Runs</div>
          <div className="mt-1 text-xl font-semibold text-zinc-100">
            {usage ? usage.call_count : '\u2014'}
          </div>
        </div>
      </div>

      {/* Conversation history (full, uncompacted) */}
      {sessionId && !loading && (
        <ConversationHistoryPanel sessionId={sessionId} />
      )}

      {loading && <p className="text-sm text-zinc-500">Loading agent runs...</p>}
      {error && <ErrorBox message={error} />}

      {!loading && runs.length === 0 && (
        <EmptyState message="No ephemeral agents have run in this session yet." />
      )}

      {!loading && runs.length > 0 && (
        <div className="overflow-hidden rounded-lg border border-zinc-800">
          <table className="w-full text-sm">
            <thead className="border-b border-zinc-800 bg-zinc-900/80 text-left text-xs text-zinc-500">
              <tr>
                <th className="w-8 px-4 py-2 text-center">#</th>
                <th className="px-4 py-2">Agent</th>
                <th className="px-4 py-2">Status</th>
                <th className="px-4 py-2">Input</th>
                <th className="px-4 py-2 text-right">Events</th>
                <th className="px-4 py-2 text-right">Duration</th>
                <th className="px-4 py-2">Started</th>
                <th className="px-4 py-2">Error</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-zinc-800/50">
              {runs.map((r, idx) => {
                const isExpanded = expandedRunId === r.id
                return (
                  <Fragment key={r.id}>
                    <tr
                      className={`text-zinc-300 transition cursor-pointer ${isExpanded ? 'bg-zinc-800/60' : 'hover:bg-zinc-800/50'}`}
                      onClick={() => setExpandedRunId(isExpanded ? null : r.id)}
                    >
                      <td className="px-4 py-2.5 text-center text-xs text-zinc-600">
                        <span className="inline-flex items-center gap-1">
                          <span className={`inline-block w-3 text-[10px] text-zinc-600 transition-transform ${isExpanded ? 'rotate-90' : ''}`}>
                            &#9654;
                          </span>
                          {runs.length - idx}
                        </span>
                      </td>
                      <td className="px-4 py-2.5 font-medium text-zinc-100">
                        <span className="rounded bg-zinc-800 px-1.5 py-0.5 text-xs">
                          {r.agent_name}
                        </span>
                      </td>
                      <td className="px-4 py-2.5">
                        <StatusBadge status={r.status} />
                      </td>
                      <td className="max-w-xs truncate px-4 py-2.5 text-xs text-zinc-400">
                        {r.input_query || '\u2014'}
                      </td>
                      <td className="px-4 py-2.5 text-right font-mono text-xs">{r.event_count}</td>
                      <td className="px-4 py-2.5 text-right font-mono text-xs">
                        {durationStr(r.started_at, r.finished_at)}
                      </td>
                      <td className="whitespace-nowrap px-4 py-2.5 text-xs text-zinc-500">
                        {formatTime(r.started_at)}
                      </td>
                      <td className="max-w-[200px] truncate px-4 py-2.5 text-xs text-red-400">
                        {r.error || ''}
                      </td>
                    </tr>
                    {isExpanded && <RunDetailPanel runId={r.id} />}
                  </Fragment>
                )
              })}
            </tbody>
          </table>
        </div>
      )}
    </div>
  )
}
