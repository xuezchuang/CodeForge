import { useMemo, useState } from 'react'
import { Info } from 'lucide-react'
import type { AgentTask, ChatMessage } from '../types/task'
import type { ToolTraceEvent } from '../types/trace'

type ProfileTab = 'overview' | 'models'
type ProfileRange = 'all' | '30d' | '7d'
type TokenBreakdownType = 'all' | 'in' | 'out' | 'cached' | 'no-cache'

interface ProfileProps {
  tasks: AgentTask[]
}

interface RunRecord {
  date: Date
  model: string
  inputTokens: number
  outputTokens: number
  totalTokens: number
  cachedTokens: number
  noCacheTokens: number
}

interface ActivityDay {
  key: string
  date: Date
  tokens: number
  active: boolean
}

const modelColors = ['#5b8def', '#8bb9ff', '#7ee0b8', '#f0b86e', '#d18cff']

function Profile({ tasks }: ProfileProps) {
  const [tab, setTab] = useState<ProfileTab>('overview')
  const [range, setRange] = useState<ProfileRange>('all')
  const stats = useMemo(() => createProfileStats(tasks, range), [tasks, range])

  return (
    <section className="profile-page" aria-label="Profile">
      <div className="profile-card">
        <div className="profile-card-header">
          <div className="profile-tabs" role="tablist" aria-label="Profile sections">
            <button
              type="button"
              className={tab === 'overview' ? 'profile-tab active' : 'profile-tab'}
              onClick={() => setTab('overview')}
            >
              Overview
            </button>
            <button
              type="button"
              className={tab === 'models' ? 'profile-tab active' : 'profile-tab'}
              onClick={() => setTab('models')}
            >
              Models
            </button>
          </div>
          <RangeTabs range={range} onChange={setRange} />
        </div>

        {tab === 'overview' ? <Overview stats={stats} /> : <Models stats={stats} />}
      </div>
    </section>
  )
}

function RangeTabs({
  range,
  onChange,
}: {
  range: ProfileRange
  onChange: (range: ProfileRange) => void
}) {
  return (
    <div className="profile-range-tabs" aria-label="Stats range">
      {(['all', '30d', '7d'] as const).map((value) => (
        <button
          type="button"
          key={value}
          className={range === value ? 'profile-range-tab active' : 'profile-range-tab'}
          onClick={() => onChange(value)}
        >
          {value === 'all' ? 'All' : value}
        </button>
      ))}
    </div>
  )
}

function Overview({ stats }: { stats: ProfileStats }) {
  const [selectedBreakdown, setSelectedBreakdown] = useState<TokenBreakdownType>('all')

  return (
    <>
      <div className="profile-metric-grid">
        <Metric label="Sessions" value={formatNumber(stats.sessions)} />
        <Metric label="Messages" value={formatNumber(stats.messages)} />
        <Metric label="Total tokens" value={formatCompactNumber(stats.totalTokens)} />
        <Metric label="Active days" value={formatNumber(stats.activeDays)} />
        <Metric label="Current streak" value={formatDays(stats.currentStreak)} />
        <Metric label="Longest streak" value={formatDays(stats.longestStreak)} />
        <Metric label="Peak hour" value={stats.peakHourLabel} />
        <Metric label="Favorite model" value={stats.favoriteModel} />
      </div>
      <TokenBreakdown
        selected={selectedBreakdown}
        stats={stats}
        onSelect={setSelectedBreakdown}
      />
      <ActivityGrid days={stats.activityDaysByToken[selectedBreakdown]} />
      <p className="profile-token-note">
        You've used ~{formatCompactNumber(stats.localTokens)} tokens locally.
      </p>
    </>
  )
}

function Models({ stats }: { stats: ProfileStats }) {
  const maxTokens = Math.max(1, ...stats.modelBuckets.map((bucket) => bucket.totalTokens))
  const yLabels = createYAxisLabels(maxTokens)

  return (
    <div className="profile-models-view">
      <div className="profile-model-chart">
        <div className="profile-y-axis">
          {yLabels.map((label) => (
            <span key={label}>{label}</span>
          ))}
        </div>
        <div className="profile-bars">
          {stats.modelBuckets.map((bucket) => (
            <div className="profile-bar-column" key={bucket.label}>
              <div className="profile-bar-stack" aria-label={`${bucket.label} tokens`}>
                {bucket.models.map((model, index) => (
                  <span
                    key={model.model}
                    className="profile-bar-segment"
                    style={{
                      height: `${Math.max(4, (model.tokens / maxTokens) * 126)}px`,
                      background: modelColors[index % modelColors.length],
                    }}
                  />
                ))}
              </div>
              <span>{bucket.label}</span>
            </div>
          ))}
        </div>
      </div>
      <div className="profile-model-legend">
        {stats.modelTotals.map((model, index) => (
          <div className="profile-model-row" key={model.model}>
            <span
              className="profile-model-color"
              style={{ background: modelColors[index % modelColors.length] }}
            />
            <span>{model.model}</span>
            <strong>{formatCompactNumber(model.tokens)}</strong>
            <small>{formatPercent(model.tokens, stats.localTokens)}</small>
          </div>
        ))}
      </div>
    </div>
  )
}

function Metric({ label, value }: { label: string; value: string }) {
  return (
    <div className="profile-metric">
      <span>{label}</span>
      <strong>{value}</strong>
    </div>
  )
}

function TokenBreakdown({
  selected,
  stats,
  onSelect,
}: {
  selected: TokenBreakdownType
  stats: ProfileStats
  onSelect: (type: TokenBreakdownType) => void
}) {
  const items = [
    { label: 'All', value: stats.totalTokens, tone: 'all' },
    { label: 'In', value: stats.inputTokens, tone: 'in' },
    { label: 'Out', value: stats.outputTokens, tone: 'out' },
    { label: 'Cached', value: stats.cachedTokens, tone: 'cached' },
    { label: 'No cache', value: stats.noCacheTokens, tone: 'no-cache' },
  ]

  return (
    <section className="profile-token-breakdown" aria-label="Token breakdown">
      <div className="profile-section-title">
        <span>Token breakdown</span>
        <Info size={14} aria-hidden="true" />
      </div>
      <div className="profile-token-breakdown-grid">
        {items.map((item) => (
          <button
            type="button"
            aria-pressed={selected === item.tone}
            className={
              selected === item.tone ?
                'profile-token-breakdown-card active'
              : 'profile-token-breakdown-card'
            }
            data-tone={item.tone}
            key={item.label}
            onClick={() => onSelect(item.tone as TokenBreakdownType)}
          >
            <span>{item.label}</span>
            <strong>{formatCompactNumber(item.value)}</strong>
          </button>
        ))}
      </div>
    </section>
  )
}

function ActivityGrid({ days }: { days: ActivityDay[] }) {
  const maxTokens = Math.max(1, ...days.map((day) => day.tokens))
  return (
    <div className="profile-activity-grid" aria-label="Token activity">
      {days.map((day) => (
        <span
          key={day.key}
          className="profile-activity-cell"
          title={`${day.key}: ${formatNumber(day.tokens)} tokens`}
          style={{ opacity: day.active ? 0.35 + (day.tokens / maxTokens) * 0.65 : 1 }}
          data-active={day.active ? 'true' : 'false'}
        />
      ))}
    </div>
  )
}

interface ProfileStats {
  sessions: number
  messages: number
  inputTokens: number
  outputTokens: number
  totalTokens: number
  cachedTokens: number
  noCacheTokens: number
  activeDays: number
  currentStreak: number
  longestStreak: number
  peakHourLabel: string
  favoriteModel: string
  localTokens: number
  activityDays: ActivityDay[]
  activityDaysByToken: Record<TokenBreakdownType, ActivityDay[]>
  modelBuckets: Array<{
    label: string
    totalTokens: number
    models: Array<{ model: string; tokens: number }>
  }>
  modelTotals: Array<{ model: string; tokens: number }>
}

function createProfileStats(tasks: AgentTask[], range: ProfileRange): ProfileStats {
  const now = new Date()
  const filteredTasks = tasks.filter((task) => isInRange(taskDate(task), now, range))
  const messages = filteredTasks.flatMap((task) =>
    task.messages.filter((message) => isInRange(parseDate(message.createdAt), now, range)),
  )
  const runs = filteredTasks.flatMap((task) => runRecordsForTask(task, now, range))
  const allRuns = tasks.flatMap((task) => runRecordsForTask(task, now, 'all'))
  const inputTokens = runs.reduce((sum, run) => sum + run.inputTokens, 0)
  const outputTokens = runs.reduce((sum, run) => sum + run.outputTokens, 0)
  const totalTokens = runs.reduce((sum, run) => sum + run.totalTokens, 0)
  const cachedTokens = runs.reduce((sum, run) => sum + run.cachedTokens, 0)
  const noCacheTokens = runs.reduce((sum, run) => sum + run.noCacheTokens, 0)
  const localTokens = allRuns.reduce((sum, run) => sum + run.totalTokens, 0)
  const filteredTokensByDay = sumTokensByDay(runs)
  const activityDaysByToken = createActivityDaysByToken(allRuns, now, 'all')
  const activityDays = activityDaysByToken.all
  const activeDayKeys = new Set(
    [...filteredTokensByDay.entries()].filter((entry) => entry[1] > 0).map((entry) => entry[0]),
  )
  const hourCounts = messages.reduce((counts, message) => {
    const date = parseDate(message.createdAt)
    if (date) {
      counts[date.getHours()] += 1
    }
    return counts
  }, Array.from({ length: 24 }, () => 0))
  const peakHour = hourCounts.reduce(
    (bestHour, count, hour) => (count > hourCounts[bestHour] ? hour : bestHour),
    0,
  )
  const filteredModelTotals = sumByModel(runs)
  const modelTotals = sumByModel(allRuns)

  return {
    sessions: filteredTasks.length,
    messages: messages.length,
    inputTokens,
    outputTokens,
    totalTokens,
    cachedTokens,
    noCacheTokens,
    activeDays: activeDayKeys.size,
    currentStreak: calculateCurrentStreak(activeDayKeys, now),
    longestStreak: calculateLongestStreak(activeDayKeys),
    peakHourLabel: formatHour(peakHour),
    favoriteModel: filteredModelTotals[0]?.model ?? 'None',
    localTokens,
    activityDays,
    activityDaysByToken,
    modelBuckets: createModelBuckets(allRuns, 'all'),
    modelTotals,
  }
}

function runRecordsForTask(task: AgentTask, now: Date, range: ProfileRange): RunRecord[] {
  const records: RunRecord[] = []
  for (const message of task.messages) {
    if (message.role !== 'assistant') {
      continue
    }
    const date = parseDate(message.createdAt)
    if (!date || !isInRange(date, now, range)) {
      continue
    }
    const traces = message.traceEvents?.length ? message.traceEvents : task.traceEvents
    records.push(createRunRecord(date, message, traces))
  }
  return records
}

function createRunRecord(
  date: Date,
  message: ChatMessage,
  traces: ToolTraceEvent[],
): RunRecord {
  const chatCompletion = traces.find((event) => event.title === 'chat_completion')
  const input = asRecord(chatCompletion?.input)
  const output = asRecord(chatCompletion?.output)
  const request = asRecord(input.request)
  const response = asRecord(output.response)
  const usage = readRunTokenUsage(output)
  const model =
    stringValue(output.model) ||
    stringValue(request.model) ||
    stringValue(response.model) ||
    'Unknown'
  const inputTokens =
    usage.inputTokens ||
    numberValue(output.inputTokens)
  const outputTokens =
    usage.outputTokens ||
    numberValue(output.outputTokens)
  const totalTokens =
    usage.totalTokens ||
    numberValue(output.totalTokens) ||
    inputTokens + outputTokens ||
    estimateTokensFromMessage(message)
  const cachedTokens = usage.cachedTokens
  const noCacheTokens =
    usage.noCacheTokens ||
    (inputTokens > 0 && cachedTokens > 0 ? Math.max(0, inputTokens - cachedTokens) : 0)

  return {
    date,
    model,
    inputTokens,
    outputTokens,
    totalTokens,
    cachedTokens,
    noCacheTokens,
  }
}

interface RunTokenUsage {
  inputTokens: number
  outputTokens: number
  totalTokens: number
  cachedTokens: number
  noCacheTokens: number
}

interface PartialRunTokenUsage {
  inputTokens: number | null
  outputTokens: number | null
  totalTokens: number | null
  cachedTokens: number | null
  noCacheTokens: number | null
}

function readRunTokenUsage(output: Record<string, unknown>): RunTokenUsage {
  const response = asRecord(output.response)
  const outputBaseResp = firstRecord(output.base_resp, output.baseResp)
  const responseBaseResp = firstRecord(response.base_resp, response.baseResp)
  const candidates = [
    asRecord(response.usage),
    asRecord(responseBaseResp.usage),
    asRecord(outputBaseResp.usage),
    output,
    asRecord(output.usage),
    asRecord(output.tokenUsage),
    response,
    responseBaseResp,
    outputBaseResp,
  ]
  let merged: PartialRunTokenUsage | null = null

  for (const candidate of candidates) {
    const usage = readRunTokenUsageRecord(candidate)
    if (usage) {
      merged = mergeRunTokenUsage(merged, usage)
    }
  }

  if (!merged) {
    return {
      inputTokens: 0,
      outputTokens: 0,
      totalTokens: 0,
      cachedTokens: 0,
      noCacheTokens: 0,
    }
  }

  const inputTokens = merged.inputTokens ?? 0
  const outputTokens = merged.outputTokens ?? 0
  const cachedTokens = merged.cachedTokens ?? 0
  const noCacheTokens =
    merged.noCacheTokens ??
    (inputTokens > 0 && cachedTokens > 0 ? Math.max(0, inputTokens - cachedTokens) : 0)

  return {
    inputTokens,
    outputTokens,
    totalTokens: merged.totalTokens ?? inputTokens + outputTokens,
    cachedTokens,
    noCacheTokens,
  }
}

function readRunTokenUsageRecord(
  record: Record<string, unknown>,
): PartialRunTokenUsage | null {
  const rawInputTokens = firstNullableNumber(record, [
    'inputTokens',
    'input_tokens',
    'promptTokens',
    'prompt_tokens',
    'promptEvalCount',
    'prompt_eval_count',
  ])
  const outputTokens = firstNullableNumber(record, [
    'outputTokens',
    'output_tokens',
    'completionTokens',
    'completion_tokens',
    'evalCount',
    'eval_count',
  ])
  const totalTokens = firstNullableNumber(record, ['totalTokens', 'total_tokens'])
  const details = firstRecord(record.promptTokensDetails, record.prompt_tokens_details)
  const cacheReadTokens = firstNullableNumber(record, [
    'cacheReadInputTokens',
    'cache_read_input_tokens',
  ])
  const cacheCreationTokens = firstNullableNumber(record, [
    'cacheCreationInputTokens',
    'cache_creation_input_tokens',
  ])
  const cachedTokens =
    firstNullableNumber(record, [
      'inputCachedTokens',
      'input_cached_tokens',
      'cachedInputTokens',
      'cached_input_tokens',
    ]) ?? firstNullableNumber(details, ['cachedTokens', 'cached_tokens'])
  const explicitNoCacheTokens = firstNullableNumber(record, [
    'inputUncachedTokens',
    'input_uncached_tokens',
    'uncachedInputTokens',
    'uncached_input_tokens',
  ])
  const hasCacheBreakdown = cacheReadTokens !== null || cacheCreationTokens !== null
  const inputTokens =
    hasCacheBreakdown ?
      sumNullableNumbers([rawInputTokens, cacheCreationTokens, cacheReadTokens])
    : rawInputTokens
  const noCacheTokens =
    explicitNoCacheTokens ??
    (hasCacheBreakdown ?
      sumNullableNumbers([rawInputTokens, cacheCreationTokens])
    : inputTokens !== null && cachedTokens !== null ?
      Math.max(0, inputTokens - cachedTokens)
    : null)

  if (
    inputTokens === null &&
    outputTokens === null &&
    totalTokens === null &&
    cachedTokens === null &&
    noCacheTokens === null
  ) {
    return null
  }

  return {
    inputTokens,
    outputTokens,
    totalTokens,
    cachedTokens: cachedTokens ?? cacheReadTokens,
    noCacheTokens,
  }
}

function mergeRunTokenUsage(
  current: PartialRunTokenUsage | null,
  next: PartialRunTokenUsage,
): PartialRunTokenUsage {
  if (!current) {
    return next
  }

  return {
    inputTokens: current.inputTokens ?? next.inputTokens,
    outputTokens: current.outputTokens ?? next.outputTokens,
    totalTokens: current.totalTokens ?? next.totalTokens,
    cachedTokens: current.cachedTokens ?? next.cachedTokens,
    noCacheTokens: current.noCacheTokens ?? next.noCacheTokens,
  }
}

function createActivityDays(
  tokensByDay: Map<string, number>,
  now: Date,
  range: ProfileRange,
): ActivityDay[] {
  const dayCount = range === '7d' ? 28 : range === '30d' ? 70 : 189
  const today = startOfDay(now)
  return Array.from({ length: dayCount }, (_, index) => {
    const date = addDays(today, index - dayCount + 1)
    const key = dateKey(date)
    const tokens = tokensByDay.get(key) ?? 0
    return {
      key,
      date,
      tokens,
      active: tokens > 0,
    }
  })
}

function createActivityDaysByToken(
  runs: RunRecord[],
  now: Date,
  range: ProfileRange,
): Record<TokenBreakdownType, ActivityDay[]> {
  return {
    all: createActivityDays(sumTokensByDay(runs, 'all'), now, range),
    in: createActivityDays(sumTokensByDay(runs, 'in'), now, range),
    out: createActivityDays(sumTokensByDay(runs, 'out'), now, range),
    cached: createActivityDays(sumTokensByDay(runs, 'cached'), now, range),
    'no-cache': createActivityDays(sumTokensByDay(runs, 'no-cache'), now, range),
  }
}

function createModelBuckets(runs: RunRecord[], range: ProfileRange) {
  const bucketCount = range === '7d' ? 7 : range === '30d' ? 8 : 8
  const sortedRuns = [...runs].sort((left, right) => left.date.getTime() - right.date.getTime())
  const buckets = new Map<string, Map<string, number>>()
  const recentRuns = sortedRuns.slice(-Math.max(bucketCount, sortedRuns.length))

  for (const run of recentRuns) {
    const label = formatDateLabel(run.date)
    const modelMap = buckets.get(label) ?? new Map<string, number>()
    modelMap.set(run.model, (modelMap.get(run.model) ?? 0) + run.totalTokens)
    buckets.set(label, modelMap)
  }

  return [...buckets.entries()].slice(-bucketCount).map(([label, modelMap]) => {
    const models = [...modelMap.entries()]
      .map(([model, tokens]) => ({ model, tokens }))
      .sort((left, right) => right.tokens - left.tokens)
    return {
      label,
      totalTokens: models.reduce((sum, model) => sum + model.tokens, 0),
      models,
    }
  })
}

function sumTokensByDay(
  runs: RunRecord[],
  tokenType: TokenBreakdownType = 'all',
): Map<string, number> {
  const totals = new Map<string, number>()
  for (const run of runs) {
    const key = dateKey(run.date)
    totals.set(key, (totals.get(key) ?? 0) + runTokenValue(run, tokenType))
  }
  return totals
}

function runTokenValue(run: RunRecord, tokenType: TokenBreakdownType): number {
  if (tokenType === 'in') {
    return run.inputTokens
  }
  if (tokenType === 'out') {
    return run.outputTokens
  }
  if (tokenType === 'cached') {
    return run.cachedTokens
  }
  if (tokenType === 'no-cache') {
    return run.noCacheTokens
  }
  return run.totalTokens
}

function sumByModel(runs: RunRecord[]): Array<{ model: string; tokens: number }> {
  const totals = new Map<string, number>()
  for (const run of runs) {
    totals.set(run.model, (totals.get(run.model) ?? 0) + run.totalTokens)
  }
  return [...totals.entries()]
    .map(([model, tokens]) => ({ model, tokens }))
    .sort((left, right) => right.tokens - left.tokens)
}

function calculateCurrentStreak(activeDayKeys: Set<string>, now: Date): number {
  let streak = 0
  let date = startOfDay(now)
  while (activeDayKeys.has(dateKey(date))) {
    streak += 1
    date = addDays(date, -1)
  }
  return streak
}

function calculateLongestStreak(activeDayKeys: Set<string>): number {
  let longest = 0
  let current = 0
  const keys = [...activeDayKeys].sort()
  let previous: Date | null = null
  for (const key of keys) {
    const date = parseDate(key)
    if (!date) {
      continue
    }
    current =
      previous && dateKey(addDays(previous, 1)) === key ? current + 1 : 1
    longest = Math.max(longest, current)
    previous = date
  }
  return longest
}

function isInRange(date: Date | null, now: Date, range: ProfileRange): boolean {
  if (!date) {
    return false
  }
  if (range === 'all') {
    return true
  }
  const days = range === '7d' ? 7 : 30
  return date.getTime() >= addDays(startOfDay(now), -days + 1).getTime()
}

function taskDate(task: AgentTask): Date | null {
  return parseDate(task.createdAt) ?? parseDate(task.messages[0]?.createdAt)
}

function parseDate(value: string | undefined): Date | null {
  if (!value) {
    return null
  }
  const date = new Date(value)
  return Number.isFinite(date.getTime()) ? date : null
}

function startOfDay(date: Date): Date {
  return new Date(date.getFullYear(), date.getMonth(), date.getDate())
}

function addDays(date: Date, days: number): Date {
  const next = new Date(date)
  next.setDate(next.getDate() + days)
  return next
}

function dateKey(date: Date): string {
  return `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, '0')}-${String(
    date.getDate(),
  ).padStart(2, '0')}`
}

function asRecord(value: unknown): Record<string, unknown> {
  if (value && typeof value === 'object' && !Array.isArray(value)) {
    return value as Record<string, unknown>
  }
  return {}
}

function firstRecord(...values: unknown[]): Record<string, unknown> {
  for (const value of values) {
    if (value && typeof value === 'object' && !Array.isArray(value)) {
      return value as Record<string, unknown>
    }
  }
  return {}
}

function stringValue(value: unknown): string {
  return typeof value === 'string' && value.trim().length > 0 ? value : ''
}

function numberValue(value: unknown): number {
  if (typeof value === 'number' && Number.isFinite(value)) {
    return value
  }
  if (typeof value === 'string') {
    const parsed = Number(value)
    return Number.isFinite(parsed) ? parsed : 0
  }
  return 0
}

function firstNullableNumber(record: Record<string, unknown>, keys: string[]): number | null {
  for (const key of keys) {
    const value = nullableNumberValue(record[key])
    if (value !== null) {
      return value
    }
  }
  return null
}

function nullableNumberValue(value: unknown): number | null {
  if (typeof value === 'number' && Number.isFinite(value)) {
    return value
  }
  if (typeof value === 'string' && value.trim().length > 0) {
    const parsed = Number(value)
    return Number.isFinite(parsed) ? parsed : null
  }
  return null
}

function sumNullableNumbers(values: Array<number | null>): number | null {
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

function estimateTokensFromMessage(message: ChatMessage): number {
  return Math.max(1, Math.ceil(message.content.length / 4))
}

function formatNumber(value: number): string {
  return new Intl.NumberFormat().format(value)
}

function formatCompactNumber(value: number): string {
  if (value >= 1_000_000) {
    return `${trimDecimal(value / 1_000_000)}M`
  }
  if (value >= 1_000) {
    return `${trimDecimal(value / 1_000)}K`
  }
  return formatNumber(value)
}

function trimDecimal(value: number): string {
  return value.toFixed(value >= 10 ? 1 : 2).replace(/\.?0+$/, '')
}

function formatDays(value: number): string {
  return `${value}d`
}

function formatHour(hour: number): string {
  const suffix = hour >= 12 ? 'PM' : 'AM'
  const display = hour % 12 === 0 ? 12 : hour % 12
  return `${display} ${suffix}`
}

function formatDateLabel(date: Date): string {
  return date.toLocaleDateString([], { month: 'short', day: 'numeric' })
}

function formatPercent(value: number, total: number): string {
  if (total <= 0) {
    return '0%'
  }
  return `${((value / total) * 100).toFixed(1)}%`
}

function createYAxisLabels(maxTokens: number): string[] {
  return [maxTokens, maxTokens * 0.75, maxTokens * 0.5, maxTokens * 0.25, 0].map(
    (value) => formatCompactNumber(Math.round(value)),
  )
}

export default Profile
