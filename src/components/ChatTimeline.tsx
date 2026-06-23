import { useCallback, useEffect, useLayoutEffect, useRef } from 'react'
import ChatMessage from './ChatMessage'
import type { AgentTask, ChatMessage as ChatMessageModel } from '../types/task'
import type { ToolTraceEvent } from '../types/trace'

const BOTTOM_PIN_THRESHOLD_PX = 48

interface ChatTimelineProps {
  task: AgentTask | null
  projectId: string
  loading?: boolean
  onCodeLinkResult: (message: string) => void
  onCodeLinkError: (message: string) => void
  onTraceChanged: (taskId: string) => void
  onOpenTrace: (message: ChatMessageModel) => void
  onEditUserMessage: (message: ChatMessageModel) => void
  onSuggestionSelect: (prompt: string) => void
}

function ChatTimeline({
  task,
  projectId,
  loading = false,
  onCodeLinkResult,
  onCodeLinkError,
  onTraceChanged,
  onOpenTrace,
  onEditUserMessage,
  onSuggestionSelect,
}: ChatTimelineProps) {
  const timelineRef = useRef<HTMLDivElement>(null)
  const pinnedToBottomRef = useRef(true)
  const lastTaskIdRef = useRef<string | null>(null)
  const scrollFrameRef = useRef<number | null>(null)
  const timelineActivityKey = task ? createTimelineActivityKey(task) : ''

  const scrollToBottom = useCallback(() => {
    const timeline = timelineRef.current
    if (!timeline) {
      return
    }
    timeline.scrollTop = timeline.scrollHeight
    pinnedToBottomRef.current = true
  }, [])

  const scheduleScrollToBottom = useCallback(() => {
    if (scrollFrameRef.current !== null) {
      return
    }

    scrollFrameRef.current = window.requestAnimationFrame(() => {
      scrollFrameRef.current = null
      if (pinnedToBottomRef.current) {
        scrollToBottom()
      }
    })
  }, [scrollToBottom])

  const handleTimelineScroll = useCallback(() => {
    const timeline = timelineRef.current
    if (!timeline) {
      return
    }
    pinnedToBottomRef.current = isPinnedToBottom(timeline)
  }, [])

  useLayoutEffect(() => {
    if (!task) {
      return
    }

    const taskChanged = lastTaskIdRef.current !== task.id
    lastTaskIdRef.current = task.id
    if (taskChanged) {
      pinnedToBottomRef.current = true
    }

    if (pinnedToBottomRef.current) {
      scheduleScrollToBottom()
    }
  }, [task, timelineActivityKey, scheduleScrollToBottom])

  useEffect(() => {
    const timeline = timelineRef.current
    if (!timeline || !task) {
      return undefined
    }

    const observer = new MutationObserver(() => {
      if (pinnedToBottomRef.current) {
        scheduleScrollToBottom()
      }
    })
    observer.observe(timeline, {
      childList: true,
      subtree: true,
      characterData: true,
    })

    return () => {
      observer.disconnect()
    }
  }, [task?.id, scheduleScrollToBottom])

  useEffect(() => {
    return () => {
      if (scrollFrameRef.current !== null) {
        window.cancelAnimationFrame(scrollFrameRef.current)
      }
    }
  }, [])

  if (!task || loading) {
    return (
      <div className="chat-empty">
        <div className="chat-empty-content">
          <h3>{loading ? 'Loading conversation...' : 'What do you want SnowAgent to change?'}</h3>
          {!loading ? (
            <>
              <p>
                Ask it to inspect code, explain files, open links in Visual Studio,
                or prepare edits.
              </p>
              <div className="suggestion-chips" aria-label="Suggested prompts">
                {suggestions.map((suggestion) => (
                  <button
                    key={suggestion}
                    type="button"
                    className="suggestion-chip"
                    onClick={() => onSuggestionSelect(suggestion)}
                  >
                    {suggestion}
                  </button>
                ))}
              </div>
            </>
          ) : null}
        </div>
      </div>
    )
  }

  return (
    <div className="chat-timeline" ref={timelineRef} onScroll={handleTimelineScroll}>
      {task.messages.map((message) => (
        <ChatMessage
          key={message.id}
          message={message}
          projectId={projectId}
          onCodeLinkResult={onCodeLinkResult}
          onCodeLinkError={onCodeLinkError}
          onTraceChanged={onTraceChanged}
          onOpenTrace={onOpenTrace}
          onEditUserMessage={onEditUserMessage}
        />
      ))}
    </div>
  )
}

function isPinnedToBottom(timeline: HTMLDivElement): boolean {
  return (
    timeline.scrollHeight - timeline.scrollTop - timeline.clientHeight <=
    BOTTOM_PIN_THRESHOLD_PX
  )
}

function createTimelineActivityKey(task: AgentTask): string {
  const messageKey = task.messages
    .map((message) =>
      [
        message.id,
        message.status ?? '',
        textActivityKey(message.content),
        traceActivityKey(message.traceEvents ?? []),
      ].join(':'),
    )
    .join('|')
  return [
    task.id,
    task.status,
    task.updatedAt ?? '',
    task.traceEvents.length,
    traceActivityKey(task.traceEvents),
    messageKey,
  ].join('::')
}

function traceActivityKey(events: ToolTraceEvent[]): string {
  return events
    .slice(-4)
    .map((event) =>
      [
        event.id,
        event.stepIndex,
        event.status,
        event.endedAt ?? '',
        event.durationMs ?? '',
        textActivityKey(event.outputSummary ?? ''),
      ].join(':'),
    )
    .join('|')
}

function textActivityKey(value: string): string {
  return `${value.length}:${value.slice(-80)}`
}

const suggestions = [
  'Inspect current project',
  'Explain selected file',
  'Find likely compile issues',
]

export default ChatTimeline
