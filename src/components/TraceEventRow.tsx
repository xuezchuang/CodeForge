import { useEffect, useState } from 'react'
import type { CSSProperties } from 'react'
import {
  CheckCircle2,
  ChevronDown,
  ChevronRight,
  CircleAlert,
  Clock3,
  LoaderCircle,
  Maximize2,
  Minimize2,
  WrapText,
} from 'lucide-react'
import type { TraceStepViewModel } from './traceViewModel'

interface TraceEventRowProps {
  step: TraceStepViewModel
}

function TraceEventRow({ step }: TraceEventRowProps) {
  const [expanded, setExpanded] = useState(false)
  const [rawInputOpen, setRawInputOpen] = useState(false)
  const [rawOutputOpen, setRawOutputOpen] = useState(false)
  const rowClass =
    step.status === 'failed' || step.status === 'warning' ?
      `drawer-trace-row ${step.status}`
    : 'drawer-trace-row'

  return (
    <article className={rowClass}>
      <button
        type="button"
        className="drawer-trace-summary"
        onClick={() => setExpanded((current) => !current)}
        aria-expanded={expanded}
      >
        {expanded ? (
          <ChevronDown className="trace-chevron" size={16} aria-hidden="true" />
        ) : (
          <ChevronRight className="trace-chevron" size={16} aria-hidden="true" />
        )}
        <span className="trace-step">{step.index}</span>
        <StatusIcon status={step.status} />
        <span className="trace-title-block">
          <span className="trace-title">{step.title}</span>
          {step.shortSummary ? (
            <span className="trace-row-summary">{step.shortSummary}</span>
          ) : null}
        </span>
        <span className={`trace-status ${step.status}`}>{step.status}</span>
        <span className="trace-duration">
          <Clock3 size={13} aria-hidden="true" />
          {step.durationMs ?? 0} ms
        </span>
      </button>

      {expanded ? (
        <div className="trace-details">
          <RawToggle
            label="View raw input"
            value={step.rawInput}
            open={rawInputOpen}
            onToggle={() => setRawInputOpen((current) => !current)}
          />
          <RawToggle
            label="View raw output"
            value={step.rawOutput}
            open={rawOutputOpen}
            onToggle={() => setRawOutputOpen((current) => !current)}
          />
        </div>
      ) : null}
    </article>
  )
}

function StatusIcon({ status }: { status: TraceStepViewModel['status'] }) {
  if (status === 'failed') {
    return <CircleAlert className="status-icon failed" size={16} aria-hidden="true" />
  }
  if (status === 'warning') {
    return <CircleAlert className="status-icon warning" size={16} aria-hidden="true" />
  }
  if (status === 'running') {
    return <LoaderCircle className="status-icon running" size={16} aria-hidden="true" />
  }
  return <CheckCircle2 className="status-icon success" size={16} aria-hidden="true" />
}

function RawToggle({
  label,
  value,
  open,
  onToggle,
}: {
  label: string
  value: unknown | null
  open: boolean
  onToggle: () => void
}) {
  if (value === null || value === undefined) {
    return null
  }

  return (
    <div className="trace-raw">
      <button type="button" className="trace-raw-toggle" onClick={onToggle}>
        {open ? label.replace('View', 'Hide') : label}
      </button>
      {open ? <JsonTree value={orderRawJson(value)} /> : null}
    </div>
  )
}

const preferredRawJsonKeyOrder = [
  'model',
  'messages',
  'tools',
  'tool_choice',
  'temperature',
  'stream',
  'role',
  'content',
  'tool_calls',
  'tool_call_id',
  'id',
  'type',
  'function',
  'name',
  'arguments',
  'projectId',
  'projectName',
  'prompt',
  'provider',
  'baseUrl',
  'request',
  'response',
  'message',
  'toolName',
  'error',
  'recoveryHint',
  'file',
  'path',
  'root',
  'line',
  'column',
  'text',
  'before',
  'after',
]

function JsonTree({ value }: { value: unknown }) {
  const [expansionCommand, setExpansionCommand] = useState<JsonTreeExpansionCommand>({
    open: null,
    version: 0,
  })
  const [lineWrap, setLineWrap] = useState(true)
  const complex = isComplexJson(value)
  const treeClass =
    lineWrap ? 'trace-raw-code trace-json-tree is-wrapped' : 'trace-raw-code trace-json-tree'

  const setAllNodesOpen = (open: boolean) => {
    setExpansionCommand((current) => ({
      open,
      version: current.version + 1,
    }))
  }

  return (
    <div className={treeClass}>
      {complex ? (
        <div className="trace-json-tree-toolbar" aria-label="JSON tree controls">
          <button
            type="button"
            className={
              lineWrap ? 'trace-json-tree-action active' : 'trace-json-tree-action'
            }
            onClick={() => setLineWrap((current) => !current)}
            title={lineWrap ? 'Disable line wrap' : 'Enable line wrap'}
            aria-label={lineWrap ? 'Disable JSON line wrap' : 'Enable JSON line wrap'}
            aria-pressed={lineWrap}
          >
            <WrapText size={14} aria-hidden="true" />
          </button>
          <button
            type="button"
            className="trace-json-tree-action"
            onClick={() => setAllNodesOpen(false)}
            title="Collapse all"
            aria-label="Collapse all JSON nodes"
          >
            <Minimize2 size={14} aria-hidden="true" />
          </button>
          <button
            type="button"
            className="trace-json-tree-action"
            onClick={() => setAllNodesOpen(true)}
            title="Expand all"
            aria-label="Expand all JSON nodes"
          >
            <Maximize2 size={14} aria-hidden="true" />
          </button>
        </div>
      ) : null}
      <JsonNode value={value} depth={0} expansionCommand={expansionCommand} />
    </div>
  )
}

type JsonTreeExpansionCommand = {
  open: boolean | null
  version: number
}

function JsonNode({
  label,
  value,
  depth,
  expansionCommand,
}: {
  label?: string
  value: unknown
  depth: number
  expansionCommand: JsonTreeExpansionCommand
}) {
  const complex = isComplexJson(value)
  const [open, setOpen] = useState(() => expansionCommand.open ?? depth === 0)

  useEffect(() => {
    if (expansionCommand.open !== null) {
      setOpen(expansionCommand.open)
    }
  }, [expansionCommand.open, expansionCommand.version])

  if (!complex) {
    return (
      <div className="trace-json-node" style={jsonNodeStyle(depth)}>
        {label ? <span className="trace-json-key">{label}: </span> : null}
        <JsonPrimitive value={value} />
      </div>
    )
  }

  const entries = jsonEntries(value)
  const bracket = Array.isArray(value) ? ['[', ']'] : ['{', '}']
  const summary = Array.isArray(value) ? `${entries.length} items` : `${entries.length} keys`

  return (
    <div className="trace-json-group">
      <button
        type="button"
        className="trace-json-node trace-json-branch"
        style={jsonNodeStyle(depth)}
        onClick={() => setOpen((current) => !current)}
        aria-expanded={open}
      >
        {open ? (
          <ChevronDown size={14} aria-hidden="true" />
        ) : (
          <ChevronRight size={14} aria-hidden="true" />
        )}
        {label ? <span className="trace-json-key">{label}: </span> : null}
        <span>{bracket[0]}</span>
        {!open ? <span className="trace-json-muted"> {summary} </span> : null}
        {!open ? <span>{bracket[1]}</span> : null}
      </button>
      {open ? (
        <div className="trace-json-children">
          {entries.map(([entryLabel, entryValue]) => (
            <JsonNode
              key={entryLabel}
              label={entryLabel}
              value={entryValue}
              depth={depth + 1}
              expansionCommand={expansionCommand}
            />
          ))}
          <div className="trace-json-node trace-json-close" style={jsonNodeStyle(depth)}>
            {bracket[1]}
          </div>
        </div>
      ) : null}
    </div>
  )
}

function JsonPrimitive({ value }: { value: unknown }) {
  if (typeof value === 'string') {
    return <JsonString value={value} />
  }
  if (typeof value === 'number') {
    return <span className="trace-json-number">{String(value)}</span>
  }
  if (typeof value === 'boolean') {
    return <span className="trace-json-boolean">{String(value)}</span>
  }
  if (value === null) {
    return <span className="trace-json-null">null</span>
  }
  return <span>{JSON.stringify(value)}</span>
}

function JsonString({ value }: { value: string }) {
  const lines = value.split(/\r\n|\r|\n/)
  if (lines.length === 1) {
    return <span className="trace-json-string">{JSON.stringify(value)}</span>
  }

  return (
    <span className="trace-json-string trace-json-string-multiline">
      <span>"{escapeJsonStringContent(lines[0])}</span>
      {lines.slice(1).map((line, index) => (
        <span key={index}>
          <br />
          {escapeJsonStringContent(line)}
        </span>
      ))}
      <span>"</span>
    </span>
  )
}

function escapeJsonStringContent(value: string): string {
  return JSON.stringify(value).slice(1, -1)
}

function jsonNodeStyle(depth: number): CSSProperties {
  return { '--json-depth': depth } as CSSProperties
}

function isComplexJson(value: unknown): boolean {
  return Array.isArray(value) || isRecord(value)
}

function jsonEntries(value: unknown): Array<[string, unknown]> {
  if (Array.isArray(value)) {
    return value.map((entry, index) => [String(index), entry])
  }
  if (isRecord(value)) {
    return Object.entries(value)
  }
  return []
}

function orderRawJson(value: unknown): unknown {
  if (Array.isArray(value)) {
    return value.map((item) => orderRawJson(item))
  }

  if (!isRecord(value)) {
    return value
  }

  const orderedEntries: [string, unknown][] = []
  const usedKeys = new Set<string>()

  for (const key of preferredRawJsonKeyOrder) {
    if (Object.prototype.hasOwnProperty.call(value, key)) {
      orderedEntries.push([key, orderRawJson(value[key])])
      usedKeys.add(key)
    }
  }

  for (const [key, entry] of Object.entries(value)) {
    if (!usedKeys.has(key)) {
      orderedEntries.push([key, orderRawJson(entry)])
    }
  }

  return Object.fromEntries(orderedEntries)
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
}

export default TraceEventRow
