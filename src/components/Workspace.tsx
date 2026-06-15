import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import type { Dispatch, SetStateAction } from 'react'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import {
  listTools,
  listTraces,
  openVisualStudio,
  runAgent,
  type ToolDefinitionSummary,
} from '../api/tauriApi'
import type { AppState } from '../state/appState'
import type { AgentTask, ChatMessage, MessageAttachment } from '../types/task'
import type { AgentConversationMessage, ToolTraceEvent } from '../types/trace'
import ChatTimeline from './ChatTimeline'
import Composer from './Composer'
import Toast, { type ToastState } from './Toast'
import TraceDrawer from './TraceDrawer'
import WorkspaceHeader from './WorkspaceHeader'
import { extractCodeLinksFromText } from './codeLinkText'
import { aggregateTokenUsage, sanitizeModelMessage } from './traceViewModel'

interface WorkspaceProps {
  state: AppState
  setState: Dispatch<SetStateAction<AppState>>
  onRefreshProjects: () => Promise<void>
  onGlobalNotice: (message: string) => void
  onGlobalError: (message: string) => void
}

interface SelectedTrace {
  taskId: string
  events: ToolTraceEvent[]
}

const traceEventName = 'agent_trace_event'

function Workspace({
  state,
  setState,
  onRefreshProjects,
  onGlobalNotice,
  onGlobalError,
}: WorkspaceProps) {
  const workspaceRef = useRef<HTMLElement>(null)
  const [busy, setBusy] = useState(false)
  const [composerDraft, setComposerDraft] = useState('')
  const [workspaceToast, setWorkspaceToast] = useState<ToastState | null>(null)
  const [headerDivided, setHeaderDivided] = useState(false)
  const [selectedTrace, setSelectedTrace] = useState<SelectedTrace | null>(null)
  const activeProject = useMemo(
    () =>
      state.projects.find((project) => project.id === state.activeProjectId) ??
      null,
    [state.activeProjectId, state.projects],
  )
  const currentTask =
    state.currentWorkspaceTaskId ?
      state.tasksById[state.currentWorkspaceTaskId] ?? null
    : null
  const selectedTraceEvents = selectedTrace?.events ?? []

  const updateHeaderDivider = useCallback(() => {
    const workspace = workspaceRef.current
    if (!workspace) {
      setHeaderDivided(false)
      return
    }
    const header = workspace.querySelector<HTMLElement>('.workspace-header')
    const actions = workspace.querySelector<HTMLElement>('.workspace-topbar-actions')
    const identity = workspace.querySelector<HTMLElement>('.workspace-identity')
    if (!header || (!actions && !identity)) {
      setHeaderDivided(false)
      return
    }

    const protectedRects = [identity, actions]
      .filter((element): element is HTMLElement => element !== null)
      .map((element) => element.getBoundingClientRect())
    const contentRects = Array.from(
      workspace.querySelectorAll<HTMLElement>(
        '.message-body, .chat-empty-content',
      ),
    ).map((element) => element.getBoundingClientRect())
    const nextDivided = contentRects.some((contentRect) =>
      protectedRects.some((protectedRect) => rectsIntersect(contentRect, protectedRect)),
    )
    setHeaderDivided((current) => (current === nextDivided ? current : nextDivided))
  }, [])

  useEffect(() => {
    const workspace = workspaceRef.current
    if (!workspace) {
      return undefined
    }

    let animationFrame = window.requestAnimationFrame(updateHeaderDivider)
    const scheduleUpdate = () => {
      window.cancelAnimationFrame(animationFrame)
      animationFrame = window.requestAnimationFrame(updateHeaderDivider)
    }

    const scrollTargets = Array.from(
      workspace.querySelectorAll<HTMLElement>('.chat-timeline'),
    )
    scrollTargets.forEach((target) => {
      target.addEventListener('scroll', scheduleUpdate, { passive: true })
    })
    window.addEventListener('resize', scheduleUpdate)

    const resizeObserver = new ResizeObserver(scheduleUpdate)
    resizeObserver.observe(workspace)

    return () => {
      window.cancelAnimationFrame(animationFrame)
      scrollTargets.forEach((target) => {
        target.removeEventListener('scroll', scheduleUpdate)
      })
      window.removeEventListener('resize', scheduleUpdate)
      resizeObserver.disconnect()
    }
  }, [currentTask?.id, updateHeaderDivider])

  useEffect(() => {
    const task = currentTask
    let active = true
    window.queueMicrotask(() => {
      if (!active) {
        return
      }
      setSelectedTrace((current) => {
        if (!task) {
          return null
        }
        if (!current) {
          const firstTrace = task.traceEvents[0]
          return firstTrace ? { taskId: firstTrace.taskId, events: task.traceEvents } : null
        }
        return taskHasTraceSelection(task, current.taskId) ? current : null
      })
    })
    return () => {
      active = false
    }
  }, [currentTask])

  const showWorkspaceToast = (kind: ToastState['kind'], message: string) => {
    const id = Date.now()
    setWorkspaceToast({ id, kind, message })
    window.setTimeout(() => {
      setWorkspaceToast((current) => (current?.id === id ? null : current))
    }, 3000)
  }

  const runTask = async (
    prompt: string,
    selection: {
      providerId: string | null
      credentialId: string | null
      modelId: string | null
      reasoningEffort: string | null
    },
    attachments: MessageAttachment[] = [],
  ) => {
    if (!activeProject) {
      return
    }
    if (isListToolsCommand(prompt)) {
      await showToolsCommand(activeProject.id, prompt)
      return
    }
    if (isStatusCommand(prompt)) {
      await showStatusCommand(activeProject.id, prompt)
      return
    }

    const sessionTaskId = currentTask?.id ?? crypto.randomUUID()
    const userMessage = createMessage(sessionTaskId, 'user', prompt, attachments)
    const pendingAssistantMessage = createPendingAssistantMessage(sessionTaskId)
    const conversationMessages = createConversationMessages([
      ...(currentTask?.messages ?? []),
      userMessage,
    ])
    const pendingTask: AgentTask =
      currentTask ?
        {
          ...currentTask,
          messages: [...currentTask.messages, userMessage, pendingAssistantMessage],
          traceEvents: [],
          status: 'running',
        }
      : {
          id: sessionTaskId,
          projectId: activeProject.id,
          prompt,
          messages: [userMessage, pendingAssistantMessage],
          traceEvents: [],
          status: 'running',
        }
    let unlisten: UnlistenFn | null = null

    setBusy(true)
    setSelectedTrace(null)
    setState((current) => addOrReplaceSessionTask(current, activeProject.id, pendingTask))

    try {
      unlisten = await listen<ToolTraceEvent>(traceEventName, (event) => {
        const traceEvent = event.payload
        if (!isToolTraceEvent(traceEvent)) {
          return
        }
        setSelectedTrace((current) => ({
          taskId: traceEvent.taskId,
          events:
            current?.taskId === traceEvent.taskId ?
              appendTraceEvent(current.events, traceEvent)
            : [traceEvent],
        }))
        setState((current) =>
          appendTraceEventToSession(
            current,
            sessionTaskId,
            traceEvent,
            pendingAssistantMessage.id,
          ),
        )
      })

      const run = await runAgent({
        projectId: activeProject.id,
        userPrompt: prompt,
        messages: conversationMessages,
        providerId: selection.providerId,
        credentialId: selection.credentialId,
        modelId: selection.modelId,
        reasoningEffort: selection.reasoningEffort,
      })
      const assistantMessage = createAssistantMessage(
        run.taskId,
        run.traces,
        pendingAssistantMessage.id,
      )
      setSelectedTrace({ taskId: run.taskId, events: run.traces })

      setState((current) =>
        completeSessionRun(
          current,
          sessionTaskId,
          assistantMessage,
          run.traces,
          pendingAssistantMessage.id,
        ),
      )
    } catch (caught) {
      const message = caught instanceof Error ? caught.message : String(caught)
      setState((current) =>
        failSessionRun(
          current,
          activeProject.id,
          sessionTaskId,
          pendingAssistantMessage.id,
          message,
        ),
      )
      showWorkspaceToast('error', message)
    } finally {
      unlisten?.()
      setBusy(false)
    }
  }

  const showToolsCommand = async (projectId: string, prompt: string) => {
    const sessionTaskId = currentTask?.id ?? crypto.randomUUID()
    const userMessage = createMessage(sessionTaskId, 'user', prompt)

    setBusy(true)
    try {
      const tools = await listTools()
      const assistantMessage = createMessage(
        sessionTaskId,
        'assistant',
        formatToolsListMessage(tools),
      )
      setSelectedTrace(null)
      setState((current) => ({
        ...appendMessagesToSession(
          current,
          projectId,
          sessionTaskId,
          [userMessage, assistantMessage],
          'completed',
        ),
        traceDrawerOpen: false,
      }))
    } catch (caught) {
      const message = caught instanceof Error ? caught.message : String(caught)
      const errorMessage = createMessage(sessionTaskId, 'system', message)
      setState((current) =>
        appendMessagesToSession(
          current,
          projectId,
          sessionTaskId,
          [userMessage, errorMessage],
          'failed',
        ),
      )
      showWorkspaceToast('error', message)
    } finally {
      setBusy(false)
    }
  }

  const showStatusCommand = async (projectId: string, prompt: string) => {
    const sessionTaskId = currentTask?.id ?? crypto.randomUUID()
    const userMessage = createMessage(sessionTaskId, 'user', prompt)

    setBusy(true)
    try {
      const traceEvents = currentTask?.traceEvents ?? []
      const aggregated = aggregateTokenUsage(traceEvents)
      const assistantMessage = createMessage(
        sessionTaskId,
        'assistant',
        formatStatusCommandMessage(aggregated),
      )
      setSelectedTrace(null)
      setState((current) => ({
        ...appendMessagesToSession(
          current,
          projectId,
          sessionTaskId,
          [userMessage, assistantMessage],
          'completed',
        ),
        traceDrawerOpen: false,
      }))
    } catch (caught) {
      const message = caught instanceof Error ? caught.message : String(caught)
      const errorMessage = createMessage(sessionTaskId, 'system', message)
      setState((current) =>
        appendMessagesToSession(
          current,
          projectId,
          sessionTaskId,
          [userMessage, errorMessage],
          'failed',
        ),
      )
      showWorkspaceToast('error', message)
    } finally {
      setBusy(false)
    }
  }

  const refreshTrace = async (taskId: string) => {
    try {
      const traces = await listTraces(taskId)
      setSelectedTrace((current) =>
        current?.taskId === taskId ? { taskId, events: traces } : current,
      )
      setState((current) => updateTraceEventsForMessage(current, taskId, traces))
    } catch (caught) {
      showWorkspaceToast(
        'error',
        caught instanceof Error ? caught.message : String(caught),
      )
    }
  }

  const openMessageTrace = (message: ChatMessage) => {
    const currentTraceEvents = currentTask?.traceEvents ?? []
    const events =
      message.traceEvents ??
      (currentTraceEvents.some((event) => event.taskId === message.taskId) ?
        currentTraceEvents
      : [])

    setSelectedTrace({ taskId: message.taskId, events })
    setState((current) => ({
      ...current,
      traceDrawerOpen: true,
    }))
    if (events.length === 0) {
      void refreshTrace(message.taskId)
    }
  }

  const launchVs = async () => {
    if (!activeProject) {
      return
    }
    try {
      setBusy(true)
      if (activeProject.vsBridgeEndpoint) {
        showWorkspaceToast('notice', 'VS already connected; bring to front is TODO.')
      }
      const result = await openVisualStudio(activeProject.id)
      onGlobalNotice(`Visual Studio started, PID ${result.processId}`)
      await onRefreshProjects()
    } catch (caught) {
      onGlobalError(caught instanceof Error ? caught.message : String(caught))
    } finally {
      setBusy(false)
    }
  }

  const refreshBridge = () => {
    void onRefreshProjects()
      .then(() => showWorkspaceToast('notice', 'Bridge status refreshed.'))
      .catch((caught) =>
        showWorkspaceToast(
          'error',
          caught instanceof Error ? caught.message : String(caught),
        ),
      )
  }

  const clearWorkspace = () => {
    if (!activeProject) {
      return
    }
    setState((current) => {
      const taskIds = current.taskIdsByProjectId[activeProject.id] ?? []
      const tasksById = { ...current.tasksById }
      taskIds.forEach((taskId) => {
        delete tasksById[taskId]
      })
      return {
        ...current,
        currentWorkspaceTaskId: null,
        traceDrawerOpen: false,
        tasksById,
        taskIdsByProjectId: {
          ...current.taskIdsByProjectId,
          [activeProject.id]: [],
        },
      }
    })
    setComposerDraft('')
    showWorkspaceToast('notice', 'Workspace cleared.')
  }

  if (!activeProject) {
    return (
      <section className="page-section">
        <div className="empty-state workspace-empty">
          {state.projects.length === 0 ?
            'Add a project first.'
          : 'Please choose a project from Projects.'}
        </div>
      </section>
    )
  }

  return (
    <section className="workspace-page" ref={workspaceRef}>
      <Toast toast={workspaceToast} onDismiss={() => setWorkspaceToast(null)} />
      <WorkspaceHeader
        project={activeProject}
        busy={busy}
        divided={headerDivided}
        onOpenVisualStudio={launchVs}
        onRefreshBridge={refreshBridge}
        onClearWorkspace={clearWorkspace}
        onNotice={(message) => showWorkspaceToast('notice', message)}
      />

      <div className="workspace-body">
        <main className="chat-shell">
          <div className="chat-main">
            <ChatTimeline
              task={currentTask}
              projectId={activeProject.id}
              onCodeLinkResult={(message) => showWorkspaceToast('notice', message)}
              onCodeLinkError={(message) =>
                showWorkspaceToast('error', normalizeCodeLinkError(message))
              }
              onTraceChanged={(taskId) => {
                void refreshTrace(taskId)
              }}
              onOpenTrace={openMessageTrace}
              onEditUserMessage={(message) => setComposerDraft(message.content)}
              onSuggestionSelect={setComposerDraft}
            />
            <Composer
              providers={state.providers}
              busy={busy}
              value={composerDraft}
              onChange={setComposerDraft}
              onSend={runTask}
            />
          </div>
        </main>
      </div>
      <TraceDrawer
        open={state.traceDrawerOpen}
        taskId={selectedTrace?.taskId ?? null}
        traceEvents={selectedTraceEvents}
        onClose={() =>
          setState((current) => ({
            ...current,
            traceDrawerOpen: false,
          }))
        }
      />
    </section>
  )
}

function addOrReplaceSessionTask(
  state: AppState,
  projectId: string,
  task: AgentTask,
): AppState {
  const existingTaskIds = state.taskIdsByProjectId[projectId] ?? []
  const taskIds =
    existingTaskIds.includes(task.id) ?
      existingTaskIds
    : [...existingTaskIds, task.id]

  return {
    ...state,
    currentWorkspaceTaskId: task.id,
    tasksById: {
      ...state.tasksById,
      [task.id]: task,
    },
    taskIdsByProjectId: {
      ...state.taskIdsByProjectId,
      [projectId]: taskIds,
    },
  }
}

function appendMessagesToSession(
  state: AppState,
  projectId: string,
  sessionTaskId: string,
  messages: ChatMessage[],
  status: AgentTask['status'],
): AppState {
  const existingTask = state.tasksById[sessionTaskId]
  const task: AgentTask =
    existingTask ??
    {
      id: sessionTaskId,
      projectId,
      prompt: messages[0]?.content ?? 'Untitled task',
      messages: [],
      traceEvents: [],
      status,
    }

  return addOrReplaceSessionTask(state, projectId, {
    ...task,
    messages: [...task.messages, ...messages],
    status,
  })
}

function completeSessionRun(
  state: AppState,
  sessionTaskId: string,
  assistantMessage: ChatMessage,
  traces: ToolTraceEvent[],
  pendingAssistantMessageId: string,
): AppState {
  const task = state.tasksById[sessionTaskId]
  if (!task) {
    return state
  }
  const failed = hasFailedTrace(traces)
  return {
    ...state,
    currentWorkspaceTaskId: sessionTaskId,
    traceDrawerOpen: state.traceDrawerOpen,
    tasksById: {
      ...state.tasksById,
      [sessionTaskId]: {
        ...task,
        messages: replaceMessageById(task.messages, pendingAssistantMessageId, assistantMessage),
        traceEvents: traces,
        status: failed ? 'failed' : 'completed',
      },
    },
  }
}

function updateTraceEventsForMessage(
  state: AppState,
  taskId: string,
  traces: ToolTraceEvent[],
): AppState {
  let changed = false
  const failed = hasFailedTrace(traces)
  const tasksById: Record<string, AgentTask> = Object.fromEntries(
    Object.entries(state.tasksById).map(([sessionTaskId, task]) => {
      let taskChanged = false
      const messages = task.messages.map((message) => {
        if (message.taskId !== taskId) {
          return message
        }
        taskChanged = true
        return {
          ...message,
          traceEvents: traces,
          status: failed ? 'failed' : message.status,
        }
      })
      const taskTraceMatches = task.traceEvents.some((event) => event.taskId === taskId)
      if (!taskChanged && !taskTraceMatches) {
        return [sessionTaskId, task]
      }
      changed = true
      return [
        sessionTaskId,
        {
          ...task,
          messages: taskChanged ? messages : task.messages,
          traceEvents: taskTraceMatches || taskChanged ? traces : task.traceEvents,
          status: failed ? 'failed' : task.status,
        },
      ]
    }),
  )

  return changed ? { ...state, tasksById } : state
}

function appendTraceEventToSession(
  state: AppState,
  sessionTaskId: string,
  traceEvent: ToolTraceEvent,
  pendingAssistantMessageId?: string,
): AppState {
  const task = state.tasksById[sessionTaskId]
  if (!task) {
    return state
  }

  const nextTaskTraceEvents = appendTraceEvent(task.traceEvents, traceEvent)

  return {
    ...state,
    tasksById: {
      ...state.tasksById,
      [sessionTaskId]: {
        ...task,
        traceEvents: nextTaskTraceEvents,
        messages:
          pendingAssistantMessageId ?
            task.messages.map((message) => {
              if (message.id !== pendingAssistantMessageId) {
                return message
              }
              const nextMessageTraceEvents = appendTraceEvent(
                message.traceEvents ?? [],
                traceEvent,
              )
              return {
                ...message,
                taskId: traceEvent.taskId,
                content: createRunningAssistantContent(nextMessageTraceEvents),
                traceEvents: nextMessageTraceEvents,
                status: 'running',
              }
            })
          : task.messages,
      },
    },
  }
}

function failSessionRun(
  state: AppState,
  projectId: string,
  sessionTaskId: string,
  pendingAssistantMessageId: string,
  error: string,
): AppState {
  const task = state.tasksById[sessionTaskId]
  const failedMessage: ChatMessage = {
    ...createMessage(sessionTaskId, 'assistant', `Run failed: ${error}`),
    id: pendingAssistantMessageId,
    status: 'failed',
    traceEvents: task?.traceEvents ?? [],
  }

  if (!task) {
    return appendMessagesToSession(state, projectId, sessionTaskId, [failedMessage], 'failed')
  }

  return {
    ...state,
    currentWorkspaceTaskId: sessionTaskId,
    tasksById: {
      ...state.tasksById,
      [sessionTaskId]: {
        ...task,
        messages: replaceMessageById(task.messages, pendingAssistantMessageId, failedMessage),
        status: 'failed',
      },
    },
  }
}

function taskHasTraceSelection(task: AgentTask, taskId: string): boolean {
  return (
    task.messages.some((message) => message.taskId === taskId) ||
    task.traceEvents.some((event) => event.taskId === taskId)
  )
}

function appendTraceEvent(
  events: ToolTraceEvent[],
  traceEvent: ToolTraceEvent,
): ToolTraceEvent[] {
  if (events.some((event) => event.id === traceEvent.id)) {
    return events
  }
  return [...events, traceEvent]
}

function createConversationMessages(messages: ChatMessage[]): AgentConversationMessage[] {
  return messages
    .filter(
      (message): message is ChatMessage & { role: 'user' | 'assistant' } =>
        (message.role === 'user' || message.role === 'assistant') &&
        !isTransientConversationMessage(message),
    )
    .map((message) => {
      const content = sanitizeConversationContent(message.content)
      return {
        role: message.role,
        content,
        attachments: message.attachments?.map(({ kind, name, mimeType, dataUrl }) => ({
          kind,
          name,
          mimeType,
          dataUrl,
        })),
      }
    })
    .filter(
      (message) =>
        message.content.trim().length > 0 ||
        Boolean(message.attachments && message.attachments.length > 0),
    )
}

function isTransientConversationMessage(message: ChatMessage): boolean {
  if (isSyntheticContinuationReminder(message.content)) {
    return true
  }
  if (message.role !== 'assistant') {
    return false
  }
  return (
    message.status === 'running' ||
    message.status === 'failed' ||
    message.content.startsWith('Thinking...\n\n') ||
    message.content.startsWith('Run failed:')
  )
}

function sanitizeConversationContent(content: string): string {
  const trimmed = content.trim()
  return isSyntheticContinuationReminder(trimmed) ? '' : content
}

function isSyntheticContinuationReminder(content: string): boolean {
  const trimmed = content.trim()
  return (
    trimmed.startsWith('[System reminder:') &&
    trimmed.includes('Output token limit hit') &&
    trimmed.includes('Resume directly')
  )
}

function createMessage(
  taskId: string,
  role: ChatMessage['role'],
  content: string,
  attachments?: MessageAttachment[],
): ChatMessage {
  return {
    id: crypto.randomUUID(),
    taskId,
    role,
    content,
    ...(attachments && attachments.length > 0 ? { attachments } : {}),
    createdAt: new Date().toISOString(),
  }
}

function createPendingAssistantMessage(taskId: string): ChatMessage {
  return {
    ...createMessage(taskId, 'assistant', createRunningAssistantContent([])),
    status: 'running',
    traceEvents: [],
  }
}

function createRunningAssistantContent(traces: ToolTraceEvent[]): string {
  const latestTrace = traces.at(-1)
  if (!latestTrace) {
    return 'Thinking...\n\nWaiting for the first trace event.'
  }
  return `Thinking...\n\n${describeRunningTrace(latestTrace)}`
}

function describeRunningTrace(event: ToolTraceEvent): string {
  const detail = runningTraceDetail(event)

  if (event.status === 'failed' || event.type === 'error') {
    return appendRunningDetail('Step failed', detail)
  }
  if (event.type === 'llm_request') {
    return appendRunningDetail('Sending model request', detail)
  }
  if (event.type === 'llm_response') {
    return appendRunningDetail(
      'Received model response; updating the thinking trace',
      detail,
    )
  }
  if (event.type === 'tool_call') {
    return appendRunningDetail(
      `Running ${runningToolLabel(event.toolName)}`,
      detail,
    )
  }
  if (event.type === 'tool_result') {
    if (event.toolName === 'chat_completion') {
      return appendRunningDetail(
        'Received model response; updating the thinking trace',
        detail,
      )
    }
    return appendRunningDetail(
      `Completed ${runningToolLabel(event.toolName)}`,
      detail,
    )
  }
  if (event.type === 'final_response') {
    return appendRunningDetail('Composing final response', detail)
  }
  if (event.type === 'model_message') {
    return appendRunningDetail('Reading model message', detail)
  }
  return appendRunningDetail(event.outputSummary ?? event.title ?? 'Working', detail)
}

function runningToolLabel(toolName: string | null): string {
  if (toolName === 'search_content') {
    return 'content search'
  }
  if (toolName === 'search_file') {
    return 'file search'
  }
  if (toolName === 'read_file') {
    return 'file read'
  }
  if (toolName === 'list_dir') {
    return 'directory listing'
  }
  if (toolName === 'get_file_context') {
    return 'context read'
  }
  return toolName ?? 'tool step'
}

function runningTraceDetail(event: ToolTraceEvent): string {
  const input = plainRecord(event.input)
  const output = plainRecord(event.output)
  const request = plainRecord(input.request)
  const response = plainRecord(output.response)
  const argumentsValue = plainRecord(input.arguments)

  return firstRunningText([
    argumentsValue.query,
    argumentsValue.pattern,
    argumentsValue.path,
    input.model,
    request.model,
    output.model,
    response.model,
    event.outputSummary,
    event.title,
  ])
}

function appendRunningDetail(text: string, detail: string): string {
  return detail ? `${text}: ${detail}` : text
}

function firstRunningText(values: unknown[]): string {
  for (const value of values) {
    const text = typeof value === 'string' ? compactRunningDetail(value) : ''
    if (text) {
      return text
    }
  }
  return ''
}

function compactRunningDetail(value: string): string {
  const normalized = value.replace(/\s+/g, ' ').trim()
  return normalized.length > 96 ? `${normalized.slice(0, 93)}...` : normalized
}

function plainRecord(value: unknown): Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value) ?
      (value as Record<string, unknown>)
    : {}
}

function createAssistantMessage(
  taskId: string,
  traces: ToolTraceEvent[],
  messageId?: string,
): ChatMessage {
  const failed = hasFailedTrace(traces)
  const summary =
    traces.find((event) => event.type === 'final_response')?.outputSummary ??
    traces.find((event) => event.type === 'model_message')?.outputSummary ??
    traces.find((event) => event.status === 'failed')?.outputSummary ??
    traces.find((event) => event.status === 'warning')?.outputSummary ??
    `Agent produced ${traces.length} trace events.`
  const content = sanitizeModelMessage(summary)
  const links = extractCodeLinksFromText(content).map((rawLink) => ({ rawLink }))

  return {
    ...createMessage(taskId, 'assistant', content),
    ...(messageId ? { id: messageId } : {}),
    codeLinks: links,
    traceEvents: traces,
    status: failed ? 'failed' : 'completed',
  }
}

function replaceMessageById(
  messages: ChatMessage[],
  messageId: string,
  replacement: ChatMessage,
): ChatMessage[] {
  let replaced = false
  const nextMessages = messages.map((message) => {
    if (message.id !== messageId) {
      return message
    }
    replaced = true
    return {
      ...replacement,
      createdAt: message.createdAt,
    }
  })
  return replaced ? nextMessages : [...messages, replacement]
}

function isListToolsCommand(prompt: string): boolean {
  const command = prompt.trim().toLowerCase()
  return command === '/skill' || command === '/skills'
}

function isStatusCommand(prompt: string): boolean {
  const command = prompt.trim().toLowerCase()
  return command === '/status' || command === '/usage'
}

function formatStatusCommandMessage(usage: {
  inputTokens: number
  outputTokens: number
  totalTokens: number
  inputCachedTokens: number
  inputUncachedTokens: number
  eventCount: number
  hasAny: boolean
}): string {
  if (!usage.hasAny) {
    return 'No token usage reported for the current task yet. Token usage is captured per LLM response in the trace drawer.'
  }
  const formatNumber = (value: number) => value.toLocaleString('en-US')
  const cacheLine = usage.inputTokens
    ? `, cached ${formatNumber(usage.inputCachedTokens)} (uncached ${formatNumber(usage.inputUncachedTokens)})`
    : ''
  return [
    `Token usage (current task, ${formatNumber(usage.eventCount)} LLM step${usage.eventCount === 1 ? '' : 's'}):`,
    '',
    `- Input: ${formatNumber(usage.inputTokens)}${cacheLine}`,
    `- Output: ${formatNumber(usage.outputTokens)}`,
    `- Total: ${formatNumber(usage.totalTokens)}`,
  ].join('\n')
}

function formatToolsListMessage(tools: ToolDefinitionSummary[]): string {
  if (tools.length === 0) {
    return 'No tools are currently registered.'
  }
  const lines = tools.map((tool) => {
    const description = tool.description.trim()
    return description ? `- \`${tool.name}\` - ${description}` : `- \`${tool.name}\``
  })
  return [`Registered tools (${tools.length}):`, '', ...lines].join('\n')
}

function hasFailedTrace(traces: ToolTraceEvent[]): boolean {
  return traces.some((event) => event.status === 'failed')
}

function isToolTraceEvent(value: unknown): value is ToolTraceEvent {
  return (
    value !== null &&
    typeof value === 'object' &&
    'id' in value &&
    'taskId' in value &&
    'stepIndex' in value &&
    'type' in value &&
    'status' in value
  )
}

function normalizeCodeLinkError(message: string): string {
  if (message.includes('Bridge not connected')) {
    return 'VS Bridge is not connected.'
  }
  if (message.startsWith('File does not exist:')) {
    return 'File does not exist.'
  }
  return message
}

export default Workspace

function rectsIntersect(left: DOMRect, right: DOMRect): boolean {
  return (
    left.left < right.right &&
    left.right > right.left &&
    left.top < right.bottom &&
    left.bottom > right.top
  )
}
