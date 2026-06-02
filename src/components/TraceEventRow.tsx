import { useState } from 'react'
import {
  CheckCircle2,
  ChevronDown,
  ChevronRight,
  CircleAlert,
  Clock3,
  LoaderCircle,
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
      {open ? <pre className="trace-raw-code">{JSON.stringify(value, null, 2)}</pre> : null}
    </div>
  )
}

export default TraceEventRow
