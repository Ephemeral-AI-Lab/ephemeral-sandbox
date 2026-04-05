import { Outlet, NavLink } from 'react-router'
import { useAppState, useConnected } from '../../lib/hooks'

export default function Layout() {
  const connected = useConnected()
  const state = useAppState()

  return (
    <div className="flex h-screen flex-col">
      {/* Header */}
      <header className="flex h-12 shrink-0 items-center justify-between border-b border-zinc-800 bg-zinc-900 px-4">
        <div className="flex items-center gap-3">
          <span className="text-sm font-semibold text-zinc-100">EphemeralOS</span>
          <span className={`inline-block h-2 w-2 rounded-full ${connected ? 'bg-emerald-500' : 'bg-red-500'}`} />
        </div>
        <div className="flex items-center gap-4 text-xs text-zinc-400">
          {state && (
            <>
              <span>{state.model}</span>
              <span className="text-zinc-600">|</span>
              <span>{state.cwd}</span>
            </>
          )}
        </div>
      </header>

      <div className="flex flex-1 overflow-hidden">
        {/* Sidebar */}
        <nav className="flex w-48 shrink-0 flex-col gap-1 border-r border-zinc-800 bg-zinc-900/50 p-3">
          <NavLink
            to="/conversation"
            className={({ isActive }) =>
              `rounded px-3 py-1.5 text-sm transition ${isActive ? 'bg-zinc-800 text-zinc-100' : 'text-zinc-400 hover:text-zinc-200'}`
            }
          >
            Conversation
          </NavLink>
          <NavLink
            to="/dashboard"
            className={({ isActive }) =>
              `rounded px-3 py-1.5 text-sm transition ${isActive ? 'bg-zinc-800 text-zinc-100' : 'text-zinc-400 hover:text-zinc-200'}`
            }
          >
            Dashboard
          </NavLink>
          <NavLink
            to="/agents"
            className={({ isActive }) =>
              `rounded px-3 py-1.5 text-sm transition ${isActive ? 'bg-zinc-800 text-zinc-100' : 'text-zinc-400 hover:text-zinc-200'}`
            }
          >
            Agents
          </NavLink>
          <NavLink
            to="/skills"
            className={({ isActive }) =>
              `rounded px-3 py-1.5 text-sm transition ${isActive ? 'bg-zinc-800 text-zinc-100' : 'text-zinc-400 hover:text-zinc-200'}`
            }
          >
            Skills
          </NavLink>
          <NavLink
            to="/sandboxes"
            className={({ isActive }) =>
              `rounded px-3 py-1.5 text-sm transition ${isActive ? 'bg-zinc-800 text-zinc-100' : 'text-zinc-400 hover:text-zinc-200'}`
            }
          >
            Sandboxes
          </NavLink>
          <NavLink
            to="/sessions"
            className={({ isActive }) =>
              `rounded px-3 py-1.5 text-sm transition ${isActive ? 'bg-zinc-800 text-zinc-100' : 'text-zinc-400 hover:text-zinc-200'}`
            }
          >
            Sessions
          </NavLink>
          <NavLink
            to="/settings"
            className={({ isActive }) =>
              `rounded px-3 py-1.5 text-sm transition ${isActive ? 'bg-zinc-800 text-zinc-100' : 'text-zinc-400 hover:text-zinc-200'}`
            }
          >
            Settings
          </NavLink>
        </nav>

        {/* Main content */}
        <main className="flex-1 overflow-auto">
          <Outlet />
        </main>
      </div>
    </div>
  )
}
