import type { ToolTraceEvent, TraceStatus } from '../types/trace'
import { normalizeDisplayText, normalizePathsInValue } from '../utils/path'

export interface TraceSummaryItem {
  label: string
  value: string
  multiline?: boolean
}

export interface TraceStepViewModel {
  id: string
  index: number
  title: string
  shortSummary: string
  status: TraceStatus
  eventType: ToolTraceEvent['type']
  toolName: string | null
  startedAt: string
  endedAt: string | null
  durationMs: number | null
  summaryItems: TraceSummaryItem[]
  inputSummary: TraceSummaryItem[]
  outputSummary: TraceSummaryItem[]
  rawInput: unknown | null
  rawOutput: unknown | null
}

export function createTraceStepViewModels(events: ToolTraceEvent[]): TraceStepViewModel[] {
  const steps: TraceStepViewModel[] = []

  for (const event of events) {
    if (event.type === 'model_message') {
      const finalMessage = extractMessage(event.output) ?? event.outputSummary ?? ''
      const sanitizedMessage = sanitizeModelMessage(finalMessage)
      const chatCompletion = [...steps]
        .reverse()
        .find((step) => step.title === 'LLM' || step.title === 'Chat completion')

      if (chatCompletion && sanitizedMessage) {
        appendOrReplaceItem(chatCompletion.outputSummary, {
          label: 'Final Message',
          value: sanitizedMessage,
          multiline: true,
        })
        chatCompletion.rawOutput = {
          chatCompletion: chatCompletion.rawOutput,
          finalMessage: normalizePathsInValue(event.output),
        }
        continue
      }
    }

    if (isMergedIntoLaterStep(event)) {
      continue
    }

    steps.push(toTraceStepViewModel(event))
  }

  return steps.map((step, index) => ({ ...step, index: index + 1 }))
}

export function sanitizeModelMessage(text: string): string {
  return text
    .replace(/<think>[\s\S]*?<\/think>/gi, '')
    .replace(/\\n/g, '\n')
    .replace(/\\"/g, '"')
    .replace(/\r\n/g, '\n')
    .replace(/\n{3,}/g, '\n\n')
    .trim()
}

function toTraceStepViewModel(event: ToolTraceEvent): TraceStepViewModel {
  const input = asRecord(event.input)
  const output = asRecord(event.output)
  const rawInput = event.input === undefined ? null : normalizePathsInValue(event.input)
  const rawOutput = event.output === undefined ? null : normalizePathsInValue(event.output)

  if (event.title === 'Start task') {
    return {
      ...baseStep(event, 'Start task', rawInput, rawOutput),
      inputSummary: compactItems([
        item('Project', stringValue(input.projectName)),
        item('Prompt', stringValue(input.prompt), true),
      ]),
    }
  }

  if (event.type === 'user_message') {
    return {
      ...baseStep(event, 'User input', rawInput, rawOutput),
      inputSummary: compactItems([
        item('Project', stringValue(input.projectName)),
        item('Prompt', event.outputSummary ?? stringValue(input.prompt), true),
      ]),
    }
  }

  if (event.title === 'select_model') {
    return {
      ...baseStep(event, 'Select model', rawInput, rawOutput),
      summaryItems: compactItems([
        item('Provider', stringValue(output.provider)),
        item('Credential', stringValue(output.credential ?? input.credentialId)),
        item('Model', stringValue(output.model)),
        item('Base URL', stringValue(output.baseUrl)),
      ]),
    }
  }

  if (event.type === 'llm_response') {
    const request = asRecord(input.request)
    const choice = firstChoice(output)
    const tokens = readTokenUsage(output)
    return {
      ...baseStep(event, 'LLM', rawInput, rawOutput),
      shortSummary: llmTokenShortSummary(tokens),
      summaryItems: compactItems([
        item('Model', stringValue(request.model)),
        item('Tools', String(arrayCount(request.tools))),
        item('Messages', String(arrayCount(request.messages))),
        item('Finish Reason', stringValue(choice.finish_reason)),
        item('Tool Calls', String(arrayCount(asRecord(choice.message).tool_calls))),
        item('Content Chars', contentCharCount(choice.message)),
      ]),
      inputSummary: compactItems([
        item('Model', stringValue(request.model)),
        item('Tools', String(arrayCount(request.tools))),
        item('Messages', String(arrayCount(request.messages))),
      ]),
      outputSummary: compactItems([
        item('Finish Reason', stringValue(choice.finish_reason)),
        item('Tool Calls', String(arrayCount(asRecord(choice.message).tool_calls))),
        item('Content Chars', contentCharCount(choice.message)),
      ]),
    }
  }

  if (event.type === 'tool_result') {
    if (event.title === 'chat_completion') {
      return createChatCompletionStep(event, input, output, rawInput, rawOutput)
    }

    const argumentsValue = asRecord(input.arguments)
    return {
      ...baseStep(event, 'Tool', rawInput, rawOutput),
      shortSummary: toolStatusSummary(event, output),
      summaryItems: compactItems([
        item('Tool', event.toolName ?? ''),
      ]),
      inputSummary: compactItems([
        item('Tool', event.toolName ?? stringValue(input.toolName)),
        item('Arguments', compactJson(argumentsValue)),
      ]),
      outputSummary: compactItems([
        item('Result', event.outputSummary ?? compactJson(event.output)),
      ]),
    }
  }

  if (event.type === 'final_response') {
    const request = asRecord(input.request)
    const response = asRecord(output.response)
    const choice = firstChoice(response)
    const message = extractMessage(event.output) ?? event.outputSummary ?? ''
    const tokens = readTokenUsage(response)
    return {
      ...baseStep(event, 'LLM', rawInput, rawOutput),
      shortSummary: llmTokenShortSummary(tokens),
      summaryItems: compactItems([
        item('Model', stringValue(request.model)),
        item('Tools', String(arrayCount(request.tools))),
        item('Messages', String(arrayCount(request.messages))),
        item('Finish Reason', stringValue(choice.finish_reason)),
        item('Content Chars', contentCharCount(choice.message)),
      ]),
      inputSummary: compactItems([
        item('Model', stringValue(request.model)),
        item('Tools', String(arrayCount(request.tools))),
        item('Messages', String(arrayCount(request.messages))),
      ]),
      outputSummary: compactItems([
        item('Finish Reason', stringValue(choice.finish_reason)),
        item('Content Chars', contentCharCount(choice.message)),
        item('Final Response', sanitizeModelMessage(message), true),
      ]),
    }
  }

  if (event.title === 'chat_completion') {
    return createChatCompletionStep(event, input, output, rawInput, rawOutput)
  }

  if (event.type === 'model_message') {
    const message = extractMessage(event.output) ?? event.outputSummary ?? ''
    return {
      ...baseStep(event, 'Final message', rawInput, rawOutput),
      outputSummary: compactItems([
        item('Final Message', sanitizeModelMessage(message), true),
      ]),
    }
  }

  if (event.status === 'failed' || event.type === 'error') {
    const error = stringValue(output.error) || event.outputSummary || 'Step failed'
    return {
      ...baseStep(event, normalizeTraceTitle(event.title), rawInput, rawOutput),
      outputSummary: compactItems([
        item('Error', normalizeDisplayText(error), true),
      ]),
    }
  }

  return {
    ...baseStep(event, normalizeTraceTitle(event.title), rawInput, rawOutput),
    summaryItems: compactItems([
      item('Tool', event.toolName ?? ''),
      item('Summary', event.outputSummary ?? '', true),
      item('Duration', formatDuration(event.durationMs)),
    ]),
  }
}

function isMergedIntoLaterStep(event: ToolTraceEvent): boolean {
  return (
    event.type === 'user_message' ||
    event.type === 'llm_request' ||
    event.type === 'tool_call'
  )
}

function createChatCompletionStep(
  event: ToolTraceEvent,
  input: Record<string, unknown>,
  output: Record<string, unknown>,
  rawInput: unknown | null,
  rawOutput: unknown | null,
): TraceStepViewModel {
  const request = asRecord(input.request)
  const message = extractMessage(event.output)
  const tokens = readTokenUsage(output)
  return {
    ...baseStep(event, 'LLM', rawInput, rawOutput),
    shortSummary: llmTokenShortSummary(tokens),
    summaryItems: compactItems([
      item('Provider', stringValue(input.provider)),
      item('Model', stringValue(request.model ?? input.model ?? output.model)),
      item('Messages', String(arrayCount(request.messages))),
      item('Tokens', tokens.display),
    ]),
    inputSummary: compactItems([
      item('Provider', stringValue(input.provider)),
      item('Model', stringValue(request.model ?? input.model)),
      item('Messages', String(arrayCount(request.messages))),
      item('Input tokens', tokens.inputTokens),
    ]),
    outputSummary: compactItems([
      item('Model', stringValue(output.model ?? request.model ?? input.model)),
      item('Message chars', stringValue(output.messageChars)),
      item('Output tokens', tokens.outputTokens),
      item('Total tokens', tokens.totalTokens),
      item('Final Message', message ? sanitizeModelMessage(message) : '', true),
    ]),
  }
}

function baseStep(
  event: ToolTraceEvent,
  title: string,
  rawInput: unknown | null,
  rawOutput: unknown | null,
): TraceStepViewModel {
  return {
    id: event.id,
    index: event.stepIndex,
    title,
    shortSummary: event.outputSummary ? sanitizeModelMessage(event.outputSummary) : '',
    status: event.status,
    eventType: event.type,
    toolName: event.toolName,
    startedAt: event.startedAt,
    endedAt: event.endedAt,
    durationMs: event.durationMs,
    summaryItems: compactItems([item('Duration', formatDuration(event.durationMs))]),
    inputSummary: [],
    outputSummary: [],
    rawInput,
    rawOutput,
  }
}

function appendOrReplaceItem(items: TraceSummaryItem[], nextItem: TraceSummaryItem): void {
  const index = items.findIndex((item) => item.label === nextItem.label)
  if (index >= 0) {
    items[index] = nextItem
  } else {
    items.push(nextItem)
  }
}

function item(label: string, value: string, multiline = false): TraceSummaryItem {
  return { label, value, multiline }
}

function compactItems(items: TraceSummaryItem[]): TraceSummaryItem[] {
  return items.filter((item) => item.value.trim().length > 0)
}

function asRecord(value: unknown): Record<string, unknown> {
  if (value && typeof value === 'object' && !Array.isArray(value)) {
    return value as Record<string, unknown>
  }
  return {}
}

function stringValue(value: unknown): string {
  if (value === null || value === undefined) {
    return ''
  }
  if (typeof value === 'string') {
    return sanitizeModelMessage(value)
  }
  return String(value)
}

function arrayCount(value: unknown): number {
  return Array.isArray(value) ? value.length : 0
}

function firstChoice(value: Record<string, unknown>): Record<string, unknown> {
  const choices = value.choices
  if (!Array.isArray(choices)) {
    return {}
  }
  return asRecord(choices[0])
}

function contentCharCount(message: unknown): string {
  const content = asRecord(message).content
  return typeof content === 'string' ? String([...content].length) : ''
}

function compactJson(value: unknown): string {
  if (value === null || value === undefined) {
    return ''
  }
  try {
    return JSON.stringify(value)
  } catch {
    return String(value)
  }
}

function toolStatusSummary(
  event: ToolTraceEvent,
  output: Record<string, unknown>,
): string {
  if (event.status === 'running') {
    return 'running'
  }
  if (event.status === 'failed' || output.ok === false || output.error) {
    return 'failed'
  }
  if (event.status === 'warning') {
    return 'warning'
  }
  return 'success'
}

function extractMessage(value: unknown): string | null {
  const record = asRecord(value)
  const message = record.message
  return typeof message === 'string' ? message : null
}

function readTokenUsage(record: Record<string, unknown>): {
  inputTokens: string
  outputTokens: string
  totalTokens: string
  inputCachedTokens: string
  inputUncachedTokens: string
  display: string
} {
  const inputTokens = tokenValue(record, ['inputTokens', 'promptTokens', 'prompt_tokens'])
  const outputTokens = tokenValue(record, [
    'outputTokens',
    'completionTokens',
    'completion_tokens',
  ])
  const promptTokenDetails = tokenDetailRecords(record)
  const inputCachedTokens =
    tokenValue(record, [
      'inputCachedTokens',
      'input_cached_tokens',
      'cachedInputTokens',
      'cached_input_tokens',
      'cacheReadInputTokens',
      'cache_read_input_tokens',
    ]) || tokenValueFromSources(promptTokenDetails, ['cachedTokens', 'cached_tokens'])
  const inputUncachedTokens =
    tokenValue(record, [
      'inputUncachedTokens',
      'input_uncached_tokens',
      'uncachedInputTokens',
      'uncached_input_tokens',
    ]) || subtractTokenStrings(inputTokens, inputCachedTokens)
  const totalTokens =
    tokenValue(record, ['totalTokens', 'total_tokens']) ||
    sumTokenStrings(inputTokens, outputTokens)
  const displayParts = compactStrings([
    inputTokens ? `${inputTokens} in` : '',
    outputTokens ? `${outputTokens} out` : '',
    totalTokens ? `${totalTokens} total` : '',
  ])

  return {
    inputTokens,
    outputTokens,
    totalTokens,
    inputCachedTokens,
    inputUncachedTokens,
    display: displayParts.join(' / ') || 'not reported',
  }
}

function llmTokenShortSummary(tokens: ReturnType<typeof readTokenUsage>): string {
  const parts = compactStrings([
    tokens.inputTokens ?
      `in ${formatTokenString(tokens.inputTokens)}${inputCacheSuffix(tokens)}`
    : '',
    tokens.outputTokens ? `out ${formatTokenString(tokens.outputTokens)}` : '',
  ])
  return parts.join(', ') || 'tokens not reported'
}

function inputCacheSuffix(tokens: ReturnType<typeof readTokenUsage>): string {
  if (!tokens.inputTokens) {
    return ''
  }
  const cachedTokens = tokens.inputCachedTokens || '0'
  const uncachedTokens =
    tokens.inputUncachedTokens || subtractTokenStrings(tokens.inputTokens, cachedTokens)
  return ` (cached ${formatTokenString(cachedTokens)}, uncached ${formatTokenString(
    uncachedTokens,
  )})`
}

function tokenValue(record: Record<string, unknown>, keys: string[]): string {
  return tokenValueFromSources(tokenSourceRecords(record), keys)
}

function tokenSourceRecords(record: Record<string, unknown>): Record<string, unknown>[] {
  return [
    record,
    asRecord(record.tokens),
    asRecord(record.usage),
    asRecord(record.tokenUsage),
  ]
}

function tokenDetailRecords(record: Record<string, unknown>): Record<string, unknown>[] {
  return tokenSourceRecords(record).flatMap((source) => [
    asRecord(source.promptTokensDetails),
    asRecord(source.prompt_tokens_details),
    asRecord(source.inputTokensDetails),
    asRecord(source.input_tokens_details),
  ])
}

function tokenValueFromSources(
  sources: Record<string, unknown>[],
  keys: string[],
): string {
  for (const source of sources) {
    for (const key of keys) {
      const value = source[key]
      if (value !== null && value !== undefined && value !== '') {
        return stringValue(value)
      }
    }
  }
  return ''
}

function sumTokenStrings(left: string, right: string): string {
  if (!left || !right) {
    return ''
  }
  const leftValue = Number(left)
  const rightValue = Number(right)
  if (!Number.isFinite(leftValue) || !Number.isFinite(rightValue)) {
    return ''
  }
  return String(leftValue + rightValue)
}

function subtractTokenStrings(left: string, right: string): string {
  if (!left || !right) {
    return ''
  }
  const leftValue = Number(left)
  const rightValue = Number(right)
  if (!Number.isFinite(leftValue) || !Number.isFinite(rightValue)) {
    return ''
  }
  return String(Math.max(0, leftValue - rightValue))
}

function formatTokenString(value: string): string {
  if (!value) {
    return '-'
  }
  const numericValue = Number(value)
  if (!Number.isFinite(numericValue)) {
    return value
  }
  return new Intl.NumberFormat().format(numericValue)
}

function compactStrings(values: string[]): string[] {
  return values.filter((value) => value.trim().length > 0)
}

function formatDuration(durationMs: number | null): string {
  return typeof durationMs === 'number' ? `${durationMs} ms` : ''
}

function normalizeTraceTitle(title: string): string {
  return normalizeDisplayText(title)
    .replace(/_/g, ' ')
    .replace(/\b\w/g, (value) => value.toUpperCase())
}

export interface AggregatedTokenUsage {
  inputTokens: number
  outputTokens: number
  totalTokens: number
  inputCachedTokens: number
  inputUncachedTokens: number
  eventCount: number
  hasAny: boolean
  display: string
}

export function aggregateTokenUsage(events: ToolTraceEvent[]): AggregatedTokenUsage {
  let inputTokens = 0
  let outputTokens = 0
  let totalTokens = 0
  let inputCachedTokens = 0
  let inputUncachedTokens = 0
  let eventCount = 0
  let hasAny = false

  const addNumber = (target: { value: number }, value: string) => {
    const numeric = Number(value)
    if (Number.isFinite(numeric) && numeric > 0) {
      target.value += numeric
      hasAny = true
    }
  }

  for (const event of events) {
    if (
      event.type !== 'llm_response' &&
      event.type !== 'final_response' &&
      event.type !== 'tool_result'
    ) {
      continue
    }
    const input = asRecord(event.input)
    const output = asRecord(event.output)
    let tokens: ReturnType<typeof readTokenUsage> | null = null

    if (event.type === 'llm_response') {
      tokens = readTokenUsage(output)
      if (!tokens.inputTokens && !tokens.outputTokens) {
        const inputTokensRecord = asRecord(input.tokenUsage)
        if (inputTokensRecord) {
          tokens = readTokenUsage(inputTokensRecord)
        }
      }
    } else if (event.type === 'final_response') {
      const responseRecord = asRecord(output.response)
      tokens = readTokenUsage(responseRecord)
      if (!tokens.inputTokens && !tokens.outputTokens) {
        const outputTokensRecord = asRecord(output.tokenUsage)
        if (outputTokensRecord) {
          tokens = readTokenUsage(outputTokensRecord)
        }
      }
    } else if (event.type === 'tool_result') {
      tokens = readTokenUsage(output)
    }

    if (!tokens) {
      continue
    }

    const inputBucket = { value: 0 }
    const outputBucket = { value: 0 }
    const totalBucket = { value: 0 }
    const cachedBucket = { value: 0 }
    const uncachedBucket = { value: 0 }
    addNumber(inputBucket, tokens.inputTokens)
    addNumber(outputBucket, tokens.outputTokens)
    addNumber(totalBucket, tokens.totalTokens)
    addNumber(cachedBucket, tokens.inputCachedTokens)
    addNumber(uncachedBucket, tokens.inputUncachedTokens)

    if (inputBucket.value || outputBucket.value || totalBucket.value || cachedBucket.value) {
      eventCount += 1
    }

    inputTokens += inputBucket.value
    outputTokens += outputBucket.value
    totalTokens += totalBucket.value
    inputCachedTokens += cachedBucket.value
    inputUncachedTokens += uncachedBucket.value
  }

  if (!totalTokens && inputTokens + outputTokens > 0) {
    totalTokens = inputTokens + outputTokens
  }
  if (!inputUncachedTokens && inputTokens > 0) {
    inputUncachedTokens = Math.max(0, inputTokens - inputCachedTokens)
  }

  const formatNumber = (value: number) => value.toLocaleString('en-US')
  const displayParts = [
    inputTokens ? `${formatNumber(inputTokens)} in` : '',
    outputTokens ? `${formatNumber(outputTokens)} out` : '',
    totalTokens ? `${formatNumber(totalTokens)} total` : '',
  ].filter((part) => part.length > 0)

  return {
    inputTokens,
    outputTokens,
    totalTokens,
    inputCachedTokens,
    inputUncachedTokens,
    eventCount,
    hasAny,
    display: displayParts.join(' / ') || 'not reported',
  }
}