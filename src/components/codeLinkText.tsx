import type { ReactNode } from 'react'
import CodeLink from './CodeLink'

const codeLinkPattern =
  /((?:[A-Za-z]:[\\/](?:[^<>:"|?*\r\n]+[\\/])*[^<>:"|?*\r\n]+\.(?:c|cc|cpp|cxx|h|hh|hpp|cs|ts|tsx|rs|ini|uplugin|uproject))|(?:[^\s<>:"|?*\r\n\\/]+[\\/](?:[^\s<>:"|?*\r\n\\/][^<>:"|?*\r\n\\/]*[\\/])*[^\s<>:"|?*\r\n\\/][^<>:"|?*\r\n\\/]*\.(?:c|cc|cpp|cxx|h|hh|hpp|cs|ts|tsx|rs|ini|uplugin|uproject))):\d+(?:-\d+)?(?::\d+)?/gi
const bareCodeLinkPattern =
  /(^|[^\w/\\.-])([^\s<>:"|?*`()[\],;\\/]+\.(?:c|cc|cpp|cxx|h|hh|hpp|cs|ts|tsx|rs|ini|uplugin|uproject):\d+(?:-\d+)?(?::\d+)?)(?=$|[^\w/\\.-])/gi
const markdownLinkLikePattern = /\[([^\]\r\n]+)\](?:\(([^)\r\n]+)\))?/g

export function renderTextWithCodeLinks(
  text: string,
  projectId: string,
  taskId: string | null,
  onResult?: (message: string) => void,
  onError?: (message: string) => void,
  onTraceChanged?: () => void,
  resolutionContext?: string[],
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
        displayText={match.displayText}
        resolutionContext={resolutionContext}
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
  displayText?: string
  start: number
  end: number
}

function collectCodeLinkMatches(text: string): CodeLinkMatch[] {
  const matches: CodeLinkMatch[] = []

  markdownLinkLikePattern.lastIndex = 0
  for (const match of text.matchAll(markdownLinkLikePattern)) {
    const targetLink = codeLinkFromMarkdownTarget(match[2])
    const labelLink = firstCodeLinkInText(match[1])
    const rawLink = targetLink ?? labelLink
    if (!rawLink) {
      continue
    }
    const start = match.index ?? 0
    matches.push({
      rawLink,
      displayText: match[1],
      start,
      end: start + match[0].length,
    })
  }

  codeLinkPattern.lastIndex = 0
  for (const match of text.matchAll(codeLinkPattern)) {
    const matchedLink = match[0]
    const rawLink = normalizeCodeLinkTarget(matchedLink)
    const start = match.index ?? 0
    const end = start + matchedLink.length
    if (matches.some((existing) => rangesOverlap(start, end, existing.start, existing.end))) {
      continue
    }
    matches.push({
      rawLink,
      displayText: rawLink === matchedLink ? undefined : matchedLink,
      start,
      end,
    })
  }

  bareCodeLinkPattern.lastIndex = 0
  for (const match of text.matchAll(bareCodeLinkPattern)) {
    const matchedLink = match[2]
    const rawLink = normalizeCodeLinkTarget(matchedLink)
    const start = (match.index ?? 0) + match[1].length
    const end = start + matchedLink.length
    if (matches.some((existing) => rangesOverlap(start, end, existing.start, existing.end))) {
      continue
    }
    matches.push({
      rawLink,
      displayText: rawLink === matchedLink ? undefined : matchedLink,
      start,
      end,
    })
  }

  return matches.sort((left, right) => left.start - right.start)
}

function firstCodeLinkInText(text: string | undefined): string | null {
  if (!text) {
    return null
  }
  codeLinkPattern.lastIndex = 0
  const direct = codeLinkPattern.exec(text)?.[0]
  if (direct) {
    return normalizeCodeLinkTarget(direct)
  }

  bareCodeLinkPattern.lastIndex = 0
  const bare = bareCodeLinkPattern.exec(text)
  return bare?.[2] ? normalizeCodeLinkTarget(bare[2]) : null
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

function normalizeCodeLinkTarget(rawLink: string): string {
  return decodeCodeLinkTarget(rawLink).replace(/:(\d+)-\d+(?=(:\d+)?$)/, ':$1')
}

function decodeCodeLinkTarget(rawLink: string): string {
  try {
    return decodeURI(rawLink)
  } catch {
    return rawLink
  }
}

function rangesOverlap(
  leftStart: number,
  leftEnd: number,
  rightStart: number,
  rightEnd: number,
): boolean {
  return leftStart < rightEnd && rightStart < leftEnd
}
