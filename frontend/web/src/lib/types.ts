export type TranscriptItem = {
  role: 'system' | 'user' | 'assistant' | 'tool' | 'tool_result' | 'log' | 'thinking'
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

// -- Session & run persistence types -----------------------------------------

export type SessionSummary = {
  session_id: string
  summary: string
  message_count: number
  model: string
  created_at: number
}

export type SessionDetail = {
  session_id: string
  cwd: string
  model: string
  summary: string | null
  message_count: number
  usage: Record<string, unknown> | null
  session_state: Record<string, unknown> | null
  created_at: string | null
  updated_at: string | null
}

export type ConversationMessagePayload = {
  role: 'user' | 'assistant'
  content: Array<{
    type: string
    text?: string
    name?: string
    input?: Record<string, unknown>
    content?: string
    tool_use_id?: string
  }>
}

export type AgentRunSummary = {
  id: string
  agent_name: string
  status: string
  input_query: string | null
  event_count: number
  error: string | null
  started_at: string | null
  finished_at: string | null
}

export type RunUsageSummary = {
  run_id: string
  model_id: string
  prompt_tokens: number
  completion_tokens: number
  total_tokens: number
}

export type SubagentRunSummary = AgentRunSummary & {
  parent_run_id: string | null
  parent_task_id: string | null
  usage: RunUsageSummary | null
}

export type AgentRunDetail = AgentRunSummary & {
  session_id: string
  response: Record<string, unknown>[] | null
  message_history: Record<string, unknown>[] | null
  compacted_history: Record<string, unknown>[] | null
  reasoning: string | null
  usage: RunUsageSummary | null
  subagent_runs: SubagentRunSummary[]
}

export type SessionUsage = {
  session_id: string
  prompt_tokens: number
  completion_tokens: number
  total_tokens: number
  call_count: number
}

export type ModelUsage = {
  model_id: string
  prompt_tokens: number
  completion_tokens: number
  total_tokens: number
  call_count: number
}

// ---------------------------------------------------------------------------
// Pipeline types
// ---------------------------------------------------------------------------

export type PipelineInputDep = {
  step: string
  keys?: string[] | null
}

export type PipelineStepConfig = {
  name: string
  agent: string
  description?: string
  enabled?: boolean
  timeout?: number | null
  tool_call_limit?: number | null
  output_schema?: Record<string, unknown> | null
  input_deps?: PipelineInputDep[]
  checkpoint?: boolean
  config?: Record<string, unknown>
}

export type PipelineConfig = {
  pipeline_id: string
  name: string
  description?: string
  version?: number
  steps: PipelineStepConfig[]
  default_timeout?: number
  tags?: string[]
  metadata?: Record<string, unknown>
}

export type PipelineStepRecord = {
  name: string
  agent: string
  status: 'pending' | 'running' | 'completed' | 'failed' | 'skipped'
  started_at?: number | null
  finished_at?: number | null
  error?: string | null
  metrics?: Record<string, unknown>
  work_session_id?: string | null
  attempt?: number
}

export type PipelineCheckpointSummary = {
  checkpoint_id: string
  step_name: string
  step_index: number
  completed_steps: string[]
  created_at: number
}

export type PipelineRun = {
  run_id: string
  pipeline_id: string
  goal: string
  status: 'pending' | 'running' | 'completed' | 'failed' | 'cancelled'
  current_step?: string | null
  completed_steps: string[]
  context_map: Record<string, Record<string, unknown>>
  step_records: PipelineStepRecord[]
  error?: string | null
  started_at?: number | null
  finished_at?: number | null
  resumed_from_checkpoint?: string | null
  attempt_number?: number
}

export type FrontendRequest =
  | { type: 'submit_line'; line: string }
  | { type: 'permission_response'; request_id: string; allowed: boolean }
  | { type: 'question_response'; request_id: string; answer: string }
  | { type: 'list_sessions' }
  | { type: 'update_config'; config: ConfigUpdate }
  | { type: 'shutdown' }
