export type TranscriptItem = {
  role: 'system' | 'user' | 'assistant' | 'tool' | 'tool_result' | 'log'
  text: string
  tool_name?: string
  tool_input?: Record<string, unknown>
  is_error?: boolean
}

export type TaskSnapshot = {
  id: string
  type: string
  status: string
  description: string
  metadata: Record<string, string>
}

export type McpServerSnapshot = {
  name: string
  state: string
  detail?: string
  transport?: string
  auth_configured?: boolean
  tool_count?: number
  resource_count?: number
}

export type BridgeSessionSnapshot = {
  session_id: string
  command: string
  cwd: string
  pid: number
  status: string
  started_at: number
  output_path: string
}

export type ToolkitSnapshot = {
  name: string
  description: string
  tools: string[]
}

export type SelectOption = {
  value: string
  label: string
  description?: string
}

export type AppState = {
  model: string
  cwd: string
  provider: string
  auth_status: string
  base_url: string
  permission_mode: string
  theme: string
  vim_enabled: boolean
  voice_enabled: boolean
  voice_available: boolean
  voice_reason: string
  fast_mode: boolean
  effort: string
  passes: number
  mcp_connected: number
  mcp_failed: number
  bridge_sessions: number
  output_style: string
  keybindings: Record<string, string>
}

export type ModalRequest = {
  kind: 'permission' | 'question' | 'mcp_auth'
  request_id: string
  tool_name?: string
  reason?: string
  question?: string
}

export type BackendEvent = {
  type: string
  message?: string | null
  item?: TranscriptItem | null
  state?: AppState | null
  tasks?: TaskSnapshot[] | null
  toolkits?: ToolkitSnapshot[] | null
  mcp_servers?: McpServerSnapshot[] | null
  bridge_sessions?: BridgeSessionSnapshot[] | null
  commands?: string[] | null
  modal?: ModalRequest | null
  select_options?: SelectOption[] | null
  tool_name?: string | null
  tool_input?: Record<string, unknown> | null
  output?: string | null
  is_error?: boolean | null
}

export type ConfigUpdate = {
  model?: string
  base_url?: string
  api_key?: string
  api_format?: string
}

// -- Database persistence types -----------------------------------------------

export type ModelRegistration = {
  id: number
  key: string
  label: string
  class_path: string
  kwargs: Record<string, unknown>
  is_active: boolean
  model_id: string | null
  created_at: string | null
  updated_at: string | null
}

export type DbHealthStatus = {
  database: 'connected' | 'not_configured'
}

export type FrontendRequest =
  | { type: 'submit_line'; line: string }
  | { type: 'permission_response'; request_id: string; allowed: boolean }
  | { type: 'question_response'; request_id: string; answer: string }
  | { type: 'list_sessions' }
  | { type: 'update_config'; config: ConfigUpdate }
  | { type: 'shutdown' }
