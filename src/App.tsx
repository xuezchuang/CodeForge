import { useCallback, useEffect, useState } from 'react'
import './App.css'
import {
  getSettings,
  importWorkspaceHistory,
  listProjects,
  loadWorkspaceHistory,
  saveWorkspaceSelection,
} from './api/tauriApi'
import Profile from './components/Profile'
import ProjectList from './components/ProjectList'
import Settings from './components/Settings'
import Sidebar from './components/Sidebar'
import Toast from './components/Toast'
import type { ToastState } from './components/Toast'
import Workspace from './components/Workspace'
import {
  ensureWorkspaceProject,
  clearLegacyWorkspaceHistoryState,
  hasWorkspaceHistory,
  initialAppState,
  latestTaskIdForProject,
  loadLegacyWorkspaceHistoryState,
  normalizeWorkspaceHistoryState,
  normalizeSettings,
} from './state/appState'
import type { AppState, View } from './state/appState'
import type { AgentTask } from './types/task'

function App() {
  const [view, setView] = useState<View>('projects')
  const [appState, setAppState] = useState<AppState>(initialAppState)
  const [historyLoaded, setHistoryLoaded] = useState(false)
  const [toast, setToast] = useState<ToastState | null>(null)
  const visualStyle = appState.settings?.uiPreferences.visualStyle ?? 'codex'
  const historyDays = appState.settings?.uiPreferences.workspaceHistoryDays ?? 7

  const showToast = useCallback((kind: ToastState['kind'], message: string) => {
    const id = Date.now()
    setToast({ id, kind, message })
    window.setTimeout(() => {
      setToast((current) => (current?.id === id ? null : current))
    }, 3000)
  }, [])

  const refreshProjects = useCallback(async () => {
    const nextProjects = await listProjects()
    setAppState((current) => {
      const activeProjectId =
        current.activeProjectId &&
        nextProjects.some((project) => project.id === current.activeProjectId)
          ? current.activeProjectId
          : null
      const nextState = {
        ...current,
        projects: nextProjects,
        activeProjectId,
      }
      const projectTaskIds =
        activeProjectId ? nextState.taskIdsByProjectId[activeProjectId] ?? [] : []
      const currentTaskIsStillAvailable =
        current.currentWorkspaceTaskId ?
          projectTaskIds.includes(current.currentWorkspaceTaskId)
        : false
      const keepEmptyWorkspace =
        current.activeProjectId === activeProjectId &&
        current.currentWorkspaceTaskId === null
      return {
        ...nextState,
        currentWorkspaceTaskId:
          currentTaskIsStillAvailable ?
            current.currentWorkspaceTaskId
          : keepEmptyWorkspace ?
            null
          : latestTaskIdForProject(nextState, activeProjectId),
      }
    })
  }, [])

  useEffect(() => {
    let cancelled = false

    const load = async () => {
      try {
        const legacyHistory = loadLegacyWorkspaceHistoryState()
        if (hasWorkspaceHistory(legacyHistory)) {
          await importWorkspaceHistory(legacyHistory)
          clearLegacyWorkspaceHistoryState()
        }
        const [nextProjects, rawSettings, rawHistory] = await Promise.all([
          listProjects(),
          getSettings(),
          loadWorkspaceHistory(),
        ])
        if (cancelled) {
          return
        }
        const settings = normalizeSettings(rawSettings)
        const history = normalizeWorkspaceHistoryState(rawHistory)
        setAppState((current) => ({
          ...current,
          ...history,
          projects: nextProjects,
          settings,
          providers: settings.providers,
        }))
        setHistoryLoaded(true)
      } catch (caught) {
        if (!cancelled) {
          showToast('error', toMessage(caught))
        }
      }
    }

    void load()

    return () => {
      cancelled = true
    }
  }, [showToast])

  useEffect(() => {
    if (!historyLoaded) {
      return undefined
    }
    const timeoutId = window.setTimeout(() => {
      void saveWorkspaceSelection(
        appState.activeProjectId,
        appState.currentWorkspaceTaskId,
      ).catch((caught) => {
        showToast('error', toMessage(caught))
      })
    }, 250)
    return () => {
      window.clearTimeout(timeoutId)
    }
  }, [
    appState.activeProjectId,
    appState.currentWorkspaceTaskId,
    historyLoaded,
    showToast,
  ])

  useEffect(() => {
    const intervalId = window.setInterval(() => {
      void refreshProjects().catch((caught) => {
        showToast('error', toMessage(caught))
      })
    }, 3000)

    return () => {
      window.clearInterval(intervalId)
    }
  }, [refreshProjects, showToast])

  const navigate = (nextView: View) => {
    if (nextView === 'workspace') {
      setAppState((current) => ensureWorkspaceProject(current))
    }
    setView(nextView)
  }

  const openWorkspace = (projectId: string) => {
    setAppState((current) => ({
      ...current,
      activeProjectId: projectId,
      currentWorkspaceTaskId: latestTaskIdForProject(current, projectId),
      traceDrawerOpen: false,
    }))
    setView('workspace')
  }

  const openHistoryTask = (task: AgentTask) => {
    setAppState((current) => ({
      ...current,
      activeProjectId: task.projectId,
      currentWorkspaceTaskId: task.id,
      traceDrawerOpen: false,
    }))
    setView('workspace')
  }

  const handleSettingsChanged = (settings: AppState['settings']) => {
    if (!settings) {
      return
    }
    const normalized = normalizeSettings(settings)
    setAppState((current) => ({
      ...current,
      settings: normalized,
      providers: normalized.providers,
    }))
  }

  return (
    <div className={`app-root ${visualStyle === 'codex' ? 'codex-style' : 'classic-style'}`}>
      <div className="app-shell">
        <Sidebar
          view={view}
          projects={appState.projects}
          activeProjectId={appState.activeProjectId}
          currentTaskId={appState.currentWorkspaceTaskId}
          historyDays={historyDays}
          tasksById={appState.tasksById}
          taskIdsByProjectId={appState.taskIdsByProjectId}
          onNavigate={navigate}
          onOpenProject={openWorkspace}
          onOpenHistoryTask={openHistoryTask}
          onNotice={(message) => showToast('notice', message)}
          onNewChat={(projectId) => {
            setAppState((current) => ({
              ...current,
              activeProjectId: projectId,
              currentWorkspaceTaskId: null,
              traceDrawerOpen: false,
            }))
            setView('workspace')
          }}
        />

        <main className="main-panel">
          <Toast toast={toast} onDismiss={() => setToast(null)} />

          <div className={view === 'workspace' ? 'main-scroll workspace-scroll' : 'main-scroll'}>
            {view === 'projects' ? (
              <ProjectList
                projects={appState.projects}
                activeProjectId={appState.activeProjectId}
                onOpenWorkspace={openWorkspace}
                onRefresh={refreshProjects}
                onError={(message) => showToast('error', message)}
                onNotice={(message) => showToast('notice', message)}
              />
            ) : null}

            {view === 'workspace' ? (
              <Workspace
                state={appState}
                setState={setAppState}
                onRefreshProjects={refreshProjects}
                onGlobalNotice={(message) => showToast('notice', message)}
                onGlobalError={(message) => showToast('error', message)}
              />
            ) : null}

            {view === 'profile' ? (
              <Profile tasks={Object.values(appState.tasksById)} />
            ) : null}

            {view === 'settings' ? (
              <Settings
                settings={appState.settings}
                providers={appState.providers}
                onSettingsChanged={handleSettingsChanged}
                onProvidersChanged={(providers) =>
                  setAppState((current) => ({ ...current, providers }))
                }
                onError={(message) => showToast('error', message)}
                onNotice={(message) => showToast('notice', message)}
              />
            ) : null}
          </div>
        </main>
      </div>
    </div>
  )
}

function toMessage(caught: unknown): string {
  if (caught instanceof Error) {
    return caught.message
  }
  return String(caught)
}

export default App
