import { useState } from 'react'
import type { AgentTask } from '../types/task'

interface WorkspaceHistoryListProps {
  tasks: AgentTask[]
  currentTaskId: string | null
  historyDays: number
  showHeader?: boolean
  onSelectTask: (task: AgentTask) => void
}

function WorkspaceHistoryList({
  tasks,
  currentTaskId,
  historyDays,
  showHeader = true,
  onSelectTask,
}: WorkspaceHistoryListProps) {
  const [showFullHistory, setShowFullHistory] = useState(false)
  const recentTasks = tasks.filter((task) => isWithinRecentDays(task, historyDays))
  const visibleTasks = showFullHistory ? tasks : recentTasks
  const hiddenCount = Math.max(0, tasks.length - visibleTasks.length)

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
        {visibleTasks.map((task) => (
          <button
            type="button"
            className={
              task.id === currentTaskId ?
                'workspace-history-item active'
              : 'workspace-history-item'
            }
            onClick={() => onSelectTask(task)}
            key={task.id}
          >
            <span className="workspace-history-row">
              <span className="workspace-history-title">{formatHistoryTitle(task.prompt)}</span>
              <span className="workspace-history-time">{formatHistoryTime(task)}</span>
            </span>
            <span className="workspace-history-meta">
              <span className={`workspace-history-status ${task.status}`}>{task.status}</span>
            </span>
          </button>
        ))}
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

export default WorkspaceHistoryList
