import {
  ArrowUp,
  Calculator,
  Check,
  ChevronDown,
  Mic,
  Plus,
  Settings2,
} from 'lucide-react'
import { useEffect, useMemo, useRef, useState } from 'react'
import type { KeyboardEvent } from 'react'
import type { ProviderConfig } from '../types/provider'
import { getSelectableModels } from '../utils/providerModels'

interface ComposerProps {
  providers: ProviderConfig[]
  busy: boolean
  value: string
  onChange: (value: string) => void
  onSend: (
    prompt: string,
    selection: { providerId: string | null; modelId: string | null },
  ) => void
  onRunToolCallTest: (
    selection: { providerId: string | null; modelId: string | null },
  ) => void
}

function Composer({
  providers,
  busy,
  value,
  onChange,
  onSend,
  onRunToolCallTest,
}: ComposerProps) {
  const textareaRef = useRef<HTMLTextAreaElement>(null)
  const modelPickerRef = useRef<HTMLDivElement>(null)
  const [selectedModelId, setSelectedModelId] = useState('')
  const [modelMenuOpen, setModelMenuOpen] = useState(false)
  const selectableModels = useMemo(() => getSelectableModels(providers), [providers])
  const selectedModel =
    selectableModels.find((model) => model.id === selectedModelId) ??
    selectableModels[0] ??
    null
  const canSend = value.trim().length > 0 && !busy && selectedModel !== null
  const canRunToolCallTest = !busy && selectedModel !== null

  useEffect(() => {
    const textarea = textareaRef.current
    if (!textarea) {
      return
    }
    textarea.style.height = '0px'
    const nextHeight = Math.min(Math.max(textarea.scrollHeight, 42), 128)
    textarea.style.height = `${nextHeight}px`
  }, [value])

  useEffect(() => {
    if (!modelMenuOpen) {
      return
    }

    const closeOnOutsideClick = (event: Event) => {
      if (
        modelPickerRef.current &&
        event.target instanceof Node &&
        !modelPickerRef.current.contains(event.target)
      ) {
        setModelMenuOpen(false)
      }
    }

    document.addEventListener('pointerdown', closeOnOutsideClick)
    return () => document.removeEventListener('pointerdown', closeOnOutsideClick)
  }, [modelMenuOpen])

  const send = () => {
    if (!canSend) {
      return
    }
    onSend(value.trim(), {
      providerId: selectedModel?.providerId ?? null,
      modelId: selectedModel?.modelId ?? null,
    })
    onChange('')
  }

  const runToolCallTest = () => {
    if (!canRunToolCallTest) {
      return
    }
    onRunToolCallTest({
      providerId: selectedModel?.providerId ?? null,
      modelId: selectedModel?.modelId ?? null,
    })
  }

  const handleKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (event.key === 'Enter' && !event.shiftKey) {
      event.preventDefault()
      send()
    }
  }

  return (
    <div className="composer">
      <div className="composer-surface">
        <textarea
          ref={textareaRef}
          className="composer-input"
          value={value}
          onChange={(event) => onChange(event.target.value)}
          onKeyDown={handleKeyDown}
          placeholder="Ask for follow-up changes"
          rows={1}
          disabled={busy}
        />
        <div className="composer-bottom-bar">
          <div className="composer-tool-group">
            <button
              type="button"
              className="composer-icon-button"
              title="Attach file/image"
              aria-label="Attach file/image"
            >
              <Plus size={18} aria-hidden="true" />
            </button>
            <button type="button" className="composer-mode-button" title="Tools">
              <Settings2 size={15} aria-hidden="true" />
              <span>Custom</span>
              <span className="button-caret" aria-hidden="true">
                v
              </span>
            </button>
            <button
              type="button"
              className="composer-mode-button composer-test-button"
              title={
                selectedModel ? 'Run Tool Call Test' : 'Enable a provider in Settings'
              }
              onClick={runToolCallTest}
              disabled={!canRunToolCallTest}
            >
              <Calculator size={15} aria-hidden="true" />
              <span>Run Tool Call Test</span>
            </button>
          </div>
          <div className="composer-action-group">
            <div className="composer-model-picker" ref={modelPickerRef}>
              <button
                type="button"
                className="composer-model-trigger"
                aria-haspopup="listbox"
                aria-expanded={modelMenuOpen}
                onClick={() => setModelMenuOpen((open) => !open)}
                disabled={selectableModels.length === 0}
                title={selectedModel?.label ?? 'Enable a provider in Settings'}
              >
                <span>{selectedModel?.modelId ?? 'No enabled model'}</span>
                <ChevronDown size={14} aria-hidden="true" />
              </button>
              {modelMenuOpen ? (
                <div className="composer-model-menu" role="listbox" aria-label="Model">
                  {selectableModels.map((model) => (
                    <button
                      type="button"
                      key={model.id}
                      className={
                        model.id === selectedModel?.id ?
                          'composer-model-option selected'
                        : 'composer-model-option'
                      }
                      role="option"
                      aria-selected={model.id === selectedModel?.id}
                      onClick={() => {
                        setSelectedModelId(model.id)
                        setModelMenuOpen(false)
                      }}
                    >
                      <span>{model.modelId}</span>
                      {model.id === selectedModel?.id ? (
                        <Check size={15} aria-hidden="true" />
                      ) : null}
                    </button>
                  ))}
                </div>
              ) : null}
            </div>
            <button
              type="button"
              className="composer-icon-button"
              title="Voice input"
              aria-label="Voice input"
            >
              <Mic size={16} aria-hidden="true" />
            </button>
            <button
              type="button"
              className="composer-send-button"
              onClick={send}
              disabled={!canSend}
              aria-label={busy ? 'Running' : 'Send'}
              title={
                busy ? 'Running'
                : selectedModel ? 'Send'
                : 'Enable a provider in Settings'
              }
            >
              {busy ? (
                <span className="send-spinner" aria-hidden="true" />
              ) : (
                <ArrowUp size={18} aria-hidden="true" />
              )}
            </button>
          </div>
        </div>
      </div>
    </div>
  )
}

export default Composer
