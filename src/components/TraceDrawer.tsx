import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { X } from 'lucide-react'
import type { ToolTraceEvent } from '../types/trace'
import TraceEventRow from './TraceEventRow'
import { createTraceStepViewModels } from './traceViewModel'

interface TraceDrawerProps {
  open: boolean
  taskId: string | null
  traceEvents: ToolTraceEvent[]
  onClose: () => void
}

interface TraceScrollbarState {
  visible: boolean
  top: number
  height: number
  thumbTop: number
  thumbHeight: number
}

const hiddenScrollbar: TraceScrollbarState = {
  visible: false,
  top: 0,
  height: 0,
  thumbTop: 0,
  thumbHeight: 0,
}

function TraceDrawer({ open, taskId, traceEvents, onClose }: TraceDrawerProps) {
  const bodyRef = useRef<HTMLDivElement>(null)
  const contentRef = useRef<HTMLDivElement>(null)
  const [scrollbar, setScrollbar] = useState<TraceScrollbarState>(hiddenScrollbar)
  const steps = useMemo(() => createTraceStepViewModels(traceEvents), [traceEvents])
  const tokenSummary = useMemo(() => createTraceTokenSummary(traceEvents), [traceEvents])

  const updateScrollbar = useCallback(() => {
    const body = bodyRef.current
    if (!body) {
      setScrollbar(hiddenScrollbar)
      return
    }

    const maxScroll = body.scrollHeight - body.clientHeight
    const trackInset = 8
    const trackHeight = Math.max(0, body.clientHeight - trackInset * 2)
    if (maxScroll <= 1 || trackHeight <= 0) {
      setScrollbar(hiddenScrollbar)
      return
    }

    const thumbHeight = Math.max(
      44,
      Math.round((body.clientHeight / body.scrollHeight) * trackHeight),
    )
    const thumbTravel = Math.max(0, trackHeight - thumbHeight)
    const thumbTop = Math.round((body.scrollTop / maxScroll) * thumbTravel)
    const nextScrollbar = {
      visible: true,
      top: body.offsetTop + trackInset,
      height: trackHeight,
      thumbTop,
      thumbHeight,
    }

    setScrollbar((current) =>
      current.visible === nextScrollbar.visible &&
      current.top === nextScrollbar.top &&
      current.height === nextScrollbar.height &&
      current.thumbTop === nextScrollbar.thumbTop &&
      current.thumbHeight === nextScrollbar.thumbHeight
        ? current
        : nextScrollbar,
    )
  }, [])

  useEffect(() => {
    if (!open) {
      return undefined
    }

    const body = bodyRef.current
    const content = contentRef.current
    if (!body) {
      return undefined
    }

    updateScrollbar()
    const animationFrame = window.requestAnimationFrame(updateScrollbar)
    body.addEventListener('scroll', updateScrollbar, { passive: true })
    window.addEventListener('resize', updateScrollbar)

    const resizeObserver = new ResizeObserver(updateScrollbar)
    resizeObserver.observe(body)
    if (content) {
      resizeObserver.observe(content)
    }

    const mutationObserver = new MutationObserver(updateScrollbar)
    mutationObserver.observe(body, {
      attributes: true,
      childList: true,
      subtree: true,
    })

    return () => {
      window.cancelAnimationFrame(animationFrame)
      body.removeEventListener('scroll', updateScrollbar)
      window.removeEventListener('resize', updateScrollbar)
      resizeObserver.disconnect()
      mutationObserver.disconnect()
    }
  }, [open, traceEvents.length, updateScrollbar])

  if (!open) {
    return null
  }

  return (
    <aside className="trace-drawer" aria-label="Trace drawer">
      <div className="trace-drawer-header">
        <div>
          <h3>Trace</h3>
          <p>{taskId ? `message trace: ${taskId}` : 'No message trace selected'}</p>
        </div>
        <button type="button" className="icon-button" onClick={onClose} aria-label="Close trace">
          <X size={16} aria-hidden="true" />
        </button>
      </div>
      <TraceTokenSummary summary={tokenSummary} />
      <div className="trace-drawer-body" ref={bodyRef}>
        <div className="trace-drawer-content" ref={contentRef}>
          {traceEvents.length === 0 ? (
            <div className="empty-state">No trace events yet.</div>
          ) : (
            <div className="trace-event-list">
              {steps.map((step) => (
                <TraceEventRow step={step} key={step.id} />
              ))}
            </div>
          )}
        </div>
      </div>
      {scrollbar.visible ? (
        <div
          className="trace-scrollbar"
          style={{ top: scrollbar.top, height: scrollbar.height }}
          aria-hidden="true"
        >
          <div
            className="trace-scrollbar-thumb"
            style={{
              height: scrollbar.thumbHeight,
              transform: `translateY(${scrollbar.thumbTop}px)`,
            }}
          />
        </div>
      ) : null}
    </aside>
  )
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

function TraceTokenSummary({ summary }: { summary: TraceTokenTotals }) {
  const cacheTitle =
    summary.inputCached !== null || summary.inputUncached !== null ?
      `cache hit: ${formatTokenValue(summary.inputCached)}\ncache miss: ${formatTokenValue(
        summary.inputUncached,
      )}`
    : 'cache hit: -\ncache miss: -'

  return (
    <section className="trace-token-summary" aria-label="Token usage">
      <TokenStat label="all" value={summary.total} />
      <TokenStat label="in" value={summary.input} tooltip={cacheTitle} />
      <TokenStat label="out" value={summary.output} />
    </section>
  )
}

function TokenStat({
  label,
  value,
  tooltip,
}: {
  label: string
  value: number | null
  tooltip?: string
}) {
  const className = tooltip ? 'trace-token-stat has-tooltip' : 'trace-token-stat'

  return (
    <span className={className} data-tooltip={tooltip}>
      <span className="trace-token-label">{label}</span>
      <span className="trace-token-value">{formatTokenValue(value)}</span>
    </span>
  )
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
