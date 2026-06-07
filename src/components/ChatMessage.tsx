import { useMemo, useState, type ReactNode } from 'react'
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
} from 'lucide-react'
import CodeLink from './CodeLink'
import { containsCodeLink, renderTextWithCodeLinks } from './codeLinkText'
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
  projectId,
  onCodeLinkResult,
  onCodeLinkError,
  onTraceChanged,
  onOpenTrace,
  onEditUserMessage,
}: ChatMessageProps) {
  const isUser = message.role === 'user'
  const displayContent = isUser ? message.content : sanitizeModelMessage(message.content)
  const [copiedTarget, setCopiedTarget] = useState<'user' | 'assistant' | null>(null)
  const thinkingSummary = useMemo(
    () => createThinkingSummary(message.traceEvents ?? []),
    [message.traceEvents],
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

  return (
    <article className={isUser ? 'chat-message user' : 'chat-message assistant'}>
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
        {!isUser && thinkingSummary ? (
          <ThinkingPanel
            summary={thinkingSummary}
            defaultOpen={message.status === 'running'}
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
        {displayContent.trim().length > 0 || !message.attachments?.length ? (
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
        {!isUser ? (
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
                  onResult={onCodeLinkResult}
                  onError={onCodeLinkError}
                  onTraceChanged={() => onTraceChanged(message.taskId)}
                />
              ))}
            </div>
          </div>
        ) : null}
      </div>
    </article>
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

interface MarkdownCodeBlockProps {
  code: string
  language: string
}

function MarkdownMessage({
  text,
  projectId,
  taskId,
  onCodeLinkResult,
  onCodeLinkError,
  onTraceChanged,
}: MarkdownMessageProps) {
  const blocks = renderMarkdownBlocks(
    text,
    projectId,
    taskId,
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

  for (const [lineIndex, line] of lines.entries()) {
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

function MarkdownCodeBlock({ code, language }: MarkdownCodeBlockProps) {
  const [open, setOpen] = useState(false)
  const lineCount = code.length === 0 ? 0 : code.split('\n').length
  const label = language ? language.toUpperCase() : 'CODE'

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
          ).map((node, nodeIndex) => (
            <span key={`${keyPrefix}-code-link-${index}-${nodeIndex}`}>{node}</span>
          )),
        )
        return
      }
      nodes.push(
        <code key={`${keyPrefix}-code-${index}`} className="markdown-inline-code">
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
      ).map((node, nodeIndex) => (
        <span key={`${keyPrefix}-${index}-${nodeIndex}`}>{node}</span>
      )),
    )
  })

  return nodes
}

interface ThinkingSummary {
  toolCalls: number
  messages: number
  steps: number
  workedFor: string
  items: ThinkingItem[]
  omitted: number
}

interface ThinkingItem {
  id: string
  kind: 'search' | 'read' | 'list' | 'tool' | 'model' | 'message' | 'error'
  text: string
  detail?: string
  details: string[]
  status: ToolTraceEvent['status']
  duration: string
}

function ThinkingPanel({
  summary,
  defaultOpen,
}: {
  summary: ThinkingSummary
  defaultOpen: boolean
}) {
  const [open, setOpen] = useState(defaultOpen)

  return (
    <section className={open ? 'message-thinking open' : 'message-thinking'}>
      <button
        type="button"
        className="thinking-toggle"
        aria-expanded={open}
        onClick={() => setOpen((current) => !current)}
      >
        {open ? (
          <ChevronDown size={14} aria-hidden="true" />
        ) : (
          <ChevronRight size={14} aria-hidden="true" />
        )}
        <span>
          {summary.workedFor ? `Worked for ${summary.workedFor}` : 'Thinking'}
        </span>
      </button>
      {open ? (
        <div className="thinking-details">
          <div className="thinking-label">
            Thinking
            <span>
              {summary.toolCalls} tool calls, {summary.messages} messages,{' '}
              {summary.steps} trace steps
            </span>
          </div>
          <div className="thinking-list">
            {summary.items.map((item) => (
              <div key={item.id} className={`thinking-item ${item.status}`}>
                <ThinkingIcon kind={item.kind} />
                <div className="thinking-item-body">
                  <div className="thinking-item-main">
                    <span>{item.text}</span>
                    {item.detail ? <code>{item.detail}</code> : null}
                    <small>{item.duration || item.status}</small>
                  </div>
                  {item.details.length > 0 ? (
                    <div className="thinking-item-details">
                      {item.details.map((detail, index) => (
                        <p key={index}>{detail}</p>
                      ))}
                    </div>
                  ) : null}
                </div>
              </div>
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

function createThinkingSummary(events: ToolTraceEvent[]): ThinkingSummary | null {
  if (events.length === 0) {
    return null
  }

  const visibleEvents = events.filter(isVisibleThinkingEvent)
  const items = visibleEvents
    .map(createThinkingItem)
    .filter((item): item is ThinkingItem => item !== null)

  if (items.length === 0) {
    return null
  }

  return {
    toolCalls: inferToolCallCount(events),
    messages: inferMessageCount(events),
    steps: visibleEvents.length,
    workedFor: formatWorkedFor(events),
    items: items.slice(0, 50),
    omitted: Math.max(0, items.length - 50),
  }
}

function createThinkingItem(event: ToolTraceEvent): ThinkingItem | null {
  const input = asRecord(event.input)
  const argumentsValue = asRecord(input.arguments)
  const toolName = event.toolName ?? stringValue(input.toolName)
  const base = baseThinkingItem(event, toolName)

  if (toolName === 'search_content') {
    return {
      ...base,
      id: event.id,
      kind: 'search',
      text: event.status === 'failed' ? 'Search failed' : 'Searched content',
      detail: compactToolDetail([
        stringValue(argumentsValue.query),
        stringValue(argumentsValue.root),
        stringValue(argumentsValue.file_glob),
      ]),
      details: thinkingDetailsForEvent(event, toolName, argumentsValue),
    }
  }

  if (toolName === 'search_file') {
    return {
      ...base,
      id: event.id,
      kind: 'search',
      text: event.status === 'failed' ? 'File search failed' : 'Searched files',
      detail: compactToolDetail([
        stringValue(argumentsValue.pattern),
        stringValue(argumentsValue.root),
      ]),
      details: thinkingDetailsForEvent(event, toolName, argumentsValue),
    }
  }

  if (toolName === 'list_dir') {
    return {
      ...base,
      id: event.id,
      kind: 'list',
      text: event.status === 'failed' ? 'List directory failed' : 'Listed directory',
      detail: stringValue(argumentsValue.path),
      details: thinkingDetailsForEvent(event, toolName, argumentsValue),
    }
  }

  if (toolName === 'read_file') {
    return {
      ...base,
      id: event.id,
      kind: 'read',
      text: event.status === 'failed' ? 'Read failed' : 'Read file',
      detail: lineRangeDetail(
        stringValue(argumentsValue.path),
        argumentsValue.start_line,
        argumentsValue.end_line,
      ),
      details: thinkingDetailsForEvent(event, toolName, argumentsValue),
    }
  }

  if (toolName === 'get_file_context') {
    return {
      ...base,
      id: event.id,
      kind: 'read',
      text: event.status === 'failed' ? 'Context read failed' : 'Read context',
      detail: lineRangeDetail(
        stringValue(argumentsValue.path),
        argumentsValue.line,
        undefined,
      ),
      details: thinkingDetailsForEvent(event, toolName, argumentsValue),
    }
  }

  return base
}

function isVisibleThinkingEvent(event: ToolTraceEvent): boolean {
  if (event.toolName === 'chat_completion') {
    return event.type === 'llm_request' || event.type === 'llm_response'
  }
  return [
    'llm_request',
    'llm_response',
    'tool_call',
    'tool_result',
    'model_message',
    'final_response',
    'system_event',
    'error',
  ].includes(event.type)
}

function baseThinkingItem(
  event: ToolTraceEvent,
  toolName: string,
): ThinkingItem {
  const duration = formatDuration(event.durationMs)
  const detail = defaultThinkingDetail(event, toolName)
  return {
    id: event.id,
    kind: thinkingKind(event, toolName),
    text: defaultThinkingText(event, toolName),
    detail,
    details: thinkingDetailsForEvent(event, toolName, asRecord(asRecord(event.input).arguments)),
    status: event.status,
    duration,
  }
}

function thinkingKind(
  event: ToolTraceEvent,
  toolName: string,
): ThinkingItem['kind'] {
  if (event.type === 'error' || event.status === 'failed') {
    return 'error'
  }
  if (event.type === 'llm_request' || event.type === 'llm_response') {
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
    return 'Sent model request'
  }
  if (event.type === 'llm_response') {
    return event.status === 'failed' ? 'Model response failed' : 'Received model response'
  }
  if (event.type === 'tool_call') {
    return 'Prepared tool call'
  }
  if (event.type === 'tool_result') {
    return event.status === 'failed' ? 'Tool failed' : 'Tool returned result'
  }
  if (event.type === 'model_message') {
    return 'Read model message'
  }
  if (event.type === 'final_response') {
    return 'Composed final response'
  }
  if (event.type === 'error') {
    return 'Trace error'
  }
  return event.title || toolName || 'Trace step'
}

function defaultThinkingDetail(event: ToolTraceEvent, toolName: string): string {
  if (toolName && toolName !== 'chat_completion') {
    return toolName
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
    appendDetail(lines, 'Content', modelResponsePreview(output, response))
    appendDetail(lines, 'Tool calls', String(responseToolCallCount(output, response) || ''))
    return lines
  }

  if (event.type === 'tool_call') {
    appendDetail(lines, 'Tool', toolName)
    appendDetail(lines, 'Arguments', compactJson(argumentsValue, 260))
    return lines
  }

  if (event.type === 'tool_result') {
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

function modelResponsePreview(
  output: Record<string, unknown>,
  response: Record<string, unknown>,
): string {
  const firstChoice = firstNonEmptyRecord([firstChoiceRecord(response), firstChoiceRecord(output)])
  const message = asRecord(firstChoice.message)
  return firstText([
    message.content,
    firstChoice.text,
    response.content,
    output.content,
    output.outputSummary,
  ])
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
  if (value >= 1000) {
    const seconds = value / 1000
    return `${seconds >= 10 ? Math.round(seconds) : seconds.toFixed(1)}s`
  }
  return `${Math.max(0, Math.round(value))} ms`
}

function inferMessageCount(events: ToolTraceEvent[]): number {
  const counts = events
    .map((event) => {
      const input = asRecord(event.input)
      const request = asRecord(input.request)
      const directMessages = Array.isArray(input.messages) ? input.messages.length : 0
      const nestedMessages = Array.isArray(request.messages) ? request.messages.length : 0
      return Math.max(directMessages, nestedMessages)
    })
    .filter((count) => count > 0)

  return counts.length > 0 ? Math.max(...counts) : events.filter((event) => event.type === 'llm_response').length
}

function formatWorkedFor(events: ToolTraceEvent[]): string {
  const starts = events
    .map((event) => Date.parse(event.startedAt))
    .filter((value) => Number.isFinite(value))
  const ends = events
    .map((event) => Date.parse(event.endedAt ?? event.startedAt))
    .filter((value) => Number.isFinite(value))
  if (starts.length === 0 || ends.length === 0) {
    return ''
  }

  const durationSeconds = Math.max(1, Math.round((Math.max(...ends) - Math.min(...starts)) / 1000))
  const minutes = Math.floor(durationSeconds / 60)
  const seconds = durationSeconds % 60
  if (minutes > 0) {
    return `${minutes}m ${seconds}s`
  }
  return `${seconds}s`
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
