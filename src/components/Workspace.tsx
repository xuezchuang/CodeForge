import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import type { Dispatch, SetStateAction } from 'react'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import {
  deleteWorkspaceSessions,
  listTools,
  listTraces,
  loadWorkspaceSession,
  openVisualStudio,
  runAgent,
  saveWorkspaceSession,
  updateSettings,
  type ToolDefinitionSummary,
} from '../api/tauriApi'
import { normalizeAgentTask, normalizeSettings } from '../state/appState'
import type { AppState } from '../state/appState'
import type { AgentTask, ChatMessage, MessageAttachment } from '../types/task'
import type {
  AgentConversationMessage,
  ContextCompactionResult,
  MockAgentRun,
  ToolTraceEvent,
} from '../types/trace'
import ChatTimeline from './ChatTimeline'
import {
  ProgressStepsPanel,
  createProgressSnapshot,
  type ProgressSnapshot,
} from './ChatMessage'
import Composer from './Composer'
import Toast, { type ToastState } from './Toast'
import TraceDrawer from './TraceDrawer'
import WorkspaceHeader from './WorkspaceHeader'
import { extractCodeLinksFromText } from './codeLinkText'
import {
  aggregateTokenUsage,
  sanitizeModelMessage,
} from './traceViewModel'
import {
  getDefaultSelectableModel,
  getSelectableModels,
} from '../utils/providerModels'

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

interface ComposerModelSelection {
  providerId: string
  credentialId: string | null
  modelId: string
  reasoningEffort: string | null
}

interface AgentRunSelection {
  providerId: string | null
  credentialId: string | null
  modelId: string | null
  reasoningEffort: string | null
}

type ReviewStatus = 'running' | 'completed' | 'failed'
type ReviewFindingSeverity = 'suggestion' | 'warning' | 'pass'

interface ReviewFinding {
  severity: ReviewFindingSeverity
  title: string
  detail?: string
}

interface CodeReviewPanelState {
  status: ReviewStatus
  parentTaskId: string
  reviewTaskId?: string
  summary?: string
  error?: string
  findings: ReviewFinding[]
  updatedAt: string
}

const traceEventName = 'agent_trace_event'
const progressUpdateStepsTool = 'progress/update_steps'

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
  const [editingUserMessageId, setEditingUserMessageId] = useState<string | null>(null)
  const [loadingSessionId, setLoadingSessionId] = useState<string | null>(null)
  const [runningSessionIds, setRunningSessionIds] = useState<Set<string>>(() => new Set())
  const [, setCodeReviewsBySessionId] = useState<
    Record<string, CodeReviewPanelState>
  >({})
  const runningSessionIdsRef = useRef<Set<string>>(new Set())
  const currentWorkspaceTaskIdRef = useRef<string | null>(state.currentWorkspaceTaskId)
  const lastRunSelectionRef = useRef<AgentRunSelection | null>(null)
  const activeProject = useMemo(
    () =>
      state.projects.find((project) => project.id === state.activeProjectId) ??
      null,
    [state.activeProjectId, state.projects],
  )
  const activeProjectId = activeProject?.id ?? null
  const currentTask =
    state.currentWorkspaceTaskId ?
      state.tasksById[state.currentWorkspaceTaskId] ?? null
    : null
  const progressSnapshot = useMemo(
    () => createTaskProgressSnapshot(currentTask),
    [currentTask],
  )
  const currentTaskIsRunning = Boolean(
    currentTask && runningSessionIds.has(currentTask.id),
  )
  const hasOtherRunningSession = runningSessionIds.size > 0 && !currentTaskIsRunning
  const selectedTraceEvents = selectedTrace?.events ?? []
  const currentTaskPersistenceKey =
    currentTask && currentTask.messagesLoaded !== false && currentTask.status !== 'running' ?
      [
        currentTask.id,
        currentTask.status,
        currentTask.messages.length,
        currentTask.updatedAt ?? '',
        currentTask.messages
          .map((message) => [
            message.id,
            message.taskId,
            message.status ?? '',
            message.createdAt,
            message.content.length,
          ].join(':'))
          .join('|'),
      ].join('::')
    : ''

  useEffect(() => {
    currentWorkspaceTaskIdRef.current = state.currentWorkspaceTaskId
  }, [state.currentWorkspaceTaskId])

  const setSessionRunning = useCallback((sessionTaskId: string, running: boolean) => {
    const next = new Set(runningSessionIdsRef.current)
    if (running) {
      next.add(sessionTaskId)
    } else {
      next.delete(sessionTaskId)
    }
    runningSessionIdsRef.current = next
    setRunningSessionIds(next)
  }, [])

  useEffect(() => {
    if (!currentTask || currentTask.messagesLoaded !== false) {
      return undefined
    }

    let cancelled = false
    setLoadingSessionId(currentTask.id)
    loadWorkspaceSession(currentTask.id)
      .then((loadedTask) => {
        if (cancelled) {
          return
        }
        const normalizedTask = normalizeAgentTask(loadedTask)
        setState((current) => {
          if (!current.tasksById[normalizedTask.id]) {
            return current
          }
          return {
            ...current,
            tasksById: {
              ...current.tasksById,
              [normalizedTask.id]: normalizedTask,
            },
          }
        })
      })
      .catch((caught) => {
        if (!cancelled) {
          onGlobalError(caught instanceof Error ? caught.message : String(caught))
        }
      })
      .finally(() => {
        if (!cancelled) {
          setLoadingSessionId(null)
        }
      })

    return () => {
      cancelled = true
    }
  }, [currentTask, onGlobalError, setState])

  useEffect(() => {
    if (!activeProjectId || !currentTask || currentTask.messagesLoaded === false) {
      return undefined
    }
    if (currentTask.status === 'running') {
      return undefined
    }

    const timeoutId = window.setTimeout(() => {
      const position = Math.max(
        0,
        (state.taskIdsByProjectId[activeProjectId] ?? []).indexOf(currentTask.id),
      )
      void saveWorkspaceSession(currentTask, position).catch((caught) => {
        onGlobalError(caught instanceof Error ? caught.message : String(caught))
      })
    }, 250)

    return () => {
      window.clearTimeout(timeoutId)
    }
  }, [
    activeProjectId,
    currentTaskPersistenceKey,
    onGlobalError,
    state.taskIdsByProjectId,
  ])

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

  const persistComposerModelSelection = useCallback(
    async (selection: ComposerModelSelection) => {
      const settings = state.settings
      if (!settings) {
        return
      }

      let changed = false
      const nextProviders = state.providers.map((provider) => {
        const isSelectedProvider = provider.id === selection.providerId
        let nextProvider = provider
        if (provider.isDefault !== isSelectedProvider) {
          nextProvider = { ...nextProvider, isDefault: isSelectedProvider }
          changed = true
        }
        if (!isSelectedProvider) {
          return nextProvider
        }

        const nextCredentialId = selection.credentialId ?? ''
        let modelsChanged = false
        const nextModels = provider.models.map((model) => {
          if (model.id !== selection.modelId || !selection.reasoningEffort) {
            return model
          }
          if (model.defaultReasoning === selection.reasoningEffort) {
            return model
          }
          modelsChanged = true
          return {
            ...model,
            defaultReasoning:
              selection.reasoningEffort as NonNullable<typeof model.defaultReasoning>,
          }
        })

        if (
          provider.defaultModel !== selection.modelId ||
          provider.defaultCredentialId !== nextCredentialId
        ) {
          changed = true
        }

        if (modelsChanged) {
          changed = true
        }

        return {
          ...nextProvider,
          defaultModel: selection.modelId,
          defaultCredentialId: nextCredentialId,
          models: nextModels,
        }
      })

      if (!changed) {
        return
      }

      try {
        const saved = await updateSettings({
          devenvPath: settings.devenvPath,
          providerNotes: settings.providerNotes ?? null,
          uiPreferences: settings.uiPreferences,
          providers: nextProviders,
        })
        const normalized = normalizeSettings(saved)
        setState((current) => ({
          ...current,
          settings: normalized,
          providers: normalized.providers,
        }))
      } catch (caught) {
        showWorkspaceToast(
          'error',
          caught instanceof Error ? caught.message : String(caught),
        )
      }
    },
    [setState, state.providers, state.settings],
  )

  const runTask = async (
    prompt: string,
    selection: AgentRunSelection,
    attachments: MessageAttachment[] = [],
  ) => {
    if (!activeProject) {
      return
    }
    const runSelection = normalizeRunSelection(selection)
    lastRunSelectionRef.current = runSelection
    if (isListToolsCommand(prompt)) {
      await showToolsCommand(activeProject.id, prompt)
      return
    }
    if (isStatusCommand(prompt)) {
      await showStatusCommand(activeProject.id, prompt)
      return
    }

    const sessionTaskId = currentTask?.id ?? crypto.randomUUID()
    const runTaskId = crypto.randomUUID()
    const userMessage = createMessage(sessionTaskId, 'user', prompt, attachments)
    const pendingAssistantMessage = createPendingAssistantMessage(
      runTaskId,
      attachments.length,
    )
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
          messagesLoaded: true,
          updatedAt: pendingAssistantMessage.createdAt,
        }
      : {
          id: sessionTaskId,
          projectId: activeProject.id,
          prompt,
          messages: [userMessage, pendingAssistantMessage],
          traceEvents: [],
          status: 'running',
          messagesLoaded: true,
          createdAt: userMessage.createdAt,
          updatedAt: pendingAssistantMessage.createdAt,
        }

    await runSessionCompletion({
      sessionTaskId,
      runTaskId,
      prompt,
      selection: runSelection,
      conversationMessages,
      pendingTask,
      pendingAssistantMessage,
    })
  }

  const runSessionCompletion = async ({
    sessionTaskId,
    runTaskId,
    prompt,
    selection,
    conversationMessages,
    pendingTask,
    pendingAssistantMessage,
  }: {
    sessionTaskId: string
    runTaskId: string
    prompt: string
    selection: AgentRunSelection
    conversationMessages: AgentConversationMessage[]
    pendingTask: AgentTask
    pendingAssistantMessage: ChatMessage
  }): Promise<boolean> => {
    if (!activeProject) {
      return false
    }
    let unlisten: UnlistenFn | null = null
    let completed = false
    let autoReviewRequest: {
      parentRun: MockAgentRun
      prompt: string
      selection: AgentRunSelection
    } | null = null

    if (
      runningSessionIdsRef.current.size > 0 &&
      !runningSessionIdsRef.current.has(sessionTaskId)
    ) {
      showWorkspaceToast(
        'notice',
        'Another chat is still running. Wait for it to finish before sending.',
      )
      return false
    }

    currentWorkspaceTaskIdRef.current = sessionTaskId
    setEditingUserMessageId(null)
    setSessionRunning(sessionTaskId, true)
    setSelectedTrace(null)
    setState((current) => addOrReplaceSessionTask(current, activeProject.id, pendingTask))

    try {
      unlisten = await listen<ToolTraceEvent>(traceEventName, (event) => {
        const traceEvent = event.payload
        if (!isToolTraceEvent(traceEvent)) {
          return
        }
        if (traceEvent.taskId !== runTaskId) {
          return
        }
        if (currentWorkspaceTaskIdRef.current === sessionTaskId) {
          setSelectedTrace((current) => ({
            taskId: traceEvent.taskId,
            events:
              current?.taskId === traceEvent.taskId ?
                upsertTraceEvent(current.events, traceEvent)
              : [traceEvent],
          }))
        }
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
        sessionId: sessionTaskId,
        taskId: runTaskId,
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
      if (currentWorkspaceTaskIdRef.current === sessionTaskId) {
        setSelectedTrace({ taskId: run.taskId, events: run.traces })
      }

      setState((current) =>
        completeSessionRun(
          current,
          sessionTaskId,
          assistantMessage,
          run.traces,
          pendingAssistantMessage.id,
          run.contextCompaction ?? null,
        ),
      )
      if (shouldStartAutoCodeReview(run.traces)) {
        autoReviewRequest = {
          parentRun: run,
          prompt,
          selection,
        }
      }
      completed = true
    } catch (caught) {
      const message = caught instanceof Error ? caught.message : String(caught)
      setState((current) =>
        failSessionRun(
          current,
          sessionTaskId,
          pendingAssistantMessage.id,
          message,
        ),
      )
      showWorkspaceToast('error', message)
    } finally {
      unlisten?.()
      setSessionRunning(sessionTaskId, false)
      if (autoReviewRequest) {
        void startBackgroundCodeReview(
          sessionTaskId,
          autoReviewRequest.parentRun,
          autoReviewRequest.prompt,
          autoReviewRequest.selection,
        )
      }
    }
    return completed
  }

  const startBackgroundCodeReview = async (
    sessionTaskId: string,
    parentRun: MockAgentRun,
    prompt: string,
    selection: AgentRunSelection,
  ) => {
    if (!activeProject) {
      return
    }
    setCodeReviewsBySessionId((current) => ({
      ...current,
      [sessionTaskId]: {
        status: 'running',
        parentTaskId: parentRun.taskId,
        findings: [],
        updatedAt: new Date().toISOString(),
      },
    }))

    try {
      const reviewTaskId = crypto.randomUUID()
      const reviewRun = await runAgent({
        projectId: activeProject.id,
        sessionId: sessionTaskId,
        taskId: reviewTaskId,
        userPrompt: buildAutoCodeReviewPrompt(prompt, parentRun.traces),
        providerId: selection.providerId,
        credentialId: selection.credentialId,
        modelId: selection.modelId,
        reasoningEffort: selection.reasoningEffort,
        parentTaskId: parentRun.taskId,
        agentName: 'code-review',
        taskName: 'automatic-code-review',
        readOnly: true,
        subagentDepth: 1,
      })
      const parsedReview = parseCodeReviewRun(reviewRun)
      setCodeReviewsBySessionId((current) => {
        if (current[sessionTaskId]?.parentTaskId !== parentRun.taskId) {
          return current
        }
        return {
          ...current,
          [sessionTaskId]: {
            status: 'completed',
            parentTaskId: parentRun.taskId,
            reviewTaskId: reviewRun.taskId,
            findings: parsedReview.findings,
            summary: parsedReview.summary,
            updatedAt: new Date().toISOString(),
          },
        }
      })
    } catch (caught) {
      const message = caught instanceof Error ? caught.message : String(caught)
      setCodeReviewsBySessionId((current) => {
        if (current[sessionTaskId]?.parentTaskId !== parentRun.taskId) {
          return current
        }
        return {
          ...current,
          [sessionTaskId]: {
            status: 'failed',
            parentTaskId: parentRun.taskId,
            findings: [],
            error: message,
            updatedAt: new Date().toISOString(),
          },
        }
      })
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
        current?.taskId === taskId ?
          { taskId, events: mergeTraceEvents(current.events, traces) }
        : current,
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

  const forkMessageSession = (message: ChatMessage) => {
    if (!activeProject || !currentTask) {
      return
    }
    const forkedTask = createForkedTaskAtMessage(currentTask, message.id)
    if (!forkedTask) {
      showWorkspaceToast('error', 'Unable to fork this message.')
      return
    }

    currentWorkspaceTaskIdRef.current = forkedTask.id
    setEditingUserMessageId(null)
    setComposerDraft('')
    setSelectedTrace(null)
    setState((current) => addOrReplaceSessionTask(current, activeProject.id, forkedTask))
    showWorkspaceToast('notice', 'Forked chat.')
  }

  const retryMessageInFork = async (message: ChatMessage) => {
    if (!activeProject || !currentTask) {
      return
    }
    const selection = resolveActionRunSelection(state.providers, lastRunSelectionRef.current)
    if (!selection) {
      showWorkspaceToast('error', 'Enable a model before retrying.')
      return
    }
    const retryDraft = createRetryForkDraft(currentTask, message.id)
    if (!retryDraft) {
      showWorkspaceToast('error', 'Unable to retry this response.')
      return
    }

    lastRunSelectionRef.current = selection
    setComposerDraft('')
    const runTaskId = crypto.randomUUID()
    const pendingAssistantMessage = createPendingAssistantMessage(
      runTaskId,
      retryDraft.userMessage.attachments?.length ?? 0,
    )
    const messages = [
      ...retryDraft.baseTask.messages,
      retryDraft.userMessage,
      pendingAssistantMessage,
    ]
    const pendingTask: AgentTask = {
      ...retryDraft.baseTask,
      messages,
      traceEvents: collectMessageTraceEvents(retryDraft.baseTask.messages),
      status: 'running',
      updatedAt: pendingAssistantMessage.createdAt,
    }

    const completed = await runSessionCompletion({
      sessionTaskId: retryDraft.baseTask.id,
      runTaskId,
      prompt: retryDraft.userMessage.content,
      selection,
      conversationMessages: createConversationMessages([
        ...retryDraft.baseTask.messages,
        retryDraft.userMessage,
      ]),
      pendingTask,
      pendingAssistantMessage,
    })
    if (completed) {
      showWorkspaceToast('notice', 'Retry forked and sent.')
    }
  }

  const startUserMessageEdit = (message: ChatMessage) => {
    if (!currentTask || !isLastUserMessage(currentTask, message.id)) {
      return
    }
    setEditingUserMessageId(message.id)
  }

  const saveUserMessageEdit = async (message: ChatMessage, content: string) => {
    if (!activeProject || !currentTask) {
      return
    }
    if (!isLastUserMessage(currentTask, message.id)) {
      showWorkspaceToast('notice', 'Only the latest user message can be edited.')
      setEditingUserMessageId(null)
      return
    }
    if (content.trim() === message.content.trim()) {
      setEditingUserMessageId(null)
      return
    }
    const selection = resolveActionRunSelection(state.providers, lastRunSelectionRef.current)
    if (!selection) {
      showWorkspaceToast('error', 'Enable a model before saving the edit.')
      return
    }
    const editDraft = createEditedSessionDraft(currentTask, message.id, content)
    if (!editDraft) {
      showWorkspaceToast('error', 'Unable to edit this message.')
      return
    }

    lastRunSelectionRef.current = selection
    setComposerDraft('')
    const runTaskId = crypto.randomUUID()
    const pendingAssistantMessage = createPendingAssistantMessage(
      runTaskId,
      editDraft.userMessage.attachments?.length ?? 0,
    )
    const messages = [
      ...editDraft.baseMessages,
      editDraft.userMessage,
      pendingAssistantMessage,
    ]
    const pendingTask: AgentTask = {
      ...currentTask,
      prompt: inferTaskPrompt(messages, content),
      messages,
      traceEvents: collectMessageTraceEvents(editDraft.baseMessages),
      status: 'running',
      messagesLoaded: true,
      updatedAt: pendingAssistantMessage.createdAt,
    }

    const completed = await runSessionCompletion({
      sessionTaskId: currentTask.id,
      runTaskId,
      prompt: editDraft.userMessage.content,
      selection,
      conversationMessages: createConversationMessages([
        ...editDraft.baseMessages,
        editDraft.userMessage,
      ]),
      pendingTask,
      pendingAssistantMessage,
    })
    if (completed) {
      showWorkspaceToast('notice', 'Message edited and resent.')
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
    const taskIds = state.taskIdsByProjectId[activeProject.id] ?? []
    setState((current) => {
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
    if (taskIds.length > 0) {
      void deleteWorkspaceSessions(taskIds).catch((caught) => {
        showWorkspaceToast('error', caught instanceof Error ? caught.message : String(caught))
      })
    }
    setSelectedTrace(null)
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
        busy={busy || currentTaskIsRunning}
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
              loading={currentTask?.messagesLoaded === false || loadingSessionId === currentTask?.id}
              onCodeLinkResult={(message) => showWorkspaceToast('notice', message)}
              onCodeLinkError={(message) =>
                showWorkspaceToast('error', normalizeCodeLinkError(message))
              }
              onTraceChanged={(taskId) => {
                void refreshTrace(taskId)
              }}
              onOpenTrace={openMessageTrace}
              editingUserMessageId={editingUserMessageId}
              onStartEditUserMessage={startUserMessageEdit}
              onCancelEditUserMessage={() => setEditingUserMessageId(null)}
              onSaveUserMessageEdit={(message, content) => {
                void saveUserMessageEdit(message, content)
              }}
              onForkMessage={forkMessageSession}
              onRetryMessage={(message) => {
                void retryMessageInFork(message)
              }}
              onSuggestionSelect={setComposerDraft}
            />
            {progressSnapshot ? (
              <div className="composer-progress">
                <ProgressStepsPanel snapshot={progressSnapshot} compact />
              </div>
            ) : null}
            <Composer
              providers={state.providers}
              busy={busy || currentTaskIsRunning || currentTask?.messagesLoaded === false}
              sendBlocked={hasOtherRunningSession}
              sendBlockTitle="Wait for the running chat to finish"
              value={composerDraft}
              onChange={setComposerDraft}
              onSend={runTask}
              onModelSelectionChange={(selection) => {
                void persistComposerModelSelection(selection)
              }}
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
      [task.id]: withTaskPersistenceMetadata(task),
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
      messagesLoaded: true,
      createdAt: messages[0]?.createdAt,
      updatedAt: messages.at(-1)?.createdAt,
    }

  return addOrReplaceSessionTask(state, projectId, {
    ...task,
    messages: [...task.messages, ...messages],
    status,
    messagesLoaded: true,
    updatedAt: messages.at(-1)?.createdAt ?? task.updatedAt,
  })
}

function normalizeRunSelection(selection: AgentRunSelection): AgentRunSelection {
  return {
    providerId: selection.providerId,
    credentialId: selection.credentialId,
    modelId: selection.modelId,
    reasoningEffort: selection.reasoningEffort,
  }
}

function resolveActionRunSelection(
  providers: AppState['providers'],
  lastSelection: AgentRunSelection | null,
): AgentRunSelection | null {
  if (lastSelection?.providerId && lastSelection.modelId) {
    return lastSelection
  }
  const selectableModels = getSelectableModels(providers)
  const selectedModel = getDefaultSelectableModel(providers, selectableModels)
  if (!selectedModel) {
    return null
  }
  return {
    providerId: selectedModel.providerId,
    credentialId: selectedModel.credentialId,
    modelId: selectedModel.modelId,
    reasoningEffort: null,
  }
}

function createForkedTaskAtMessage(
  sourceTask: AgentTask,
  targetMessageId: string,
): AgentTask | null {
  const targetIndex = findMessageIndex(sourceTask, targetMessageId)
  if (targetIndex < 0) {
    return null
  }

  const sessionTaskId = crypto.randomUUID()
  const messages = sourceTask.messages
    .slice(0, targetIndex + 1)
    .map((message) => cloneMessageForFork(message, sourceTask.id, sessionTaskId))

  return {
    id: sessionTaskId,
    projectId: sourceTask.projectId,
    prompt: inferTaskPrompt(messages, sourceTask.prompt),
    messages,
    traceEvents: collectMessageTraceEvents(messages),
    status: inferTaskStatus(messages),
    messagesLoaded: true,
    createdAt: messages[0]?.createdAt ?? new Date().toISOString(),
    updatedAt: new Date().toISOString(),
  }
}

function createRetryForkDraft(
  sourceTask: AgentTask,
  targetMessageId: string,
): { baseTask: AgentTask; userMessage: ChatMessage } | null {
  const targetIndex = findMessageIndex(sourceTask, targetMessageId)
  if (targetIndex < 0) {
    return null
  }
  const userIndex = findPreviousUserMessageIndex(sourceTask.messages, targetIndex)
  if (userIndex < 0) {
    return null
  }

  const sessionTaskId = crypto.randomUUID()
  const baseMessages = sourceTask.messages
    .slice(0, userIndex)
    .map((message) => cloneMessageForFork(message, sourceTask.id, sessionTaskId))
  const userMessage = cloneUserMessageForFork(
    sourceTask.messages[userIndex],
    sourceTask.id,
    sessionTaskId,
  )

  return {
    baseTask: {
      id: sessionTaskId,
      projectId: sourceTask.projectId,
      prompt: inferTaskPrompt([...baseMessages, userMessage], userMessage.content),
      messages: baseMessages,
      traceEvents: collectMessageTraceEvents(baseMessages),
      status: inferTaskStatus(baseMessages),
      messagesLoaded: true,
      createdAt: baseMessages[0]?.createdAt ?? userMessage.createdAt,
      updatedAt: new Date().toISOString(),
    },
    userMessage,
  }
}

function createEditedSessionDraft(
  sourceTask: AgentTask,
  userMessageId: string,
  content: string,
): { baseMessages: ChatMessage[]; userMessage: ChatMessage } | null {
  const userIndex = findMessageIndex(sourceTask, userMessageId)
  if (userIndex < 0 || sourceTask.messages[userIndex].role !== 'user') {
    return null
  }

  const userMessage: ChatMessage = {
    ...sourceTask.messages[userIndex],
    content,
    codeLinks: undefined,
    status: undefined,
    traceEvents: undefined,
  }
  return {
    baseMessages: sourceTask.messages.slice(0, userIndex),
    userMessage,
  }
}

function cloneMessageForFork(
  message: ChatMessage,
  sourceSessionId: string,
  targetSessionId: string,
): ChatMessage {
  return {
    ...message,
    id: crypto.randomUUID(),
    taskId: message.taskId === sourceSessionId ? targetSessionId : message.taskId,
    status: message.status === 'running' ? 'failed' : message.status,
  }
}

function cloneUserMessageForFork(
  message: ChatMessage,
  sourceSessionId: string,
  targetSessionId: string,
): ChatMessage {
  return {
    ...cloneMessageForFork(message, sourceSessionId, targetSessionId),
    codeLinks: undefined,
    status: undefined,
    traceEvents: undefined,
  }
}

function isLastUserMessage(task: AgentTask, messageId: string): boolean {
  for (let index = task.messages.length - 1; index >= 0; index -= 1) {
    const message = task.messages[index]
    if (message.role === 'user') {
      return message.id === messageId
    }
  }
  return false
}

function findMessageIndex(task: AgentTask, messageId: string): number {
  return task.messages.findIndex((message) => message.id === messageId)
}

function findPreviousUserMessageIndex(messages: ChatMessage[], beforeIndex: number): number {
  for (let index = beforeIndex - 1; index >= 0; index -= 1) {
    if (messages[index].role === 'user') {
      return index
    }
  }
  return -1
}

function inferTaskPrompt(messages: ChatMessage[], fallback: string): string {
  return messages.find((message) => message.role === 'user')?.content ?? fallback
}

function inferTaskStatus(messages: ChatMessage[]): AgentTask['status'] {
  return messages.some((message) => message.status === 'failed') ? 'failed' : 'completed'
}

function collectMessageTraceEvents(messages: ChatMessage[]): ToolTraceEvent[] {
  return messages.reduce<ToolTraceEvent[]>(
    (events, message) => mergeTraceEvents(events, message.traceEvents ?? []),
    [],
  )
}

function completeSessionRun(
  state: AppState,
  sessionTaskId: string,
  assistantMessage: ChatMessage,
  traces: ToolTraceEvent[],
  pendingAssistantMessageId: string,
  contextCompaction: ContextCompactionResult | null,
): AppState {
  const task = state.tasksById[sessionTaskId]
  if (!task) {
    return state
  }
  const mergedTraces = mergeTraceEvents(task.traceEvents, traces)
  const failed = hasFailedTrace(mergedTraces)
  const completedAssistantMessage = {
    ...assistantMessage,
    traceEvents: mergedTraces,
    status: failed ? 'failed' : assistantMessage.status,
  }
  const completedMessages = replaceMessageById(
    task.messages,
    pendingAssistantMessageId,
    completedAssistantMessage,
  )
  const nextMessages =
    contextCompaction && !failed ?
      applyContextCompactionToMessages(
        sessionTaskId,
        completedMessages,
        completedAssistantMessage,
        contextCompaction,
      )
    : completedMessages
  return {
    ...state,
    traceDrawerOpen: state.traceDrawerOpen,
    tasksById: {
      ...state.tasksById,
      [sessionTaskId]: {
        ...task,
        messages: nextMessages,
        traceEvents: mergedTraces,
        status: failed ? 'failed' : 'completed',
        messagesLoaded: true,
        updatedAt: completedAssistantMessage.createdAt,
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
        const nextMessageTraceEvents = mergeTraceEvents(message.traceEvents ?? [], traces)
        return {
          ...message,
          traceEvents: nextMessageTraceEvents,
          status: hasTaskFailedTrace(nextMessageTraceEvents) ? 'failed' : message.status,
        }
      })
      const taskTraceMatches = task.traceEvents.some((event) => event.taskId === taskId)
      if (!taskChanged && !taskTraceMatches) {
        return [sessionTaskId, task]
      }
      changed = true
      const nextTaskTraceEvents = mergeTraceEvents(task.traceEvents, traces)
      const failed = hasTaskFailedTrace(nextTaskTraceEvents)
      return [
        sessionTaskId,
        {
          ...task,
          messages: taskChanged ? messages : task.messages,
          traceEvents: taskTraceMatches || taskChanged ? nextTaskTraceEvents : task.traceEvents,
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

  const nextTaskTraceEvents = upsertTraceEvent(task.traceEvents, traceEvent)

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
              const nextMessageTraceEvents = upsertTraceEvent(
                message.traceEvents ?? [],
                traceEvent,
              )
              return {
                ...message,
                taskId: traceEvent.taskId,
                content: createRunningAssistantContent(nextMessageTraceEvents),
                traceEvents: nextMessageTraceEvents,
                status: hasFailedTrace(nextMessageTraceEvents) ? 'failed' : 'running',
              }
            })
          : task.messages,
      },
    },
  }
}

function failSessionRun(
  state: AppState,
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
    return state
  }

  return {
    ...state,
    tasksById: {
      ...state.tasksById,
      [sessionTaskId]: {
        ...task,
        messages: replaceMessageById(task.messages, pendingAssistantMessageId, failedMessage),
        status: 'failed',
        messagesLoaded: true,
        updatedAt: failedMessage.createdAt,
      },
    },
  }
}

function withTaskPersistenceMetadata(task: AgentTask): AgentTask {
  return {
    ...task,
    messagesLoaded: task.messagesLoaded ?? true,
    createdAt: task.createdAt ?? task.messages[0]?.createdAt,
    updatedAt: task.updatedAt ?? task.messages.at(-1)?.createdAt,
  }
}

function taskHasTraceSelection(task: AgentTask, taskId: string): boolean {
  return (
    task.messages.some((message) => message.taskId === taskId) ||
    task.traceEvents.some((event) => event.taskId === taskId)
  )
}

function upsertTraceEvent(
  events: ToolTraceEvent[],
  traceEvent: ToolTraceEvent,
): ToolTraceEvent[] {
  let replaced = false
  const nextEvents = events.map((event) => {
    if (event.id !== traceEvent.id) {
      return event
    }
    replaced = true
    return traceEvent
  })
  if (replaced) {
    return nextEvents
  }
  return [...events, traceEvent]
}

function mergeTraceEvents(
  existingEvents: ToolTraceEvent[],
  incomingEvents: ToolTraceEvent[],
): ToolTraceEvent[] {
  return incomingEvents.reduce(
    (events, event) => upsertTraceEvent(events, event),
    existingEvents,
  )
}

function createConversationMessages(messages: ChatMessage[]): AgentConversationMessage[] {
  return messages
    .filter(isConversationHistoryMessage)
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

function isConversationHistoryMessage(message: ChatMessage): boolean {
  if (message.role === 'system') {
    return isContextCompactionMessage(message)
  }
  return (
    (message.role === 'user' || message.role === 'assistant') &&
    !isTransientConversationMessage(message)
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

const CONTEXT_COMPACTION_MESSAGE_PREFIX = '[CodeForge context compacted]'

function createContextCompactionMessage(
  taskId: string,
  compaction: ContextCompactionResult,
): ChatMessage {
  return createMessage(
    taskId,
    'system',
    `${CONTEXT_COMPACTION_MESSAGE_PREFIX}\n\nEarlier conversation summary:\n${compaction.summary.trim()}`,
  )
}

function isContextCompactionMessage(message: ChatMessage): boolean {
  return message.content.trimStart().startsWith(CONTEXT_COMPACTION_MESSAGE_PREFIX)
}

function applyContextCompactionToMessages(
  sessionTaskId: string,
  messages: ChatMessage[],
  completedAssistantMessage: ChatMessage,
  compaction: ContextCompactionResult,
): ChatMessage[] {
  const retainedMessages = messages
    .filter((message) => message.id !== completedAssistantMessage.id)
    .filter(isConversationHistoryMessage)
    .slice(-compaction.retainedMessageCount)

  return [
    createContextCompactionMessage(sessionTaskId, compaction),
    ...retainedMessages,
    completedAssistantMessage,
  ]
}

function createPendingAssistantMessage(taskId: string, attachmentCount = 0): ChatMessage {
  return {
    ...createMessage(
      taskId,
      'assistant',
      createRunningAssistantContent([], attachmentCount),
    ),
    status: 'running',
    traceEvents: [],
  }
}

function createRunningAssistantContent(
  traces: ToolTraceEvent[],
  _attachmentCount = 0,
): string {
  const latestTrace = latestDescribableTrace(traces)
  if (!latestTrace) {
    return 'Thinking...\n\n'
  }
  const detail = describeRunningTrace(latestTrace)
  return detail ? `Thinking...\n\n${detail}` : 'Thinking...\n\n'
}

function latestDescribableTrace(traces: ToolTraceEvent[]): ToolTraceEvent | undefined {
  for (let index = traces.length - 1; index >= 0; index -= 1) {
    const trace = traces[index]
    if (describeRunningTrace(trace).trim().length > 0) {
      return trace
    }
  }
  return undefined
}

function describeRunningTrace(event: ToolTraceEvent): string {
  const detail = runningTraceDetail(event)

  if (event.type === 'user_message') {
    return ''
  }
  if (event.toolName === progressUpdateStepsTool) {
    return ''
  }
  if (event.status === 'failed' || event.type === 'error') {
    return appendRunningDetail('Step failed', detail)
  }
  if (event.type === 'llm_request') {
    return ''
  }
  if (event.type === 'llm_response') {
    return ''
  }
  if (event.type === 'tool_call') {
    return appendRunningDetail(
      `Running ${runningToolLabel(event.toolName)}`,
      detail,
    )
  }
  if (event.type === 'tool_result') {
    if (event.toolName === 'chat_completion') {
      return ''
    }
    return appendRunningDetail(
      `Completed ${runningToolLabel(event.toolName)}`,
      detail,
    )
  }
  if (event.type === 'final_response') {
    return ''
  }
  if (event.type === 'model_message') {
    return ''
  }
  if (event.type === 'system_event') {
    return ''
  }
  return appendRunningDetail(event.outputSummary ?? event.title ?? 'Working', detail)
}

function createTaskProgressSnapshot(task: AgentTask | null): ProgressSnapshot | null {
  if (!task) {
    return null
  }

  const latestAssistantMessage = [...task.messages]
    .reverse()
    .find((message) => message.role === 'assistant')

  return createProgressSnapshot(latestAssistantMessage?.traceEvents ?? [])
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

function shouldStartAutoCodeReview(traces: ToolTraceEvent[]): boolean {
  return traces.some((event) => {
    if (event.type !== 'tool_result' || event.status !== 'success') {
      return false
    }
    return isWriteToolName(event.toolName)
  })
}

function isWriteToolName(toolName: string | null): boolean {
  return (
    toolName === 'edit_file' ||
    toolName === 'workspace/edit_file' ||
    toolName === 'write_file' ||
    toolName === 'workspace/write_file' ||
    toolName === 'apply_patch_raw'
  )
}

function buildAutoCodeReviewPrompt(originalPrompt: string, traces: ToolTraceEvent[]): string {
  const changedFiles = inferChangedFiles(traces)
  const changedFileLines =
    changedFiles.length > 0 ?
      changedFiles.map((file) => `- ${file}`).join('\n')
    : '- Unknown from trace; inspect recent code paths and write-tool outputs.'
  return [
    'Automatically review the code changes made by the immediately preceding implementation run.',
    '',
    'Rules:',
    '- Read-only review only. Do not edit files, run shell commands, install packages, or spawn subagents.',
    '- Focus on correctness bugs, regressions, unsafe behavior, missing validation, and missing tests.',
    '- Ignore style-only comments unless they hide a real defect.',
    '- Prefer concrete file references when available.',
    '',
    `Original user request:\n${originalPrompt.trim()}`,
    '',
    `Files changed or touched according to trace:\n${changedFileLines}`,
    '',
    'Return only compact JSON with this shape:',
    '{"summary":"one sentence","findings":[{"severity":"suggestion|warning|pass","title":"short finding","detail":"short evidence or file reference"}]}',
  ].join('\n')
}

function inferChangedFiles(traces: ToolTraceEvent[]): string[] {
  const files = new Set<string>()
  for (const event of traces) {
    if (!isWriteToolName(event.toolName)) {
      continue
    }
    const input = plainRecord(event.input)
    const argumentsValue = plainRecord(input.arguments)
    const output = plainRecord(event.output)
    const outputValue = plainRecord(output.output)
    for (const value of [
      argumentsValue.file,
      argumentsValue.path,
      outputValue.file,
      outputValue.path,
    ]) {
      if (typeof value === 'string' && value.trim()) {
        files.add(value.trim())
      }
    }
  }
  return [...files].slice(0, 20)
}

function parseCodeReviewRun(run: MockAgentRun): {
  summary?: string
  findings: ReviewFinding[]
} {
  const text = finalTraceText(run.traces)
  const parsed = parseReviewJson(text)
  if (parsed) {
    return parsed
  }
  const fallback = sanitizeModelMessage(text)
  return {
    summary: fallback,
    findings:
      fallback ?
        [
          {
            severity: 'suggestion',
            title: compactReviewText(fallback, 96),
            detail: compactReviewText(fallback, 180),
          },
        ]
      : [],
  }
}

function finalTraceText(traces: ToolTraceEvent[]): string {
  return (
    traces.find((event) => event.type === 'final_response')?.outputSummary ??
    traces.find((event) => event.type === 'model_message')?.outputSummary ??
    traces.find((event) => event.status === 'warning')?.outputSummary ??
    traces.find((event) => event.status === 'failed')?.outputSummary ??
    ''
  )
}

function parseReviewJson(text: string): { summary?: string; findings: ReviewFinding[] } | null {
  const jsonText = extractJsonObject(text)
  if (!jsonText) {
    return null
  }
  try {
    const parsed = plainRecord(JSON.parse(jsonText))
    const findingsValue = parsed.findings
    const findings =
      Array.isArray(findingsValue) ?
        findingsValue
          .map(normalizeReviewFinding)
          .filter((finding): finding is ReviewFinding => finding !== null)
      : []
    return {
      summary: typeof parsed.summary === 'string' ? parsed.summary.trim() : undefined,
      findings,
    }
  } catch {
    return null
  }
}

function extractJsonObject(text: string): string | null {
  const trimmed = sanitizeModelMessage(text)
  if (!trimmed) {
    return null
  }
  const fenced = trimmed.match(/```(?:json)?\s*([\s\S]*?)```/i)
  if (fenced?.[1]) {
    return fenced[1].trim()
  }
  const start = trimmed.indexOf('{')
  const end = trimmed.lastIndexOf('}')
  if (start < 0 || end <= start) {
    return null
  }
  return trimmed.slice(start, end + 1)
}

function normalizeReviewFinding(value: unknown): ReviewFinding | null {
  const record = plainRecord(value)
  const title = typeof record.title === 'string' ? record.title.trim() : ''
  if (!title) {
    return null
  }
  return {
    severity: normalizeReviewSeverity(record.severity),
    title,
    detail: typeof record.detail === 'string' && record.detail.trim() ?
      record.detail.trim()
    : undefined,
  }
}

function normalizeReviewSeverity(value: unknown): ReviewFindingSeverity {
  const severity = typeof value === 'string' ? value.trim().toLowerCase() : ''
  if (severity === 'warning' || severity === 'warn' || severity === 'issue') {
    return 'warning'
  }
  if (severity === 'pass' || severity === 'ok') {
    return 'pass'
  }
  return 'suggestion'
}

function compactReviewText(value: string, maxLength: number): string {
  const normalized = value.replace(/\s+/g, ' ').trim()
  return normalized.length > maxLength ?
      `${normalized.slice(0, Math.max(0, maxLength - 3))}...`
    : normalized
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
    ? ` (hit ${formatNumber(usage.inputCachedTokens)} / miss ${formatNumber(usage.inputUncachedTokens)})`
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
  if (traces.some((event) => event.type === 'final_response' && event.status === 'success')) {
    return false
  }
  return traces.some((event) => event.status === 'failed')
}

function hasTaskFailedTrace(traces: ToolTraceEvent[]): boolean {
  return hasFailedTrace(traces.filter((event) => event.toolName !== 'open_code_link'))
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
