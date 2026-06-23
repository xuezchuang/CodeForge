import type { ToolTraceEvent } from './trace'

export type ChatRole = 'user' | 'assistant' | 'system'
export type AgentTaskStatus = 'running' | 'completed' | 'failed'

export interface CodeLinkRef {
  rawLink: string
}

export interface MessageAttachment {
  id: string
  kind: 'image'
  name: string
  mimeType: string
  dataUrl: string
}

export interface ChatMessage {
  id: string
  taskId: string
  role: ChatRole
  content: string
  status?: AgentTaskStatus
  codeLinks?: CodeLinkRef[]
  attachments?: MessageAttachment[]
  traceEvents?: ToolTraceEvent[]
  createdAt: string
}

export interface AgentTask {
  id: string
  projectId: string
  prompt: string
  messages: ChatMessage[]
  traceEvents: ToolTraceEvent[]
  status: AgentTaskStatus
  messagesLoaded?: boolean
  createdAt?: string
  updatedAt?: string
}
