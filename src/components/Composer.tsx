import {
  ArrowUp,
  Check,
  ChevronDown,
  Mic,
  Plus,
  X,
  Settings2,
} from 'lucide-react'
import { useEffect, useMemo, useRef, useState } from 'react'
import type { ChangeEvent, ClipboardEvent, KeyboardEvent } from 'react'
import type { ProviderConfig } from '../types/provider'
import type { MessageAttachment } from '../types/task'
import { getSelectableModels } from '../utils/providerModels'

interface ComposerProps {
  providers: ProviderConfig[]
  busy: boolean
  value: string
  onChange: (value: string) => void
  onSend: (
    prompt: string,
    selection: { providerId: string | null; credentialId: string | null; modelId: string | null },
    attachments: MessageAttachment[],
  ) => void
}

function Composer({
  providers,
  busy,
  value,
  onChange,
  onSend,
}: ComposerProps) {
  const textareaRef = useRef<HTMLTextAreaElement>(null)
  const fileInputRef = useRef<HTMLInputElement>(null)
  const modelPickerRef = useRef<HTMLDivElement>(null)
  const [selectedModelId, setSelectedModelId] = useState('')
  const [modelMenuOpen, setModelMenuOpen] = useState(false)
  const [attachments, setAttachments] = useState<MessageAttachment[]>([])
  const [attachmentError, setAttachmentError] = useState('')
  const selectableModels = useMemo(() => getSelectableModels(providers), [providers])
  const selectedModel =
    selectableModels.find((model) => model.id === selectedModelId) ??
    selectableModels[0] ??
    null
  const selectedModelTriggerLabel = selectedModel?.modelName
  const canSend =
    (value.trim().length > 0 || attachments.length > 0) && !busy && selectedModel !== null

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
    onSend(
      value.trim(),
      {
        providerId: selectedModel?.providerId ?? null,
        credentialId: selectedModel?.credentialId ?? null,
        modelId: selectedModel?.modelId ?? null,
      },
      attachments,
    )
    onChange('')
    setAttachments([])
    setAttachmentError('')
  }

  const handleKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (event.key === 'Enter' && !event.shiftKey) {
      event.preventDefault()
      send()
    }
  }

  const addImageFiles = async (files: File[]) => {
    const images = files.filter((file) => file.type.startsWith('image/'))
    if (images.length === 0) {
      return
    }

    setAttachmentError('')
    try {
      const nextAttachments = await Promise.all(images.map(fileToImageAttachment))
      setAttachments((current) => [...current, ...nextAttachments])
    } catch (caught) {
      setAttachmentError(caught instanceof Error ? caught.message : String(caught))
    }
  }

  const handlePaste = (event: ClipboardEvent<HTMLTextAreaElement>) => {
    const files = Array.from(event.clipboardData.files)
    const images = files.filter((file) => file.type.startsWith('image/'))
    if (images.length === 0) {
      return
    }
    event.preventDefault()
    void addImageFiles(images)
  }

  const handleFileChange = (event: ChangeEvent<HTMLInputElement>) => {
    const files = Array.from(event.target.files ?? [])
    void addImageFiles(files)
    event.target.value = ''
  }

  return (
    <div className="composer">
      <div className="composer-surface">
        {attachments.length > 0 ? (
          <div className="composer-attachments" aria-label="Image attachments">
            {attachments.map((attachment) => (
              <div className="composer-attachment" key={attachment.id}>
                <img src={attachment.dataUrl} alt={attachment.name} />
                <button
                  type="button"
                  className="composer-attachment-remove"
                  onClick={() =>
                    setAttachments((current) =>
                      current.filter((item) => item.id !== attachment.id),
                    )
                  }
                  aria-label={`Remove ${attachment.name}`}
                  title="Remove image"
                >
                  <X size={12} aria-hidden="true" />
                </button>
              </div>
            ))}
          </div>
        ) : null}
        {attachmentError ? (
          <div className="composer-attachment-error">{attachmentError}</div>
        ) : null}
        <textarea
          ref={textareaRef}
          className="composer-input"
          value={value}
          onChange={(event) => onChange(event.target.value)}
          onKeyDown={handleKeyDown}
          onPaste={handlePaste}
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
              onClick={() => fileInputRef.current?.click()}
              disabled={busy}
            >
              <Plus size={18} aria-hidden="true" />
            </button>
            <input
              ref={fileInputRef}
              type="file"
              accept="image/*"
              multiple
              className="composer-file-input"
              onChange={handleFileChange}
              tabIndex={-1}
            />
            <button type="button" className="composer-mode-button" title="Tools">
              <Settings2 size={15} aria-hidden="true" />
              <span>Custom</span>
              <span className="button-caret" aria-hidden="true">
                v
              </span>
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
                <span>{selectedModelTriggerLabel ?? 'No enabled model'}</span>
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
                      <span>{model.label}</span>
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

function fileToImageAttachment(file: File): Promise<MessageAttachment> {
  const maxImageBytes = 8 * 1024 * 1024
  if (file.size > maxImageBytes) {
    throw new Error(`Image is too large: ${file.name}`)
  }

  return new Promise((resolve, reject) => {
    const reader = new FileReader()
    reader.onload = () => {
      const result = reader.result
      if (typeof result !== 'string') {
        reject(new Error(`Image read failed: ${file.name}`))
        return
      }
      resolve({
        id: crypto.randomUUID(),
        kind: 'image',
        name: file.name || 'pasted-image.png',
        mimeType: file.type || 'image/png',
        dataUrl: result,
      })
    }
    reader.onerror = () => reject(new Error(`Image read failed: ${file.name}`))
    reader.readAsDataURL(file)
  })
}

export default Composer
