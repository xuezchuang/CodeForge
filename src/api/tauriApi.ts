import { invoke } from '@tauri-apps/api/core'
import { open } from '@tauri-apps/plugin-dialog'
import type {
  OpenVisualStudioResult,
  ProjectInput,
  ProjectSession,
} from '../types/project'
import type { AppSettings, SettingsInput } from '../types/settings'
import type { WorkspaceHistoryState } from '../state/appState'
import type {
  AgentRunInput,
  MockAgentRun,
  ToolCallTestInput,
  ToolTraceEvent,
} from '../types/trace'
import type {
  OpenCodeLinkResult,
  VSInstance,
  VSRegisterPayload,
} from '../types/vs'
import type { ProviderModel } from '../types/provider'
import type { AgentTask } from '../types/task'

export interface ToolDefinitionSummary {
  name: string
  description: string
}

export interface CodeforgeSkillSummary {
  name: string
  description: string
  path: string
}

export interface McpToolDefinitionSummary {
  serverId: string
  serverName: string
  name: string
  rawName: string
  description: string
}

export interface McpServerSummary {
  id: string
  name: string
  status: string
  enabled: boolean
  autoConnect: boolean
  required: boolean
  toolCount: number
  error: string | null
  transport: string
  url: string | null
  connectedAt: string | null
}

export interface McpInventorySummary {
  configPath: string | null
  configError: string | null
  servers: McpServerSummary[]
  tools: McpToolDefinitionSummary[]
}

export interface McpServerActionResult {
  serverId: string
  name: string
  status: string
  toolCount: number
  message: string
  error: string | null
}

export interface ToolOutput {
  status: string
  output?: unknown
  error?: string
  elapsedMs: number
  summary?: string
}

declare global {
  interface Window {
    __TAURI_INTERNALS__?: unknown
  }
}

async function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  try {
    assertTauriBackend()
    return await invoke<T>(command, args)
  } catch (caught) {
    throw new Error(toMessage(caught), { cause: caught })
  }
}

export function listProjects(): Promise<ProjectSession[]> {
  return call<ProjectSession[]>('list_projects')
}

export function addProject(projectInput: ProjectInput): Promise<ProjectSession> {
  return call<ProjectSession>('add_project', { projectInput })
}

export function updateProject(
  projectId: string,
  projectInput: ProjectInput,
): Promise<ProjectSession> {
  return call<ProjectSession>('update_project', { projectId, projectInput })
}

export function deleteProject(projectId: string): Promise<void> {
  return call<void>('delete_project', { projectId })
}

export function getProject(projectId: string): Promise<ProjectSession> {
  return call<ProjectSession>('get_project', { projectId })
}

export function openVisualStudio(
  projectId: string,
): Promise<OpenVisualStudioResult> {
  return call<OpenVisualStudioResult>('open_visual_studio', { projectId })
}

export function registerVsInstance(
  payload: VSRegisterPayload,
): Promise<VSInstance> {
  return call<VSInstance>('register_vs_instance', { payload })
}

export function unregisterVsInstance(instanceId: string): Promise<VSInstance> {
  return call<VSInstance>('unregister_vs_instance', { instanceId })
}

export function heartbeatVsInstance(instanceId: string): Promise<VSInstance> {
  return call<VSInstance>('heartbeat_vs_instance', { instanceId })
}

export function listVsInstances(): Promise<VSInstance[]> {
  return call<VSInstance[]>('list_vs_instances')
}

export function listTools(): Promise<ToolDefinitionSummary[]> {
  return call<ToolDefinitionSummary[]>('list_tools')
}

export function listSkills(projectId: string): Promise<CodeforgeSkillSummary[]> {
  return call<CodeforgeSkillSummary[]>('list_skills', { projectId })
}

export function listMcpTools(): Promise<McpToolDefinitionSummary[]> {
  return call<McpToolDefinitionSummary[]>('list_mcp_tools')
}

export function inspectMcpInventory(): Promise<McpInventorySummary> {
  return call<McpInventorySummary>('inspect_mcp_inventory')
}

export function connectMcpServer(serverId: string): Promise<McpServerActionResult> {
  return call<McpServerActionResult>('connect_mcp_server', {
    input: { serverId },
  })
}

export function disconnectMcpServer(serverId: string): Promise<McpServerActionResult> {
  return call<McpServerActionResult>('disconnect_mcp_server', {
    input: { serverId },
  })
}

export function callMcpTool(
  toolName: string,
  args: Record<string, unknown>,
): Promise<ToolOutput> {
  return call<ToolOutput>('call_mcp_tool', {
    input: {
      toolName,
      arguments: args,
    },
  })
}

export function loadWorkspaceHistory(): Promise<WorkspaceHistoryState> {
  return call<WorkspaceHistoryState>('load_workspace_history')
}

export function loadWorkspaceSession(sessionId: string): Promise<AgentTask> {
  return call<AgentTask>('load_workspace_session', { sessionId })
}

export function saveWorkspaceHistory(
  historyState: WorkspaceHistoryState,
): Promise<void> {
  return call<void>('save_workspace_history', { historyState })
}

export function saveWorkspaceSession(task: AgentTask, position: number): Promise<void> {
  return call<void>('save_workspace_session', { task, position })
}

export function saveWorkspaceSelection(
  activeProjectId: string | null,
  currentWorkspaceTaskId: string | null,
): Promise<void> {
  return call<void>('save_workspace_selection', {
    activeProjectId,
    currentWorkspaceTaskId,
  })
}

export function deleteWorkspaceSessions(sessionIds: string[]): Promise<void> {
  return call<void>('delete_workspace_sessions', { sessionIds })
}

export function importWorkspaceHistory(
  historyState: WorkspaceHistoryState,
): Promise<void> {
  return call<void>('import_workspace_history', { historyState })
}

export function runMockAgent(
  projectId: string,
  userPrompt: string,
): Promise<MockAgentRun> {
  return call<MockAgentRun>('run_mock_agent', { projectId, userPrompt })
}

export function runAgent(input: AgentRunInput): Promise<MockAgentRun> {
  return call<MockAgentRun>('run_agent', { input })
}

export function runToolCallTest(input: ToolCallTestInput): Promise<MockAgentRun> {
  return call<MockAgentRun>('run_tool_call_test', { input })
}

export function listTraces(taskId: string): Promise<ToolTraceEvent[]> {
  return call<ToolTraceEvent[]>('list_traces', { taskId })
}

export function openCodeLink(
  projectId: string,
  rawLink: string,
  taskId: string | null,
  contextLinks?: string[],
): Promise<OpenCodeLinkResult> {
  return call<OpenCodeLinkResult>('open_code_link', {
    projectId,
    rawLink,
    taskId,
    contextLinks,
  })
}

export function getSettings(): Promise<AppSettings> {
  return call<AppSettings>('get_settings')
}

export function updateSettings(settings: SettingsInput): Promise<AppSettings> {
  return call<AppSettings>('update_settings', { settings })
}

export function fetchMiniMaxModels(apiKey: string): Promise<ProviderModel[]> {
  return call<ProviderModel[]>('fetch_minimax_models', { apiKey })
}

export function fetchOpenAiCompatibleModels(
  baseUrl: string,
  apiKey: string,
): Promise<ProviderModel[]> {
  return call<ProviderModel[]>('fetch_openai_compatible_models', { baseUrl, apiKey })
}

export async function browseDirectory(title: string): Promise<string | null> {
  assertTauriBackend()
  const selected = await open({
    title,
    directory: true,
    multiple: false,
  })
  return singlePath(selected)
}

export async function browseSolutionFile(title: string): Promise<string | null> {
  assertTauriBackend()
  const selected = await open({
    title,
    directory: false,
    multiple: false,
    filters: [{ name: 'Visual Studio Solution', extensions: ['sln'] }],
  })
  return singlePath(selected)
}

export async function browseExecutableFile(title: string): Promise<string | null> {
  assertTauriBackend()
  const selected = await open({
    title,
    directory: false,
    multiple: false,
    filters: [{ name: 'Executable', extensions: ['exe'] }],
  })
  return singlePath(selected)
}

function toMessage(caught: unknown): string {
  if (caught instanceof Error) {
    return caught.message
  }
  return String(caught)
}

function assertTauriBackend(): void {
  if (typeof window !== 'undefined' && window.__TAURI_INTERNALS__ === undefined) {
    throw new Error(
      'Tauri backend is unavailable. Use npm.cmd run tauri dev after installing Rust/Cargo; npm.cmd run dev only serves the frontend.',
    )
  }
}

function singlePath(selected: string | string[] | null): string | null {
  if (Array.isArray(selected)) {
    return selected[0] ?? null
  }
  return selected
}
