import { MonitorUp, MoreHorizontal, Server } from 'lucide-react'
import { useState } from 'react'
import type { ProjectSession } from '../types/project'
import { normalizeDisplayPath } from '../utils/path'
import { vsStatus, vsStatusClass } from '../utils/projectStatus'

interface WorkspaceHeaderProps {
  project: ProjectSession
  busy: boolean
  divided: boolean
  mcpPanelOpen: boolean
  onOpenVisualStudio: () => void
  onToggleMcpPanel: () => void
  onRefreshBridge: () => void
  onClearWorkspace: () => void
  onNotice: (message: string) => void
}

function WorkspaceHeader({
  project,
  busy,
  divided,
  mcpPanelOpen,
  onOpenVisualStudio,
  onToggleMcpPanel,
  onRefreshBridge,
  onClearWorkspace,
  onNotice,
}: WorkspaceHeaderProps) {
  const [moreOpen, setMoreOpen] = useState(false)
  const [projectInfoOpen, setProjectInfoOpen] = useState(false)
  const statusClass = vsStatusClass(project)
  const statusLabel = vsStatus(project)

  const copyValue = (label: string, value: string) => {
    void navigator.clipboard
      .writeText(value)
      .then(() => onNotice(`${label} copied.`))
      .catch(() => onNotice(`${label} copy is unavailable.`))
  }

  return (
    <header className={divided ? 'workspace-header divided' : 'workspace-header'}>
      <div className="workspace-topbar">
        <div className="workspace-identity" title={project.name}>
          <span className="workspace-project-name">{project.name}</span>
          <span
            className={`workspace-status-dot ${statusClass}`}
            aria-hidden="true"
          />
          <span className="workspace-status-text">{statusLabel}</span>
        </div>

        <div className="workspace-topbar-actions">
          <button
            type="button"
            className={
              mcpPanelOpen ?
                'icon-button topbar-icon-button side-rail-toggle active'
              : 'icon-button topbar-icon-button side-rail-toggle'
            }
            onClick={onToggleMcpPanel}
            title={mcpPanelOpen ? 'Hide MCP servers' : 'Show MCP servers'}
            aria-label={mcpPanelOpen ? 'Hide MCP servers' : 'Show MCP servers'}
            aria-controls="mcp-server-panel"
            aria-expanded={mcpPanelOpen}
          >
            <Server size={16} aria-hidden="true" />
          </button>
          <button
            type="button"
            className="icon-button topbar-icon-button"
            onClick={onOpenVisualStudio}
            disabled={busy || !project.solutionPath}
            title={project.solutionPath ? 'Open Visual Studio' : 'Map a solutionPath first'}
            aria-label="Open Visual Studio"
          >
            <MonitorUp size={16} aria-hidden="true" />
          </button>
          <div className="workspace-more-wrap">
            <button
              type="button"
              className="icon-button topbar-icon-button"
              title="More"
              aria-label="More"
              aria-expanded={moreOpen}
              onClick={() => setMoreOpen((open) => !open)}
            >
              <MoreHorizontal size={17} aria-hidden="true" />
            </button>
            {moreOpen ? (
              <div className="workspace-more-menu" role="menu">
                <button
                  type="button"
                  className="workspace-more-item"
                  role="menuitem"
                  onClick={() => {
                    setProjectInfoOpen(true)
                    setMoreOpen(false)
                  }}
                >
                  Project Info
                </button>
                <button
                  type="button"
                  className="workspace-more-item"
                  role="menuitem"
                  onClick={() => {
                    onRefreshBridge()
                    setMoreOpen(false)
                  }}
                >
                  Refresh Bridge
                </button>
                <button
                  type="button"
                  className="workspace-more-item"
                  role="menuitem"
                  onClick={() => {
                    onClearWorkspace()
                    setMoreOpen(false)
                  }}
                >
                  Clear Workspace
                </button>
              </div>
            ) : null}
          </div>
        </div>
      </div>
      {projectInfoOpen ? (
        <div className="project-info-popover" role="dialog" aria-label="Project Info">
          <div className="project-info-header">
            <div>
              <h3>Project Info</h3>
              <p>{project.name}</p>
            </div>
            <button
              type="button"
              className="ghost-button project-info-close"
              onClick={() => setProjectInfoOpen(false)}
            >
              Close
            </button>
          </div>
          <div className="project-info-list">
            <InfoRow label="Project name" value={project.name} />
            <InfoRow
              label="repoRoot"
              value={normalizeDisplayPath(project.repoRoot)}
              copyLabel="Copy repoRoot"
              onCopy={() => copyValue('repoRoot', project.repoRoot)}
            />
            <InfoRow
              label="solutionPath"
              value={project.solutionPath ? normalizeDisplayPath(project.solutionPath) : 'None'}
              copyLabel={project.solutionPath ? 'Copy solutionPath' : undefined}
              onCopy={
                project.solutionPath ?
                  () => copyValue('solutionPath', project.solutionPath ?? '')
                : undefined
              }
            />
            <InfoRow
              label="vsProcessId"
              value={project.vsProcessId ? String(project.vsProcessId) : 'None'}
            />
            <InfoRow
              label="vsBridgeEndpoint"
              value={project.vsBridgeEndpoint ?? 'None'}
            />
            <InfoRow label="status" value={statusLabel} />
          </div>
        </div>
      ) : null}
    </header>
  )
}

interface InfoRowProps {
  label: string
  value: string
  copyLabel?: string
  onCopy?: () => void
}

function InfoRow({ label, value, copyLabel, onCopy }: InfoRowProps) {
  return (
    <div className="project-info-row">
      <span>{label}</span>
      <code title={value}>{value}</code>
      {copyLabel && onCopy ? (
        <button type="button" className="ghost-button project-copy-button" onClick={onCopy}>
          {copyLabel}
        </button>
      ) : null}
    </div>
  )
}

export default WorkspaceHeader
