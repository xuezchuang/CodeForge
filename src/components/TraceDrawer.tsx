import { useEffect, useMemo, useState } from 'react'
import type { MouseEvent as ReactMouseEvent, ReactNode } from 'react'
import {
  AlertTriangle,
  Box,
  Braces,
  CheckCircle2,
  CircleAlert,
  CircleCheck,
  CircleX,
  ChevronRight,
  Clock3,
  Download,
  FileText,
  Maximize2,
  Search,
  X,
  Zap,
} from 'lucide-react'
import type { ToolTraceEvent, TraceStatus } from '../types/trace'
import { listTraces } from '../api/tauriApi'
import { JsonTree } from './TraceEventRow'

interface TraceDrawerProps {
  open: boolean
  taskId: string | null
  traceEvents: ToolTraceEvent[]
  onClose: () => void
}

type TraceTab = 'overview' | 'request' | 'response'
type TraceRawKind = 'request' | 'response'

const traceRawDefaultCollapsedKeys = [
  'tool',
  'tools',
  'tool_call',
  'tool_calls',
  'toolCall',
  'toolCalls',
  'available_tools',
  'availableTools',
]

interface TraceRound {
  id: string
  index: number
  request: ToolTraceEvent | null
  response: ToolTraceEvent | null
  events: ToolTraceEvent[]
  toolCalls: TraceToolCall[]
  toolResults: TraceToolResult[]
  availableTools: TraceToolDefinition[]
  messages: TraceMessage[]
  model: string
  provider: string
  status: TraceStatus
  startedAt: string | null
  durationMs: number | null
  tokenUsage: TraceTokenUsage | null
}

interface TraceToolCall {
  id: string
  index: number
  name: string
  argumentsValue: unknown
}

interface TraceToolResult {
  id: string
  index: number
  name: string
  status: TraceStatus
  startedAt: string
  durationMs: number | null
  argumentsValue: unknown
  outputSummary: string
  rawInput: unknown
  rawOutput: unknown
}

interface TraceToolDefinition {
  id: string
  index: number
  name: string
  description: string
  parameterCount: number
}

interface TraceMessage {
  id: string
  index: number
  role: string
  content: string
}

interface TraceRawDialogState {
  round: TraceRound
  kind: TraceRawKind
}

type TraceToolDetailDialogState =
  | { kind: 'call'; toolCall: TraceToolCall }
  | { kind: 'result'; toolResult: TraceToolResult }

interface TraceChildTraceDialogState {
  taskId: string
  label: string
  events: ToolTraceEvent[]
  loading: boolean
  error: string | null
}

interface TraceSubagentSummary {
  childTaskId: string
  agentName: string
  taskName: string
  status: string
  summary: string
  traceCount: number | null
}

type OpenTraceRawJson = (round: TraceRound, kind: TraceRawKind) => void
type OpenTraceToolDetail = (detail: TraceToolDetailDialogState) => void
type OpenChildTrace = (subagent: TraceSubagentSummary) => void

interface TraceRunSummary {
  totalDurationMs: number | null
  requestCount: number
  successCount: number
  failedCount: number
  warningCount: number
  startMs: number | null
  endMs: number | null
}

interface TraceTokenTotals {
  input: number | null
  output: number | null
  total: number | null
  inputCached: number | null
  inputUncached: number | null
}

interface TraceTokenUsage {
  input: number | null
  output: number | null
  total: number | null
  inputCached: number | null
  inputUncached: number | null
}

function TraceDrawer({ open, taskId, traceEvents, onClose }: TraceDrawerProps) {
  const [selectedRoundId, setSelectedRoundId] = useState<string | null>(null)
  const [activeTab, setActiveTab] = useState<TraceTab>('overview')
  const [selectedToolId, setSelectedToolId] = useState<string | null>(null)
  const [rawDialog, setRawDialog] = useState<TraceRawDialogState | null>(null)
  const [toolDetailDialog, setToolDetailDialog] = useState<TraceToolDetailDialogState | null>(null)
  const [childTraceDialog, setChildTraceDialog] = useState<TraceChildTraceDialogState | null>(null)
  const rounds = useMemo(() => createTraceRounds(traceEvents), [traceEvents])
  const tokenSummary = useMemo(() => createTraceTokenSummary(traceEvents), [traceEvents])
  const runSummary = useMemo(() => createTraceRunSummary(traceEvents, rounds), [rounds, traceEvents])

  useEffect(() => {
    if (!open) {
      return undefined
    }

    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        if (childTraceDialog) {
          setChildTraceDialog(null)
        } else if (toolDetailDialog) {
          setToolDetailDialog(null)
        } else if (rawDialog) {
          setRawDialog(null)
        } else {
          onClose()
        }
      }
    }

    window.addEventListener('keydown', handleKeyDown)
    return () => window.removeEventListener('keydown', handleKeyDown)
  }, [childTraceDialog, onClose, open, rawDialog, toolDetailDialog])

  useEffect(() => {
    if (!open) {
      setRawDialog(null)
      setToolDetailDialog(null)
      setChildTraceDialog(null)
      return
    }

    setSelectedRoundId((current) => {
      if (current && rounds.some((round) => round.id === current)) {
        return current
      }
      return rounds[0]?.id ?? null
    })
    setActiveTab('overview')
  }, [open, rounds])

  const selectedRound = rounds.find((round) => round.id === selectedRoundId) ?? rounds[0] ?? null

  useEffect(() => {
    const firstToolId =
      selectedRound?.toolResults[0]?.id ?? selectedRound?.toolCalls[0]?.id ?? null
    setSelectedToolId(firstToolId)
  }, [selectedRound?.id, selectedRound?.toolCalls, selectedRound?.toolResults])

  if (!open) {
    return null
  }

  const handleBackdropMouseDown = (event: ReactMouseEvent<HTMLDivElement>) => {
    if (event.target === event.currentTarget) {
      onClose()
    }
  }

  const openRawDialog: OpenTraceRawJson = (round, kind) => {
    setRawDialog({ round, kind })
  }

  const openToolDetailDialog: OpenTraceToolDetail = (detail) => {
    setToolDetailDialog(detail)
  }

  const openChildTrace: OpenChildTrace = (subagent) => {
    const label = subagent.taskName || subagent.agentName || subagent.childTaskId
    setChildTraceDialog({
      taskId: subagent.childTaskId,
      label,
      events: [],
      loading: true,
      error: null,
    })
    void listTraces(subagent.childTaskId)
      .then((events) => {
        setChildTraceDialog((current) =>
          current?.taskId === subagent.childTaskId ?
            { ...current, events, loading: false, error: null }
          : current,
        )
      })
      .catch((caught) => {
        const message = caught instanceof Error ? caught.message : String(caught)
        setChildTraceDialog((current) =>
          current?.taskId === subagent.childTaskId ?
            { ...current, loading: false, error: message }
          : current,
        )
      })
  }

  return (
    <div className="trace-modal-backdrop" onMouseDown={handleBackdropMouseDown}>
      <aside
        className="trace-drawer trace-workbench"
        role="dialog"
        aria-modal="true"
        aria-labelledby="trace-drawer-title"
      >
        <header className="trace-drawer-header trace-window-header">
          <div>
            <h3 id="trace-drawer-title">Trace / 调用链追踪</h3>
            <p>{taskId ? `trace id: ${taskId}` : 'No message trace selected'}</p>
          </div>
          <button type="button" className="icon-button" onClick={onClose} aria-label="Close trace">
            <X size={16} aria-hidden="true" />
          </button>
        </header>

        <TraceMetricGrid runSummary={runSummary} tokenSummary={tokenSummary} />

        <section className="trace-chain-shell">
          <TraceRoundSidebar
            rounds={rounds}
            selectedRoundId={selectedRound?.id ?? null}
            runSummary={runSummary}
            onSelectRound={setSelectedRoundId}
          />
          <main className="trace-chain-main">
            <TraceTabBar
              activeTab={activeTab}
              round={selectedRound}
              onSelectTab={setActiveTab}
              onOpenRawJson={openRawDialog}
            />
            <div className="trace-chain-content">
              {!selectedRound ? (
                <div className="trace-detail-empty">No trace round selected.</div>
              ) : (
                <TraceRoundContent
                  round={selectedRound}
                  activeTab={activeTab}
                  selectedToolId={selectedToolId}
                  onSelectTool={setSelectedToolId}
                  onOpenToolDetail={openToolDetailDialog}
                />
              )}
            </div>
          </main>
        </section>

        <TraceWorkbenchFooter
          runSummary={runSummary}
          traceEvents={traceEvents}
          taskId={taskId}
        />
      </aside>
      {rawDialog ? (
        <TraceRawJsonDialog
          round={rawDialog.round}
          kind={rawDialog.kind}
          onChangeKind={(kind) => setRawDialog((current) => current ? { ...current, kind } : current)}
          onClose={() => setRawDialog(null)}
        />
      ) : null}
      {toolDetailDialog ? (
        <TraceToolDetailDialog
          detail={toolDetailDialog}
          onOpenChildTrace={openChildTrace}
          onClose={() => setToolDetailDialog(null)}
        />
      ) : null}
      {childTraceDialog ? (
        <TraceChildTraceDialog
          childTrace={childTraceDialog}
          onClose={() => setChildTraceDialog(null)}
        />
      ) : null}
    </div>
  )
}

function TraceMetricGrid({
  runSummary,
  tokenSummary,
}: {
  runSummary: TraceRunSummary
  tokenSummary: TraceTokenTotals
}) {
  const cachePercent = percentText(tokenSummary.inputCached, tokenSummary.input)
  const uncachedPercent = percentText(tokenSummary.inputUncached, tokenSummary.input)

  return (
    <section className="trace-metric-grid" aria-label="Trace summary">
      <TraceMetricCard
        icon={<Clock3 size={16} aria-hidden="true" />}
        label="总耗时"
        value={formatDurationShort(runSummary.totalDurationMs)}
      />
      <TraceMetricCard
        icon={<Box size={16} aria-hidden="true" />}
        label="总 Tokens"
        value={formatTokenValue(tokenSummary.total)}
      />
      <TraceMetricCard
        icon={<Zap size={16} aria-hidden="true" />}
        label="缓存命中 Tokens"
        value={formatTokenValue(tokenSummary.inputCached)}
        detail={cachePercent}
        tone="success"
      />
      <TraceMetricCard
        icon={<Braces size={16} aria-hidden="true" />}
        label="请求消耗 Tokens"
        value={formatTokenValue(tokenSummary.inputUncached)}
        detail={uncachedPercent}
        tone="info"
      />
      <TraceMetricCard
        icon={<FileText size={16} aria-hidden="true" />}
        label="请求数"
        value={String(runSummary.requestCount)}
      />
      <TraceMetricCard
        icon={<CircleCheck size={16} aria-hidden="true" />}
        label="成功"
        value={String(runSummary.successCount)}
        tone="success"
      />
      <TraceMetricCard
        icon={<CircleX size={16} aria-hidden="true" />}
        label="失败"
        value={String(runSummary.failedCount)}
        tone="danger"
      />
      <TraceMetricCard
        icon={<AlertTriangle size={16} aria-hidden="true" />}
        label="警告"
        value={String(runSummary.warningCount)}
        tone="warning"
      />
    </section>
  )
}

function TraceMetricCard({
  icon,
  label,
  value,
  detail,
  tone = 'neutral',
}: {
  icon: ReactNode
  label: string
  value: string
  detail?: string
  tone?: 'neutral' | 'success' | 'info' | 'danger' | 'warning'
}) {
  return (
    <div className="trace-metric-card" data-tone={tone}>
      <span className="trace-metric-icon">{icon}</span>
      <span className="trace-metric-copy">
        <span>{label}</span>
        <strong>
          {value}
          {detail ? <small>{detail}</small> : null}
        </strong>
      </span>
    </div>
  )
}

function TraceRoundSidebar({
  rounds,
  selectedRoundId,
  runSummary,
  onSelectRound,
}: {
  rounds: TraceRound[]
  selectedRoundId: string | null
  runSummary: TraceRunSummary
  onSelectRound: (roundId: string) => void
}) {
  const [query, setQuery] = useState('')
  const queryText = query.trim().toLowerCase()
  const filteredRounds = queryText
    ? rounds.filter((round) => roundSearchText(round).includes(queryText))
    : rounds

  return (
    <aside className="trace-round-sidebar" aria-label="模型回合">
      <div className="trace-round-header">
        <strong>模型回合</strong>
        <label className="trace-step-search">
          <Search size={15} aria-hidden="true" />
          <input
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            placeholder="搜索模型回合..."
          />
        </label>
      </div>
      <div className="trace-round-list">
        {filteredRounds.map((round) => (
          <button
            type="button"
            className={
              round.id === selectedRoundId ? 'trace-round-row selected' : 'trace-round-row'
            }
            onClick={() => onSelectRound(round.id)}
            key={round.id}
          >
            <StatusGlyph status={round.status} />
            <span>{round.index}</span>
            <strong>{round.model || 'Model'}</strong>
            <small>{formatDurationShort(round.durationMs)}</small>
            <em>工具 {round.toolCalls.length || round.toolResults.length}</em>
          </button>
        ))}
        {filteredRounds.length === 0 ? (
          <div className="trace-step-empty">No matching rounds.</div>
        ) : null}
      </div>
      <div className="trace-round-footer">
        <span>共 {rounds.length} 回合</span>
        <span>
          <CircleCheck size={14} aria-hidden="true" />
          成功 {runSummary.successCount}
        </span>
        <span>
          <CircleX size={14} aria-hidden="true" />
          失败 {runSummary.failedCount}
        </span>
        <span>
          <AlertTriangle size={14} aria-hidden="true" />
          警告 {runSummary.warningCount}
        </span>
      </div>
    </aside>
  )
}

function TraceTabBar({
  activeTab,
  round,
  onSelectTab,
  onOpenRawJson,
}: {
  activeTab: TraceTab
  round: TraceRound | null
  onSelectTab: (tab: TraceTab) => void
  onOpenRawJson: OpenTraceRawJson
}) {
  const tabs: Array<{ id: TraceTab; label: string }> = [
    { id: 'overview', label: '概览' },
    { id: 'request', label: 'Request' },
    { id: 'response', label: 'Response' },
  ]
  const rawAction = traceTabRawAction(activeTab, round)

  return (
    <div className="trace-chain-tabs" role="tablist" aria-label="Trace views">
      <div className="trace-chain-tab-list">
        {tabs.map((tab) => (
          <button
            type="button"
            role="tab"
            aria-selected={tab.id === activeTab}
            className={tab.id === activeTab ? 'trace-chain-tab active' : 'trace-chain-tab'}
            onClick={() => onSelectTab(tab.id)}
            key={tab.id}
          >
            {tab.label}
          </button>
        ))}
      </div>
      <div className="trace-chain-tab-actions">
        {rawAction && round ? (
          <button
            type="button"
            className="trace-json-open-button"
            onClick={() => onOpenRawJson(round, rawAction.kind)}
          >
            <Maximize2 size={14} aria-hidden="true" />
            {rawAction.label}
          </button>
        ) : null}
      </div>
    </div>
  )
}

function traceTabRawAction(
  activeTab: TraceTab,
  round: TraceRound | null,
): { kind: TraceRawKind; label: string } | null {
  if (!round) {
    return null
  }
  if (activeTab === 'overview') {
    return {
      kind: 'response',
      label: '查看 Raw',
    }
  }
  if (activeTab === 'request') {
    return {
      kind: 'request',
      label: '查看 Request Raw',
    }
  }
  if (activeTab === 'response') {
    return {
      kind: 'response',
      label: '查看 Response Raw',
    }
  }
  return null
}

function TraceRoundContent({
  round,
  activeTab,
  selectedToolId,
  onSelectTool,
  onOpenToolDetail,
}: {
  round: TraceRound
  activeTab: TraceTab
  selectedToolId: string | null
  onSelectTool: (toolId: string) => void
  onOpenToolDetail: OpenTraceToolDetail
}) {
  if (activeTab === 'request') {
    return <TraceRequestView round={round} />
  }
  if (activeTab === 'response') {
    return <TraceResponseView round={round} onOpenToolDetail={onOpenToolDetail} />
  }
  return (
    <TraceOverviewView
      round={round}
      selectedToolId={selectedToolId}
      onSelectTool={onSelectTool}
      onOpenToolDetail={onOpenToolDetail}
    />
  )
}

function TraceOverviewView({
  round,
  selectedToolId,
  onSelectTool,
  onOpenToolDetail,
}: {
  round: TraceRound
  selectedToolId: string | null
  onSelectTool: (toolId: string) => void
  onOpenToolDetail: OpenTraceToolDetail
}) {
  return (
    <div className="trace-chain-scroll">
      <TraceRoundSummary round={round} />
      <TraceSection
        title="Tool 执行结果"
        count={round.toolResults.length}
        hint="以下为当前回合工具执行结果，已作为后续上下文使用"
      >
        <TraceToolResultGrid
          toolResults={round.toolResults}
          selectedToolId={selectedToolId}
          onSelectTool={onSelectTool}
          onOpenToolDetail={onOpenToolDetail}
        />
      </TraceSection>
    </div>
  )
}

function TraceRequestView({ round }: { round: TraceRound }) {
  const [expandedMessageIds, setExpandedMessageIds] = useState<Set<string>>(() => new Set())

  const toggleMessage = (messageId: string) => {
    setExpandedMessageIds((current) => {
      const next = new Set(current)
      if (next.has(messageId)) {
        next.delete(messageId)
      } else {
        next.add(messageId)
      }
      return next
    })
  }

  return (
    <div className="trace-chain-scroll">
      <TraceRoundSummary round={round} />
      <section className="trace-request-stack">
        <TraceSection title="Messages" count={round.messages.length}>
          <div className="trace-message-list">
            {round.messages.map((message) => (
              <div
                className={`trace-message-row ${traceMessageRoleClass(message.role)} ${
                  expandedMessageIds.has(message.id) ? 'expanded' : ''
                }`}
                key={message.id}
              >
                <span className="trace-message-dot" aria-hidden="true" />
                <em>{message.role || 'message'}</em>
                <p>{message.content || '(empty)'}</p>
                <button
                  type="button"
                  className="trace-message-expand"
                  onClick={() => toggleMessage(message.id)}
                  aria-label={expandedMessageIds.has(message.id) ? 'Collapse message' : 'Expand message'}
                  aria-expanded={expandedMessageIds.has(message.id)}
                >
                  <ChevronRight className="trace-message-chevron" size={15} aria-hidden="true" />
                </button>
              </div>
            ))}
            {round.messages.length === 0 ? (
              <div className="trace-card-empty">No request messages captured.</div>
            ) : null}
          </div>
        </TraceSection>
        <TraceSection title="可用 Tools" count={round.availableTools.length}>
          <TraceToolDefinitionGrid tools={round.availableTools} />
        </TraceSection>
      </section>
    </div>
  )
}

function TraceResponseView({
  round,
  onOpenToolDetail,
}: {
  round: TraceRound
  onOpenToolDetail: OpenTraceToolDetail
}) {
  const preview = responsePreview(round.response)

  return (
    <div className="trace-chain-scroll">
      <TraceRoundSummary round={round} />
      <TraceSection title="本次 Response 决定调用的工具" count={round.toolCalls.length}>
        <TraceToolCallGrid
          toolCalls={round.toolCalls}
          compact
          onOpenToolDetail={onOpenToolDetail}
        />
      </TraceSection>
      <TraceSection title="Response 预览">
        {preview ? (
          <pre className="trace-response-preview">{preview}</pre>
        ) : (
          <div className="trace-card-empty">No natural-language response captured.</div>
        )}
      </TraceSection>
    </div>
  )
}

function TraceRoundSummary({ round }: { round: TraceRound }) {
  return (
    <section className="trace-round-summary">
      <div>
        <strong>Round {round.index}</strong>
        <StatusPill status={round.status} />
      </div>
      <TraceSummaryItem label="模型" value={round.model || '-'} />
      <TraceSummaryItem label="请求时间" value={formatTimestamp(round.startedAt)} />
      <TraceSummaryItem label="耗时" value={formatDurationShort(round.durationMs)} />
      <TraceSummaryItem label="输入 Tokens" value={formatTokenValue(round.tokenUsage?.input ?? null)} />
      <TraceSummaryItem label="输出 Tokens" value={formatTokenValue(round.tokenUsage?.output ?? null)} />
      <TraceSummaryItem label="工具数量" value={String(round.toolCalls.length)} />
    </section>
  )
}

function TraceSummaryItem({ label, value }: { label: string; value: string }) {
  return (
    <span className="trace-round-summary-item">
      <small>{label}</small>
      <strong>{value}</strong>
    </span>
  )
}

function TraceSection({
  title,
  count,
  hint,
  children,
}: {
  title: string
  count?: number
  hint?: string
  children: ReactNode
}) {
  return (
    <section className="trace-chain-section">
      <header>
        <div>
          <strong>{title}</strong>
          {typeof count === 'number' ? <span>{count}</span> : null}
        </div>
        {hint ? <small>{hint}</small> : null}
      </header>
      {children}
    </section>
  )
}

function TraceToolCallGrid({
  toolCalls,
  compact = false,
  onOpenToolDetail,
}: {
  toolCalls: TraceToolCall[]
  compact?: boolean
  onOpenToolDetail?: OpenTraceToolDetail
}) {
  if (toolCalls.length === 0) {
    return <div className="trace-card-empty">No tool calls in this response.</div>
  }

  return (
    <div className={compact ? 'trace-tool-call-strip' : 'trace-tool-call-grid'}>
      {toolCalls.map((tool) => (
        <button
          type="button"
          className="trace-tool-call-card"
          onClick={() => onOpenToolDetail?.({ kind: 'call', toolCall: tool })}
          key={tool.id}
        >
          <span>{tool.index}</span>
          <strong>{tool.name}</strong>
          {!compact ? <p>{compactJson(tool.argumentsValue, 120)}</p> : null}
        </button>
      ))}
    </div>
  )
}

function TraceToolDefinitionGrid({ tools }: { tools: TraceToolDefinition[] }) {
  if (tools.length === 0) {
    return <div className="trace-card-empty">No tools attached to this request.</div>
  }

  return (
    <div className="trace-tool-definition-grid">
      {tools.map((tool) => (
        <div className="trace-tool-definition-card" key={tool.id}>
          <strong>{tool.name}</strong>
          <p>{tool.description || 'No description.'}</p>
          <small>参数: {tool.parameterCount}</small>
        </div>
      ))}
    </div>
  )
}

function TraceToolResultGrid({
  toolResults,
  selectedToolId,
  onSelectTool,
  onOpenToolDetail,
}: {
  toolResults: TraceToolResult[]
  selectedToolId: string | null
  onSelectTool: (toolId: string) => void
  onOpenToolDetail: OpenTraceToolDetail
}) {
  if (toolResults.length === 0) {
    return <div className="trace-card-empty">No tool results captured for this round.</div>
  }

  return (
    <div className="trace-tool-result-grid">
      {toolResults.map((result) => (
        <button
          type="button"
          className={
            result.id === selectedToolId ? 'trace-tool-result-card selected' : 'trace-tool-result-card'
          }
          onClick={() => {
            onSelectTool(result.id)
            onOpenToolDetail({ kind: 'result', toolResult: result })
          }}
          key={result.id}
        >
          <span>
            <strong>{result.name}</strong>
            <StatusPill status={result.status} />
            <small>{formatDurationShort(result.durationMs)}</small>
          </span>
          <dl>
            <dt>输入</dt>
            <dd>{compactJson(result.argumentsValue, 120) || '-'}</dd>
            <dt>输出</dt>
            <dd>{result.outputSummary || compactJson(result.rawOutput, 160) || '-'}</dd>
          </dl>
        </button>
      ))}
    </div>
  )
}

function TraceRawJsonDialog({
  round,
  kind,
  onChangeKind,
  onClose,
}: {
  round: TraceRound
  kind: TraceRawKind
  onChangeKind: (kind: TraceRawKind) => void
  onClose: () => void
}) {
  const rawView = traceRawDialogView(round, kind)
  const handleBackdropMouseDown = (event: ReactMouseEvent<HTMLDivElement>) => {
    if (event.target === event.currentTarget) {
      onClose()
    }
  }

  return (
    <div className="trace-raw-dialog-backdrop" onMouseDown={handleBackdropMouseDown}>
      <section
        className="trace-raw-dialog"
        role="dialog"
        aria-modal="true"
        aria-label={rawView.title}
      >
        <header>
          <div className="trace-raw-dialog-heading">
            <div className="trace-raw-kind-switch" aria-label="Raw JSON source">
              <button
                type="button"
                className={kind === 'request' ? 'active' : ''}
                onClick={() => onChangeKind('request')}
              >
                Request
              </button>
              <button
                type="button"
                className={kind === 'response' ? 'active' : ''}
                onClick={() => onChangeKind('response')}
              >
                Response
              </button>
            </div>
            <div className="trace-raw-dialog-title">
              <strong>{rawView.title}</strong>
              <span>Raw JSON</span>
            </div>
          </div>
          <button type="button" className="icon-button" onClick={onClose} aria-label="Close raw JSON">
            <X size={16} aria-hidden="true" />
          </button>
        </header>
        <JsonTree
          value={maskSecrets(rawView.value)}
          defaultExpandAll
          defaultCollapsedKeys={traceRawDefaultCollapsedKeys}
          allowLineWrapToggle={false}
        />
      </section>
    </div>
  )
}

function TraceToolDetailDialog({
  detail,
  onOpenChildTrace,
  onClose,
}: {
  detail: TraceToolDetailDialogState
  onOpenChildTrace: OpenChildTrace
  onClose: () => void
}) {
  const title =
    detail.kind === 'call' ?
      `Tool Call #${detail.toolCall.index}: ${detail.toolCall.name}`
    : `Tool Result #${detail.toolResult.index}: ${detail.toolResult.name}`
  const subtitle = detail.kind === 'call' ? 'Tool Call' : 'Tool Result'
  const subagents =
    detail.kind === 'result' ? extractSubagentSummaries(detail.toolResult.rawOutput) : []
  const handleBackdropMouseDown = (event: ReactMouseEvent<HTMLDivElement>) => {
    if (event.target === event.currentTarget) {
      onClose()
    }
  }

  return (
    <div className="trace-raw-dialog-backdrop" onMouseDown={handleBackdropMouseDown}>
      <section
        className="trace-raw-dialog trace-tool-detail-dialog"
        role="dialog"
        aria-modal="true"
        aria-label={title}
      >
        <header>
          <div className="trace-raw-dialog-heading">
            <div className="trace-raw-dialog-title">
              <strong>{title}</strong>
              <span>{subtitle}</span>
            </div>
          </div>
          <button type="button" className="icon-button" onClick={onClose} aria-label="Close tool detail">
            <X size={16} aria-hidden="true" />
          </button>
        </header>
        <div className="trace-tool-detail-dialog-body">
          {detail.kind === 'call' ? (
            <>
              <TraceToolDetailMeta
                items={[
                  ['工具', detail.toolCall.name],
                  ['序号', String(detail.toolCall.index)],
                ]}
              />
              <TraceToolDetailBlock title="输入">
                <JsonTree
                  value={maskSecrets(detail.toolCall.argumentsValue)}
                  defaultExpandAll
                  allowLineWrapToggle={false}
                  quoteStrings={false}
                />
              </TraceToolDetailBlock>
            </>
          ) : (
            <>
              <TraceToolDetailMeta
                items={[
                  ['工具', detail.toolResult.name],
                  ['状态', statusLabel(detail.toolResult.status)],
                  ['开始时间', formatTimestamp(detail.toolResult.startedAt)],
                  ['耗时', formatDurationShort(detail.toolResult.durationMs)],
                ]}
              />
              <TraceToolDetailBlock title="输入">
                <JsonTree
                  value={maskSecrets(detail.toolResult.argumentsValue)}
                  defaultExpandAll
                  allowLineWrapToggle={false}
                  quoteStrings={false}
                />
              </TraceToolDetailBlock>
              <TraceToolDetailBlock title="输出">
                <TraceToolResultPreview result={detail.toolResult} />
              </TraceToolDetailBlock>
              {subagents.length > 0 ? (
                <TraceToolDetailBlock title="Subagents">
                  <TraceSubagentList
                    subagents={subagents}
                    onOpenChildTrace={onOpenChildTrace}
                  />
                </TraceToolDetailBlock>
              ) : null}
            </>
          )}
        </div>
      </section>
    </div>
  )
}

function TraceToolResultPreview({ result }: { result: TraceToolResult }) {
  const output = toolResultDisplayOutput(result)
  const readableText = typeof output === 'string' ? readableToolText(output) : ''

  if (readableText) {
    return <pre className="trace-tool-readable-output">{readableText}</pre>
  }

  return (
    <JsonTree
      value={maskSecrets(output)}
      defaultExpandAll
      allowLineWrapToggle={false}
      quoteStrings={false}
    />
  )
}

function TraceSubagentList({
  subagents,
  onOpenChildTrace,
}: {
  subagents: TraceSubagentSummary[]
  onOpenChildTrace: OpenChildTrace
}) {
  return (
    <div className="trace-tool-result-grid">
      {subagents.map((subagent) => (
        <button
          type="button"
          className="trace-tool-result-card"
          key={subagent.childTaskId}
          onClick={() => onOpenChildTrace(subagent)}
        >
          <span>
            <strong>{subagent.taskName || subagent.agentName || subagent.childTaskId}</strong>
            <small>{subagent.status || 'unknown'}</small>
          </span>
          <dl>
            <dt>Child</dt>
            <dd>{subagent.childTaskId}</dd>
            <dt>Summary</dt>
            <dd>{subagent.summary || '-'}</dd>
            <dt>Trace</dt>
            <dd>
              {subagent.traceCount === null ? 'Open child trace' : `${subagent.traceCount} events`}
            </dd>
          </dl>
        </button>
      ))}
    </div>
  )
}

function TraceChildTraceDialog({
  childTrace,
  onClose,
}: {
  childTrace: TraceChildTraceDialogState
  onClose: () => void
}) {
  const handleBackdropMouseDown = (event: ReactMouseEvent<HTMLDivElement>) => {
    if (event.target === event.currentTarget) {
      onClose()
    }
  }

  return (
    <div className="trace-raw-dialog-backdrop" onMouseDown={handleBackdropMouseDown}>
      <section
        className="trace-raw-dialog"
        role="dialog"
        aria-modal="true"
        aria-label={`Child trace ${childTrace.label}`}
      >
        <header>
          <div className="trace-raw-dialog-heading">
            <div className="trace-raw-dialog-title">
              <strong>{childTrace.label}</strong>
              <span>{childTrace.taskId}</span>
            </div>
          </div>
          <button type="button" className="icon-button" onClick={onClose} aria-label="Close child trace">
            <X size={16} aria-hidden="true" />
          </button>
        </header>
        {childTrace.loading ? (
          <div className="trace-card-empty">Loading child trace...</div>
        ) : childTrace.error ? (
          <div className="trace-card-empty">{childTrace.error}</div>
        ) : (
          <JsonTree
            value={maskSecrets({
              taskId: childTrace.taskId,
              traceEvents: childTrace.events,
            })}
            defaultExpandAll
            defaultCollapsedKeys={traceRawDefaultCollapsedKeys}
            allowLineWrapToggle={false}
          />
        )}
      </section>
    </div>
  )
}

function TraceToolDetailMeta({ items }: { items: Array<[string, string]> }) {
  return (
    <section className="trace-tool-detail-meta">
      {items.map(([label, value]) => (
        <span key={label}>
          <small>{label}</small>
          <strong>{value || '-'}</strong>
        </span>
      ))}
    </section>
  )
}

function TraceToolDetailBlock({
  title,
  children,
}: {
  title: string
  children: ReactNode
}) {
  return (
    <section className="trace-tool-detail-block">
      <strong>{title}</strong>
      {children}
    </section>
  )
}

function traceRawDialogView(
  round: TraceRound,
  kind: TraceRawKind,
): { title: string; value: unknown } {
  if (kind === 'request') {
    return {
      title: '原始 Request JSON',
      value: round.request?.input ?? null,
    }
  }
  return {
    title: '原始 Response JSON',
    value: round.response?.output ?? null,
  }
}

function traceMessageRoleClass(role: string): string {
  const normalized = role.trim().toLowerCase().replace(/[^a-z0-9]+/g, '-')
  return normalized ? `role-${normalized}` : 'role-message'
}

function StatusPill({ status }: { status: TraceStatus }) {
  return <em className={`trace-status-pill ${status}`}>{statusLabel(status)}</em>
}

function StatusGlyph({ status }: { status: TraceStatus }) {
  if (status === 'failed') {
    return <CircleAlert className="trace-status-glyph failed" size={15} aria-hidden="true" />
  }
  if (status === 'warning') {
    return <AlertTriangle className="trace-status-glyph warning" size={15} aria-hidden="true" />
  }
  if (status === 'running') {
    return <Clock3 className="trace-status-glyph running" size={15} aria-hidden="true" />
  }
  return <CheckCircle2 className="trace-status-glyph success" size={15} aria-hidden="true" />
}

function TraceWorkbenchFooter({
  runSummary,
  traceEvents,
  taskId,
}: {
  runSummary: TraceRunSummary
  traceEvents: ToolTraceEvent[]
  taskId: string | null
}) {
  return (
    <footer className="trace-workbench-footer">
      <span>更新时间：{formatTimestamp(msToIso(runSummary.endMs))}</span>
      <span>显示时区：UTC+8</span>
      <button
        type="button"
        className="trace-export-button"
        onClick={() => exportTraceEvents(traceEvents, taskId)}
      >
        <Download size={15} aria-hidden="true" />
        导出
      </button>
    </footer>
  )
}

function createTraceRounds(events: ToolTraceEvent[]): TraceRound[] {
  const requestIndexes = events
    .map((event, index) => (event.type === 'llm_request' ? index : -1))
    .filter((index) => index >= 0)

  if (requestIndexes.length === 0) {
    const responseEvents = events.filter((event) =>
      ['llm_response', 'final_response'].includes(event.type),
    )
    return responseEvents.map((event, index) =>
      createTraceRound(index + 1, null, event, events.filter((candidate) => candidate === event)),
    )
  }

  return requestIndexes.map((requestIndex, index) => {
    const nextRequestIndex = requestIndexes[index + 1] ?? events.length
    const segment = events.slice(requestIndex, nextRequestIndex)
    const request = events[requestIndex]
    const response =
      segment.find((event) => event.type === 'llm_response' || event.type === 'final_response') ??
      null
    return createTraceRound(index + 1, request, response, segment)
  })
}

function createTraceRound(
  index: number,
  request: ToolTraceEvent | null,
  response: ToolTraceEvent | null,
  events: ToolTraceEvent[],
): TraceRound {
  const requestPayload = request ? traceRequestPayload(request) : {}
  const responsePayload = response ? traceResponsePayload(response) : {}
  const model = firstText([
    requestPayload.model,
    responsePayload.model,
    asRecord(response?.output).model,
    response?.toolName,
  ])
  const provider = firstText([
    asRecord(request?.input).provider,
    asRecord(request?.input).providerType,
    asRecord(response?.output).provider,
  ])
  const status = inferRoundStatus(events, response)
  const tokenUsage = response ? readTraceTokenUsage(response.output) : null
  const toolCalls = response ? extractToolCalls(response) : extractToolCallsFromEvents(events)
  const toolResults = events
    .filter((event) => isToolResultEvent(event))
    .map((event, resultIndex) => createToolResult(event, resultIndex + 1))
  const startedAt = request?.startedAt ?? response?.startedAt ?? events[0]?.startedAt ?? null

  return {
    id: request?.id ?? response?.id ?? `round-${index}`,
    index,
    request,
    response,
    events,
    toolCalls,
    toolResults,
    availableTools: extractToolDefinitions(requestPayload),
    messages: extractMessages(requestPayload),
    model,
    provider,
    status,
    startedAt,
    durationMs: response?.durationMs ?? inferDurationMs(events),
    tokenUsage,
  }
}

function traceRequestPayload(event: ToolTraceEvent): Record<string, unknown> {
  const input = asRecord(event.input)
  const request = asRecord(input.request)
  return Object.keys(request).length > 0 ? request : input
}

function traceResponsePayload(event: ToolTraceEvent): Record<string, unknown> {
  const output = asRecord(event.output)
  const response = asRecord(output.response)
  return Object.keys(response).length > 0 ? response : output
}

function inferRoundStatus(events: ToolTraceEvent[], response: ToolTraceEvent | null): TraceStatus {
  if (events.some((event) => event.status === 'failed')) {
    return 'failed'
  }
  if (events.some((event) => event.status === 'warning')) {
    return 'warning'
  }
  if (!response || events.some((event) => event.status === 'running')) {
    return 'running'
  }
  return 'success'
}

function inferDurationMs(events: ToolTraceEvent[]): number | null {
  const startMs = minKnown(events.map((event) => timeToMs(event.startedAt)))
  const endMs = maxKnown(
    events.map((event) => {
      const endedMs = timeToMs(event.endedAt)
      if (endedMs !== null) {
        return endedMs
      }
      const startedMs = timeToMs(event.startedAt)
      return startedMs !== null && event.durationMs !== null ? startedMs + event.durationMs : null
    }),
  )
  return startMs !== null && endMs !== null ? Math.max(0, endMs - startMs) : null
}

function extractMessages(requestPayload: Record<string, unknown>): TraceMessage[] {
  return arrayValue(requestPayload.messages).map((message, index) => {
    const record = asRecord(message)
    return {
      id: `message-${index}`,
      index: index + 1,
      role: firstText([record.role, record.type]) || 'message',
      content: messageContentText(record.content),
    }
  })
}

function extractToolDefinitions(requestPayload: Record<string, unknown>): TraceToolDefinition[] {
  return arrayValue(requestPayload.tools).map((tool, index) => {
    const record = asRecord(tool)
    const fn = asRecord(record.function)
    const parameters = asRecord(fn.parameters ?? record.parameters)
    const properties = asRecord(parameters.properties)
    return {
      id: `tool-def-${index}`,
      index: index + 1,
      name: firstText([fn.name, record.name, record.toolName]) || `tool-${index + 1}`,
      description: firstText([fn.description, record.description]),
      parameterCount: Object.keys(properties).length,
    }
  })
}

function extractToolCalls(responseEvent: ToolTraceEvent): TraceToolCall[] {
  const output = asRecord(responseEvent.output)
  const response = traceResponsePayload(responseEvent)
  const choice = firstChoice(response) ?? firstChoice(output)
  const message = asRecord(choice?.message)
  const toolCalls = firstArray([
    message.tool_calls,
    message.toolCalls,
    response.tool_calls,
    response.toolCalls,
    output.tool_calls,
    output.toolCalls,
  ])

  return toolCalls.map((toolCall, index) => createToolCall(toolCall, index + 1))
}

function extractToolCallsFromEvents(events: ToolTraceEvent[]): TraceToolCall[] {
  return events
    .filter((event) => event.type === 'tool_call' && event.toolName !== 'chat_completion')
    .map((event, index) => {
      const input = asRecord(event.input)
      return {
        id: event.id,
        index: index + 1,
        name: event.toolName ?? firstText([input.toolName]) ?? `tool-${index + 1}`,
        argumentsValue: asRecord(input.arguments),
      }
    })
}

function createToolCall(value: unknown, index: number): TraceToolCall {
  const record = asRecord(value)
  const fn = asRecord(record.function)
  const name = firstText([fn.name, record.name, record.toolName]) || `tool-${index}`
  const rawArguments = fn.arguments ?? record.arguments ?? record.input ?? {}

  return {
    id: firstText([record.id]) || `tool-call-${index}`,
    index,
    name,
    argumentsValue: parseJsonMaybe(rawArguments),
  }
}

function createToolResult(event: ToolTraceEvent, index: number): TraceToolResult {
  const input = asRecord(event.input)
  return {
    id: event.id,
    index,
    name: event.toolName ?? firstText([input.toolName]) ?? `tool-${index}`,
    status: event.status,
    startedAt: event.startedAt,
    durationMs: event.durationMs,
    argumentsValue: asRecord(input.arguments),
    outputSummary: event.outputSummary ?? readableToolOutput(event.output),
    rawInput: event.input,
    rawOutput: event.output,
  }
}

function isToolResultEvent(event: ToolTraceEvent): boolean {
  return (
    (event.type === 'tool_result' || event.type === 'error') &&
    event.toolName !== 'chat_completion'
  )
}

function createTraceRunSummary(
  traceEvents: ToolTraceEvent[],
  rounds: TraceRound[],
): TraceRunSummary {
  const startMs = minKnown(traceEvents.map((event) => timeToMs(event.startedAt)))
  const endMs = maxKnown(
    traceEvents.map((event) => {
      const endedMs = timeToMs(event.endedAt)
      if (endedMs !== null) {
        return endedMs
      }
      const startedMs = timeToMs(event.startedAt)
      return startedMs !== null && event.durationMs !== null ? startedMs + event.durationMs : null
    }),
  )

  return {
    totalDurationMs: startMs !== null && endMs !== null ? Math.max(0, endMs - startMs) : null,
    requestCount: rounds.length,
    successCount: rounds.filter((round) => round.status === 'success').length,
    failedCount: rounds.filter((round) => round.status === 'failed').length,
    warningCount: rounds.filter((round) => round.status === 'warning').length,
    startMs,
    endMs,
  }
}

function createTraceTokenSummary(traceEvents: ToolTraceEvent[]): TraceTokenTotals {
  const usages = traceEvents
    .map((event) => readTraceTokenUsage(event.output))
    .filter((usage): usage is TraceTokenUsage => usage !== null)

  return {
    input: sumKnown(usages.map((usage) => usage.input)),
    output: sumKnown(usages.map((usage) => usage.output)),
    total: sumKnown(usages.map((usage) => usage.total)),
    inputCached: sumKnown(usages.map((usage) => usage.inputCached)),
    inputUncached: sumKnown(usages.map((usage) => usage.inputUncached)),
  }
}

function readTraceTokenUsage(value: unknown): TraceTokenUsage | null {
  const record = asRecord(value)
  const response = asRecord(record.response)
  const candidates = [
    asRecord(record.tokenUsage),
    asRecord(record.usage),
    asRecord(record.tokens),
    asRecord(response.usage),
    response,
    record,
  ]
  let merged: TraceTokenUsage | null = null

  for (const candidate of candidates) {
    const usage = readTokenUsage(candidate)
    if (usage) {
      merged = mergeTokenUsage(merged, usage)
    }
  }

  return merged ? completeTokenUsage(merged) : null
}

function readTokenUsage(record: Record<string, unknown>): TraceTokenUsage | null {
  const input = firstNumber(record, [
    'inputTokens',
    'input_tokens',
    'promptTokens',
    'prompt_tokens',
    'prompt_eval_count',
  ])
  const output = firstNumber(record, [
    'outputTokens',
    'output_tokens',
    'completionTokens',
    'completion_tokens',
    'eval_count',
  ])
  const total = firstNumber(record, ['totalTokens', 'total_tokens'])
  const details = asRecord(record.promptTokensDetails ?? record.prompt_tokens_details)
  const inputCached =
    firstNumber(record, [
      'inputCachedTokens',
      'input_cached_tokens',
      'cachedInputTokens',
      'cached_input_tokens',
      'cache_read_input_tokens',
    ]) ?? firstNumber(details, ['cachedTokens', 'cached_tokens'])
  const inputUncached = firstNumber(record, [
    'inputUncachedTokens',
    'input_uncached_tokens',
    'uncachedInputTokens',
    'uncached_input_tokens',
  ])

  if (
    input === null &&
    output === null &&
    total === null &&
    inputCached === null &&
    inputUncached === null
  ) {
    return null
  }

  return {
    input,
    output,
    total,
    inputCached,
    inputUncached,
  }
}

function mergeTokenUsage(
  current: TraceTokenUsage | null,
  next: TraceTokenUsage,
): TraceTokenUsage {
  if (!current) {
    return next
  }
  return {
    input: current.input ?? next.input,
    output: current.output ?? next.output,
    total: current.total ?? next.total,
    inputCached: current.inputCached ?? next.inputCached,
    inputUncached: current.inputUncached ?? next.inputUncached,
  }
}

function completeTokenUsage(usage: TraceTokenUsage): TraceTokenUsage {
  const inputUncached =
    usage.inputUncached ??
    (usage.input !== null && usage.inputCached !== null ?
      Math.max(0, usage.input - usage.inputCached)
    : null)

  return {
    ...usage,
    total: usage.total ?? sumNullable(usage.input, usage.output),
    inputUncached,
  }
}

function responsePreview(responseEvent: ToolTraceEvent | null): string {
  if (!responseEvent) {
    return ''
  }
  const output = asRecord(responseEvent.output)
  const response = traceResponsePayload(responseEvent)
  const choice = firstChoice(response) ?? firstChoice(output)
  const message = asRecord(choice?.message)
  return normalizeReadableText(
    firstText([
      message.content,
      output.message,
      output.content,
      responseEvent.outputSummary,
    ]),
  )
}

function readableToolOutput(value: unknown): string {
  const output = asRecord(value)
  return firstText([
    output.message,
    output.summary,
    output.error,
    output.count !== undefined ? `count=${String(output.count)}` : '',
    compactJson(output, 180),
  ])
}

function toolResultDisplayOutput(result: TraceToolResult): unknown {
  const rawOutput = asRecord(result.rawOutput)
  if (Object.prototype.hasOwnProperty.call(rawOutput, 'output')) {
    return parseJsonMaybeDeep(rawOutput.output)
  }
  if (result.outputSummary) {
    return parseJsonMaybeDeep(result.outputSummary)
  }
  return parseJsonMaybeDeep(result.rawOutput)
}

function extractSubagentSummaries(rawOutput: unknown): TraceSubagentSummary[] {
  const output = asRecord(rawOutput)
  const payload = asRecord(output.output)
  const records =
    Array.isArray(payload.subagents) ? payload.subagents
    : payload.childTaskId ? [payload]
    : []

  return records
    .map((value) => {
      const record = asRecord(value)
      const childTaskId = firstText([record.childTaskId, record.childRunId])
      if (!childTaskId) {
        return null
      }
      const traceCount =
        typeof record.traceCount === 'number' && Number.isFinite(record.traceCount) ?
          record.traceCount
        : null
      return {
        childTaskId,
        agentName: firstText([record.agentName]),
        taskName: firstText([record.taskName]),
        status: firstText([record.status]),
        summary: firstText([record.summary]),
        traceCount,
      }
    })
    .filter((value): value is TraceSubagentSummary => value !== null)
}

function readableToolText(value: unknown): string {
  if (typeof value === 'string') {
    return normalizeReadableText(value)
  }
  const record = asRecord(value)
  return firstText([record.message, record.summary, record.error])
}

function roundSearchText(round: TraceRound): string {
  return [
    round.index,
    round.model,
    round.provider,
    round.status,
    ...round.toolCalls.map((tool) => tool.name),
    ...round.toolResults.map((tool) => tool.name),
  ]
    .filter(Boolean)
    .join(' ')
    .toLowerCase()
}

function exportTraceEvents(traceEvents: ToolTraceEvent[], taskId: string | null): void {
  const payload = JSON.stringify({ taskId, traceEvents: maskSecrets(traceEvents) }, null, 2)
  const blob = new Blob([payload], { type: 'application/json' })
  const url = URL.createObjectURL(blob)
  const link = document.createElement('a')
  const safeTaskId = taskId?.replace(/[^a-zA-Z0-9_.-]+/g, '-') || 'trace'

  link.href = url
  link.download = `${safeTaskId}.json`
  document.body.appendChild(link)
  link.click()
  link.remove()
  URL.revokeObjectURL(url)
}

function messageContentText(value: unknown): string {
  if (typeof value === 'string') {
    return normalizeReadableText(value)
  }
  if (Array.isArray(value)) {
    return value
      .map((part) => {
        const record = asRecord(part)
        return firstText([
          record.text,
          record.content,
          record.type ? `[${String(record.type)}]` : '',
        ])
      })
      .filter(Boolean)
      .join(' ')
  }
  return compactJson(value, 180)
}

function firstChoice(record: Record<string, unknown>): Record<string, unknown> | null {
  const choices = record.choices
  if (!Array.isArray(choices)) {
    return null
  }
  return asRecord(choices[0])
}

function firstArray(values: unknown[]): unknown[] {
  for (const value of values) {
    if (Array.isArray(value)) {
      return value
    }
  }
  return []
}

function arrayValue(value: unknown): unknown[] {
  return Array.isArray(value) ? value : []
}

function asRecord(value: unknown): Record<string, unknown> {
  if (value && typeof value === 'object' && !Array.isArray(value)) {
    return value as Record<string, unknown>
  }
  return {}
}

function firstText(values: unknown[]): string {
  for (const value of values) {
    const text = stringValue(value)
    if (text) {
      return text
    }
  }
  return ''
}

function stringValue(value: unknown): string {
  if (value === null || value === undefined) {
    return ''
  }
  if (typeof value === 'string') {
    return normalizeReadableText(value)
  }
  if (typeof value === 'number' || typeof value === 'boolean') {
    return String(value)
  }
  return ''
}

function firstNumber(record: Record<string, unknown>, keys: string[]): number | null {
  for (const key of keys) {
    const number = numberValue(record[key])
    if (number !== null) {
      return number
    }
  }
  return null
}

function numberValue(value: unknown): number | null {
  if (typeof value === 'number' && Number.isFinite(value)) {
    return value
  }
  if (typeof value === 'string' && value.trim().length > 0) {
    const parsed = Number(value)
    return Number.isFinite(parsed) ? parsed : null
  }
  return null
}

function parseJsonMaybe(value: unknown): unknown {
  if (typeof value !== 'string') {
    return value
  }
  try {
    return JSON.parse(value)
  } catch {
    return value
  }
}

function parseJsonMaybeDeep(value: unknown): unknown {
  let current = value
  for (let index = 0; index < 3; index += 1) {
    if (typeof current !== 'string') {
      return current
    }
    const parsed = parseJsonMaybe(current)
    if (parsed === current) {
      return normalizeReadableText(current)
    }
    current = parsed
  }
  return current
}

function compactJson(value: unknown, maxLength: number): string {
  if (value === null || value === undefined) {
    return ''
  }
  const json = typeof value === 'string' ? value : formatJson(value)
  return json.length > maxLength ? `${json.slice(0, maxLength - 1)}...` : json
}

function formatJson(value: unknown): string {
  if (value === null || value === undefined) {
    return '{}'
  }
  try {
    return JSON.stringify(maskSecrets(value), null, 2)
  } catch {
    return String(value)
  }
}

function maskSecrets(value: unknown, key = ''): unknown {
  if (value === null || value === undefined) {
    return value
  }
  if (typeof value === 'string') {
    if (isSecretKey(key)) {
      return '***'
    }
    return value
      .replace(/sk-[A-Za-z0-9_-]{10,}/g, 'sk-***')
      .replace(/(bearer\s+)[A-Za-z0-9._-]{10,}/gi, '$1***')
  }
  if (Array.isArray(value)) {
    return value.map((item) => maskSecrets(item, key))
  }
  if (typeof value === 'object') {
    return Object.fromEntries(
      Object.entries(value as Record<string, unknown>).map(([entryKey, entryValue]) => [
        entryKey,
        maskSecrets(entryValue, entryKey),
      ]),
    )
  }
  return value
}

function isSecretKey(key: string): boolean {
  return /api[_-]?key|authorization|access[_-]?token|secret|password/i.test(key)
}

function normalizeReadableText(value: string): string {
  return value
    .replace(/<think>[\s\S]*?<\/think>/gi, '')
    .replace(/\\n/g, '\n')
    .replace(/\\"/g, '"')
    .replace(/\r\n/g, '\n')
    .replace(/\n{3,}/g, '\n\n')
    .replace(/sk-[A-Za-z0-9_-]{10,}/g, 'sk-***')
    .replace(/(api[_-]?key["']?\s*[:=]\s*["']?)[^"',\s}]+/gi, '$1***')
    .replace(/(bearer\s+)[A-Za-z0-9._-]{10,}/gi, '$1***')
    .trim()
}

function minKnown(values: Array<number | null>): number | null {
  const known = values.filter((value): value is number => value !== null)
  return known.length ? Math.min(...known) : null
}

function maxKnown(values: Array<number | null>): number | null {
  const known = values.filter((value): value is number => value !== null)
  return known.length ? Math.max(...known) : null
}

function sumKnown(values: Array<number | null>): number | null {
  let total = 0
  let hasValue = false

  for (const value of values) {
    if (value !== null) {
      total += value
      hasValue = true
    }
  }

  return hasValue ? total : null
}

function sumNullable(left: number | null, right: number | null): number | null {
  if (left !== null && right !== null) {
    return left + right
  }
  return null
}

function timeToMs(value: string | null): number | null {
  if (!value) {
    return null
  }
  const ms = Date.parse(value)
  return Number.isFinite(ms) ? ms : null
}

function msToIso(value: number | null): string | null {
  return value === null ? null : new Date(value).toISOString()
}

function formatTimestamp(value: string | null): string {
  const date = value ? new Date(value) : null
  if (!date || Number.isNaN(date.getTime())) {
    return '-'
  }

  return `${pad2(date.getHours())}:${pad2(date.getMinutes())}:${pad2(
    date.getSeconds(),
  )}.${pad3(date.getMilliseconds())}`
}

function formatDurationShort(value: number | null): string {
  if (value === null || !Number.isFinite(value)) {
    return '-'
  }
  if (value >= 1000) {
    return `${(value / 1000).toFixed(3)} s`
  }
  return `${formatMs(value)} ms`
}

function formatMs(value: number): string {
  return Number.isInteger(value) ? String(value) : value.toFixed(3)
}

function percentText(part: number | null, total: number | null): string | undefined {
  if (part === null || total === null || total <= 0) {
    return undefined
  }
  return `(${((part / total) * 100).toFixed(1)}%)`
}

function statusLabel(status: TraceStatus): string {
  if (status === 'success') {
    return '成功'
  }
  if (status === 'failed') {
    return '失败'
  }
  if (status === 'warning') {
    return '警告'
  }
  return '运行中'
}

function formatTokenValue(value: number | null): string {
  return value === null ? '-' : tokenFormatter.format(value)
}

function pad2(value: number): string {
  return String(value).padStart(2, '0')
}

function pad3(value: number): string {
  return String(value).padStart(3, '0')
}

const tokenFormatter = new Intl.NumberFormat()

export default TraceDrawer
