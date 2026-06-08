import { useEffect, useMemo, useRef, useState } from 'react'
import type { MouseEvent as ReactMouseEvent, ReactNode } from 'react'
import {
  AlertTriangle,
  Box,
  Braces,
  CheckCircle2,
  CircleAlert,
  CircleCheck,
  CircleX,
  Clock3,
  Copy,
  Download,
  FileText,
  Search,
  X,
  Zap,
} from 'lucide-react'
import type { ToolTraceEvent } from '../types/trace'
import {
  createTraceStepViewModels,
  type TraceStepViewModel,
} from './traceViewModel'
import { JsonTree } from './TraceEventRow'

interface TraceDrawerProps {
  open: boolean
  taskId: string | null
  traceEvents: ToolTraceEvent[]
  onClose: () => void
}

type TraceDetailTab = 'input' | 'output'

function TraceDrawer({ open, taskId, traceEvents, onClose }: TraceDrawerProps) {
  const dialogRef = useRef<HTMLElement>(null)
  const [selectedStepId, setSelectedStepId] = useState<string | null>(null)
  const [detailTab, setDetailTab] = useState<TraceDetailTab>('input')
  const steps = useMemo(() => createTraceStepViewModels(traceEvents), [traceEvents])
  const tokenSummary = useMemo(() => createTraceTokenSummary(traceEvents), [traceEvents])
  const runSummary = useMemo(
    () => createTraceRunSummary(traceEvents, steps),
    [steps, traceEvents],
  )

  useEffect(() => {
    if (!open) {
      return undefined
    }

    dialogRef.current?.focus()

    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        onClose()
      }
    }

    window.addEventListener('keydown', handleKeyDown)
    return () => window.removeEventListener('keydown', handleKeyDown)
  }, [onClose, open])

  useEffect(() => {
    if (!open) {
      return
    }

    setSelectedStepId((current) => {
      if (current && steps.some((step) => step.id === current)) {
        return current
      }

      return preferredInitialStep(steps)?.id ?? null
    })
  }, [open, steps])

  if (!open) {
    return null
  }

  const selectedStep =
    steps.find((step) => step.id === selectedStepId) ?? preferredInitialStep(steps) ?? null

  const handleBackdropMouseDown = (event: ReactMouseEvent<HTMLDivElement>) => {
    if (event.target === event.currentTarget) {
      onClose()
    }
  }

  return (
    <div className="trace-modal-backdrop" onMouseDown={handleBackdropMouseDown}>
      <aside
        className="trace-drawer trace-workbench"
        role="dialog"
        aria-modal="true"
        aria-labelledby="trace-drawer-title"
        ref={dialogRef}
        tabIndex={-1}
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

        <section className="trace-workbench-lower">
          <TraceStepNavigator
            steps={steps}
            selectedStepId={selectedStep?.id ?? null}
            onSelectStep={setSelectedStepId}
            runSummary={runSummary}
          />
          <TraceDetailPanel
            step={selectedStep}
            tab={detailTab}
            onTabChange={setDetailTab}
          />
        </section>

        <TraceWorkbenchFooter
          runSummary={runSummary}
          traceEvents={traceEvents}
          taskId={taskId}
        />
      </aside>
    </div>
  )
}

interface TraceRunSummary {
  totalDurationMs: number | null
  requestCount: number
  successCount: number
  failedCount: number
  warningCount: number
  startMs: number | null
  endMs: number | null
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
        label="非缓存命中 Tokens"
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

function TraceStepNavigator({
  steps,
  selectedStepId,
  onSelectStep,
  runSummary,
}: {
  steps: TraceStepViewModel[]
  selectedStepId: string | null
  onSelectStep: (stepId: string) => void
  runSummary: TraceRunSummary
}) {
  const [query, setQuery] = useState('')
  const filteredSteps = query.trim()
    ? steps.filter((step) => traceSearchText(step).includes(query.trim().toLowerCase()))
    : steps

  return (
    <aside className="trace-step-nav" aria-label="Trace step navigation">
      <div className="trace-step-nav-header">
        <strong>步骤导航</strong>
        <label className="trace-step-search">
          <Search size={15} aria-hidden="true" />
          <input
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            placeholder="搜索步骤或工具..."
          />
        </label>
      </div>
      <div className="trace-step-list">
        {filteredSteps.map((step) => (
          <button
            type="button"
            className={step.id === selectedStepId ? 'trace-step-item selected' : 'trace-step-item'}
            onClick={() => onSelectStep(step.id)}
            key={step.id}
          >
            <StatusGlyph status={step.status} />
            <span>{step.index}</span>
            <strong>{step.title}</strong>
            <small>{formatDurationShort(step.durationMs)}</small>
          </button>
        ))}
        {filteredSteps.length === 0 ? (
          <div className="trace-step-empty">No matching steps.</div>
        ) : null}
      </div>
      <div className="trace-step-nav-footer">
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

function TraceDetailPanel({
  step,
  tab,
  onTabChange,
}: {
  step: TraceStepViewModel | null
  tab: TraceDetailTab
  onTabChange: (tab: TraceDetailTab) => void
}) {
  const tabs: Array<{ id: TraceDetailTab; label: string }> = [
    { id: 'input', label: '请求输入' },
    { id: 'output', label: '响应输出' },
  ]

  return (
    <section className="trace-detail-panel" aria-label="Trace detail">
      <div className="trace-detail-tabs" role="tablist" aria-label="Trace detail tabs">
        {tabs.map((item) => (
          <button
            type="button"
            role="tab"
            aria-selected={tab === item.id}
            className={tab === item.id ? 'trace-detail-tab active' : 'trace-detail-tab'}
            onClick={() => onTabChange(item.id)}
            key={item.id}
          >
            {item.label}
          </button>
        ))}
      </div>

      {!step ? (
        <div className="trace-detail-empty">No trace step selected.</div>
      ) : (
        <TraceDetailContent step={step} tab={tab} />
      )}
    </section>
  )
}

function TraceDetailContent({
  step,
  tab,
}: {
  step: TraceStepViewModel
  tab: TraceDetailTab
}) {
  if (tab === 'output') {
    return (
      <div className="trace-json-single-pane">
        <JsonCodePanel
          title="响应输出"
          subtitle={formatTimestamp(step.endedAt)}
          value={step.rawOutput}
          badge={statusLabel(step.status)}
        />
      </div>
    )
  }

  return (
    <div className="trace-json-single-pane">
      <JsonCodePanel
        title="请求输入"
        subtitle={formatTimestamp(step.startedAt)}
        value={step.rawInput}
        badge={step.title}
      />
    </div>
  )
}

function JsonCodePanel({
  title,
  subtitle,
  value,
  badge,
}: {
  title: string
  subtitle?: string
  value: unknown
  badge?: string
}) {
  const [copied, setCopied] = useState(false)
  const text = formatJsonForPanel(value)
  const treeValue = useMemo(() => normalizeJsonForPanel(value), [value])

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(text)
    } catch {
      return
    }
    setCopied(true)
    window.setTimeout(() => setCopied(false), 1200)
  }

  return (
    <section className="trace-json-pane">
      <header className="trace-json-pane-header">
        <span>
          {badge ? <small>{badge}</small> : null}
          <strong>{title}</strong>
          {subtitle ? <em>{subtitle}</em> : null}
        </span>
        <button type="button" className="trace-copy-button" onClick={copy}>
          <Copy size={14} aria-hidden="true" />
          {copied ? '已复制' : '复制 JSON'}
        </button>
      </header>
      <div className="trace-json-pane-body" aria-label={`${title} JSON`}>
        <JsonTree value={treeValue} />
      </div>
    </section>
  )
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
      <span>显示时区：UTC+8</span>
      <button
        type="button"
        className="trace-export-button"
        onClick={() => exportTraceEvents(traceEvents, taskId)}
      >
        <Download size={15} aria-hidden="true" />
        导出 Trace
      </button>
      <span>{formatDurationShort(runSummary.totalDurationMs)}</span>
    </footer>
  )
}

function createTraceRunSummary(
  traceEvents: ToolTraceEvent[],
  steps: TraceStepViewModel[],
): TraceRunSummary {
  const stepStartTimes = steps
    .map((step) => timeToMs(step.startedAt))
    .filter((value): value is number => value !== null)
  const stepEndTimes = steps
    .map((step) => {
      const startMs = timeToMs(step.startedAt)
      const endMs = timeToMs(step.endedAt)
      if (endMs !== null) {
        return endMs
      }
      if (startMs !== null && step.durationMs !== null) {
        return startMs + step.durationMs
      }
      return null
    })
    .filter((value): value is number => value !== null)
  const startMs = stepStartTimes.length ? Math.min(...stepStartTimes) : null
  const endMs = stepEndTimes.length ? Math.max(...stepEndTimes) : startMs
  const countedEvents = traceEvents.filter((event) => event.type === 'tool_result')
  const statusSource = countedEvents.length > 0 ? countedEvents : traceEvents

  return {
    totalDurationMs: startMs !== null && endMs !== null ? Math.max(0, endMs - startMs) : null,
    requestCount: traceEvents.filter((event) =>
      ['llm_response', 'final_response', 'tool_result'].includes(event.type),
    ).length,
    successCount: statusSource.filter((event) => event.status === 'success').length,
    failedCount: statusSource.filter((event) => event.status === 'failed').length,
    warningCount: statusSource.filter((event) => event.status === 'warning').length,
    startMs,
    endMs,
  }
}

function preferredInitialStep(steps: TraceStepViewModel[]): TraceStepViewModel | null {
  return (
    steps.find((step) => step.title === 'LLM') ??
    steps.find((step) => step.eventType === 'tool_result') ??
    steps[0] ??
    null
  )
}

function StatusGlyph({ status }: { status: TraceStepViewModel['status'] }) {
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

function traceSearchText(step: TraceStepViewModel): string {
  return [
    step.index,
    step.title,
    step.toolName,
    step.shortSummary,
    step.eventType,
  ]
    .filter(Boolean)
    .join(' ')
    .toLowerCase()
}

function exportTraceEvents(traceEvents: ToolTraceEvent[], taskId: string | null): void {
  const payload = JSON.stringify({ taskId, traceEvents }, null, 2)
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

function formatJsonForPanel(value: unknown): string {
  if (value === null || value === undefined || value === '') {
    return '{\n}'
  }

  if (typeof value === 'string') {
    try {
      return JSON.stringify(JSON.parse(value), null, 2)
    } catch {
      return JSON.stringify(value, null, 2)
    }
  }

  try {
    return JSON.stringify(value, null, 2)
  } catch {
    return JSON.stringify(String(value), null, 2)
  }
}

function normalizeJsonForPanel(value: unknown): unknown {
  if (value === null || value === undefined || value === '') {
    return {}
  }

  if (typeof value === 'string') {
    try {
      return JSON.parse(value)
    } catch {
      return value
    }
  }

  return value
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

function statusLabel(status: TraceStepViewModel['status']): string {
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

function timeToMs(value: string | null): number | null {
  if (!value) {
    return null
  }
  const ms = Date.parse(value)
  return Number.isFinite(ms) ? ms : null
}

function pad2(value: number): string {
  return String(value).padStart(2, '0')
}

function pad3(value: number): string {
  return String(value).padStart(3, '0')
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

const tokenFormatter = new Intl.NumberFormat()

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
  if (!record) {
    return null
  }

  const candidates = tokenCandidatesForProvider(readProviderType(record), record)
  let merged: TraceTokenUsage | null = null

  for (const candidate of candidates) {
    if (!candidate) {
      continue
    }

    const usage = readTokenUsage(candidate)
    if (usage) {
      merged = mergeTokenUsage(merged, usage)
    }
  }

  if (!merged) {
    return null
  }

  return completeTokenUsage(merged)
}

function tokenCandidatesForProvider(
  providerType: string,
  record: Record<string, unknown>,
): Array<Record<string, unknown> | null> {
  const response = asRecord(record.response)
  const recordBaseResp = asRecord(record.base_resp) ?? asRecord(record.baseResp)
  const responseBaseResp = asRecord(response?.base_resp) ?? asRecord(response?.baseResp)
  const normalizedCandidates = [
    record,
    asRecord(record.tokenUsage),
    asRecord(record.usage),
    asRecord(record.tokens),
  ]
  const openAiLikeCandidates = [
    asRecord(response?.usage),
    asRecord(responseBaseResp?.usage),
    asRecord(recordBaseResp?.usage),
    response,
    responseBaseResp,
    recordBaseResp,
  ]

  if (providerType === 'claude') {
    return [
      asRecord(response?.usage),
      asRecord(record.usage),
      response,
      ...normalizedCandidates,
    ]
  }

  if (providerType === 'ollama') {
    return [response, ...normalizedCandidates]
  }

  if (isOpenAiLikeProvider(providerType)) {
    return [...openAiLikeCandidates, ...normalizedCandidates]
  }

  return [...normalizedCandidates, ...openAiLikeCandidates]
}

function readTokenUsage(record: Record<string, unknown>): TraceTokenUsage | null {
  const rawInput = firstNumber(record, [
    'inputTokens',
    'input_tokens',
    'promptTokens',
    'prompt_tokens',
    'promptEvalCount',
    'prompt_eval_count',
  ])
  const output = firstNumber(record, [
    'outputTokens',
    'output_tokens',
    'completionTokens',
    'completion_tokens',
    'evalCount',
    'eval_count',
  ])
  const reportedTotal = firstNumber(record, ['totalTokens', 'total_tokens'])
  const details = asRecord(record.promptTokensDetails) ?? asRecord(record.prompt_tokens_details)
  const cacheRead = firstNumber(record, [
    'cacheReadInputTokens',
    'cache_read_input_tokens',
  ])
  const cacheCreation = firstNumber(record, [
    'cacheCreationInputTokens',
    'cache_creation_input_tokens',
  ])
  const reportedCached =
    firstNumber(record, [
      'inputCachedTokens',
      'input_cached_tokens',
      'cachedInputTokens',
      'cached_input_tokens',
    ]) ?? firstNumber(details, ['cachedTokens', 'cached_tokens'])
  const inputCached = reportedCached ?? cacheRead
  const explicitUncached = firstNumber(record, [
    'inputUncachedTokens',
    'input_uncached_tokens',
    'uncachedInputTokens',
    'uncached_input_tokens',
  ])
  const hasClaudeCacheShape = cacheRead !== null || cacheCreation !== null
  const input =
    hasClaudeCacheShape ? sumKnown([rawInput, cacheCreation, cacheRead]) : rawInput
  const inputUncached =
    explicitUncached ??
    (hasClaudeCacheShape ?
      sumKnown([rawInput, cacheCreation])
    : input !== null && inputCached !== null ? Math.max(0, input - inputCached)
    : null)
  const resolvedTotal = reportedTotal ?? sumNullable(input, output)

  if (
    input === null &&
    output === null &&
    resolvedTotal === null &&
    inputCached === null &&
    inputUncached === null
  ) {
    return null
  }

  return {
    input,
    output,
    total: resolvedTotal,
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
  return {
    ...usage,
    total: usage.total ?? sumNullable(usage.input, usage.output),
    inputUncached:
      usage.inputUncached ??
      (usage.input !== null && usage.inputCached !== null ?
        Math.max(0, usage.input - usage.inputCached)
      : null),
  }
}

function readProviderType(record: Record<string, unknown>): string {
  return stringValue(record.type ?? record.providerType ?? record.provider_type).toLowerCase()
}

function isOpenAiLikeProvider(providerType: string): boolean {
  return [
    'openai',
    'openai-compatible',
    'codebuddy',
    'deepseek',
    'minimax',
    'local-gateway',
  ].includes(providerType)
}

function asRecord(value: unknown): Record<string, unknown> | null {
  if (value && typeof value === 'object' && !Array.isArray(value)) {
    return value as Record<string, unknown>
  }
  return null
}

function firstNumber(record: Record<string, unknown> | null, keys: string[]): number | null {
  if (!record) {
    return null
  }

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

function stringValue(value: unknown): string {
  return typeof value === 'string' ? value.trim() : ''
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

function formatTokenValue(value: number | null): string {
  return value === null ? '-' : tokenFormatter.format(value)
}

export default TraceDrawer
