import type { ReactNode } from 'react'
import CodeLink from './CodeLink'

const codeLinkPattern =
  /((?:[A-Za-z]:[\\/](?:[^<>:"|?*\r\n]+[\\/])*[^<>:"|?*\r\n]+\.(?:c|cc|cpp|cxx|h|hh|hpp|cs|ts|tsx|rs|ini|uplugin|uproject))|(?:[^\s<>:"|?*\r\n\\/]+[\\/](?:[^\s<>:"|?*\r\n\\/][^<>:"|?*\r\n\\/]*[\\/])*[^\s<>:"|?*\r\n\\/][^<>:"|?*\r\n\\/]*\.(?:c|cc|cpp|cxx|h|hh|hpp|cs|ts|tsx|rs|ini|uplugin|uproject))):\d+(?::\d+)?/gi
const markdownLinkLikePattern = /\[([^\]\r\n]+)\](?:\(([^)\r\n]+)\))?/g

export function renderTextWithCodeLinks(
  text: string,
  projectId: string,
  taskId: string | null,
  onResult?: (message: string) => void,
  onError?: (message: string) => void,
  onTraceChanged?: () => void,
): ReactNode[] {
  const nodes: ReactNode[] = []
  let lastIndex = 0
  const matches = collectCodeLinkMatches(text)

  for (const match of matches) {
    const { rawLink, start, end } = match
    const index = start
    if (index > lastIndex) {
      nodes.push(text.slice(lastIndex, index))
    }
    nodes.push(
      <CodeLink
        key={`${rawLink}-${index}`}
        projectId={projectId}
        taskId={taskId}
        rawLink={rawLink}
        onResult={onResult}
        onError={onError}
        onTraceChanged={onTraceChanged}
      />,
    )
    lastIndex = end
  }

  if (lastIndex < text.length) {
    nodes.push(text.slice(lastIndex))
  }

  return nodes.length > 0 ? nodes : [text]
}

export function extractCodeLinksFromText(text: string): string[] {
  return collectCodeLinkMatches(text).map((match) => match.rawLink)
}

export function containsCodeLink(text: string): boolean {
  return collectCodeLinkMatches(text).length > 0
}

interface CodeLinkMatch {
  rawLink: string
  start: number
  end: number
}

function collectCodeLinkMatches(text: string): CodeLinkMatch[] {
  const matches: CodeLinkMatch[] = []

  markdownLinkLikePattern.lastIndex = 0
  for (const match of text.matchAll(markdownLinkLikePattern)) {
    const rawLink = firstCodeLinkInText(match[1]) ?? codeLinkFromMarkdownTarget(match[2])
    if (!rawLink) {
      continue
    }
    const start = match.index ?? 0
    matches.push({
      rawLink,
      start,
      end: start + match[0].length,
    })
  }

  codeLinkPattern.lastIndex = 0
  for (const match of text.matchAll(codeLinkPattern)) {
    const rawLink = match[0]
    const start = match.index ?? 0
    const end = start + rawLink.length
    if (matches.some((existing) => rangesOverlap(start, end, existing.start, existing.end))) {
      continue
    }
    matches.push({ rawLink, start, end })
  }

  return matches.sort((left, right) => left.start - right.start)
}

function firstCodeLinkInText(text: string | undefined): string | null {
  if (!text) {
    return null
  }
  codeLinkPattern.lastIndex = 0
  return codeLinkPattern.exec(text)?.[0] ?? null
}

function codeLinkFromMarkdownTarget(target: string | undefined): string | null {
  if (!target) {
    return null
  }

  const direct = firstCodeLinkInText(target)
  if (direct) {
    return direct
  }

  const lineTarget = target.match(
    /^(.+\.(?:c|cc|cpp|cxx|h|hh|hpp|cs|ts|tsx|rs|ini|uplugin|uproject))#L(\d+)(?:C(\d+))?$/i,
  )
  if (!lineTarget) {
    return null
  }

  return lineTarget[3] ?
      `${lineTarget[1]}:${lineTarget[2]}:${lineTarget[3]}`
    : `${lineTarget[1]}:${lineTarget[2]}`
}

function rangesOverlap(
  leftStart: number,
  leftEnd: number,
  rightStart: number,
  rightEnd: number,
): boolean {
  return leftStart < rightEnd && rightStart < leftEnd
}
