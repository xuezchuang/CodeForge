import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import type {
  CSSProperties,
  Dispatch,
  PointerEvent as ReactPointerEvent,
  SetStateAction,
} from 'react'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import {
  listTraces,
  openVisualStudio,
  runAgent,
  runToolCallTest,
} from '../api/tauriApi'
import type { AppState } from '../state/appState'
import type { AgentTask, ChatMessage } from '../types/task'
import type { AgentConversationMessage, ToolTraceEvent } from '../types/trace'
import ChatTimeline from './ChatTimeline'
import Composer from './Composer'
import Toast, { type ToastState } from './Toast'
import TraceDrawer from './TraceDrawer'
import WorkspaceHeader from './WorkspaceHeader'
import { extractCodeLinksFromText } from './codeLinkText'
import { sanitizeModelMessage } from './traceViewModel'

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

const toolCallTestPrompt = '请必须调用 calculator.add 工具计算 1+1，然后告诉我结果。'
const traceEventName = 'agent_trace_event'

function Workspace({
  state,
  setState,
  onRefreshProjects,
  onGlobalNotice,
  onGlobalError,
}: WorkspaceProps) {
  const workspaceRef = useRef<HTMLElement>(null)
  const bodyRef = useRef<HTMLDivElement>(null)
  const [busy, setBusy] = useState(false)
  const [composerDraft, setComposerDraft] = useState('')
  const [workspaceToast, setWorkspaceToast] = useState<ToastState | null>(null)
  const [traceWidth, setTraceWidth] = useState(loadTraceWidth)
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
    const chatTimeline = workspace.querySelector<HTMLElement>('.chat-timeline')
    const traceBody = workspace.querySelector<HTMLElement>('.trace-drawer-body')
    if (!header || (!actions && !identity)) {
      setHeaderDivided(false)
      return
    }

    const protectedRects = [identity, actions]
      .filter((element): element is HTMLElement => element !== null)
      .map((element) => element.getBoundingClientRect())
    const contentRects = [chatTimeline, traceBody]
      .filter((element): element is HTMLElement => element !== null)
      .map((element) => element.getBoundingClientRect())
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
      workspace.querySelectorAll<HTMLElement>('.chat-timeline, .trace-drawer-body'),
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
  }, [currentTask?.id, state.traceDrawerOpen, traceWidth, updateHeaderDivider])

  useEffect(() => {
    const task = currentTask
    let active = true
    window.queueMicrotask(() => {
      if (!active) {
        return
      }
      setSelectedTrace((current) => {
        if (!current || !task) {
          return null
        }
        return task.messages.some((message) => message.taskId === current.taskId) ?
            current
          : null
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
    selection: { providerId: string | null; modelId: string | null },
  ) => {
    if (!activeProject) {
      return
    }

    const sessionTaskId = currentTask?.id ?? crypto.randomUUID()
    const userMessage = createMessage(sessionTaskId, 'user', prompt)
    const pendingTask: AgentTask =
      currentTask ?
        {
          ...currentTask,
          messages: [...currentTask.messages, userMessage],
          status: 'running',
        }
      : {
          id: sessionTaskId,
          projectId: activeProject.id,
          prompt,
          messages: [userMessage],
          traceEvents: [],
          status: 'running',
        }
    const messages = createConversationMessages(pendingTask.messages)

    setBusy(true)
    setState((current) => addOrReplaceSessionTask(current, activeProject.id, pendingTask))

    try {
      const run = await runAgent({
        projectId: activeProject.id,
        userPrompt: prompt,
        messages,
        providerId: selection.providerId,
        modelId: selection.modelId,
      })
      const assistantMessage = createAssistantMessage(run.taskId, run.traces)
      setSelectedTrace({ taskId: run.taskId, events: run.traces })

      setState((current) =>
        completeSessionRun(
          current,
          sessionTaskId,
          assistantMessage,
          run.traces,
        ),
      )
    } catch (caught) {
      const message = caught instanceof Error ? caught.message : String(caught)
      const errorMessage = createMessage(sessionTaskId, 'system', message)
      setState((current) =>
        appendMessagesToSession(current, activeProject.id, sessionTaskId, [errorMessage], 'failed'),
      )
      showWorkspaceToast('error', message)
    } finally {
      setBusy(false)
    }
  }

  const runToolCallTestTask = async (
    selection: { providerId: string | null; modelId: string | null },
  ) => {
    if (!activeProject) {
      return
    }

    const sessionTaskId = crypto.randomUUID()
    const userMessage = createMessage(sessionTaskId, 'user', toolCallTestPrompt)
    const pendingTask: AgentTask = {
      id: sessionTaskId,
      projectId: activeProject.id,
      prompt: toolCallTestPrompt,
      messages: [userMessage],
      traceEvents: [],
      status: 'running',
    }
    let unlisten: UnlistenFn | null = null

    setBusy(true)
    setSelectedTrace(null)
    setState((current) => ({
      ...addOrReplaceSessionTask(current, activeProject.id, pendingTask),
      traceDrawerOpen: true,
    }))

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
          appendTraceEventToSession(current, sessionTaskId, traceEvent),
        )
      })

      const run = await runToolCallTest({
        projectId: activeProject.id,
        providerId: selection.providerId,
        modelId: selection.modelId,
      })
      const assistantMessage = createAssistantMessage(run.taskId, run.traces)
      setSelectedTrace({ taskId: run.taskId, events: run.traces })

      setState((current) =>
        completeSessionRun(
          current,
          sessionTaskId,
          assistantMessage,
          run.traces,
        ),
      )
    } catch (caught) {
      const message = caught instanceof Error ? caught.message : String(caught)
      const errorMessage = createMessage(sessionTaskId, 'system', message)
      setState((current) =>
        appendMessagesToSession(current, activeProject.id, sessionTaskId, [errorMessage], 'failed'),
      )
      showWorkspaceToast('error', message)
    } finally {
      unlisten?.()
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

  const beginTraceResize = (event: ReactPointerEvent<HTMLDivElement>) => {
    const body = bodyRef.current
    if (!body) {
      return
    }
    event.preventDefault()
    event.currentTarget.setPointerCapture(event.pointerId)

    const bodyRect = body.getBoundingClientRect()
    const minTraceWidth = 340
    const minChatWidth = 420
    const maxTraceWidth = Math.max(minTraceWidth, bodyRect.width - minChatWidth)

    const move = (pointerEvent: PointerEvent) => {
      const nextWidth = Math.min(
        maxTraceWidth,
        Math.max(minTraceWidth, bodyRect.right - pointerEvent.clientX),
      )
      setTraceWidth(nextWidth)
      saveTraceWidth(nextWidth)
    }

    const stop = () => {
      window.removeEventListener('pointermove', move)
      window.removeEventListener('pointerup', stop)
      document.body.classList.remove('workspace-resizing')
    }

    document.body.classList.add('workspace-resizing')
    window.addEventListener('pointermove', move)
    window.addEventListener('pointerup', stop, { once: true })
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

      <div
        className={state.traceDrawerOpen ? 'workspace-body trace-open' : 'workspace-body'}
        style={{ '--trace-width': `${traceWidth}px` } as CSSProperties}
        ref={bodyRef}
      >
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
              onSuggestionSelect={setComposerDraft}
            />
            <Composer
              providers={state.providers}
              busy={busy}
              value={composerDraft}
              onChange={setComposerDraft}
              onSend={runTask}
              onRunToolCallTest={runToolCallTestTask}
            />
          </div>
        </main>
        {state.traceDrawerOpen ? (
          <div
            className="workspace-resizer"
            role="separator"
            aria-orientation="vertical"
            aria-label="Resize Trace panel"
            onPointerDown={beginTraceResize}
          />
        ) : null}
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
      </div>
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
): AppState {
  const task = state.tasksById[sessionTaskId]
  if (!task) {
    return state
  }
  const failed = hasFailedTrace(traces)
  return {
    ...state,
    currentWorkspaceTaskId: sessionTaskId,
    traceDrawerOpen:
      state.traceDrawerOpen ||
      state.settings?.uiPreferences.defaultWorkspaceLayout === 'split-chat-trace' ||
      (state.settings?.uiPreferences.autoOpenTraceOnErrors === true && failed),
    tasksById: {
      ...state.tasksById,
      [sessionTaskId]: {
        ...task,
        messages: [...task.messages, assistantMessage],
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
          status: hasFailedTrace(traces) ? 'failed' : task.status,
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
): AppState {
  const task = state.tasksById[sessionTaskId]
  if (!task) {
    return state
  }

  return {
    ...state,
    tasksById: {
      ...state.tasksById,
      [sessionTaskId]: {
        ...task,
        traceEvents: appendTraceEvent(task.traceEvents, traceEvent),
      },
    },
  }
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
        message.role === 'user' || message.role === 'assistant',
    )
    .map((message) => ({
      role: message.role,
      content: message.content,
    }))
}

function createMessage(
  taskId: string,
  role: ChatMessage['role'],
  content: string,
): ChatMessage {
  return {
    id: crypto.randomUUID(),
    taskId,
    role,
    content,
    createdAt: new Date().toISOString(),
  }
}

function createAssistantMessage(
  taskId: string,
  traces: ToolTraceEvent[],
): ChatMessage {
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
    codeLinks: links,
    traceEvents: traces,
  }
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

function loadTraceWidth(): number {
  if (typeof window === 'undefined') {
    return 560
  }
  const value = Number(window.localStorage.getItem('snowagent.traceWidth'))
  if (!Number.isFinite(value)) {
    return 560
  }
  return Math.min(900, Math.max(340, value))
}

function saveTraceWidth(width: number): void {
  if (typeof window === 'undefined') {
    return
  }
  try {
    window.localStorage.setItem('snowagent.traceWidth', String(Math.round(width)))
  } catch {
    // Best-effort UI preference.
  }
}

function rectsIntersect(left: DOMRect, right: DOMRect): boolean {
  return (
    left.left < right.right &&
    left.right > right.left &&
    left.top < right.bottom &&
    left.bottom > right.top
  )
}
