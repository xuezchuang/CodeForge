import { useEffect, useRef } from 'react'
import ChatMessage from './ChatMessage'
import type { AgentTask, ChatMessage as ChatMessageModel } from '../types/task'

interface ChatTimelineProps {
  task: AgentTask | null
  projectId: string
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
  onCodeLinkResult,
  onCodeLinkError,
  onTraceChanged,
  onOpenTrace,
  onEditUserMessage,
  onSuggestionSelect,
}: ChatTimelineProps) {
  const timelineRef = useRef<HTMLDivElement>(null)
  const traceActivityKey =
    task ?
      `${task.id}:${task.messages.length}:${task.traceEvents.length}:${task.messages
        .map((message) => message.traceEvents?.length ?? 0)
        .join(',')}`
    : ''

  useEffect(() => {
    if (task?.status !== 'running') {
      return
    }
    const timeline = timelineRef.current
    if (!timeline) {
      return
    }
    timeline.scrollTop = timeline.scrollHeight
  }, [task?.status, traceActivityKey])

  if (!task) {
    return (
      <div className="chat-empty">
        <div className="chat-empty-content">
          <h3>What do you want SnowAgent to change?</h3>
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
        </div>
      </div>
    )
  }

  return (
    <div className="chat-timeline" ref={timelineRef}>
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

const suggestions = [
  'Inspect current project',
  'Explain selected file',
  'Find likely compile issues',
]

export default ChatTimeline
