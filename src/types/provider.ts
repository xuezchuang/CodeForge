export type ProviderType =
  | 'codex-cli'
  | 'openai-compatible'
  | 'codebuddy'
  | 'claude'
  | 'deepseek'
  | 'minimax'
  | 'ollama'
  | 'local-gateway'

export type ModelReasoningMode = 'none' | 'toggle' | 'effort'
export type ModelDefaultReasoning = 'off' | 'on' | 'minimal' | 'low' | 'medium' | 'high'

export interface ProviderConfig {
  id: string
  type: ProviderType
  name: string
  enabled: boolean
  isDefault?: boolean
  baseUrl: string
  baseUrlLocked: boolean
  supportsToolCall?: boolean | null
  apiKey?: string
  defaultCredentialId: string
  defaultModel: string
  temperature: number
  envKey?: string
  wireApi?: 'responses' | string
  requiresOpenAiAuth?: boolean
  credentials: ProviderCredential[]
  models: ProviderModel[]
}

export interface ProviderCredential {
  id: string
  name: string
  enabled: boolean
  apiKey: string
}

export interface ProviderModel {
  id: string
  name: string
  enabled: boolean
  credentialId?: string
  reasoningMode?: ModelReasoningMode
  defaultReasoning?: ModelDefaultReasoning
  ownedBy?: string | null
  created?: number | null
}

export const providerTypeLabels: Record<ProviderType, string> = {
  'codex-cli': 'Codex CLI',
  'openai-compatible': 'OpenAI-Compatible',
  codebuddy: 'CodeBuddy',
  claude: 'Claude',
  deepseek: 'DeepSeek',
  minimax: 'MiniMax',
  ollama: 'Ollama',
  'local-gateway': 'Local Gateway',
}

export const minimaxOpenAiBaseUrl = 'https://api.minimaxi.com/v1'
export const codeBuddyOpenAiBaseUrl = 'https://copilot.tencent.com/v2'
