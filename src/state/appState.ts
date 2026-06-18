import type {
  ModelDefaultReasoning,
  ModelReasoningMode,
  ProviderConfig,
  ProviderModel,
} from '../types/provider'
import { codeBuddyOpenAiBaseUrl, minimaxOpenAiBaseUrl } from '../types/provider'
import type { ProjectSession } from '../types/project'
import type { AppSettings, UiPreferences } from '../types/settings'
import type { AgentTask } from '../types/task'

export type View = 'projects' | 'workspace' | 'profile' | 'settings'
export type WorkspaceHistoryState = Pick<
  AppState,
  'activeProjectId' | 'currentWorkspaceTaskId' | 'tasksById' | 'taskIdsByProjectId'
>

export interface AppState {
  projects: ProjectSession[]
  activeProjectId: string | null
  settings: AppSettings | null
  providers: ProviderConfig[]
  currentWorkspaceTaskId: string | null
  traceDrawerOpen: boolean
  tasksById: Record<string, AgentTask>
  taskIdsByProjectId: Record<string, string[]>
}

export const defaultUiPreferences: UiPreferences = {
  showTraceButton: true,
  autoOpenTraceOnErrors: true,
  defaultWorkspaceLayout: 'chat-only',
  visualStyle: 'codex',
  workspaceHistoryDays: 7,
}

export const defaultProviders: ProviderConfig[] = [
  {
    id: 'codex-cli',
    type: 'codex-cli',
    name: 'Codex CLI',
    enabled: true,
    baseUrl: '',
    baseUrlLocked: true,
    defaultCredentialId: '',
    defaultModel: 'default',
    temperature: 0.2,
    credentials: [],
    models: [],
  },
  {
    id: 'openai-compatible',
    type: 'openai-compatible',
    name: 'OpenAI-Compatible',
    enabled: false,
    baseUrl: '',
    baseUrlLocked: false,
    defaultCredentialId: '',
    defaultModel: 'gpt-4.1',
    temperature: 0.2,
    credentials: [],
    models: [],
  },
  {
    id: 'codebuddy',
    type: 'codebuddy',
    name: 'CodeBuddy VSCode',
    enabled: false,
    baseUrl: codeBuddyOpenAiBaseUrl,
    baseUrlLocked: true,
    defaultCredentialId: '',
    defaultModel: 'glm-5.1',
    temperature: 1,
    credentials: [],
    models: [
      { id: 'glm-5.1', name: 'GLM-5.1', enabled: false },
      { id: 'glm-5.0-turbo', name: 'GLM-5.0-Turbo', enabled: false },
      { id: 'glm-5v-turbo', name: 'GLM-5v-Turbo', enabled: false },
      { id: 'kimi-k2.6', name: 'Kimi-K2.6', enabled: false },
      { id: 'hy3-preview', name: 'Hy3 preview', enabled: false },
      { id: 'deepseek-v4-pro', name: 'Deepseek-V4-Pro', enabled: false },
      { id: 'deepseek-v4-flash', name: 'Deepseek-V4-Flash', enabled: false },
      { id: 'deepseek-v3-2-volc', name: 'DeepSeek V3.2', enabled: false },
    ],
  },
  {
    id: 'claude',
    type: 'claude',
    name: 'Claude',
    enabled: false,
    baseUrl: '',
    baseUrlLocked: false,
    defaultCredentialId: '',
    defaultModel: 'Claude 4.1 Sonnet',
    temperature: 0.2,
    credentials: [],
    models: [],
  },
  {
    id: 'deepseek',
    type: 'deepseek',
    name: 'DeepSeek',
    enabled: false,
    baseUrl: '',
    baseUrlLocked: false,
    defaultCredentialId: '',
    defaultModel: 'deepseek-chat',
    temperature: 0.2,
    credentials: [],
    models: [],
  },
  {
    id: 'minimax',
    type: 'minimax',
    name: 'MiniMax',
    enabled: false,
    baseUrl: minimaxOpenAiBaseUrl,
    baseUrlLocked: true,
    defaultCredentialId: '',
    defaultModel: 'MiniMax-M2.7',
    temperature: 0.2,
    credentials: [],
    models: [],
  },
  {
    id: 'ollama',
    type: 'ollama',
    name: 'Ollama',
    enabled: false,
    baseUrl: 'http://127.0.0.1:11434',
    baseUrlLocked: false,
    defaultCredentialId: '',
    defaultModel: 'llama3.1',
    temperature: 0.2,
    credentials: [],
    models: [],
  },
  {
    id: 'local-gateway',
    type: 'local-gateway',
    name: 'Local Gateway',
    enabled: false,
    baseUrl: '',
    baseUrlLocked: false,
    defaultCredentialId: '',
    defaultModel: 'local-default',
    temperature: 0.2,
    credentials: [],
    models: [],
  },
]

export const initialAppState: AppState = {
  projects: [],
  activeProjectId: null,
  settings: null,
  providers: defaultProviders,
  currentWorkspaceTaskId: null,
  traceDrawerOpen: false,
  tasksById: {},
  taskIdsByProjectId: {},
}

const workspaceHistoryStorageKey = 'snowagent.workspaceHistory.v1'

export function normalizeSettings(settings: AppSettings): AppSettings {
  const providers =
    settings.providers && settings.providers.length > 0
      ? settings.configPath.toLowerCase().endsWith('.toml')
        ? normalizeConfiguredProviders(settings.providers)
        : mergeProviders(settings.providers)
      : defaultProviders

  return {
    ...settings,
    uiPreferences: {
      ...defaultUiPreferences,
      ...(settings.uiPreferences ?? {}),
    },
    providers,
  }
}

export function latestTaskIdForProject(
  state: AppState,
  projectId: string | null,
): string | null {
  if (!projectId) {
    return null
  }
  const taskIds = state.taskIdsByProjectId[projectId] ?? []
  return taskIds.at(-1) ?? null
}

export function ensureWorkspaceProject(state: AppState): AppState {
  if (state.activeProjectId) {
    return state
  }
  if (state.projects.length !== 1) {
    return state
  }
  const projectId = state.projects[0].id
  return {
    ...state,
    activeProjectId: projectId,
    currentWorkspaceTaskId: latestTaskIdForProject(state, projectId),
  }
}

export function loadLegacyWorkspaceHistoryState(): WorkspaceHistoryState {
  if (typeof window === 'undefined') {
    return emptyWorkspaceHistoryState()
  }
  try {
    const raw = window.localStorage.getItem(workspaceHistoryStorageKey)
    if (!raw) {
      return emptyWorkspaceHistoryState()
    }
    const parsed: unknown = JSON.parse(raw)
    return normalizeWorkspaceHistoryState(parsed)
  } catch {
    return emptyWorkspaceHistoryState()
  }
}

export function clearLegacyWorkspaceHistoryState(): void {
  if (typeof window === 'undefined') {
    return
  }
  window.localStorage.removeItem(workspaceHistoryStorageKey)
}

export function normalizeWorkspaceHistoryState(value: unknown): WorkspaceHistoryState {
  if (!isRecord(value)) {
    return emptyWorkspaceHistoryState()
  }
  const tasksById = readTasksById(value.tasksById)
  return {
    activeProjectId: readNullableString(value.activeProjectId),
    currentWorkspaceTaskId: readNullableString(value.currentWorkspaceTaskId),
    tasksById,
    taskIdsByProjectId: readTaskIdsByProjectId(value.taskIdsByProjectId, tasksById),
  }
}

export function createWorkspaceHistorySnapshot(state: AppState): WorkspaceHistoryState {
  const tasksById = Object.fromEntries(
    Object.entries(state.tasksById).map(([taskId, task]) => [
      taskId,
      {
        ...task,
        traceEvents: [],
        messages: task.messages.map((message) => ({
          ...message,
          traceEvents: undefined,
        })),
      },
    ]),
  )
  return {
    activeProjectId: state.activeProjectId,
    currentWorkspaceTaskId: state.currentWorkspaceTaskId,
    tasksById,
    taskIdsByProjectId: state.taskIdsByProjectId,
  }
}

export function hasWorkspaceHistory(state: WorkspaceHistoryState): boolean {
  return (
    Boolean(state.activeProjectId) ||
    Boolean(state.currentWorkspaceTaskId) ||
    Object.keys(state.tasksById).length > 0 ||
    Object.keys(state.taskIdsByProjectId).length > 0
  )
}

function mergeProviders(providers: ProviderConfig[]): ProviderConfig[] {
  const providedById = new Map(providers.map((provider) => [provider.id, provider]))
  const mergedDefaults = defaultProviders.map((provider) => {
    const storedProvider = providedById.get(provider.id)
    return {
      ...provider,
      ...(storedProvider ?? {}),
      baseUrl:
        provider.id === 'minimax'
          ? minimaxOpenAiBaseUrl
          : provider.id === 'codebuddy'
            ? codeBuddyOpenAiBaseUrl
          : storedProvider?.baseUrl ?? provider.baseUrl,
      baseUrlLocked:
        provider.id === 'minimax' || provider.id === 'codebuddy' || provider.id === 'codex-cli'
          ? true
          : storedProvider?.baseUrlLocked ?? provider.baseUrlLocked,
      credentials: normalizeProviderCredentials(storedProvider ?? provider),
      models: normalizeProviderModels(
        storedProvider?.models?.length ? storedProvider.models : provider.models,
      ),
    }
  })
  const customProviders = providers.filter(
    (provider) => !defaultProviders.some((defaultProvider) => defaultProvider.id === provider.id),
  )
  return [...mergedDefaults, ...customProviders]
}

function normalizeProviderCredentials(provider: ProviderConfig): ProviderConfig['credentials'] {
  if (provider.type === 'codex-cli') {
    return []
  }
  if (provider.credentials?.length > 0) {
    return provider.credentials.map((credential, index) => ({
      ...credential,
      id: credential.id?.trim() || `key-${index + 1}`,
      name: credential.name?.trim() || `key-${index + 1}`,
      apiKey: credential.apiKey ?? '',
      enabled: Boolean(credential.enabled),
    }))
  }
  if (provider.apiKey?.trim()) {
    return [
      {
        id: 'default',
        name: 'key-1',
        enabled: true,
        apiKey: provider.apiKey,
      },
    ]
  }
  return []
}

function normalizeConfiguredProviders(providers: ProviderConfig[]): ProviderConfig[] {
  const normalized = providers
    .map((provider) => ({
      ...provider,
      id: provider.id.trim(),
      name: provider.name.trim() || provider.id.trim(),
      type: provider.type || 'openai-compatible',
      baseUrl: provider.baseUrl ?? '',
      baseUrlLocked: false,
      defaultModel:
        provider.defaultModel?.trim() ||
        provider.models.find((model) => model.enabled)?.id ||
        provider.models[0]?.id ||
        '',
      envKey: provider.envKey?.trim() ?? '',
      wireApi: provider.wireApi || 'responses',
      requiresOpenAiAuth: Boolean(provider.requiresOpenAiAuth),
      credentials: normalizeProviderCredentials(provider),
      models: normalizeProviderModels(provider.models ?? []),
    }))
    .filter((provider) => provider.id.length > 0 && provider.name.length > 0)

  if (!normalized.some((provider) => provider.isDefault) && normalized.length > 0) {
    return normalized.map((provider, index) => ({ ...provider, isDefault: index === 0 }))
  }
  return normalized
}

function normalizeProviderModels(models: ProviderModel[]): ProviderModel[] {
  return models.map((model) => {
    const id = model.id.trim()
    const name = (model.name ?? '').trim() || id
    const reasoningMode = normalizeReasoningMode(model.reasoningMode, id, name)
    return {
      ...model,
      id,
      name,
      reasoningMode,
      defaultReasoning: normalizeDefaultReasoning(reasoningMode, model.defaultReasoning),
    }
  })
}

function normalizeReasoningMode(
  value: string | undefined,
  modelId: string,
  modelName: string,
): ModelReasoningMode {
  const inferred = inferReasoningMode(modelId, modelName)
  if (inferred !== 'none' && (!value || value === 'none')) {
    return inferred
  }
  if (value === 'toggle' || value === 'effort' || value === 'none') {
    return value
  }
  return inferred
}

function normalizeDefaultReasoning(
  mode: ModelReasoningMode,
  value: string | undefined,
): ModelDefaultReasoning {
  if (mode === 'toggle') {
    return value === 'on' ? 'on' : 'off'
  }
  if (mode === 'effort') {
    return value === 'minimal' ||
      value === 'low' ||
      value === 'medium' ||
      value === 'high'
      ? value
      : 'medium'
  }
  return 'off'
}

function inferReasoningMode(modelId: string, modelName: string): ModelReasoningMode {
  const normalized = `${modelId} ${modelName}`.toLowerCase()
  return normalized.includes('minimax-m3') ? 'toggle' : 'none'
}

export function emptyWorkspaceHistoryState(): WorkspaceHistoryState {
  return {
    activeProjectId: null,
    currentWorkspaceTaskId: null,
    tasksById: {},
    taskIdsByProjectId: {},
  }
}

function readNullableString(value: unknown): string | null {
  return typeof value === 'string' && value.length > 0 ? value : null
}

function readTasksById(value: unknown): Record<string, AgentTask> {
  if (!isRecord(value)) {
    return {}
  }
  return Object.fromEntries(
    Object.entries(value)
      .filter((entry): entry is [string, AgentTask] => isAgentTask(entry[1]))
      .map(([taskId, task]) => [taskId, restorePersistedTask(task)]),
  )
}

function restorePersistedTask(task: AgentTask): AgentTask {
  if (task.status !== 'running' && task.status !== 'failed') {
    return task
  }

  const messages = task.messages.map((message) =>
    message.status === 'running' ? { ...message, status: 'failed' as const } : message,
  )
  const messagesChanged = messages.some((message, index) => message !== task.messages[index])
  if (task.status === 'failed' && !messagesChanged) {
    return task
  }

  return {
    ...task,
    status: 'failed',
    messages: messagesChanged ? messages : task.messages,
  }
}

function readTaskIdsByProjectId(
  value: unknown,
  tasksById: Record<string, AgentTask>,
): Record<string, string[]> {
  if (!isRecord(value)) {
    return {}
  }
  return Object.fromEntries(
    Object.entries(value)
      .filter((entry): entry is [string, unknown[]] => Array.isArray(entry[1]))
      .map(([projectId, taskIds]) => [
        projectId,
        taskIds.filter((taskId): taskId is string =>
          typeof taskId === 'string' && tasksById[taskId] !== undefined,
        ),
      ]),
  )
}

function isAgentTask(value: unknown): value is AgentTask {
  if (!isRecord(value)) {
    return false
  }
  return (
    typeof value.id === 'string' &&
    typeof value.projectId === 'string' &&
    typeof value.prompt === 'string' &&
    Array.isArray(value.messages) &&
    Array.isArray(value.traceEvents) &&
    (value.status === 'running' ||
      value.status === 'completed' ||
      value.status === 'failed')
  )
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
}
