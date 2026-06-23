import { MoreHorizontal } from 'lucide-react'
import { useEffect, useRef, useState } from 'react'
import type { AgentTask } from '../types/task'

interface WorkspaceHistoryListProps {
  tasks: AgentTask[]
  currentTaskId: string | null
  historyDays: number
  showHeader?: boolean
  onSelectTask: (task: AgentTask) => void
  onNotice?: (message: string) => void
}

function WorkspaceHistoryList({
  tasks,
  currentTaskId,
  historyDays,
  showHeader = true,
  onSelectTask,
  onNotice,
}: WorkspaceHistoryListProps) {
  const [showFullHistory, setShowFullHistory] = useState(false)
  const [sessionMenu, setSessionMenu] = useState<SessionMenuState | null>(null)
  const sessionMenuRef = useRef<HTMLDivElement>(null)
  const recentTasks = tasks.filter((task) => isWithinRecentDays(task, historyDays))
  const visibleTasks = showFullHistory ? tasks : recentTasks
  const hiddenCount = Math.max(0, tasks.length - visibleTasks.length)

  useEffect(() => {
    if (!sessionMenu) {
      return undefined
    }

    const closeOnOutsidePointer = (event: PointerEvent) => {
      const target = event.target
      if (target instanceof Node && sessionMenuRef.current?.contains(target)) {
        return
      }
      setSessionMenu(null)
    }
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        setSessionMenu(null)
      }
    }

    window.addEventListener('pointerdown', closeOnOutsidePointer)
    window.addEventListener('keydown', closeOnEscape)
    window.addEventListener('resize', closeSessionMenu)
    window.addEventListener('scroll', closeSessionMenu, true)

    return () => {
      window.removeEventListener('pointerdown', closeOnOutsidePointer)
      window.removeEventListener('keydown', closeOnEscape)
      window.removeEventListener('resize', closeSessionMenu)
      window.removeEventListener('scroll', closeSessionMenu, true)
    }
  }, [sessionMenu])

  function closeSessionMenu() {
    setSessionMenu(null)
  }

  function openSessionMenu(
    task: AgentTask,
    position: { clientX: number; clientY: number },
  ) {
    const menuWidth = 176
    const menuHeight = 44
    setSessionMenu({
      taskId: task.id,
      left: clamp(position.clientX, 8, window.innerWidth - menuWidth - 8),
      top: clamp(position.clientY, 8, window.innerHeight - menuHeight - 8),
      copied: false,
    })
  }

  async function copySessionId(taskId: string) {
    try {
      await navigator.clipboard.writeText(taskId)
      onNotice?.('Session ID copied.')
      setSessionMenu((current) =>
        current?.taskId === taskId ? { ...current, copied: true } : current,
      )
      window.setTimeout(() => {
        setSessionMenu((current) => (current?.taskId === taskId ? null : current))
      }, 700)
    } catch {
      onNotice?.('Clipboard is unavailable.')
    }
  }

  if (tasks.length === 0) {
    return null
  }

  return (
    <aside className="workspace-history" aria-label="Workspace history">
      {showHeader ? (
        <div className="workspace-history-header">
          <span>History</span>
          <small>{showFullHistory ? 'All' : `Last ${historyDays}d`}</small>
        </div>
      ) : null}
      <div className="workspace-history-list">
        {visibleTasks.length === 0 ? (
          <div className="workspace-history-empty">No recent history.</div>
        ) : null}
        {visibleTasks.map((task) => {
          const title = formatHistoryTitle(task.prompt)
          return (
            <div
              className={
                task.id === currentTaskId ?
                  'workspace-history-item active'
                : 'workspace-history-item'
              }
              key={task.id}
              onContextMenu={(event) => {
                event.preventDefault()
                openSessionMenu(task, event)
              }}
              title={`${title} (${task.status})`}
            >
              <button
                type="button"
                className="workspace-history-main"
                onClick={() => onSelectTask(task)}
              >
                <span className="workspace-history-title">{title}</span>
              </button>
              <span className="workspace-history-time">{formatHistoryTime(task)}</span>
              <button
                type="button"
                className="workspace-history-menu-button"
                onClick={(event) => {
                  const rect = event.currentTarget.getBoundingClientRect()
                  openSessionMenu(task, {
                    clientX: rect.left,
                    clientY: rect.bottom + 4,
                  })
                }}
                aria-label={`Open menu for ${title}`}
                title="More"
              >
                <MoreHorizontal size={15} aria-hidden="true" />
              </button>
            </div>
          )
        })}
        {sessionMenu ? (
          <div
            className="workspace-session-menu"
            ref={sessionMenuRef}
            role="menu"
            style={{ left: sessionMenu.left, top: sessionMenu.top }}
          >
            <button
              type="button"
              className="workspace-session-menu-item"
              role="menuitem"
              onClick={() => void copySessionId(sessionMenu.taskId)}
            >
              {sessionMenu.copied ? 'Copied' : 'Copy session ID'}
            </button>
          </div>
        ) : null}
        {!showFullHistory && hiddenCount > 0 ? (
          <button
            type="button"
            className="workspace-history-show-more"
            onClick={() => setShowFullHistory(true)}
          >
            Show more
          </button>
        ) : null}
      </div>
    </aside>
  )
}

interface SessionMenuState {
  taskId: string
  left: number
  top: number
  copied: boolean
}

function formatHistoryTitle(prompt: string): string {
  const title = prompt.split(/\r?\n/).find((line) => line.trim().length > 0)?.trim()
  return title || 'Untitled task'
}

function formatHistoryTime(task: AgentTask): string {
  const createdAt = task.updatedAt ?? task.createdAt ?? task.messages[0]?.createdAt
  if (!createdAt) {
    return ''
  }
  const elapsedMs = Date.now() - new Date(createdAt).getTime()
  if (!Number.isFinite(elapsedMs) || elapsedMs < 0) {
    return 'now'
  }
  const minute = 60 * 1000
  const hour = 60 * minute
  const day = 24 * hour
  if (elapsedMs < minute) {
    return 'now'
  }
  if (elapsedMs < hour) {
    return `${Math.floor(elapsedMs / minute)}m`
  }
  if (elapsedMs < day) {
    return `${Math.floor(elapsedMs / hour)}h`
  }
  return `${Math.floor(elapsedMs / day)}d`
}

function isWithinRecentDays(task: AgentTask, days: number): boolean {
  const createdAt = task.updatedAt ?? task.createdAt ?? task.messages[0]?.createdAt
  if (!createdAt) {
    return false
  }
  const createdTime = new Date(createdAt).getTime()
  if (!Number.isFinite(createdTime)) {
    return false
  }
  return Date.now() - createdTime <= normalizeHistoryDays(days) * 24 * 60 * 60 * 1000
}

function normalizeHistoryDays(value: number): number {
  if (!Number.isFinite(value)) {
    return 7
  }
  return Math.min(365, Math.max(1, Math.round(value)))
}

function clamp(value: number, min: number, max: number): number {
  const boundedMax = Math.max(min, max)
  return Math.min(boundedMax, Math.max(min, value))
}

export default WorkspaceHistoryList
