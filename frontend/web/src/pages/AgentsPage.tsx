import { useCallback, useEffect, useState } from 'react'

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

interface AgentSummary {
  name: string
  description: string
  source: string
  model: string | null
  subagent_type: string
  background: boolean
}

interface AgentDetail {
  id?: string
  name: string
  description: string
  system_prompt: string | null
  model: string | null
  effort: string | null
  max_turns: number | null
  tools: string[] | null
  disallowed_tools: string[] | null
  toolkits: string[] | null
  skills: string[]
  hooks: Record<string, unknown> | null
  background: boolean
  initial_prompt: string | null
  subagent_type: string
  version?: number
  is_active?: boolean
  tags?: string[] | null
  source?: string
  created_at?: string
  updated_at?: string
}

interface AvailableTool {
  name: string
  description: string
}

interface ValidationResult {
  valid: boolean
  errors: string[]
  warnings: string[]
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const EFFORT_LEVELS = ['low', 'medium', 'high']

// ---------------------------------------------------------------------------
// API helpers
// ---------------------------------------------------------------------------

const API = '/api/agents'

async function parseJsonResponse<T>(res: Response): Promise<T> {
  const text = await res.text()
  try {
    return JSON.parse(text)
  } catch {
    throw new Error(res.ok ? 'Backend returned non-JSON response — is the API server running?' : `${res.status}: ${text.slice(0, 100)}`)
  }
}

async function fetchAgents(): Promise<AgentSummary[]> {
  const res = await fetch(API)
  const data = await parseJsonResponse<AgentSummary[] | { detail: string }>(res)
  if (!res.ok) throw new Error((data as { detail: string }).detail ?? res.statusText)
  return data as AgentSummary[]
}

async function fetchAgent(name: string): Promise<AgentDetail> {
  const res = await fetch(`${API}/${encodeURIComponent(name)}`)
  const data = await parseJsonResponse<AgentDetail | { detail: string }>(res)
  if (!res.ok) throw new Error((data as { detail: string }).detail ?? res.statusText)
  return data as AgentDetail
}

async function createAgent(data: Record<string, unknown>): Promise<AgentDetail> {
  const res = await fetch(API, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(data),
  })
  if (!res.ok) throw new Error((await res.json()).detail ?? res.statusText)
  return res.json()
}

async function updateAgent(name: string, data: Record<string, unknown>): Promise<AgentDetail> {
  const res = await fetch(`${API}/${encodeURIComponent(name)}`, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(data),
  })
  if (!res.ok) throw new Error((await res.json()).detail ?? res.statusText)
  return res.json()
}

async function deleteAgent(name: string): Promise<void> {
  const res = await fetch(`${API}/${encodeURIComponent(name)}`, { method: 'DELETE' })
  if (!res.ok) throw new Error((await res.json()).detail ?? res.statusText)
}

async function cloneAgent(name: string, newName: string): Promise<AgentDetail> {
  const res = await fetch(`${API}/${encodeURIComponent(name)}/clone`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ new_name: newName }),
  })
  if (!res.ok) throw new Error((await res.json()).detail ?? res.statusText)
  return res.json()
}

async function validateAgent(data: Record<string, unknown>): Promise<ValidationResult> {
  const res = await fetch(`${API}/validate`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(data),
  })
  if (!res.ok) throw new Error((await res.json()).detail ?? res.statusText)
  return res.json()
}

async function fetchAvailableTools(): Promise<AvailableTool[]> {
  const res = await fetch(`${API}/tools/available`)
  if (!res.ok) return []
  return res.json()
}

async function fetchAvailableToolkits(): Promise<string[]> {
  const res = await fetch(`${API}/toolkits/available`)
  if (!res.ok) return []
  return res.json()
}

// ---------------------------------------------------------------------------
// Sub-components
// ---------------------------------------------------------------------------

function SourceBadge({ source }: { source: string }) {
  const color = source === 'builtin'
    ? 'bg-indigo-900/30 text-indigo-400 border-indigo-800/50'
    : 'bg-emerald-900/30 text-emerald-400 border-emerald-800/50'
  return (
    <span className={`inline-flex items-center rounded-full border px-2 py-0.5 text-xs font-medium ${color}`}>
      {source}
    </span>
  )
}

// ---------------------------------------------------------------------------
// Agent Card (list view)
// ---------------------------------------------------------------------------

function AgentCard({
  agent,
  onSelect,
  onClone,
  onDelete,
}: {
  agent: AgentSummary
  onSelect: () => void
  onClone: () => void
  onDelete: () => void
}) {
  return (
    <div className="rounded-lg bg-zinc-900 border border-zinc-800 px-4 py-3 hover:border-zinc-600 transition-colors">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0 flex-1 cursor-pointer" onClick={onSelect}>
          <div className="flex items-center gap-2">
            <span className="text-sm font-medium text-zinc-100 hover:text-cyan-400 transition-colors">
              {agent.name}
            </span>
            <SourceBadge source={agent.source} />
            {agent.background && (
              <span className="inline-flex items-center rounded border border-zinc-700 bg-zinc-800 px-1.5 py-0.5 text-xs text-zinc-500">
                bg
              </span>
            )}
          </div>
          <p className="mt-1 text-xs text-zinc-500 line-clamp-2">{agent.description}</p>
        </div>
        <div className="flex gap-1.5 shrink-0">
          <button
            className="rounded px-2 py-1 text-xs text-zinc-500 border border-zinc-700 hover:text-zinc-200 hover:border-zinc-500"
            onClick={onClone}
            title="Clone"
          >
            Clone
          </button>
          {agent.source === 'user' && (
            <button
              className="rounded px-2 py-1 text-xs text-red-500 border border-red-800/50 hover:text-red-300 hover:border-red-700"
              onClick={onDelete}
              title="Delete"
            >
              Delete
            </button>
          )}
        </div>
      </div>
      <div className="mt-2 flex gap-2 text-xs text-zinc-600">
        {agent.model && <span>model: {agent.model}</span>}
        <span>type: {agent.subagent_type}</span>
      </div>
    </div>
  )
}

// ---------------------------------------------------------------------------
// Agent Builder Form
// ---------------------------------------------------------------------------

interface FormData {
  name: string
  description: string
  system_prompt: string
  model: string
  effort: string
  max_turns: string
  tools: string
  disallowed_tools: string
  toolkits: string
  skills: string
  background: boolean
  initial_prompt: string
  subagent_type: string
  tags: string
}

const EMPTY_FORM: FormData = {
  name: '',
  description: '',
  system_prompt: '',
  model: '',
  effort: '',
  max_turns: '',
  tools: '',
  disallowed_tools: '',
  toolkits: '',
  skills: '',
  background: false,
  initial_prompt: '',
  subagent_type: '',
  tags: '',
}

function agentToForm(agent: AgentDetail): FormData {
  return {
    name: agent.name,
    description: agent.description,
    system_prompt: agent.system_prompt ?? '',
    model: agent.model ?? '',
    effort: agent.effort ?? '',
    max_turns: agent.max_turns?.toString() ?? '',
    tools: agent.tools?.join(', ') ?? '',
    disallowed_tools: agent.disallowed_tools?.join(', ') ?? '',
    toolkits: agent.toolkits?.join(', ') ?? '',
    skills: agent.skills?.join(', ') ?? '',
    background: agent.background,
    initial_prompt: agent.initial_prompt ?? '',
    subagent_type: agent.subagent_type ?? '',
    tags: agent.tags?.join(', ') ?? '',
  }
}

function formToPayload(form: FormData): Record<string, unknown> {
  const splitList = (s: string): string[] | null => {
    const items = s.split(',').map(t => t.trim()).filter(Boolean)
    return items.length > 0 ? items : null
  }
  return {
    name: form.name,
    description: form.description,
    system_prompt: form.system_prompt || null,
    model: form.model || null,
    effort: form.effort || null,
    max_turns: form.max_turns ? parseInt(form.max_turns, 10) : null,
    tools: splitList(form.tools),
    disallowed_tools: splitList(form.disallowed_tools),
    toolkits: splitList(form.toolkits),
    skills: splitList(form.skills) ?? [],
    background: form.background,
    initial_prompt: form.initial_prompt || null,
    subagent_type: form.subagent_type || form.name,
    tags: splitList(form.tags),
  }
}

function SelectField({
  label,
  value,
  onChange,
  options,
  placeholder = 'None',
}: {
  label: string
  value: string
  onChange: (v: string) => void
  options: string[]
  placeholder?: string
}) {
  return (
    <div>
      <label className="block text-xs font-medium text-zinc-400 uppercase tracking-wide mb-1">{label}</label>
      <select
        value={value}
        onChange={e => onChange(e.target.value)}
        className="w-full rounded border border-zinc-700 bg-zinc-800 px-2.5 py-1.5 text-sm text-zinc-100 focus:border-cyan-600 focus:outline-none"
      >
        <option value="">{placeholder}</option>
        {options.map(o => <option key={o} value={o}>{o}</option>)}
      </select>
    </div>
  )
}

function TextField({
  label,
  value,
  onChange,
  placeholder = '',
  multiline = false,
  mono = false,
}: {
  label: string
  value: string
  onChange: (v: string) => void
  placeholder?: string
  multiline?: boolean
  mono?: boolean
}) {
  const cls = `w-full rounded border border-zinc-700 bg-zinc-800 px-2.5 py-1.5 text-sm text-zinc-100 placeholder:text-zinc-600 focus:border-cyan-600 focus:outline-none ${mono ? 'font-mono' : ''}`
  return (
    <div>
      <label className="block text-xs font-medium text-zinc-400 uppercase tracking-wide mb-1">{label}</label>
      {multiline ? (
        <textarea
          value={value}
          onChange={e => onChange(e.target.value)}
          placeholder={placeholder}
          rows={5}
          className={cls + ' resize-y'}
        />
      ) : (
        <input
          type="text"
          value={value}
          onChange={e => onChange(e.target.value)}
          placeholder={placeholder}
          className={cls}
        />
      )}
    </div>
  )
}

function AgentBuilderForm({
  initial,
  editing,
  availableTools,
  availableToolkits,
  onSave,
  onCancel,
}: {
  initial: FormData
  editing: boolean
  availableTools: AvailableTool[]
  availableToolkits: string[]
  onSave: (payload: Record<string, unknown>) => Promise<void>
  onCancel: () => void
}) {
  const [form, setForm] = useState<FormData>(initial)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [validation, setValidation] = useState<ValidationResult | null>(null)

  const set = <K extends keyof FormData>(key: K, value: FormData[K]) =>
    setForm(prev => ({ ...prev, [key]: value }))

  const handleValidate = async () => {
    try {
      const result = await validateAgent(formToPayload(form))
      setValidation(result)
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    }
  }

  const handleSubmit = async () => {
    setSaving(true)
    setError(null)
    try {
      await onSave(formToPayload(form))
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setSaving(false)
    }
  }

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <h2 className="text-lg font-semibold text-zinc-100">
          {editing ? `Edit: ${initial.name}` : 'New Agent'}
        </h2>
        <button
          className="text-xs text-zinc-500 hover:text-zinc-300"
          onClick={onCancel}
        >
          Cancel
        </button>
      </div>

      {error && (
        <div className="rounded-lg border border-red-800 bg-red-950 px-4 py-3 text-sm text-red-300">
          {error}
        </div>
      )}

      {validation && (
        <div className={`rounded-lg border px-4 py-3 text-sm ${
          validation.valid
            ? 'border-emerald-800 bg-emerald-950 text-emerald-300'
            : 'border-red-800 bg-red-950 text-red-300'
        }`}>
          <p className="font-medium">{validation.valid ? 'Valid' : 'Invalid'}</p>
          {validation.errors.map((e, i) => <p key={i} className="text-xs mt-1">Error: {e}</p>)}
          {validation.warnings.map((w, i) => <p key={i} className="text-xs mt-1 text-yellow-400">Warning: {w}</p>)}
        </div>
      )}

      {/* Basic Info */}
      <section className="space-y-3">
        <h3 className="text-xs font-semibold text-zinc-500 uppercase tracking-wide">Basic Info</h3>
        <div className="grid grid-cols-1 sm:grid-cols-2 gap-3">
          <TextField label="Name" value={form.name} onChange={v => set('name', v)} placeholder="my-agent" />
          <TextField label="Subagent Type" value={form.subagent_type} onChange={v => set('subagent_type', v)} placeholder="Defaults to name" />
        </div>
        <TextField label="Description" value={form.description} onChange={v => set('description', v)} placeholder="When to use this agent..." multiline />
      </section>

      {/* System Prompt */}
      <section className="space-y-3">
        <h3 className="text-xs font-semibold text-zinc-500 uppercase tracking-wide">System Prompt</h3>
        <TextField label="System Prompt" value={form.system_prompt} onChange={v => set('system_prompt', v)} placeholder="You are a..." multiline mono />
        <TextField label="Initial Prompt" value={form.initial_prompt} onChange={v => set('initial_prompt', v)} placeholder="Prepended to first user turn" />
      </section>

      {/* Model & Behavior */}
      <section className="space-y-3">
        <h3 className="text-xs font-semibold text-zinc-500 uppercase tracking-wide">Model & Behavior</h3>
        <div className="grid grid-cols-2 sm:grid-cols-4 gap-3">
          <TextField label="Model" value={form.model} onChange={v => set('model', v)} placeholder="inherit" />
          <SelectField label="Effort" value={form.effort} onChange={v => set('effort', v)} options={EFFORT_LEVELS} />
          <TextField label="Max Turns" value={form.max_turns} onChange={v => set('max_turns', v)} placeholder="e.g. 20" />
        </div>
      </section>

      {/* Tools & Skills */}
      <section className="space-y-3">
        <h3 className="text-xs font-semibold text-zinc-500 uppercase tracking-wide">Tools & Skills</h3>
        <TextField label="Allowed Tools" value={form.tools} onChange={v => set('tools', v)} placeholder="Read, Write, Bash (comma-separated, empty = all)" />
        {availableTools.length > 0 && (
          <div className="flex flex-wrap gap-1">
            {availableTools.map(t => (
              <button
                key={t.name}
                className="rounded border border-zinc-700 bg-zinc-800 px-1.5 py-0.5 text-xs text-zinc-500 hover:text-zinc-300 hover:border-zinc-500"
                title={t.description}
                onClick={() => {
                  const current = form.tools ? form.tools.split(',').map(s => s.trim()).filter(Boolean) : []
                  if (!current.includes(t.name)) {
                    set('tools', [...current, t.name].join(', '))
                  }
                }}
              >
                + {t.name}
              </button>
            ))}
          </div>
        )}
        <TextField label="Disallowed Tools" value={form.disallowed_tools} onChange={v => set('disallowed_tools', v)} placeholder="agent, file_edit (comma-separated)" />
        <TextField label="Toolkits" value={form.toolkits} onChange={v => set('toolkits', v)} placeholder="daytona, mcp (comma-separated)" />
        {availableToolkits.length > 0 && (
          <div className="flex flex-wrap gap-1">
            {availableToolkits.map(tk => (
              <button
                key={tk}
                className="rounded border border-zinc-700 bg-zinc-800 px-1.5 py-0.5 text-xs text-zinc-500 hover:text-zinc-300 hover:border-zinc-500"
                onClick={() => {
                  const current = form.toolkits ? form.toolkits.split(',').map(s => s.trim()).filter(Boolean) : []
                  if (!current.includes(tk)) {
                    set('toolkits', [...current, tk].join(', '))
                  }
                }}
              >
                + {tk}
              </button>
            ))}
          </div>
        )}
        <TextField label="Skills" value={form.skills} onChange={v => set('skills', v)} placeholder="skill-slug-1, skill-slug-2" />
      </section>

      {/* UI & Lifecycle */}
      <section className="space-y-3">
        <h3 className="text-xs font-semibold text-zinc-500 uppercase tracking-wide">UI & Lifecycle</h3>
        <div className="grid grid-cols-2 sm:grid-cols-4 gap-3">
          <div>
            <label className="block text-xs font-medium text-zinc-400 uppercase tracking-wide mb-1">Background</label>
            <button
              className={`rounded border px-3 py-1.5 text-sm font-medium transition ${
                form.background
                  ? 'border-emerald-700 bg-emerald-950 text-emerald-400'
                  : 'border-zinc-700 bg-zinc-800 text-zinc-500'
              }`}
              onClick={() => set('background', !form.background)}
            >
              {form.background ? 'Yes' : 'No'}
            </button>
          </div>
        </div>
        <TextField label="Tags" value={form.tags} onChange={v => set('tags', v)} placeholder="coding, research (comma-separated)" />
      </section>

      {/* Actions */}
      <div className="flex gap-3 pt-2 border-t border-zinc-800">
        <button
          className="rounded px-4 py-2 text-sm font-medium bg-cyan-900 text-cyan-300 border border-cyan-700 hover:bg-cyan-800 disabled:opacity-50"
          disabled={saving || !form.name.trim() || !form.description.trim()}
          onClick={handleSubmit}
        >
          {saving ? 'Saving...' : editing ? 'Update Agent' : 'Create Agent'}
        </button>
        <button
          className="rounded px-4 py-2 text-sm font-medium text-zinc-400 border border-zinc-700 hover:text-zinc-200 hover:border-zinc-500"
          onClick={handleValidate}
        >
          Validate
        </button>
        <button
          className="rounded px-4 py-2 text-sm font-medium text-zinc-500 hover:text-zinc-300"
          onClick={onCancel}
        >
          Cancel
        </button>
      </div>
    </div>
  )
}

// ---------------------------------------------------------------------------
// Agent Detail View
// ---------------------------------------------------------------------------

function AgentDetailView({
  agent,
  onEdit,
  onBack,
}: {
  agent: AgentDetail
  onEdit: () => void
  onBack: () => void
}) {
  const isUser = agent.source === 'user'

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-3">
          <button className="text-xs text-zinc-500 hover:text-zinc-300" onClick={onBack}>&larr; Back</button>
          <h2 className="text-lg font-semibold text-zinc-100">{agent.name}</h2>
          <SourceBadge source={agent.source ?? 'builtin'} />
          {agent.version && (
            <span className="text-xs text-zinc-600">v{agent.version}</span>
          )}
        </div>
        {isUser && (
          <button
            className="rounded px-3 py-1.5 text-xs font-medium bg-cyan-950 text-cyan-400 border border-cyan-800 hover:bg-cyan-900"
            onClick={onEdit}
          >
            Edit
          </button>
        )}
      </div>

      <p className="text-sm text-zinc-400">{agent.description}</p>

      <div className="grid grid-cols-2 sm:grid-cols-4 gap-3">
        {agent.model && <DetailField label="Model" value={agent.model} />}
        {agent.effort && <DetailField label="Effort" value={agent.effort} />}
        {agent.max_turns && <DetailField label="Max Turns" value={String(agent.max_turns)} />}
        <DetailField label="Background" value={agent.background ? 'Yes' : 'No'} />
        <DetailField label="Type" value={agent.subagent_type} />
      </div>

      {agent.tools && (
        <TagSection label="Allowed Tools" items={agent.tools} />
      )}
      {agent.disallowed_tools && (
        <TagSection label="Disallowed Tools" items={agent.disallowed_tools} accent="text-red-400 border-red-800" />
      )}
      {agent.toolkits && (
        <TagSection label="Toolkits" items={agent.toolkits} accent="text-purple-400 border-purple-800" />
      )}
      {agent.skills && agent.skills.length > 0 && (
        <TagSection label="Skills" items={agent.skills} accent="text-emerald-400 border-emerald-800" />
      )}
      {agent.tags && agent.tags.length > 0 && (
        <TagSection label="Tags" items={agent.tags} accent="text-yellow-400 border-yellow-800" />
      )}

      {agent.system_prompt && (
        <section>
          <h3 className="text-xs font-semibold text-zinc-500 uppercase tracking-wide mb-2">System Prompt</h3>
          <pre className="max-h-64 overflow-auto rounded-lg border border-zinc-800 bg-zinc-950 p-3 text-xs text-zinc-300 font-mono whitespace-pre-wrap">
            {agent.system_prompt}
          </pre>
        </section>
      )}

      {agent.created_at && (
        <div className="text-xs text-zinc-600 pt-2 border-t border-zinc-800">
          Created: {new Date(agent.created_at).toLocaleString()}
          {agent.updated_at && <> | Updated: {new Date(agent.updated_at).toLocaleString()}</>}
        </div>
      )}
    </div>
  )
}

function DetailField({ label, value }: { label: string; value: string }) {
  return (
    <div className="rounded-lg bg-zinc-900 border border-zinc-800 px-3 py-2">
      <span className="block text-xs text-zinc-500 uppercase tracking-wide">{label}</span>
      <span className="text-sm text-zinc-200">{value}</span>
    </div>
  )
}

function TagSection({ label, items, accent }: { label: string; items: string[]; accent?: string }) {
  return (
    <section>
      <h3 className="text-xs font-semibold text-zinc-500 uppercase tracking-wide mb-2">{label}</h3>
      <div className="flex flex-wrap gap-1">
        {items.map(item => (
          <span
            key={item}
            className={`inline-flex items-center rounded border bg-zinc-800 px-2 py-0.5 text-xs font-mono ${accent ?? 'text-zinc-400 border-zinc-700'}`}
          >
            {item}
          </span>
        ))}
      </div>
    </section>
  )
}

// ---------------------------------------------------------------------------
// Page
// ---------------------------------------------------------------------------

type View = 'list' | 'detail' | 'create' | 'edit'

export default function AgentsPage() {
  const [view, setView] = useState<View>('list')
  const [agents, setAgents] = useState<AgentSummary[]>([])
  const [selectedAgent, setSelectedAgent] = useState<AgentDetail | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [filter, setFilter] = useState<'all' | 'builtin' | 'user'>('all')
  const [availableTools, setAvailableTools] = useState<AvailableTool[]>([])
  const [availableToolkits, setAvailableToolkits] = useState<string[]>([])

  const refresh = useCallback(async () => {
    try {
      const data = await fetchAgents()
      setAgents(data)
      setError(null)
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => { refresh() }, [refresh])

  const loadMeta = useCallback(async () => {
    const [tools, toolkits] = await Promise.all([fetchAvailableTools(), fetchAvailableToolkits()])
    setAvailableTools(tools)
    setAvailableToolkits(toolkits)
  }, [])

  const selectAgent = async (name: string) => {
    try {
      const detail = await fetchAgent(name)
      setSelectedAgent(detail)
      setView('detail')
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    }
  }

  const handleClone = async (name: string) => {
    const newName = prompt(`Clone "${name}" as:`)
    if (!newName?.trim()) return
    try {
      await cloneAgent(name, newName.trim())
      await refresh()
    } catch (e) {
      alert(`Clone failed: ${e instanceof Error ? e.message : e}`)
    }
  }

  const handleDelete = async (name: string) => {
    if (!confirm(`Delete agent "${name}"?`)) return
    try {
      await deleteAgent(name)
      await refresh()
      if (selectedAgent?.name === name) {
        setSelectedAgent(null)
        setView('list')
      }
    } catch (e) {
      alert(`Delete failed: ${e instanceof Error ? e.message : e}`)
    }
  }

  const filtered = filter === 'all' ? agents : agents.filter(a => a.source === filter)

  return (
    <div className="min-h-full bg-zinc-950 text-zinc-100 p-6 space-y-6">
      {/* List View */}
      {view === 'list' && (
        <>
          <div className="flex items-center justify-between">
            <h1 className="text-lg font-semibold">Agents</h1>
            <div className="flex gap-2">
              <div className="flex rounded border border-zinc-700 overflow-hidden">
                {(['all', 'builtin', 'user'] as const).map(f => (
                  <button
                    key={f}
                    className={`px-3 py-1 text-xs font-medium transition ${
                      filter === f ? 'bg-zinc-700 text-zinc-100' : 'text-zinc-500 hover:text-zinc-300'
                    }`}
                    onClick={() => setFilter(f)}
                  >
                    {f}
                  </button>
                ))}
              </div>
              <button
                className="rounded px-3 py-1.5 text-xs font-medium text-zinc-400 border border-zinc-700 hover:text-zinc-200 hover:border-zinc-500"
                onClick={refresh}
              >
                Refresh
              </button>
              <button
                className="rounded px-3 py-1.5 text-xs font-medium bg-cyan-900 text-cyan-300 border border-cyan-700 hover:bg-cyan-800"
                onClick={() => { loadMeta(); setView('create') }}
              >
                + New Agent
              </button>
            </div>
          </div>

          {error && (
            <div className="rounded-lg border border-red-800 bg-red-950 px-4 py-3 text-sm text-red-300">
              {error}
            </div>
          )}

          {loading ? (
            <div className="text-center text-sm text-zinc-500 py-8">Loading...</div>
          ) : filtered.length === 0 ? (
            <div className="rounded-lg border border-zinc-800 px-4 py-8 text-center text-sm text-zinc-600">
              No agents found
            </div>
          ) : (
            <div className="space-y-2">
              {filtered.map(agent => (
                <AgentCard
                  key={agent.name}
                  agent={agent}
                  onSelect={() => selectAgent(agent.name)}
                  onClone={() => handleClone(agent.name)}
                  onDelete={() => handleDelete(agent.name)}
                />
              ))}
            </div>
          )}
        </>
      )}

      {/* Detail View */}
      {view === 'detail' && selectedAgent && (
        <AgentDetailView
          agent={selectedAgent}
          onEdit={() => { loadMeta(); setView('edit') }}
          onBack={() => setView('list')}
        />
      )}

      {/* Create Form */}
      {view === 'create' && (
        <AgentBuilderForm
          initial={EMPTY_FORM}
          editing={false}
          availableTools={availableTools}
          availableToolkits={availableToolkits}
          onSave={async (payload) => {
            await createAgent(payload)
            await refresh()
            setView('list')
          }}
          onCancel={() => setView('list')}
        />
      )}

      {/* Edit Form */}
      {view === 'edit' && selectedAgent && (
        <AgentBuilderForm
          initial={agentToForm(selectedAgent)}
          editing={true}
          availableTools={availableTools}
          availableToolkits={availableToolkits}
          onSave={async (payload) => {
            const updated = await updateAgent(selectedAgent.name, payload)
            setSelectedAgent(updated)
            setView('detail')
            await refresh()
          }}
          onCancel={() => setView('detail')}
        />
      )}
    </div>
  )
}
