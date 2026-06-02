import { invoke } from '@tauri-apps/api/core'
import { open } from '@tauri-apps/plugin-dialog'
import type {
  OpenVisualStudioResult,
  ProjectInput,
  ProjectSession,
} from '../types/project'
import type { AppSettings, SettingsInput } from '../types/settings'
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
): Promise<OpenCodeLinkResult> {
  return call<OpenCodeLinkResult>('open_code_link', {
    projectId,
    rawLink,
    taskId,
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
