import { useEffect, useMemo, useState, type ReactNode } from 'react'
import {
  Bot,
  Check,
  ChevronDown,
  ChevronRight,
  CircleAlert,
  Copy,
  Eye,
  ListTree,
  PanelRightOpen,
  Pencil,
  Search,
  ThumbsDown,
  ThumbsUp,
  UserRound,
  X,
} from 'lucide-react'
import CodeLink from './CodeLink'
import { containsCodeLink, extractCodeLinksFromText, renderTextWithCodeLinks } from './codeLinkText'
import { sanitizeModelMessage } from './traceViewModel'
import type { ChatMessage as ChatMessageModel } from '../types/task'
import type { ToolTraceEvent } from '../types/trace'

interface ChatMessageProps {
  message: ChatMessageModel
  projectId: string
  onCodeLinkResult: (message: string) => void
  onCodeLinkError: (message: string) => void
  onTraceChanged: (taskId: string) => void
  onOpenTrace: (message: ChatMessageModel) => void
  onEditUserMessage: (message: ChatMessageModel) => void
}

function ChatMessage({
  message,
  ...props
}: ChatMessageProps) {
  if (isContextCompactionMessage(message)) {
    return (
      <div className="context-compaction-divider" role="separator">
        <span>Context compacted</span>
      </div>
    )
  }

  return <ConversationChatMessage message={message} {...props} />
}

function ConversationChatMessage({
  message,
  projectId,
  onCodeLinkResult,
  onCodeLinkError,
  onTraceChanged,
  onOpenTrace,
  onEditUserMessage,
}: ChatMessageProps) {
  const isUser = message.role === 'user'
  const displayContent = isUser ? message.content : sanitizeModelMessage(message.content)
  const isRunningAssistant =
    !isUser && message.status === 'running' && !hasTerminalTraceEvent(message.traceEvents ?? [])
  const hideAssistantActions = !isUser && message.status === 'running'
  const thinkingNowMs = useThinkingClock(isRunningAssistant)
  const animateThinkingPrefix =
    isRunningAssistant && displayContent.startsWith(THINKING_PREFIX)
  const [copiedTarget, setCopiedTarget] = useState<'user' | 'assistant' | null>(null)
  const [activeToolTrace, setActiveToolTrace] = useState<ToolTraceEvent | null>(null)
  const thinkingSummary = useMemo(
    () =>
      createThinkingSummary(message.traceEvents ?? [], {
        nowMs: thinkingNowMs,
        running: isRunningAssistant,
      }),
    [isRunningAssistant, message.traceEvents, thinkingNowMs],
  )
  const copyText = (value: string, target: 'user' | 'assistant') => {
    if (!navigator.clipboard) {
      return
    }

    void navigator.clipboard
      .writeText(value)
      .then(() => {
        setCopiedTarget(target)
        window.setTimeout(() => {
          setCopiedTarget((current) => (current === target ? null : current))
        }, 1200)
      })
      .catch(() => undefined)
  }

  const messageClassName = [
    'chat-message',
    isUser ? 'user' : 'assistant',
    message.status === 'failed' ? 'failed' : '',
  ]
    .filter(Boolean)
    .join(' ')

  return (
    <>
    <article className={messageClassName}>
      <div className="message-avatar">
        {isUser ? (
          <UserRound size={16} aria-hidden="true" />
        ) : (
          <Bot size={16} aria-hidden="true" />
        )}
      </div>
      <div className="message-body">
        {!isUser ? (
          <div className="message-meta">
            <span>SnowAgent</span>
            <time>{formatTime(message.createdAt)}</time>
          </div>
        ) : null}
        {!isUser && thinkingSummary && !isRunningAssistant ? (
          <ThinkingPanel
            summary={thinkingSummary}
            defaultOpen={displayContent.startsWith(THINKING_PREFIX)}
            onToolTraceOpen={setActiveToolTrace}
          />
        ) : null}
        {message.attachments && message.attachments.length > 0 ? (
          <div className="message-attachments" aria-label="Message images">
            {message.attachments.map((attachment) => (
              <img
                key={attachment.id}
                src={attachment.dataUrl}
                alt={attachment.name}
                className="message-attachment-image"
              />
            ))}
          </div>
        ) : null}
        {!animateThinkingPrefix && (displayContent.trim().length > 0 || !message.attachments?.length) ? (
          <div className="message-content">
            <MarkdownMessage
              text={displayContent}
              projectId={projectId}
              taskId={message.taskId}
              onCodeLinkResult={onCodeLinkResult}
              onCodeLinkError={onCodeLinkError}
              onTraceChanged={() => onTraceChanged(message.taskId)}
            />
          </div>
        ) : null}
        {isUser ? (
          <div className="user-message-actions" aria-label="User message actions">
            <button
              type="button"
              className={
                copiedTarget === 'user' ? 'message-action-button is-copied' : 'message-action-button'
              }
              aria-label="Copy message"
              title={copiedTarget === 'user' ? 'Copied' : 'Copy'}
              onClick={() => copyText(message.content, 'user')}
            >
              {copiedTarget === 'user' ? (
                <Check size={15} aria-hidden="true" />
              ) : (
                <Copy size={15} aria-hidden="true" />
              )}
            </button>
            <button
              type="button"
              className="message-action-button"
              aria-label="Edit message"
              title="Edit"
              onClick={() => onEditUserMessage(message)}
            >
              <Pencil size={15} aria-hidden="true" />
            </button>
          </div>
        ) : null}
        {message.codeLinks && message.codeLinks.length > 0 ? (
          <div className="suggested-edit-card">
            <div>
              <strong>Suggested edit</strong>
              <span>Review the referenced file in Visual Studio.</span>
            </div>
            <div className="code-link-row">
              {message.codeLinks.map((link) => (
                <CodeLink
                  key={link.rawLink}
                  projectId={projectId}
                  taskId={message.taskId}
                  rawLink={link.rawLink}
                  resolutionContext={message.codeLinks?.map((item) => item.rawLink)}
                  onResult={onCodeLinkResult}
                  onError={onCodeLinkError}
                  onTraceChanged={() => onTraceChanged(message.taskId)}
                />
              ))}
            </div>
          </div>
        ) : null}
        {animateThinkingPrefix ? (
          <div className="message-content running-message-content">
            <RunningAssistantContent
              text={displayContent}
              thinkingSummary={thinkingSummary}
              projectId={projectId}
              taskId={message.taskId}
              onCodeLinkResult={onCodeLinkResult}
              onCodeLinkError={onCodeLinkError}
              onTraceChanged={() => onTraceChanged(message.taskId)}
              onToolTraceOpen={setActiveToolTrace}
            />
          </div>
        ) : null}
        {!isUser && !hideAssistantActions ? (
          <div className="message-actions" aria-label="Message actions">
            <button
              type="button"
              className={
                copiedTarget === 'assistant'
                  ? 'message-action-button is-copied'
                  : 'message-action-button'
              }
              aria-label="Copy response"
              title={copiedTarget === 'assistant' ? 'Copied' : 'Copy response'}
              onClick={() => copyText(displayContent, 'assistant')}
            >
              {copiedTarget === 'assistant' ? (
                <Check size={15} aria-hidden="true" />
              ) : (
                <Copy size={15} aria-hidden="true" />
              )}
            </button>
            <button
              type="button"
              className="message-action-button"
              aria-label="Good response"
              title="Good response"
            >
              <ThumbsUp size={15} aria-hidden="true" />
            </button>
            <button
              type="button"
              className="message-action-button"
              aria-label="Bad response"
              title="Bad response"
            >
              <ThumbsDown size={15} aria-hidden="true" />
            </button>
            <button
              type="button"
              className="message-action-button trace-message-button"
              aria-label="Show response trace"
              title="Show response trace"
              onClick={() => onOpenTrace(message)}
            >
              <PanelRightOpen size={15} aria-hidden="true" />
            </button>
          </div>
        ) : null}
      </div>
    </article>
    {activeToolTrace ? (
      <ToolTraceModal event={activeToolTrace} onClose={() => setActiveToolTrace(null)} />
    ) : null}
    </>
  )
}

interface ToolTraceModalProps {
  event: ToolTraceEvent
  onClose: () => void
}

function ToolTraceModal({ event, onClose }: ToolTraceModalProps) {
  useEffect(() => {
    const onKeyDown = (keyboardEvent: KeyboardEvent) => {
      if (keyboardEvent.key === 'Escape') {
        onClose()
      }
    }
    window.addEventListener('keydown', onKeyDown)
    return () => {
      window.removeEventListener('keydown', onKeyDown)
    }
  }, [onClose])

  const input = asRecord(event.input)
  const argumentsValue = asRecord(input.arguments)
  const hasArguments = Object.keys(argumentsValue).length > 0
  const inputText = prettyTraceValue(event.input)
  const outputText = prettyTraceValue(event.output)
  const summary = event.outputSummary?.trim() ?? ''

  return (
    <div
      className="tool-trace-modal-backdrop"
      role="presentation"
      onMouseDown={(mouseEvent) => {
        if (mouseEvent.target === mouseEvent.currentTarget) {
          onClose()
        }
      }}
    >
      <section className="tool-trace-modal" role="dialog" aria-modal="true" aria-label="Tool trace">
        <header className="tool-trace-modal-header">
          <div>
            <span>{event.type === 'tool_call' ? 'Tool call' : 'Tool result'}</span>
            <strong>{event.toolName ?? event.title}</strong>
          </div>
          <button
            type="button"
            className="tool-trace-close"
            aria-label="Close tool trace"
            onClick={onClose}
          >
            <X size={16} aria-hidden="true" />
          </button>
        </header>
        <div className="tool-trace-modal-body">
          <dl className="tool-trace-meta">
            <div>
              <dt>Status</dt>
              <dd>{event.status}</dd>
            </div>
            <div>
              <dt>Duration</dt>
              <dd>{formatDuration(event.durationMs) || '0 ms'}</dd>
            </div>
            <div>
              <dt>Step</dt>
              <dd>{event.stepIndex}</dd>
            </div>
          </dl>
          {summary ? (
            <section className="tool-trace-section">
              <h4>Summary</h4>
              <pre>{maskSensitiveText(summary)}</pre>
            </section>
          ) : null}
          <section className="tool-trace-section">
            <h4>Arguments</h4>
            <pre>
              {hasArguments ? prettyTraceValue(argumentsValue) : inputText || 'No arguments captured.'}
            </pre>
          </section>
          <section className="tool-trace-section">
            <h4>Output</h4>
            <pre>{outputText || 'No output captured yet.'}</pre>
          </section>
        </div>
      </section>
    </div>
  )
}

interface MarkdownMessageProps {
  text: string
  projectId: string
  taskId: string | null
  onCodeLinkResult: (message: string) => void
  onCodeLinkError: (message: string) => void
  onTraceChanged: () => void
}

interface RunningAssistantContentProps extends MarkdownMessageProps {
  thinkingSummary: ThinkingSummary | null
  onToolTraceOpen: (event: ToolTraceEvent) => void
}

interface MarkdownCodeBlockProps {
  code: string
  language: string
}

interface MarkdownCodeLineData {
  code: string
  language: string
}

interface MarkdownTableData {
  headers: string[]
  rows: string[][]
  nextLineIndex: number
}

const THINKING_PREFIX = 'Thinking...\n\n'
const THINKING_RUNNING_TEXT = 'Thinking...'
const CONTEXT_COMPACTION_MESSAGE_PREFIX = '[CodeForge context compacted]'

function isContextCompactionMessage(message: ChatMessageModel): boolean {
  return message.role === 'system' &&
    message.content.trimStart().startsWith(CONTEXT_COMPACTION_MESSAGE_PREFIX)
}

function useThinkingClock(enabled: boolean): number {
  const [nowMs, setNowMs] = useState(() => Date.now())

  useEffect(() => {
    if (!enabled) {
      setNowMs(Date.now())
      return undefined
    }

    const timerId = window.setInterval(() => {
      setNowMs(Date.now())
    }, 1000)

    return () => {
      window.clearInterval(timerId)
    }
  }, [enabled])

  return nowMs
}

function RunningAssistantContent({
  text,
  thinkingSummary,
  projectId,
  taskId,
  onCodeLinkResult,
  onCodeLinkError,
  onTraceChanged,
  onToolTraceOpen,
}: RunningAssistantContentProps) {
  const detail = text.startsWith(THINKING_PREFIX) ? text.slice(THINKING_PREFIX.length) : text

  return (
    <>
      {thinkingSummary && thinkingSummary.items.length > 0 ? (
        <ThinkingPanel
          summary={thinkingSummary}
          defaultOpen
          onToolTraceOpen={onToolTraceOpen}
        />
      ) : detail.trim().length > 0 ? (
        <MarkdownMessage
          text={detail}
          projectId={projectId}
          taskId={taskId}
          onCodeLinkResult={onCodeLinkResult}
          onCodeLinkError={onCodeLinkError}
          onTraceChanged={onTraceChanged}
        />
      ) : null}
      <p className="markdown-paragraph running-thinking-line" aria-label={THINKING_RUNNING_TEXT}>
        {THINKING_RUNNING_TEXT.split('').map((character, index) => (
          <span
            key={`${character}-${index}`}
            className="thinking-character"
            style={{ animationDelay: `${index * 0.075}s` }}
            aria-hidden="true"
          >
            {character}
          </span>
        ))}
      </p>
    </>
  )
}

function MarkdownMessage({
  text,
  projectId,
  taskId,
  onCodeLinkResult,
  onCodeLinkError,
  onTraceChanged,
}: MarkdownMessageProps) {
  const codeLinkContext = extractCodeLinksFromText(text)
  const blocks = renderMarkdownBlocks(
    text,
    projectId,
    taskId,
    codeLinkContext,
    onCodeLinkResult,
    onCodeLinkError,
    onTraceChanged,
  )

  return <>{blocks}</>
}

function renderMarkdownBlocks(
  text: string,
  projectId: string,
  taskId: string | null,
  codeLinkContext: string[],
  onCodeLinkResult: (message: string) => void,
  onCodeLinkError: (message: string) => void,
  onTraceChanged: () => void,
): ReactNode[] {
  const lines = text.replace(/\r\n/g, '\n').split('\n')
  const blocks: ReactNode[] = []
  let paragraph: string[] = []
  let listItems: string[] = []
  let orderedItems: string[] = []
  let codeLines: string[] | null = null
  let codeLanguage = ''

  const renderInline = (value: string, keyPrefix: string) =>
    renderInlineMarkdown(
      value,
      keyPrefix,
      projectId,
      taskId,
      codeLinkContext,
      onCodeLinkResult,
      onCodeLinkError,
      onTraceChanged,
    )

  const flushParagraph = () => {
    if (paragraph.length === 0) {
      return
    }
    const content = paragraph.join(' ')
    blocks.push(
      <p key={`p-${blocks.length}`} className="markdown-paragraph">
        {renderInline(content, `p-${blocks.length}`)}
      </p>,
    )
    paragraph = []
  }

  const flushList = () => {
    if (listItems.length > 0) {
      blocks.push(
        <ul key={`ul-${blocks.length}`} className="markdown-list">
          {listItems.map((item, index) => (
            <li key={`${index}-${item}`}>{renderInline(item, `ul-${blocks.length}-${index}`)}</li>
          ))}
        </ul>,
      )
      listItems = []
    }
    if (orderedItems.length > 0) {
      blocks.push(
        <ol key={`ol-${blocks.length}`} className="markdown-list">
          {orderedItems.map((item, index) => (
            <li key={`${index}-${item}`}>{renderInline(item, `ol-${blocks.length}-${index}`)}</li>
          ))}
        </ol>,
      )
      orderedItems = []
    }
  }

  const flushTextBlocks = () => {
    flushParagraph()
    flushList()
  }

  for (let lineIndex = 0; lineIndex < lines.length; lineIndex += 1) {
    const line = lines[lineIndex]
    const fenceMatch = line.match(/^```([\w#+.-]*)\s*$/)
    if (fenceMatch) {
      if (codeLines) {
        blocks.push(
          <MarkdownCodeBlock
            key={`code-${blocks.length}`}
            code={codeLines.join('\n')}
            language={codeLanguage}
          />,
        )
        codeLines = null
        codeLanguage = ''
      } else {
        flushTextBlocks()
        codeLines = []
        codeLanguage = fenceMatch[1] ?? ''
      }
      continue
    }

    if (codeLines !== null) {
      codeLines.push(line)
      continue
    }

    if (line.trim().length === 0) {
      flushTextBlocks()
      continue
    }

    const codeLine = parseMarkdownCodeLine(line)
    if (codeLine) {
      flushTextBlocks()
      blocks.push(
        <MarkdownCodeLine
          key={`code-line-${blocks.length}`}
          code={codeLine.code}
          language={codeLine.language}
        />,
      )
      continue
    }

    if (isMarkdownHorizontalRule(line)) {
      flushTextBlocks()
      blocks.push(<hr key={`hr-${blocks.length}`} className="markdown-divider" />)
      continue
    }

    const table = parseMarkdownTable(lines, lineIndex)
    if (table) {
      flushTextBlocks()
      blocks.push(
        <MarkdownTable
          key={`table-${blocks.length}`}
          table={table}
          renderInline={(value, keyPrefix) => renderInline(value, keyPrefix)}
        />,
      )
      lineIndex = table.nextLineIndex - 1
      continue
    }

    const headingMatch = line.match(/^(#{1,3})\s+(.+)$/)
    if (headingMatch) {
      flushTextBlocks()
      const level = headingMatch[1].length
      blocks.push(
        renderHeading(level, lineIndex, renderInline(headingMatch[2], `h-${lineIndex}`)),
      )
      continue
    }

    const unorderedMatch = line.match(/^\s*[-*]\s+(.+)$/)
    if (unorderedMatch) {
      flushParagraph()
      orderedItems = []
      listItems.push(unorderedMatch[1])
      continue
    }

    const orderedMatch = line.match(/^\s*\d+[.)]\s+(.+)$/)
    if (orderedMatch) {
      flushParagraph()
      listItems = []
      orderedItems.push(orderedMatch[1])
      continue
    }

    paragraph.push(line.trim())
  }

  if (codeLines !== null) {
    blocks.push(
      <MarkdownCodeBlock
        key={`code-${blocks.length}`}
        code={codeLines.join('\n')}
        language={codeLanguage}
      />,
    )
  }
  flushTextBlocks()

  return blocks.length > 0 ? blocks : [text]
}

function parseMarkdownCodeLine(line: string): MarkdownCodeLineData | null {
  const trimmed = line.trim()
  const languageTokenPrefix = trimmed.match(/^`([A-Za-z][\w#+.-]*)`\s+(.+)$/)
  if (
    languageTokenPrefix &&
    isCodeLanguage(languageTokenPrefix[1]) &&
    looksLikeCodeLine(languageTokenPrefix[2])
  ) {
    return {
      language: normalizeCodeLanguage(languageTokenPrefix[1]),
      code: languageTokenPrefix[2].trim(),
    }
  }

  const singleTick = trimmed.match(/^`([A-Za-z][\w#+.-]*)\s+(.+)`$/)
  if (singleTick && isCodeLanguage(singleTick[1])) {
    return {
      language: normalizeCodeLanguage(singleTick[1]),
      code: singleTick[2].trim(),
    }
  }

  const tripleTick = trimmed.match(/^```([A-Za-z][\w#+.-]*)\s+(.+)```$/)
  if (tripleTick && isCodeLanguage(tripleTick[1])) {
    return {
      language: normalizeCodeLanguage(tripleTick[1]),
      code: tripleTick[2].trim(),
    }
  }

  return null
}

function looksLikeCodeLine(value: string): boolean {
  return /[;{}()[\]=]|->|::|\/\/|#include/.test(value)
}

function isCodeLanguage(value: string): boolean {
  return [
    'c',
    'cc',
    'cpp',
    'cxx',
    'c++',
    'h',
    'hpp',
    'cs',
    'ts',
    'tsx',
    'js',
    'jsx',
    'rs',
    'rust',
    'json',
    'xml',
    'toml',
    'ini',
    'bat',
    'cmd',
    'powershell',
    'ps1',
  ].includes(value.toLowerCase())
}

function normalizeCodeLanguage(value: string): string {
  return value.toLowerCase() === 'c++' ? 'cpp' : value
}

function MarkdownCodeLine({ code, language }: MarkdownCodeLineData) {
  if (shouldSuppressSourceCodeBlock(language)) {
    return null
  }
  return (
    <pre className="markdown-code-block markdown-code-line" data-language={language}>
      <code>{code}</code>
    </pre>
  )
}

function isMarkdownHorizontalRule(line: string): boolean {
  return /^(\s*)(-{3,}|\*{3,}|_{3,})\s*$/.test(line)
}

function parseMarkdownTable(lines: string[], startIndex: number): MarkdownTableData | null {
  if (startIndex + 1 >= lines.length) {
    return null
  }
  const headers = parseMarkdownTableRow(lines[startIndex])
  const separatorCellCount = parseMarkdownTableSeparator(lines[startIndex + 1])
  if (!headers || separatorCellCount !== headers.length) {
    return null
  }

  const rows: string[][] = []
  let nextLineIndex = startIndex + 2
  while (nextLineIndex < lines.length) {
    const row = parseMarkdownTableRow(lines[nextLineIndex])
    if (!row) {
      break
    }
    rows.push(normalizeMarkdownTableRow(row, headers.length))
    nextLineIndex += 1
  }

  return {
    headers,
    rows,
    nextLineIndex,
  }
}

function parseMarkdownTableSeparator(line: string): number | null {
  const cells = parseMarkdownTableRow(line)
  if (!cells) {
    return null
  }
  if (cells.every((cell) => /^:?-{3,}:?$/.test(cell.trim()))) {
    return cells.length
  }
  return null
}

function parseMarkdownTableRow(line: string): string[] | null {
  const trimmed = line.trim()
  if (!trimmed.startsWith('|') || !trimmed.endsWith('|')) {
    return null
  }
  const cells = splitMarkdownTableCells(trimmed)
  if (cells.length < 2) {
    return null
  }
  return cells.map((cell) => cell.trim())
}

function splitMarkdownTableCells(line: string): string[] {
  const cells: string[] = []
  const content = line.slice(1, -1)
  let current = ''
  let inCode = false
  let escaping = false

  for (const character of content) {
    if (escaping) {
      current += character
      escaping = false
      continue
    }
    if (character === '\\') {
      escaping = true
      continue
    }
    if (character === '`') {
      inCode = !inCode
      current += character
      continue
    }
    if (character === '|' && !inCode) {
      cells.push(current)
      current = ''
      continue
    }
    current += character
  }

  if (escaping) {
    current += '\\'
  }
  cells.push(current)
  return cells
}

function normalizeMarkdownTableRow(row: string[], expectedLength: number): string[] {
  if (row.length === expectedLength) {
    return row
  }
  if (row.length > expectedLength) {
    return row.slice(0, expectedLength)
  }
  return [...row, ...Array.from({ length: expectedLength - row.length }, () => '')]
}

function MarkdownTable({
  table,
  renderInline,
}: {
  table: MarkdownTableData
  renderInline: (value: string, keyPrefix: string) => ReactNode[]
}) {
  return (
    <div className="markdown-table-wrap">
      <table className="markdown-table">
        <thead>
          <tr>
            {table.headers.map((header, index) => (
              <th key={`${index}-${header}`}>
                {renderInline(header, `table-h-${index}`)}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {table.rows.map((row, rowIndex) => (
            <tr key={`${rowIndex}-${row.join('|')}`}>
              {row.map((cell, cellIndex) => (
                <td key={`${cellIndex}-${cell}`}>
                  {renderInline(cell, `table-${rowIndex}-${cellIndex}`)}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  )
}

function MarkdownCodeBlock({ code, language }: MarkdownCodeBlockProps) {
  const [open, setOpen] = useState(false)
  const lineCount = code.length === 0 ? 0 : code.split('\n').length
  const label = language ? language.toUpperCase() : 'CODE'

  if (shouldSuppressSourceCodeBlock(language)) {
    return null
  }

  return (
    <section className={open ? 'markdown-code-section open' : 'markdown-code-section'}>
      <button
        type="button"
        className="markdown-code-toggle"
        aria-expanded={open}
        onClick={() => setOpen((current) => !current)}
      >
        {open ? (
          <ChevronDown size={14} aria-hidden="true" />
        ) : (
          <ChevronRight size={14} aria-hidden="true" />
        )}
        <span>{label}</span>
        <small>{lineCount} lines</small>
      </button>
      {open ? (
        <pre className="markdown-code-block">
          <code>{code}</code>
        </pre>
      ) : null}
    </section>
  )
}

function renderHeading(level: number, lineIndex: number, children: ReactNode[]): ReactNode {
  if (level <= 1) {
    return (
      <h3 key={`h-${lineIndex}`} className="markdown-heading">
        {children}
      </h3>
    )
  }
  return (
    <h4 key={`h-${lineIndex}`} className="markdown-heading">
      {children}
    </h4>
  )
}

function renderInlineMarkdown(
  text: string,
  keyPrefix: string,
  projectId: string,
  taskId: string | null,
  codeLinkContext: string[],
  onCodeLinkResult: (message: string) => void,
  onCodeLinkError: (message: string) => void,
  onTraceChanged: () => void,
): ReactNode[] {
  const segments = text.split(/(`[^`]+`|\*\*[^*]+\*\*)/g).filter((segment) => segment.length > 0)
  const nodes: ReactNode[] = []

  segments.forEach((segment, index) => {
    if (segment.startsWith('`') && segment.endsWith('`')) {
      const codeText = segment.slice(1, -1)
      if (containsCodeLink(codeText)) {
        nodes.push(
          ...renderTextWithCodeLinks(
            codeText,
            projectId,
            taskId,
            onCodeLinkResult,
            onCodeLinkError,
            onTraceChanged,
            codeLinkContext,
          ).map((node, nodeIndex) => (
            <span key={`${keyPrefix}-code-link-${index}-${nodeIndex}`}>{node}</span>
          )),
        )
        return
      }
      const inlineCodeClassName = isLongInlineCode(codeText)
        ? 'markdown-inline-code markdown-inline-code-long'
        : 'markdown-inline-code'
      nodes.push(
        <code key={`${keyPrefix}-code-${index}`} className={inlineCodeClassName}>
          {codeText}
        </code>,
      )
      return
    }

    if (segment.startsWith('**') && segment.endsWith('**')) {
      nodes.push(
        <strong key={`${keyPrefix}-strong-${index}`}>
          {renderTextWithCodeLinks(
            segment.slice(2, -2),
            projectId,
            taskId,
            onCodeLinkResult,
            onCodeLinkError,
            onTraceChanged,
            codeLinkContext,
          )}
        </strong>,
      )
      return
    }

    nodes.push(
      ...renderTextWithCodeLinks(
        segment,
        projectId,
        taskId,
        onCodeLinkResult,
        onCodeLinkError,
        onTraceChanged,
        codeLinkContext,
      ).map((node, nodeIndex) => (
        <span key={`${keyPrefix}-${index}-${nodeIndex}`}>{node}</span>
      )),
    )
  })

  return nodes
}

function isLongInlineCode(codeText: string): boolean {
  return codeText.length > 72
}

interface ThinkingSummary {
  toolCalls: number
  llmCalls: number
  steps: number
  workedFor: string
  items: ThinkingItem[]
  omitted: number
}

interface ThinkingItem {
  id: string
  kind: 'search' | 'read' | 'list' | 'tool' | 'model' | 'message' | 'error'
  event: ToolTraceEvent
  text: string
  detail?: string
  details: string[]
  status: ToolTraceEvent['status']
  duration: string
  progressive: boolean
}

const THINKING_REVEAL_DELAY_MS = 120
const THINKING_TEXT_TICK_MS = 40
const THINKING_TEXT_CHARS_PER_SECOND = 120
const THINKING_MAX_VISIBLE_ITEMS = 50

function ThinkingPanel({
  summary,
  defaultOpen,
  onToolTraceOpen,
}: {
  summary: ThinkingSummary
  defaultOpen: boolean
  onToolTraceOpen: (event: ToolTraceEvent) => void
}) {
  const [open, setOpen] = useState(defaultOpen)
  const [visibleItemCount, setVisibleItemCount] = useState(() =>
    defaultOpen ? Math.min(1, summary.items.length) : summary.items.length,
  )
  const visibleItems = summary.items.slice(0, visibleItemCount)
  const latestVisibleItemId = visibleItems.at(-1)?.id ?? null

  useEffect(() => {
    setOpen(defaultOpen)
  }, [defaultOpen])

  useEffect(() => {
    setVisibleItemCount((current) => {
      if (!defaultOpen) {
        return summary.items.length
      }
      if (summary.items.length === 0) {
        return 0
      }
      if (current === 0) {
        return 1
      }
      return Math.min(current, summary.items.length)
    })
  }, [defaultOpen, summary.items.length])

  useEffect(() => {
    if (!defaultOpen || visibleItemCount >= summary.items.length) {
      return undefined
    }

    const timerId = window.setTimeout(() => {
      setVisibleItemCount((current) => Math.min(current + 1, summary.items.length))
    }, THINKING_REVEAL_DELAY_MS)

    return () => {
      window.clearTimeout(timerId)
    }
  }, [defaultOpen, summary.items.length, visibleItemCount])

  return (
    <section className={open ? 'message-thinking open' : 'message-thinking collapsed'}>
      <button
        type="button"
        className="thinking-toggle"
        aria-expanded={open}
        onClick={() => setOpen((current) => !current)}
      >
        <span>
          {summary.workedFor ? `Worked for ${summary.workedFor}` : 'Thinking'}
        </span>
        {open ? (
          <ChevronDown size={14} aria-hidden="true" />
        ) : (
          <ChevronRight size={14} aria-hidden="true" />
        )}
      </button>
      {open ? (
        <div className="thinking-details">
          <div className="thinking-list">
            {visibleItems.map((item) => (
              <ThinkingItemRow
                key={item.id}
                item={item}
                autoOpen={defaultOpen && item.id === latestVisibleItemId}
                onToolTraceOpen={onToolTraceOpen}
              />
            ))}
            {summary.omitted > 0 ? (
              <div className="thinking-more">
                {summary.omitted} more trace steps are available in Trace.
              </div>
            ) : null}
          </div>
        </div>
      ) : null}
    </section>
  )
}

function ThinkingItemRow({
  item,
  autoOpen,
  onToolTraceOpen,
}: {
  item: ThinkingItem
  autoOpen: boolean
  onToolTraceOpen: (event: ToolTraceEvent) => void
}) {
  const displayText = useProgressiveText(item.text, item.progressive && autoOpen)
  const clickable = item.event ? isClickableToolTrace(item.event) : false
  const className = `thinking-item ${item.status} ${item.kind}${clickable ? ' clickable' : ''}`
  const content = (
    <>
      <ThinkingIcon kind={item.kind} />
      <div className="thinking-item-body">
        <div className="thinking-item-main">
          <span>{displayText}</span>
          {item.detail ? <code>{item.detail}</code> : null}
          <small>{item.duration || item.status}</small>
        </div>
      </div>
    </>
  )

  if (clickable && item.event) {
    return (
      <button
        type="button"
        className={className}
        title="View tool trace"
        onClick={() => onToolTraceOpen(item.event as ToolTraceEvent)}
      >
        {content}
      </button>
    )
  }

  return <div className={className}>{content}</div>
}

function useProgressiveText(text: string, enabled: boolean): string {
  const [visibleChars, setVisibleChars] = useState(text.length)

  useEffect(() => {
    setVisibleChars(enabled ? 0 : text.length)
  }, [enabled, text])

  useEffect(() => {
    if (!enabled || visibleChars >= text.length) {
      return undefined
    }

    const charsPerTick = Math.max(
      1,
      Math.round((THINKING_TEXT_CHARS_PER_SECOND * THINKING_TEXT_TICK_MS) / 1000),
    )
    const timerId = window.setTimeout(() => {
      setVisibleChars((current) => Math.min(text.length, current + charsPerTick))
    }, THINKING_TEXT_TICK_MS)

    return () => {
      window.clearTimeout(timerId)
    }
  }, [enabled, text, visibleChars])

  return text.slice(0, visibleChars)
}

function ThinkingIcon({ kind }: { kind: ThinkingItem['kind'] }) {
  if (kind === 'search') {
    return <Search size={13} aria-hidden="true" />
  }
  if (kind === 'read') {
    return <Eye size={13} aria-hidden="true" />
  }
  if (kind === 'list') {
    return <ListTree size={13} aria-hidden="true" />
  }
  if (kind === 'error') {
    return <CircleAlert size={13} aria-hidden="true" />
  }
  return <Bot size={13} aria-hidden="true" />
}

function isClickableToolTrace(event: ToolTraceEvent): boolean {
  return Boolean(
    event.toolName &&
      (event.type === 'tool_call' || event.type === 'tool_result' || event.type === 'error'),
  )
}

function shouldSuppressSourceCodeBlock(language: string): boolean {
  return ['c', 'cc', 'cpp', 'cxx', 'c++', 'h', 'hh', 'hpp'].includes(
    language.trim().toLowerCase(),
  )
}

function createThinkingSummary(
  events: ToolTraceEvent[],
  options: { nowMs: number; running: boolean },
): ThinkingSummary | null {
  if (events.length === 0) {
    return null
  }

  const visibleEvents = events.filter(
    (event, index) =>
      isVisibleThinkingEvent(event) &&
      !isSupersededToolCall(event, events, index) &&
      !isDuplicateThinkingEvent(event, events, index) &&
      !isDuplicateSearchEvent(event, events, index),
  )
  const items = visibleEvents
    .map(createThinkingItem)
    .filter((item): item is ThinkingItem => item !== null)

  if (items.length === 0) {
    return null
  }

  return {
    toolCalls: inferToolCallCount(events),
    llmCalls: inferLlmCallCount(visibleEvents),
    steps: visibleEvents.length,
    workedFor: formatWorkedFor(events, options.nowMs, options.running),
    items: items.slice(0, THINKING_MAX_VISIBLE_ITEMS),
    omitted: Math.max(0, items.length - THINKING_MAX_VISIBLE_ITEMS),
  }
}

function createThinkingItem(event: ToolTraceEvent): ThinkingItem | null {
  const input = asRecord(event.input)
  const argumentsValue = asRecord(input.arguments)
  const toolName = event.toolName ?? stringValue(input.toolName)

  if (event.type === 'llm_response' || event.type === 'model_message') {
    const thinkingText = modelThinkingContent(input, asRecord(event.output), event)
    if (!thinkingText) {
      return null
    }
    return {
      id: event.id,
      kind: 'model',
      event,
      text: thinkingText,
      detail: modelLabel(input, asRecord(event.output)),
      details: thinkingDetailsForEvent(event, toolName, argumentsValue),
      status: event.status,
      duration: formatDuration(event.durationMs),
      progressive: true,
    }
  }

  if (event.type === 'final_response') {
    return null
  }

  return baseThinkingItem(event, toolName)
}

function isVisibleThinkingEvent(event: ToolTraceEvent): boolean {
  if (event.type === 'user_message') {
    return false
  }
  if (event.toolName === 'open_code_link') {
    return false
  }
  if (event.type === 'llm_response' || event.type === 'model_message') {
    return Boolean(modelThinkingContent(asRecord(event.input), asRecord(event.output), event))
  }
  if (event.type === 'llm_request' || event.type === 'system_event') {
    return false
  }
  if (event.type === 'tool_call' || event.type === 'tool_result' || event.type === 'error') {
    return true
  }
  return false
}

function isDuplicateThinkingEvent(
  event: ToolTraceEvent,
  events: ToolTraceEvent[],
  index: number,
): boolean {
  if (event.type !== 'llm_response' && event.type !== 'model_message') {
    return false
  }
  const text = normalizedThinkingText(event)
  if (!text) {
    return false
  }
  return events.slice(0, index).some((earlier) => {
    if (earlier.type !== 'llm_response' && earlier.type !== 'model_message') {
      return false
    }
    return normalizedThinkingText(earlier) === text
  })
}

function isDuplicateSearchEvent(
  event: ToolTraceEvent,
  events: ToolTraceEvent[],
  index: number,
): boolean {
  const term = normalizedSearchTerm(event)
  if (!term) {
    return false
  }
  const toolName = event.toolName ?? ''
  if (isFileSearchToolName(toolName)) {
    const laterContentSearch = events.slice(index + 1).some((later) => {
      return isContentSearchToolName(later.toolName ?? '') && normalizedSearchTerm(later) === term
    })
    if (laterContentSearch) {
      return true
    }
  }
  return events.slice(0, index).some((earlier) => {
    const earlierTerm = normalizedSearchTerm(earlier)
    if (!earlierTerm || earlierTerm !== term) {
      return false
    }
    if (isContentSearchToolName(toolName)) {
      return isContentSearchToolName(earlier.toolName ?? '')
    }
    return true
  })
}

function normalizedThinkingText(event: ToolTraceEvent): string {
  return modelThinkingContent(asRecord(event.input), asRecord(event.output), event)
    .replace(/\s+/g, ' ')
    .trim()
}

function normalizedSearchTerm(event: ToolTraceEvent): string {
  if (event.type !== 'tool_result' && event.type !== 'error') {
    return ''
  }
  if (!isSearchToolName(event.toolName ?? '')) {
    return ''
  }
  const argumentsValue = asRecord(asRecord(event.input).arguments)
  return firstRawText([argumentsValue.query, argumentsValue.pattern])
    .replace(/\s+/g, ' ')
    .trim()
    .toLowerCase()
}

function isSearchToolName(toolName: string): boolean {
  return isFileSearchToolName(toolName) || isContentSearchToolName(toolName)
}

function isFileSearchToolName(toolName: string): boolean {
  return (
    toolName === 'search_file' ||
    toolName === 'workspace/search_file'
  )
}

function isContentSearchToolName(toolName: string): boolean {
  return (
    toolName === 'search_content' ||
    toolName === 'workspace/search' ||
    toolName === 'workspace/search_content'
  )
}

function isSupersededToolCall(
  event: ToolTraceEvent,
  events: ToolTraceEvent[],
  index: number,
): boolean {
  if (event.type !== 'tool_call') {
    return false
  }

  const toolName = event.toolName ?? stringValue(asRecord(event.input).toolName)
  const argumentsKey = stableJson(asRecord(asRecord(event.input).arguments))
  return events.slice(index + 1).some((later) => {
    if (later.toolName !== toolName || !['tool_result', 'error'].includes(later.type)) {
      return false
    }
    return stableJson(asRecord(asRecord(later.input).arguments)) === argumentsKey
  })
}

function baseThinkingItem(
  event: ToolTraceEvent,
  toolName: string,
): ThinkingItem {
  const duration = formatDuration(event.durationMs)
  const detail = defaultThinkingDetail(event)
  return {
    id: event.id,
    kind: thinkingKind(event, toolName),
    event,
    text: defaultThinkingText(event, toolName),
    detail,
    details: thinkingDetailsForEvent(event, toolName, asRecord(asRecord(event.input).arguments)),
    status: event.status,
    duration,
    progressive: false,
  }
}

function thinkingKind(
  event: ToolTraceEvent,
  toolName: string,
): ThinkingItem['kind'] {
  if (event.type === 'user_message') {
    return 'message'
  }
  if (event.type === 'error' || event.status === 'failed') {
    return 'error'
  }
  if (event.type === 'llm_request' || event.type === 'llm_response') {
    return 'model'
  }
  if (event.type === 'system_event') {
    return 'message'
  }
  if (toolName === 'chat_completion') {
    return 'model'
  }
  if (event.type === 'model_message' || event.type === 'final_response') {
    return 'message'
  }
  if (toolName === 'search_content' || toolName === 'search_file') {
    return 'search'
  }
  if (toolName === 'list_dir') {
    return 'list'
  }
  if (toolName === 'read_file' || toolName === 'get_file_context') {
    return 'read'
  }
  return 'tool'
}

function defaultThinkingText(event: ToolTraceEvent, toolName: string): string {
  if (event.type === 'llm_request') {
    return 'Sending model request'
  }
  if (event.title === 'select_model') {
    return 'Selected model'
  }
  if (toolName) {
    return toolName
  }
  if (event.type === 'error') {
    return event.title || 'error'
  }
  return event.title || event.type
}

function defaultThinkingDetail(event: ToolTraceEvent): string {
  if (event.type === 'tool_call' || event.type === 'tool_result' || event.type === 'error') {
    const argumentsValue = asRecord(asRecord(event.input).arguments)
    return formatToolArguments(argumentsValue)
  }
  const input = asRecord(event.input)
  const output = asRecord(event.output)
  const request = asRecord(input.request)
  const response = asRecord(output.response)
  const model = firstText([
    input.model,
    request.model,
    output.model,
    response.model,
  ])
  return model || event.title
}

function formatToolArguments(argumentsValue: Record<string, unknown>): string {
  if (Object.keys(argumentsValue).length === 0) {
    return ''
  }

  const path = stringValue(argumentsValue.path)
  if (path) {
    const pathDetail = lineRangeDetail(
      path,
      argumentsValue.start_line ?? argumentsValue.line,
      argumentsValue.end_line,
    )
    const rest = Object.fromEntries(
      Object.entries(argumentsValue).filter(
        ([key]) => !['path', 'start_line', 'end_line', 'line'].includes(key),
      ),
    )
    return compactToolDetail([
      pathDetail,
      Object.keys(rest).length > 0 ? compactJson(rest, 180) : '',
    ])
  }

  return compactJson(argumentsValue, 420)
}

function thinkingDetailsForEvent(
  event: ToolTraceEvent,
  toolName: string,
  argumentsValue: Record<string, unknown>,
): string[] {
  const input = asRecord(event.input)
  const output = asRecord(event.output)
  const request = asRecord(input.request)
  const response = asRecord(output.response)
  const lines: string[] = []

  appendDetail(lines, 'Step', event.title)
  appendDetail(lines, 'Status', event.status)
  appendToolPerformanceDetail(lines, event, output)

  if (event.type === 'llm_request') {
    appendDetail(lines, 'Provider', firstText([input.provider, input.providerType, request.provider]))
    appendDetail(lines, 'Model', firstText([input.model, request.model]))
    appendDetail(lines, 'Messages', String(messageArray(input, request).length || ''))
    appendDetail(lines, 'Tools', String(toolArray(input, request).length || ''))
    appendDetail(lines, 'Prompt', messagePreview(messageArray(input, request)))
    return lines
  }

  if (event.type === 'llm_response') {
    appendDetail(lines, 'Model', firstText([output.model, response.model]))
    appendDetail(lines, 'Tokens', tokenUsageDetail(output, response))
    appendDetail(lines, 'Tool calls', String(responseToolCallCount(output, response) || ''))
    return lines
  }

  if (event.type === 'tool_call') {
    appendDetail(lines, 'Tool', toolName)
    appendDetail(lines, 'Arguments', compactJson(argumentsValue, 260))
    return lines
  }

  if (event.type === 'tool_result') {
    if (toolName === 'chat_completion') {
      appendDetail(lines, 'Provider', firstText([input.provider, output.provider]))
      appendDetail(lines, 'Model', firstText([output.model, request.model]))
      appendDetail(lines, 'Tokens', tokenUsageDetail(output, response))
      appendDetail(lines, 'Message chars', firstText([output.messageChars]))
      return lines
    }
    appendDetail(lines, 'Tool', toolName)
    appendDetail(lines, 'Result', compactText(event.outputSummary ?? extractOutputMessage(output), 260))
    appendDetail(lines, 'Output', compactJson(output, 260))
    return lines
  }

  if (event.type === 'model_message' || event.type === 'final_response') {
    appendDetail(lines, 'Content', compactText(modelMessageContent(input, output, event), 320))
    return lines
  }

  appendDetail(lines, 'Input', compactJson(input, 220))
  appendDetail(lines, 'Output', compactJson(output, 220))
  return lines
}

function inferToolCallCount(events: ToolTraceEvent[]): number {
  const calls = events.filter(
    (event) => event.type === 'tool_call' && event.toolName !== 'chat_completion',
  )
  if (calls.length > 0) {
    return calls.length
  }
  return events.filter(
    (event) => event.type === 'tool_result' && event.toolName !== 'chat_completion',
  ).length
}

function appendDetail(lines: string[], label: string, value: string): void {
  const trimmed = value.trim()
  if (trimmed.length > 0) {
    lines.push(`${label}: ${trimmed}`)
  }
}

function appendToolPerformanceDetail(
  lines: string[],
  event: ToolTraceEvent,
  output: Record<string, unknown>,
): void {
  if (event.type !== 'tool_result' && event.type !== 'error') {
    return
  }

  const parts: string[] = []
  if (typeof event.durationMs === 'number') {
    parts.push(`duration=${formatDuration(event.durationMs)}`)
  }
  appendPerformancePart(parts, 'engine', stringValue(output.engine))
  appendPerformancePart(parts, 'scannedFiles', numberString(output.scannedFiles))
  appendPerformancePart(parts, 'count', numberString(output.count))
  appendPerformancePart(parts, 'totalMatches', numberString(output.totalMatches))
  appendPerformancePart(parts, 'maxResults', numberString(output.maxResults))
  appendPerformancePart(parts, 'complete', booleanString(output.complete))
  appendPerformancePart(parts, 'truncated', booleanString(output.truncated))
  appendPerformancePart(parts, 'filesPerSecond', toolThroughput(event.durationMs, output.scannedFiles))

  if (parts.length > 0) {
    appendDetail(lines, 'Performance', parts.join(', '))
  }
}

function appendPerformancePart(parts: string[], label: string, value: string): void {
  if (value) {
    parts.push(`${label}=${value}`)
  }
}

function numberString(value: unknown): string {
  return typeof value === 'number' && Number.isFinite(value) ? String(value) : ''
}

function booleanString(value: unknown): string {
  return typeof value === 'boolean' ? String(value) : ''
}

function toolThroughput(durationMs: number | null, scannedFiles: unknown): string {
  if (
    typeof durationMs !== 'number' ||
    durationMs <= 0 ||
    typeof scannedFiles !== 'number' ||
    scannedFiles <= 0
  ) {
    return ''
  }
  return Math.round((scannedFiles * 1000) / durationMs).toLocaleString()
}

function firstText(values: unknown[]): string {
  for (const value of values) {
    const text = stringValue(value).trim()
    if (text.length > 0) {
      return compactText(text, 160)
    }
  }
  return ''
}

function messageArray(
  input: Record<string, unknown>,
  request: Record<string, unknown>,
): unknown[] {
  if (Array.isArray(input.messages)) {
    return input.messages
  }
  if (Array.isArray(request.messages)) {
    return request.messages
  }
  return []
}

function toolArray(
  input: Record<string, unknown>,
  request: Record<string, unknown>,
): unknown[] {
  if (Array.isArray(input.tools)) {
    return input.tools
  }
  if (Array.isArray(request.tools)) {
    return request.tools
  }
  return []
}

function messagePreview(messages: unknown[]): string {
  const candidate =
    messages
      .map(asRecord)
      .reverse()
      .find((message) => stringValue(message.role) === 'user') ??
    messages
      .map(asRecord)
      .reverse()
      .find((message) => stringValue(message.role) !== 'system')

  if (!candidate) {
    return ''
  }

  const role = stringValue(candidate.role) || 'message'
  const content = messageContentPreview(candidate.content)
  return content ? `${role}: ${content}` : role
}

function messageContentPreview(value: unknown): string {
  if (typeof value === 'string') {
    return compactText(value, 220)
  }
  if (Array.isArray(value)) {
    const textParts = value
      .map(asRecord)
      .map((part) => stringValue(part.text ?? part.content))
      .filter((part) => part.trim().length > 0)
    return compactText(textParts.join(' '), 220)
  }
  return compactJson(value, 220)
}

function firstChoiceRecord(record: Record<string, unknown>): Record<string, unknown> {
  const choices = record.choices
  if (Array.isArray(choices)) {
    return asRecord(choices[0])
  }
  return {}
}

function responseToolCallCount(
  output: Record<string, unknown>,
  response: Record<string, unknown>,
): number {
  const firstChoice = firstNonEmptyRecord([firstChoiceRecord(response), firstChoiceRecord(output)])
  const message = asRecord(firstChoice.message)
  const candidates = [
    output.tool_calls,
    output.toolCalls,
    response.tool_calls,
    response.toolCalls,
    message.tool_calls,
    message.toolCalls,
  ]
  return candidates.reduce<number>((max, candidate) => {
    return Array.isArray(candidate) ? Math.max(max, candidate.length) : max
  }, 0)
}

function tokenUsageDetail(
  output: Record<string, unknown>,
  response: Record<string, unknown>,
): string {
  const usage = firstNonEmptyRecord([
    asRecord(output.usage),
    asRecord(output.tokenUsage),
    asRecord(output.tokens),
    asRecord(response.usage),
    asRecord(response.tokenUsage),
    asRecord(response.tokens),
  ])
  const input = firstText([
    usage.inputTokens,
    usage.input_tokens,
    usage.promptTokens,
    usage.prompt_tokens,
  ])
  const outputTokens = firstText([
    usage.outputTokens,
    usage.output_tokens,
    usage.completionTokens,
    usage.completion_tokens,
  ])
  const total = firstText([usage.totalTokens, usage.total_tokens])
  return [
    input ? `in ${input}` : '',
    outputTokens ? `out ${outputTokens}` : '',
    total ? `total ${total}` : '',
  ]
    .filter((part) => part.length > 0)
    .join(', ')
}

function extractOutputMessage(output: Record<string, unknown>): string {
  return firstText([
    output.message,
    output.error,
    output.text,
    output.content,
    asRecord(output.response).message,
  ])
}

function modelMessageContent(
  input: Record<string, unknown>,
  output: Record<string, unknown>,
  event: ToolTraceEvent,
): string {
  return firstText([
    input.content,
    output.content,
    output.message,
    event.outputSummary,
  ])
}

function modelThinkingContent(
  input: Record<string, unknown>,
  output: Record<string, unknown>,
  event: ToolTraceEvent,
): string {
  const response = asRecord(output.response)
  const choice = firstNonEmptyRecord([firstChoiceRecord(response), firstChoiceRecord(output)])
  const message = asRecord(choice.message)
  const directThinking = firstRawText([
    output.reasoning_content,
    output.reasoningContent,
    output.reasoning,
    response.reasoning_content,
    response.reasoningContent,
    response.reasoning,
    message.reasoning_content,
    message.reasoningContent,
    message.reasoning,
  ])
  if (directThinking) {
    return normalizeThinkingText(directThinking)
  }

  const thinkBlock = extractThinkBlock(
    firstRawText([message.content, output.content, output.message, event.outputSummary]),
  )
  if (thinkBlock) {
    return normalizeThinkingText(thinkBlock)
  }

  if (event.type === 'model_message') {
    return normalizeThinkingText(
      firstRawText([input.content, output.content, output.message, event.outputSummary]),
    )
  }

  return ''
}

function modelLabel(input: Record<string, unknown>, output: Record<string, unknown>): string {
  const request = asRecord(input.request)
  const response = asRecord(output.response)
  return firstText([input.model, request.model, output.model, response.model])
}

function firstRawText(values: unknown[]): string {
  for (const value of values) {
    if (typeof value === 'string' && value.trim().length > 0) {
      return value
    }
  }
  return ''
}

function extractThinkBlock(value: string): string {
  const match = value.match(/<think>([\s\S]*?)<\/think>/i)
  return match?.[1] ?? ''
}

function normalizeThinkingText(value: string): string {
  return maskSensitiveText(value.replace(/\\n/g, '\n').replace(/\r\n/g, '\n').trim())
}

function firstNonEmptyRecord(records: Array<Record<string, unknown>>): Record<string, unknown> {
  return records.find((record) => Object.keys(record).length > 0) ?? {}
}

function compactJson(value: unknown, maxLength: number): string {
  if (value === null || value === undefined) {
    return ''
  }
  try {
    return compactText(JSON.stringify(value), maxLength)
  } catch {
    return compactText(String(value), maxLength)
  }
}

function prettyTraceValue(value: unknown): string {
  if (value === null || value === undefined) {
    return ''
  }
  try {
    return maskSensitiveText(JSON.stringify(value, null, 2))
  } catch {
    return maskSensitiveText(String(value))
  }
}

function stableJson(value: unknown): string {
  try {
    return JSON.stringify(value)
  } catch {
    return String(value)
  }
}

function compactText(value: string, maxLength: number): string {
  const normalized = maskSensitiveText(
    sanitizeModelMessage(value).replace(/\s+/g, ' ').trim(),
  )
  if (normalized.length <= maxLength) {
    return normalized
  }
  return `${normalized.slice(0, Math.max(0, maxLength - 1)).trimEnd()}…`
}

function maskSensitiveText(value: string): string {
  return value
    .replace(/sk-[A-Za-z0-9_-]{10,}/g, 'sk-***')
    .replace(/(api[_-]?key["']?\s*[:=]\s*["']?)[^"',\s}]+/gi, '$1***')
    .replace(/(bearer\s+)[A-Za-z0-9._-]{10,}/gi, '$1***')
}

function formatDuration(value: number | null): string {
  if (value === null || !Number.isFinite(value)) {
    return ''
  }
  return formatElapsedMs(value)
}

function formatElapsedMs(value: number): string {
  if (value >= 1000) {
    const seconds = value / 1000
    if (seconds >= 60) {
      const totalSeconds = Math.round(seconds)
      const minutes = Math.floor(totalSeconds / 60)
      const remainingSeconds = totalSeconds % 60
      return `${minutes}m ${remainingSeconds}s`
    }
    return `${seconds >= 10 ? Math.round(seconds) : seconds.toFixed(1)}s`
  }
  return `${Math.max(0, Math.round(value))} ms`
}

function inferLlmCallCount(events: ToolTraceEvent[]): number {
  const completedCalls = events.filter(
    (event) =>
      event.type === 'llm_response' ||
      (event.type === 'tool_result' && event.toolName === 'chat_completion'),
  ).length

  return completedCalls || events.filter((event) => event.type === 'llm_request').length
}

function formatWorkedFor(
  events: ToolTraceEvent[],
  nowMs: number,
  running: boolean,
): string {
  const starts = events
    .map((event) => Date.parse(event.startedAt))
    .filter((value) => Number.isFinite(value))
  const ends = events
    .map((event) => Date.parse(event.endedAt ?? event.startedAt))
    .filter((value) => Number.isFinite(value))
  if (starts.length === 0 || ends.length === 0) {
    return ''
  }

  const startMs = Math.min(...starts)
  const endMs = running ? Math.max(nowMs, ...ends) : Math.max(...ends)
  const durationSeconds = Math.max(1, Math.round((endMs - startMs) / 1000))
  const minutes = Math.floor(durationSeconds / 60)
  const seconds = durationSeconds % 60
  if (minutes > 0) {
    return `${minutes}m ${seconds}s`
  }
  return `${seconds}s`
}

function hasTerminalTraceEvent(events: ToolTraceEvent[]): boolean {
  if (events.some((event) => event.type === 'final_response')) {
    return true
  }
  return false
}

function compactToolDetail(parts: string[]): string {
  return parts.filter((part) => part.trim().length > 0).join(' ')
}

function lineRangeDetail(path: string, start: unknown, end: unknown): string {
  const startText = stringValue(start)
  const endText = stringValue(end)
  if (startText && endText) {
    return `${path} L${startText}-${endText}`
  }
  if (startText) {
    return `${path} L${startText}`
  }
  return path
}

function asRecord(value: unknown): Record<string, unknown> {
  if (value && typeof value === 'object' && !Array.isArray(value)) {
    return value as Record<string, unknown>
  }
  if (typeof value === 'string' && value.trim().startsWith('{')) {
    try {
      const parsed: unknown = JSON.parse(value)
      if (parsed && typeof parsed === 'object' && !Array.isArray(parsed)) {
        return parsed as Record<string, unknown>
      }
    } catch {
      return {}
    }
  }
  return {}
}

function stringValue(value: unknown): string {
  if (value === null || value === undefined) {
    return ''
  }
  return typeof value === 'string' ? value : String(value)
}

function formatTime(value: string): string {
  const date = new Date(value)
  if (Number.isNaN(date.getTime())) {
    return ''
  }
  return date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
}

export default ChatMessage
