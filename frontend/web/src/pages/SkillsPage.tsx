import { useCallback, useEffect, useState } from 'react'

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

interface SkillSummary {
  name: string
  description: string
  source: string
  path: string | null
}

interface SkillDetail extends SkillSummary {
  content: string
}

// ---------------------------------------------------------------------------
// API
// ---------------------------------------------------------------------------

const API = '/api/skills'

async function fetchSkills(): Promise<SkillSummary[]> {
  const res = await fetch(API)
  if (!res.ok) throw new Error(`${res.status}: ${await res.text()}`)
  return res.json()
}

async function fetchSkill(name: string): Promise<SkillDetail> {
  const res = await fetch(`${API}/${encodeURIComponent(name)}`)
  if (!res.ok) throw new Error(`${res.status}: ${await res.text()}`)
  return res.json()
}

// ---------------------------------------------------------------------------
// Sub-components
// ---------------------------------------------------------------------------

function SourceBadge({ source }: { source: string }) {
  const color =
    source === 'bundled'
      ? 'bg-indigo-900/30 text-indigo-400 border-indigo-800/50'
      : 'bg-emerald-900/30 text-emerald-400 border-emerald-800/50'
  return (
    <span className={`inline-flex items-center rounded-full border px-2 py-0.5 text-xs font-medium ${color}`}>
      {source}
    </span>
  )
}

function SkillCard({
  skill,
  onSelect,
}: {
  skill: SkillSummary
  onSelect: () => void
}) {
  return (
    <div
      className="rounded-lg bg-zinc-900 border border-zinc-800 px-4 py-3 hover:border-zinc-600 transition-colors cursor-pointer"
      onClick={onSelect}
    >
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <span className="text-sm font-medium text-zinc-100 hover:text-emerald-400 transition-colors font-mono">
              {skill.name}
            </span>
            <SourceBadge source={skill.source} />
          </div>
          <p className="mt-1 text-xs text-zinc-500 line-clamp-2">{skill.description}</p>
        </div>
      </div>
      {skill.path && (
        <div className="mt-2 text-xs text-zinc-600 font-mono truncate" title={skill.path}>
          {skill.path}
        </div>
      )}
    </div>
  )
}

// ---------------------------------------------------------------------------
// Skill Detail View
// ---------------------------------------------------------------------------

function SkillDetailView({
  skill,
  onBack,
}: {
  skill: SkillDetail
  onBack: () => void
}) {
  return (
    <div className="space-y-6">
      <div className="flex items-center gap-3">
        <button className="text-xs text-zinc-500 hover:text-zinc-300" onClick={onBack}>
          &larr; Back
        </button>
        <h2 className="text-lg font-semibold text-zinc-100 font-mono">{skill.name}</h2>
        <SourceBadge source={skill.source} />
      </div>

      <p className="text-sm text-zinc-400">{skill.description}</p>

      {skill.path && (
        <div>
          <h3 className="text-xs font-semibold text-zinc-500 uppercase tracking-wide mb-1">Path</h3>
          <code className="text-xs text-zinc-500 font-mono">{skill.path}</code>
        </div>
      )}

      <section>
        <h3 className="text-xs font-semibold text-zinc-500 uppercase tracking-wide mb-2">Content</h3>
        <pre className="max-h-[60vh] overflow-auto rounded-lg border border-zinc-800 bg-zinc-950 p-4 text-xs text-zinc-300 font-mono whitespace-pre-wrap leading-relaxed">
          {skill.content}
        </pre>
      </section>
    </div>
  )
}

// ---------------------------------------------------------------------------
// Page
// ---------------------------------------------------------------------------

export default function SkillsPage() {
  const [skills, setSkills] = useState<SkillSummary[]>([])
  const [selectedSkill, setSelectedSkill] = useState<SkillDetail | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [view, setView] = useState<'list' | 'detail'>('list')
  const [filter, setFilter] = useState<'all' | 'bundled' | 'user'>('all')

  const refresh = useCallback(async () => {
    try {
      const data = await fetchSkills()
      setSkills(data)
      setError(null)
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => {
    refresh()
  }, [refresh])

  const selectSkill = async (name: string) => {
    try {
      const detail = await fetchSkill(name)
      setSelectedSkill(detail)
      setView('detail')
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    }
  }

  const filtered = filter === 'all' ? skills : skills.filter(s => s.source === filter)

  return (
    <div className="min-h-full bg-zinc-950 text-zinc-100 p-6 space-y-6">
      {/* List View */}
      {view === 'list' && (
        <>
          <div className="flex items-center justify-between">
            <div>
              <h1 className="text-lg font-semibold">Skills</h1>
              <p className="text-xs text-zinc-500 mt-0.5">
                {loading ? '...' : `${skills.length} skill${skills.length !== 1 ? 's' : ''} loaded`}
              </p>
            </div>
            <div className="flex gap-2">
              <div className="flex rounded border border-zinc-700 overflow-hidden">
                {(['all', 'bundled', 'user'] as const).map(f => (
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
              No skills found
            </div>
          ) : (
            <div className="space-y-2">
              {filtered.map(skill => (
                <SkillCard
                  key={skill.name}
                  skill={skill}
                  onSelect={() => selectSkill(skill.name)}
                />
              ))}
            </div>
          )}
        </>
      )}

      {/* Detail View */}
      {view === 'detail' && selectedSkill && (
        <SkillDetailView
          skill={selectedSkill}
          onBack={() => setView('list')}
        />
      )}
    </div>
  )
}
