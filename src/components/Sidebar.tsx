import {
  ChevronRight,
  FolderKanban,
  Settings as SettingsIcon,
  UserRound,
} from 'lucide-react'
import { useState } from 'react'
import closedFolderIcon from '../assets/icons/codex-folder-closed.png'
import openFolderIcon from '../assets/icons/codex-folder-open.png'
import newChatIcon from '../assets/icons/codex-new-chat.png'
import type { View } from '../state/appState'
import type { ProjectSession } from '../types/project'
import type { AgentTask } from '../types/task'
import WorkspaceHistoryList from './WorkspaceHistoryList'

interface SidebarProps {
  view: View
  projects: ProjectSession[]
  activeProjectId: string | null
  currentTaskId: string | null
  historyDays: number
  tasksById: Record<string, AgentTask>
  taskIdsByProjectId: Record<string, string[]>
  onNavigate: (view: View) => void
  onOpenProject: (projectId: string) => void
  onOpenHistoryTask: (task: AgentTask) => void
  onNewChat: (projectId: string) => void
  onNotice: (message: string) => void
}

function Sidebar({
  view,
  projects,
  activeProjectId,
  currentTaskId,
  historyDays,
  tasksById,
  taskIdsByProjectId,
  onNavigate,
  onOpenProject,
  onOpenHistoryTask,
  onNewChat,
  onNotice,
}: SidebarProps) {
  const [expandedProjectIds, setExpandedProjectIds] = useState<Set<string>>(() => new Set())

  function toggleProjectHistory(projectId: string) {
    setExpandedProjectIds((current) => {
      const next = new Set(current)
      if (next.has(projectId)) {
        next.delete(projectId)
      } else {
        next.add(projectId)
      }
      return next
    })
  }

  function openProjectAndToggleHistory(project: ProjectSession, hasHistory: boolean) {
    onOpenProject(project.id)
    if (hasHistory) {
      toggleProjectHistory(project.id)
    }
  }

  return (
    <aside className="sidebar">
      <div className="brand">
        <div className="brand-mark">S</div>
        <div>
          <h1>SnowAgent</h1>
          <span>Desktop MVP</span>
        </div>
      </div>

      <nav className="nav">
        <button
          type="button"
          className={view === 'profile' ? 'nav-item active' : 'nav-item'}
          onClick={() => onNavigate('profile')}
        >
          <UserRound size={18} aria-hidden="true" />
          Profile
        </button>

        <button
          type="button"
          className={view === 'projects' ? 'nav-item active' : 'nav-item'}
          onClick={() => onNavigate('projects')}
        >
          <FolderKanban size={18} aria-hidden="true" />
          Projects
        </button>

        <div className="sidebar-project-list" aria-label="Workspace projects">
          {projects.map((project) => {
            const projectTasks = (taskIdsByProjectId[project.id] ?? [])
              .map((taskId) => tasksById[taskId])
              .filter((task): task is AgentTask => Boolean(task))
              .reverse()
            const active = view === 'workspace' && project.id === activeProjectId
            const hasHistory = projectTasks.length > 0
            const expanded = hasHistory && expandedProjectIds.has(project.id)

            return (
              <div className="sidebar-project-group" key={project.id}>
                <div className="sidebar-project-row">
                  <button
                    type="button"
                    className={
                      active ?
                        'sidebar-project-button active'
                      : 'sidebar-project-button'
                    }
                    onClick={() => openProjectAndToggleHistory(project, hasHistory)}
                    title={project.name}
                    aria-expanded={hasHistory ? expanded : undefined}
                  >
                    <img
                      className={
                        expanded ?
                          'sidebar-folder-icon open'
                        : 'sidebar-folder-icon'
                      }
                      src={expanded ? openFolderIcon : closedFolderIcon}
                      alt=""
                      draggable={false}
                    />
                    <span>{project.name}</span>
                  </button>
                  <button
                    type="button"
                    className="sidebar-project-toggle"
                    onClick={() => openProjectAndToggleHistory(project, hasHistory)}
                    title={`${expanded ? 'Hide' : 'Show'} chats in ${project.name}`}
                    aria-label={`${expanded ? 'Hide' : 'Show'} chats in ${project.name}`}
                    aria-expanded={expanded}
                    disabled={!hasHistory}
                  >
                    <ChevronRight
                      className={expanded ? 'expanded' : undefined}
                      size={14}
                      aria-hidden="true"
                    />
                  </button>
                  <button
                    type="button"
                    className="sidebar-new-chat-button"
                    onClick={() => onNewChat(project.id)}
                    title={`Start new chat in ${project.name}`}
                    aria-label={`Start new chat in ${project.name}`}
                  >
                    <img
                      className="sidebar-new-chat-icon"
                      src={newChatIcon}
                      alt=""
                      draggable={false}
                    />
                  </button>
                </div>
                {expanded ? (
                  <WorkspaceHistoryList
                    key={`${project.id}:${historyDays}`}
                    tasks={projectTasks}
                    currentTaskId={currentTaskId}
                    historyDays={historyDays}
                    showHeader={false}
                    onSelectTask={onOpenHistoryTask}
                    onNotice={onNotice}
                  />
                ) : null}
              </div>
            )
          })}
        </div>
      </nav>
      <div className="sidebar-bottom-nav">
        <button
          type="button"
          className={view === 'settings' ? 'nav-item active' : 'nav-item'}
          onClick={() => onNavigate('settings')}
        >
          <SettingsIcon size={18} aria-hidden="true" />
          Settings
        </button>
      </div>
    </aside>
  )
}

export default Sidebar
