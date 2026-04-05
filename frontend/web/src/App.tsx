import { Routes, Route, Navigate } from 'react-router'
import { HarnessProvider } from './providers/HarnessProvider'
import Layout from './components/layout/Layout'
import AgentsPage from './pages/AgentsPage'
import DashboardPage from './pages/DashboardPage'
import ConversationPage from './pages/ConversationPage'
import SandboxesPage from './pages/SandboxesPage'
import TaskDetailPage from './pages/TaskDetailPage'
import SettingsPage from './pages/SettingsPage'
import SessionsPage from './pages/SessionsPage'
import AgentRunsPage from './pages/AgentRunsPage'
import SkillsPage from './pages/SkillsPage'

export default function App() {
  return (
    <HarnessProvider>
      <Routes>
        <Route element={<Layout />}>
          <Route index element={<Navigate to="/conversation" replace />} />
          <Route path="conversation" element={<ConversationPage />} />
          <Route path="dashboard" element={<DashboardPage />} />
          <Route path="agents" element={<AgentsPage />} />
          <Route path="skills" element={<SkillsPage />} />
          <Route path="sandboxes" element={<SandboxesPage />} />
          <Route path="sessions" element={<SessionsPage />} />
          <Route path="sessions/:sessionId/runs" element={<AgentRunsPage />} />
          <Route path="settings" element={<SettingsPage />} />
          <Route path="tasks/:taskId" element={<TaskDetailPage />} />
        </Route>
      </Routes>
    </HarnessProvider>
  )
}
