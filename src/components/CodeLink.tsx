import { FileCode } from 'lucide-react'
import { openCodeLink } from '../api/tauriApi'
import { normalizeDisplayPath } from '../utils/path'

interface CodeLinkProps {
  projectId: string
  taskId: string | null
  rawLink: string
  displayText?: string
  resolutionContext?: string[]
  onResult?: (message: string) => void
  onError?: (message: string) => void
  onTraceChanged?: () => void
}

function CodeLink({
  projectId,
  taskId,
  rawLink,
  displayText,
  resolutionContext,
  onResult,
  onError,
  onTraceChanged,
}: CodeLinkProps) {
  const displayLink = displayText ?? compactCodeLinkDisplay(rawLink)
  const normalizedRawLink = normalizeDisplayPath(rawLink)

  const handleClick = async () => {
    try {
      const result = await openCodeLink(projectId, rawLink, taskId, resolutionContext)
      onResult?.(`${result.message}.`)
    } catch (caught) {
      onError?.(caught instanceof Error ? caught.message : String(caught))
    } finally {
      onTraceChanged?.()
    }
  }

  return (
    <button
      type="button"
      className="code-link"
      onClick={handleClick}
      title={`Open ${normalizedRawLink} in Visual Studio`}
    >
      <FileCode size={14} aria-hidden="true" />
      {normalizeDisplayPath(displayLink)}
    </button>
  )
}

function compactCodeLinkDisplay(rawLink: string): string {
  const normalized = normalizeDisplayPath(rawLink).replace(/\\/g, '/')
  const match = normalized.match(/([^/:]+?\.(?:c|cc|cpp|cxx|h|hh|hpp|cs|ts|tsx|rs|ini|uplugin|uproject):\d+(?:-\d+)?(?::\d+)?)$/i)
  return match?.[1] ?? normalized
}

export default CodeLink
