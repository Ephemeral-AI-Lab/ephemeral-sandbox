import { useEffect, useRef, useState, useCallback, KeyboardEvent } from 'react'
import { useTranscript } from '@/lib/hooks'
import { useModal } from '@/lib/hooks'
import { useConnected } from '@/lib/hooks'
import type { TranscriptItem } from '@/lib/types'

interface AgentSummary {
  name: string
  description: string
  source: string
  model: string | null
}

interface SandboxInfo {
  id: string
  name: string
  state: string
}

// ── MessageBubble ──────────────────────────────────────────────────────────────

function ToolCard({
  item,
}: {
  item: TranscriptItem & { role: 'tool' | 'tool_result' }
}) {
  const [expanded, setExpanded] = useState(false)

  if (item.role === 'tool') {
    const inputStr = item.tool_input
      ? JSON.stringify(item.tool_input, null, 2)
      : ''
    const summary = inputStr.length > 100 ? inputStr.slice(0, 100) + '…' : inputStr

    return (
      <div className="my-2 rounded-lg border border-zinc-700 bg-zinc-800 text-sm">
        <button
          className="flex w-full items-center gap-2 px-3 py-2 text-left hover:bg-zinc-700/50 transition-colors rounded-lg"
          onClick={() => setExpanded(e => !e)}
        >
          <span className="font-mono text-xs bg-zinc-700 text-zinc-300 px-1.5 py-0.5 rounded">
            {item.tool_name ?? 'tool'}
          </span>
          {!expanded && (
            <span className="text-zinc-400 font-mono text-xs truncate flex-1">
              {summary}
            </span>
          )}
          <span className="text-zinc-500 text-xs ml-auto shrink-0">
            {expanded ? '▲' : '▼'}
          </span>
        </button>
        {expanded && (
          <pre className="px-3 pb-3 text-xs font-mono text-zinc-300 whitespace-pre-wrap break-all border-t border-zinc-700 pt-2">
            {inputStr || '(no input)'}
          </pre>
        )}
      </div>
    )
  }

  // tool_result
  const preview = item.text.length > 200 ? item.text.slice(0, 200) + '…' : item.text
  const borderClass = item.is_error ? 'border-red-700' : 'border-zinc-700'

  return (
    <div className={`my-2 rounded-lg border ${borderClass} bg-zinc-800 text-sm`}>
      <button
        className="flex w-full items-center gap-2 px-3 py-2 text-left hover:bg-zinc-700/50 transition-colors rounded-lg"
        onClick={() => setExpanded(e => !e)}
      >
        <span className={`font-mono text-xs px-1.5 py-0.5 rounded ${item.is_error ? 'bg-red-900 text-red-300' : 'bg-zinc-700 text-zinc-300'}`}>
          {item.tool_name ?? 'result'}
        </span>
        {item.is_error && (
          <span className="text-red-400 text-xs">error</span>
        )}
        {!expanded && (
          <span className="text-zinc-400 font-mono text-xs truncate flex-1">
            {preview}
          </span>
        )}
        <span className="text-zinc-500 text-xs ml-auto shrink-0">
          {expanded ? '▲' : '▼'}
        </span>
      </button>
      {expanded && (
        <pre className="px-3 pb-3 text-xs font-mono text-zinc-300 whitespace-pre-wrap break-all border-t border-zinc-700 pt-2">
          {item.text || '(empty)'}
        </pre>
      )}
    </div>
  )
}

function MessageBubble({ item }: { item: TranscriptItem }) {
  if (item.role === 'user') {
    return (
      <div className="flex justify-end mb-3">
        <div className="max-w-[75%] rounded-2xl rounded-tr-sm bg-blue-600 px-4 py-2.5 text-white text-sm whitespace-pre-wrap break-words">
          {item.text}
        </div>
      </div>
    )
  }

  if (item.role === 'assistant') {
    return (
      <div className="flex justify-start mb-3">
        <div className="max-w-[85%] rounded-2xl rounded-tl-sm bg-zinc-800 px-4 py-2.5 text-zinc-100 text-sm whitespace-pre-wrap break-words">
          {item.text}
        </div>
      </div>
    )
  }

  if (item.role === 'system') {
    return (
      <div className="flex justify-center mb-2">
        <div className="rounded-full bg-yellow-500/20 px-3 py-1 text-yellow-300/80 text-xs">
          {item.text}
        </div>
      </div>
    )
  }

  if (item.role === 'tool' || item.role === 'tool_result') {
    return (
      <div className="mb-1">
        <ToolCard item={item as TranscriptItem & { role: 'tool' | 'tool_result' }} />
      </div>
    )
  }

  if (item.role === 'log') {
    return (
      <div className="mb-1 px-1 text-zinc-500 text-xs font-mono">
        {item.text}
      </div>
    )
  }

  return null
}

// ── StreamingIndicator ─────────────────────────────────────────────────────────

function StreamingIndicator({ text }: { text: string }) {
  if (!text) return null
  return (
    <div className="flex justify-start mb-3">
      <div className="max-w-[85%] rounded-2xl rounded-tl-sm bg-zinc-800 px-4 py-2.5 text-zinc-100 text-sm whitespace-pre-wrap break-words">
        {text}
        <span className="inline-block w-2 h-4 bg-zinc-300 ml-0.5 align-text-bottom animate-pulse" />
      </div>
    </div>
  )
}

// ── PromptInput ────────────────────────────────────────────────────────────────

function PromptInput({
  onSubmit,
  disabled,
  busy,
  agents,
  sandboxes,
  selectedAgent,
  selectedSandbox,
  onAgentChange,
  onSandboxChange,
}: {
  onSubmit: (line: string, options?: { agent_name?: string; sandbox_id?: string }) => void
  disabled: boolean
  busy: boolean
  agents: AgentSummary[]
  sandboxes: SandboxInfo[]
  selectedAgent: string
  selectedSandbox: string
  onAgentChange: (v: string) => void
  onSandboxChange: (v: string) => void
}) {
  const [value, setValue] = useState('')
  const textareaRef = useRef<HTMLTextAreaElement>(null)

  useEffect(() => {
    textareaRef.current?.focus()
  }, [])

  // Auto-resize textarea up to ~6 lines
  useEffect(() => {
    const el = textareaRef.current
    if (!el) return
    el.style.height = 'auto'
    const lineHeight = 24
    const maxHeight = lineHeight * 6 + 16 // 6 lines + padding
    el.style.height = Math.min(el.scrollHeight, maxHeight) + 'px'
  }, [value])

  const handleKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault()
      handleSubmit()
    }
  }

  const handleSubmit = () => {
    const trimmed = value.trim()
    if (!trimmed || disabled) return
    const opts: { agent_name?: string; sandbox_id?: string } = {}
    if (selectedAgent) opts.agent_name = selectedAgent
    if (selectedSandbox) opts.sandbox_id = selectedSandbox
    onSubmit(trimmed, Object.keys(opts).length > 0 ? opts : undefined)
    setValue('')
    if (textareaRef.current) {
      textareaRef.current.style.height = 'auto'
    }
  }

  const hasSelectors = agents.length > 0 || sandboxes.length > 0

  return (
    <div className="border-t border-zinc-800 bg-zinc-950 px-4 py-3">
      <div className="max-w-4xl mx-auto">
        {/* Agent & Sandbox selectors */}
        {hasSelectors && (
          <div className="flex items-center gap-3 mb-2">
            {agents.length > 0 && (
              <div className="flex items-center gap-1.5">
                <label className="text-xs text-zinc-500">Agent</label>
                <select
                  value={selectedAgent}
                  onChange={e => onAgentChange(e.target.value)}
                  className="rounded-lg bg-zinc-800 border border-zinc-700 px-2 py-1 text-xs text-zinc-300 outline-none focus:ring-1 focus:ring-blue-500 max-w-[180px]"
                >
                  <option value="">Default</option>
                  {agents.map(a => (
                    <option key={a.name} value={a.name}>
                      {a.name}
                    </option>
                  ))}
                </select>
              </div>
            )}
            {sandboxes.length > 0 && (
              <div className="flex items-center gap-1.5">
                <label className="text-xs text-zinc-500">Sandbox</label>
                <select
                  value={selectedSandbox}
                  onChange={e => onSandboxChange(e.target.value)}
                  className="rounded-lg bg-zinc-800 border border-zinc-700 px-2 py-1 text-xs text-zinc-300 outline-none focus:ring-1 focus:ring-blue-500 max-w-[180px]"
                >
                  <option value="">None</option>
                  {sandboxes.map(s => (
                    <option key={s.id} value={s.id} disabled={s.state !== 'started'}>
                      {s.name} {s.state !== 'started' ? `(${s.state})` : ''}
                    </option>
                  ))}
                </select>
              </div>
            )}
            {(selectedAgent || selectedSandbox) && (
              <button
                onClick={() => { onAgentChange(''); onSandboxChange('') }}
                className="text-xs text-zinc-500 hover:text-zinc-300 transition-colors"
              >
                Clear
              </button>
            )}
          </div>
        )}

        {/* Input row */}
        <div className="flex items-end gap-3">
          <textarea
            ref={textareaRef}
            value={value}
            onChange={e => setValue(e.target.value)}
            onKeyDown={handleKeyDown}
            disabled={disabled}
            placeholder={disabled ? 'Connecting…' : 'Send a message… (Shift+Enter for newline)'}
            rows={1}
            className="flex-1 resize-none rounded-xl bg-zinc-800 px-4 py-2.5 text-sm text-zinc-100 placeholder-zinc-500 outline-none focus:ring-1 focus:ring-blue-500 disabled:opacity-50 disabled:cursor-not-allowed min-h-[42px] leading-6"
          />
          <button
            onClick={handleSubmit}
            disabled={disabled || !value.trim()}
            className="shrink-0 rounded-xl bg-blue-600 px-4 py-2.5 text-sm font-medium text-white hover:bg-blue-500 disabled:opacity-40 disabled:cursor-not-allowed transition-colors min-h-[42px] flex items-center gap-2"
          >
            {busy ? (
              <span className="inline-block w-4 h-4 border-2 border-white/30 border-t-white rounded-full animate-spin" />
            ) : (
              <span>Send</span>
            )}
          </button>
        </div>
      </div>
    </div>
  )
}

// ── PermissionModal ────────────────────────────────────────────────────────────

function PermissionModal({
  toolName,
  reason,
  requestId,
  onRespond,
}: {
  toolName?: string
  reason?: string
  requestId: string
  onRespond: (id: string, allowed: boolean) => void
}) {
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60">
      <div className="w-full max-w-md rounded-2xl bg-zinc-900 border border-zinc-700 p-6 shadow-2xl">
        <h2 className="text-base font-semibold text-zinc-100 mb-1">Permission Request</h2>
        {toolName && (
          <span className="inline-block font-mono text-xs bg-zinc-700 text-zinc-300 px-2 py-0.5 rounded mb-3">
            {toolName}
          </span>
        )}
        {reason && (
          <p className="text-sm text-zinc-300 mb-5 whitespace-pre-wrap">{reason}</p>
        )}
        <div className="flex gap-3 justify-end">
          <button
            onClick={() => onRespond(requestId, false)}
            className="px-4 py-2 rounded-lg bg-red-900/60 text-red-300 text-sm font-medium hover:bg-red-800 transition-colors"
          >
            Deny
          </button>
          <button
            onClick={() => onRespond(requestId, true)}
            className="px-4 py-2 rounded-lg bg-green-800/60 text-green-300 text-sm font-medium hover:bg-green-700 transition-colors"
          >
            Allow
          </button>
        </div>
      </div>
    </div>
  )
}

// ── QuestionModal ──────────────────────────────────────────────────────────────

function QuestionModal({
  question,
  requestId,
  onRespond,
}: {
  question?: string
  requestId: string
  onRespond: (id: string, answer: string) => void
}) {
  const [answer, setAnswer] = useState('')

  const handleSubmit = () => {
    if (!answer.trim()) return
    onRespond(requestId, answer.trim())
  }

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60">
      <div className="w-full max-w-md rounded-2xl bg-zinc-900 border border-zinc-700 p-6 shadow-2xl">
        <h2 className="text-base font-semibold text-zinc-100 mb-3">Question</h2>
        {question && (
          <p className="text-sm text-zinc-300 mb-4 whitespace-pre-wrap">{question}</p>
        )}
        <input
          type="text"
          value={answer}
          onChange={e => setAnswer(e.target.value)}
          onKeyDown={e => e.key === 'Enter' && handleSubmit()}
          autoFocus
          placeholder="Your answer…"
          className="w-full rounded-lg bg-zinc-800 px-3 py-2 text-sm text-zinc-100 placeholder-zinc-500 outline-none focus:ring-1 focus:ring-blue-500 mb-4"
        />
        <div className="flex justify-end">
          <button
            onClick={handleSubmit}
            disabled={!answer.trim()}
            className="px-4 py-2 rounded-lg bg-blue-600 text-white text-sm font-medium hover:bg-blue-500 disabled:opacity-40 transition-colors"
          >
            Submit
          </button>
        </div>
      </div>
    </div>
  )
}

// ── ConversationPage ───────────────────────────────────────────────────────────

export default function ConversationPage() {
  const { items, streamingText, busy, submitLine } = useTranscript()
  const { modal, respondPermission, respondQuestion } = useModal()
  const connected = useConnected()

  const sentinelRef = useRef<HTMLDivElement>(null)
  const [agents, setAgents] = useState<AgentSummary[]>([])
  const [sandboxes, setSandboxes] = useState<SandboxInfo[]>([])
  const [selectedAgent, setSelectedAgent] = useState('')
  const [selectedSandbox, setSelectedSandbox] = useState('')

  // Fetch agents and sandboxes when connected
  useEffect(() => {
    if (!connected) return
    fetch('/api/agents')
      .then(r => r.ok ? r.json() : [])
      .then(data => setAgents(Array.isArray(data) ? data : []))
      .catch(() => {})
    fetch('/api/sandboxes')
      .then(r => r.ok ? r.json() : { sandboxes: [] })
      .then(data => setSandboxes(data.sandboxes ?? []))
      .catch(() => {})
  }, [connected])

  // Auto-scroll to bottom when items or streaming text change
  useEffect(() => {
    sentinelRef.current?.scrollIntoView({ behavior: 'smooth' })
  }, [items, streamingText])

  const handleSubmit = useCallback((line: string, options?: { agent_name?: string; sandbox_id?: string }) => {
    submitLine(line, options)
  }, [submitLine])

  const isEmpty = items.length === 0 && !streamingText

  return (
    <div className="flex flex-col h-full bg-zinc-950">
      {/* Conversation area */}
      <div className="flex-1 overflow-y-auto px-4 py-4">
        <div className="max-w-4xl mx-auto">
          {isEmpty ? (
            <div className="flex flex-col items-center justify-center h-64 text-zinc-500">
              <p className="text-base">Send a message to get started</p>
            </div>
          ) : (
            <>
              {items.map((item, i) => (
                <MessageBubble key={i} item={item} />
              ))}
              <StreamingIndicator text={streamingText} />
            </>
          )}
          <div ref={sentinelRef} />
        </div>
      </div>

      {/* Modals */}
      {modal?.kind === 'permission' && (
        <PermissionModal
          toolName={modal.tool_name}
          reason={modal.reason}
          requestId={modal.request_id}
          onRespond={respondPermission}
        />
      )}
      {modal?.kind === 'question' && (
        <QuestionModal
          question={modal.question}
          requestId={modal.request_id}
          onRespond={respondQuestion}
        />
      )}

      {/* Input */}
      <PromptInput
        onSubmit={handleSubmit}
        disabled={!connected || busy}
        busy={busy}
        agents={agents}
        sandboxes={sandboxes}
        selectedAgent={selectedAgent}
        selectedSandbox={selectedSandbox}
        onAgentChange={setSelectedAgent}
        onSandboxChange={setSelectedSandbox}
      />
    </div>
  )
}
