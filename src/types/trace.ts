export type TraceEventType =
  | 'user_message'
  | 'llm_request'
  | 'llm_response'
  | 'tool_call'
  | 'tool_result'
  | 'final_response'
  | 'model_message'
  | 'system_event'
  | 'error'

export type TraceStatus = 'running' | 'success' | 'warning' | 'failed'

export interface ToolTraceEvent {
  id: string
  taskId: string
  parentTaskId?: string | null
  agentName?: string | null
  taskName?: string | null
  readOnly?: boolean | null
  subagentDepth?: number | null
  stepIndex: number
  type: TraceEventType
  toolName: string | null
  title: string
  input: unknown | null
  output: unknown | null
  outputSummary: string | null
  startedAt: string
  endedAt: string | null
  durationMs: number | null
  status: TraceStatus
}

export interface MockAgentRun {
  taskId: string
  traces: ToolTraceEvent[]
  subagentRuns?: SubagentTraceRun[]
  contextCompaction?: ContextCompactionResult | null
}

export interface SubagentTraceRun {
  taskId: string
  parentTaskId: string
  agentName: string
  taskName: string
  readOnly: boolean
  subagentDepth: number
  status: string
  summary?: string | null
  traces: ToolTraceEvent[]
}

export interface ContextCompactionResult {
  summary: string
  originalMessageCount: number
  retainedMessageCount: number
  droppedMessageCount: number
  estimatedOriginalTokens: number
  estimatedCompactedTokens: number
}

export interface AgentConversationMessage {
  role: 'user' | 'assistant' | 'system'
  content: string
  attachments?: AgentMessageAttachment[]
}

export interface AgentMessageAttachment {
  kind: 'image'
  name: string
  mimeType: string
  dataUrl: string
}

export interface AgentRunInput {
  projectId: string
  sessionId?: string | null
  taskId?: string | null
  userPrompt: string
  messages?: AgentConversationMessage[]
  providerId: string | null
  credentialId: string | null
  modelId: string | null
  reasoningEffort?: string | null
  parentTaskId?: string | null
  agentName?: string | null
  taskName?: string | null
  readOnly?: boolean
  subagentDepth?: number
}

export interface ToolCallTestInput {
  projectId: string
  providerId: string | null
  credentialId: string | null
  modelId: string | null
}
