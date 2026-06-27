import {
  ArrowUp,
  Check,
  ChevronDown,
  ChevronRight,
  Mic,
  Plus,
  X,
  Settings2,
} from 'lucide-react'
import { useEffect, useMemo, useRef, useState } from 'react'
import type { ChangeEvent, ClipboardEvent, KeyboardEvent } from 'react'
import type { ModelReasoningMode, ProviderConfig, ProviderModel } from '../types/provider'
import type { MessageAttachment } from '../types/task'
import {
  getDefaultSelectableModel,
  getSelectableModels,
  type SelectableModel,
} from '../utils/providerModels'

interface ComposerProps {
  providers: ProviderConfig[]
  busy: boolean
  sendBlocked?: boolean
  sendBlockTitle?: string
  value: string
  onChange: (value: string) => void
  onSend: (
    prompt: string,
    selection: {
      providerId: string | null
      credentialId: string | null
      modelId: string | null
      reasoningEffort: string | null
    },
    attachments: MessageAttachment[],
  ) => void
  onModelSelectionChange?: (selection: {
    providerId: string
    credentialId: string | null
    modelId: string
    reasoningEffort: string | null
  }) => void
}

type ReasoningChoice = {
  value: string
  label: string
  description: string
}

type SlashCommand = {
  command: string
  title: string
  description: string
}

const slashCommands: SlashCommand[] = [
  {
    command: '/init',
    title: 'Init AI context',
    description: 'Create or update doc/ai-context for this workspace.',
  },
]

function Composer({
  providers,
  busy,
  sendBlocked = false,
  sendBlockTitle = 'Send unavailable',
  value,
  onChange,
  onSend,
  onModelSelectionChange,
}: ComposerProps) {
  const textareaRef = useRef<HTMLTextAreaElement>(null)
  const fileInputRef = useRef<HTMLInputElement>(null)
  const pickerRef = useRef<HTMLDivElement>(null)
  const [selectedModelId, setSelectedModelId] = useState('')
  const [selectedReasoning, setSelectedReasoning] = useState('')
  const [pickerOpen, setPickerOpen] = useState(false)
  const [reasoningSubmenuOpen, setReasoningSubmenuOpen] = useState(false)
  const [attachments, setAttachments] = useState<MessageAttachment[]>([])
  const [attachmentError, setAttachmentError] = useState('')
  const [composerFocused, setComposerFocused] = useState(false)
  const [dismissedSlashValue, setDismissedSlashValue] = useState('')
  const selectableModels = useMemo(() => getSelectableModels(providers), [providers])
  const defaultSelectableModel = useMemo(
    () => getDefaultSelectableModel(providers, selectableModels),
    [providers, selectableModels],
  )
  const selectedModel =
    selectableModels.find((model) => model.id === selectedModelId) ??
    defaultSelectableModel ??
    null
  const selectedProviderModel = useMemo(
    () => resolveProviderModel(selectedModel, providers),
    [selectedModel, providers],
  )
  const selectedModelReasoningMode = useMemo(
    () => resolveReasoningMode(selectedModel, providers, selectedProviderModel),
    [selectedModel, providers, selectedProviderModel],
  )
  const selectedModelDefaultReasoning = useMemo(
    () => resolveDefaultReasoning(selectedModel, providers, selectedProviderModel),
    [selectedModel, providers, selectedProviderModel],
  )
  const reasoningChoices = useMemo(
    () => buildReasoningChoices(selectedModelReasoningMode, selectedProviderModel),
    [selectedModelReasoningMode, selectedProviderModel],
  )
  const slashMatches = useMemo(() => matchingSlashCommands(value), [value])
  const showSlashMenu =
    composerFocused &&
    !busy &&
    value !== dismissedSlashValue &&
    slashMatches.length > 0

  useEffect(() => {
    if (reasoningChoices.length === 0) {
      if (selectedReasoning !== '') {
        setSelectedReasoning('')
      }
      if (reasoningSubmenuOpen) {
        setReasoningSubmenuOpen(false)
      }
      return
    }
    if (
      selectedReasoning === '' ||
      !reasoningChoices.some((choice) => choice.value === selectedReasoning)
    ) {
      // Prefer the model's configured default so admins can flip it via
      // settings.json (matching the CLI behavior). Fall back to the first
      // choice when the config omits a usable default.
      const fromConfig = reasoningChoices.find(
        (choice) => choice.value === selectedModelDefaultReasoning,
      )
      const initial = fromConfig?.value ?? reasoningChoices[0]?.value ?? ''
      setSelectedReasoning(initial)
    }
  }, [
    reasoningChoices,
    selectedReasoning,
    selectedModelDefaultReasoning,
    reasoningSubmenuOpen,
  ])

  useEffect(() => {
    const textarea = textareaRef.current
    if (!textarea) {
      return
    }
    textarea.style.height = '0px'
    const nextHeight = Math.min(Math.max(textarea.scrollHeight, 36), 128)
    textarea.style.height = `${nextHeight}px`
  }, [value])

  useEffect(() => {
    if (!pickerOpen) {
      return
    }

    const closeOnOutsideClick = (event: Event) => {
      if (
        pickerRef.current &&
        event.target instanceof Node &&
        !pickerRef.current.contains(event.target)
      ) {
        setPickerOpen(false)
        setReasoningSubmenuOpen(false)
      }
    }

    document.addEventListener('pointerdown', closeOnOutsideClick)
    return () => document.removeEventListener('pointerdown', closeOnOutsideClick)
  }, [pickerOpen])

  const triggerReasoningLabel = useMemo(() => {
    return reasoningChoices.find((choice) => choice.value === selectedReasoning)?.label ?? ''
  }, [reasoningChoices, selectedReasoning])

  const triggerTitle = useMemo(() => {
    if (!selectedModel) {
      return 'No enabled model'
    }
    if (!triggerReasoningLabel) {
      return selectedModel.modelName
    }
    return `${selectedModel.modelName} ${triggerReasoningLabel}`
  }, [selectedModel, triggerReasoningLabel])

  const openPicker = () => {
    setPickerOpen((open) => {
      const nextOpen = !open
      if (nextOpen) {
        setReasoningSubmenuOpen(false)
      }
      return nextOpen
    })
  }

  const canSend =
    (value.trim().length > 0 || attachments.length > 0) &&
    !busy &&
    !sendBlocked &&
    selectedModel !== null

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
        reasoningEffort: reasoningChoices.length > 0 ? selectedReasoning : null,
      },
      attachments,
    )
    onChange('')
    setAttachments([])
    setAttachmentError('')
  }

  const selectSlashCommand = (command: SlashCommand) => {
    onChange(command.command)
    setDismissedSlashValue('')
    window.requestAnimationFrame(() => textareaRef.current?.focus())
  }

  const handleKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (showSlashMenu) {
      const firstCommand = slashMatches[0]
      if (event.key === 'Escape') {
        event.preventDefault()
        setDismissedSlashValue(value)
        return
      }
      if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
        event.preventDefault()
        return
      }
      if (
        firstCommand &&
        (event.key === 'Tab' || (event.key === 'Enter' && value.trim() !== firstCommand.command))
      ) {
        event.preventDefault()
        selectSlashCommand(firstCommand)
        return
      }
    }
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
          onChange={(event) => {
            setDismissedSlashValue('')
            onChange(event.target.value)
          }}
          onKeyDown={handleKeyDown}
          onPaste={handlePaste}
          onFocus={() => setComposerFocused(true)}
          onBlur={() => setComposerFocused(false)}
          placeholder="Ask for follow-up changes"
          rows={1}
          disabled={busy}
        />
        {showSlashMenu ? (
          <div className="composer-slash-menu" role="listbox" aria-label="Slash commands">
            {slashMatches.map((command) => (
              <button
                type="button"
                className="composer-slash-command"
                key={command.command}
                onMouseDown={(event) => event.preventDefault()}
                onClick={() => selectSlashCommand(command)}
              >
                <span className="composer-slash-command-name">{command.command}</span>
                <span className="composer-slash-command-copy">
                  <strong>{command.title}</strong>
                  <small>{command.description}</small>
                </span>
              </button>
            ))}
          </div>
        ) : null}
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
            <div className="composer-model-picker" ref={pickerRef}>
              <button
                type="button"
                className="composer-model-trigger"
                aria-haspopup="dialog"
                aria-expanded={pickerOpen}
                onClick={openPicker}
                disabled={selectableModels.length === 0}
                title={selectedModel ? triggerTitle : 'Enable a provider in Settings'}
              >
                <span className="composer-model-trigger-label">
                  <span className="composer-model-trigger-model">
                    {selectedModel?.modelName ?? 'No enabled model'}
                  </span>
                  {triggerReasoningLabel ? (
                    <span className="composer-model-trigger-reasoning">
                      {triggerReasoningLabel}
                    </span>
                  ) : null}
                </span>
                <ChevronDown size={14} aria-hidden="true" />
              </button>
              {pickerOpen ? (
                <div
                  className={
                    reasoningChoices.length > 0 ?
                      'composer-model-menu with-submenu'
                    : 'composer-model-menu'
                  }
                  role="dialog"
                  aria-label="Model and reasoning"
                  onMouseLeave={() => setReasoningSubmenuOpen(false)}
                >
                  <div className="composer-picker-column" aria-label="Model">
                    <div className="composer-picker-header">Model</div>
                    <div className="composer-picker-list">
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
                            const nextProviderModel = resolveProviderModel(model, providers)
                            const nextReasoningMode = resolveReasoningMode(
                              model,
                              providers,
                              nextProviderModel,
                            )
                            const nextReasoningChoices = buildReasoningChoices(
                              nextReasoningMode,
                              nextProviderModel,
                            )
                            const nextDefaultReasoning = resolveDefaultReasoning(
                              model,
                              providers,
                              nextProviderModel,
                            )
                            const nextReasoning =
                              nextReasoningChoices.find(
                                (choice) => choice.value === nextDefaultReasoning,
                              )?.value ??
                              nextReasoningChoices[0]?.value ??
                              ''
                            setSelectedModelId(model.id)
                            setSelectedReasoning(nextReasoning)
                            onModelSelectionChange?.({
                              providerId: model.providerId,
                              credentialId: model.credentialId,
                              modelId: model.modelId,
                              reasoningEffort:
                                nextReasoningChoices.length > 0 ? nextReasoning : null,
                            })
                          }}
                        >
                          <span>{model.label}</span>
                          {model.id === selectedModel?.id ? (
                            <Check size={15} aria-hidden="true" />
                          ) : null}
                        </button>
                      ))}
                    </div>
                    {reasoningChoices.length > 0 ? (
                      <>
                        <div className="composer-picker-divider" />
                        <button
                          type="button"
                          className={
                            reasoningSubmenuOpen ?
                              'composer-model-option composer-reasoning-entry selected'
                            : 'composer-model-option composer-reasoning-entry'
                          }
                          aria-haspopup="menu"
                          aria-expanded={reasoningSubmenuOpen}
                          onMouseEnter={() => setReasoningSubmenuOpen(true)}
                          onClick={() => setReasoningSubmenuOpen((open) => !open)}
                        >
                          <span className="composer-model-option-copy">
                            <span>Reasoning</span>
                            <small>{triggerReasoningLabel}</small>
                          </span>
                          <ChevronRight size={15} aria-hidden="true" />
                        </button>
                      </>
                    ) : null}
                  </div>
                  {reasoningSubmenuOpen && reasoningChoices.length > 0 ? (
                    <div className="composer-reasoning-submenu" role="menu" aria-label="Reasoning">
                      <div className="composer-picker-header">Reasoning</div>
                      <div className="composer-picker-list">
                        {reasoningChoices.map((choice) => (
                          <button
                            type="button"
                            key={choice.value}
                            className={
                              choice.value === selectedReasoning ?
                                'composer-model-option selected'
                              : 'composer-model-option'
                            }
                            role="menuitemradio"
                            aria-checked={choice.value === selectedReasoning}
                            onClick={() => {
                              setSelectedReasoning(choice.value)
                              if (selectedModel) {
                                onModelSelectionChange?.({
                                  providerId: selectedModel.providerId,
                                  credentialId: selectedModel.credentialId,
                                  modelId: selectedModel.modelId,
                                  reasoningEffort: choice.value,
                                })
                              }
                              setPickerOpen(false)
                              setReasoningSubmenuOpen(false)
                            }}
                            title={choice.description}
                          >
                            <span className="composer-model-option-copy">
                              <span>{choice.label}</span>
                              {choice.description ? <small>{choice.description}</small> : null}
                            </span>
                            {choice.value === selectedReasoning ? (
                              <Check size={15} aria-hidden="true" />
                            ) : null}
                          </button>
                        ))}
                      </div>
                    </div>
                  ) : null}
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
              aria-label={
                busy ? 'Running'
                : sendBlocked ? 'Send unavailable'
                : 'Send'
              }
              title={
                busy ? 'Running'
                : sendBlocked ? sendBlockTitle
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

function matchingSlashCommands(value: string): SlashCommand[] {
  if (!value.startsWith('/') || /\s/.test(value)) {
    return []
  }
  const query = value.slice(1).toLowerCase()
  return slashCommands.filter((command) =>
    command.command.slice(1).toLowerCase().startsWith(query),
  )
}

function resolveReasoningMode(
  model: SelectableModel | null,
  providers: ProviderConfig[],
  providerModel = resolveProviderModel(model, providers),
): ModelReasoningMode {
  if (!model || !providerModel) {
    return 'none'
  }
  if (providerModel.reasoning?.levels?.length) {
    return 'custom'
  }
  const raw = (providerModel.reasoningMode ?? '').toString().trim().toLowerCase()
  if (raw === 'toggle' || raw === 'effort' || raw === 'none' || raw === 'custom') {
    return raw
  }
  // Mirror the inference used in src/state/appState.ts so the picker stays
  // consistent with whatever the rest of the desktop considers "thinking only".
  const combined = `${providerModel.id} ${providerModel.name}`.toLowerCase()
  if (combined.includes('minimax-m3')) {
    return 'toggle'
  }
  return 'none'
}

function resolveDefaultReasoning(
  model: SelectableModel | null,
  providers: ProviderConfig[],
  providerModel = resolveProviderModel(model, providers),
): string {
  if (!model || !providerModel) {
    return ''
  }
  if (providerModel.reasoning?.levels?.length) {
    return (
      matchingReasoningValue(providerModel.defaultReasoning, providerModel) ??
      matchingReasoningValue(providerModel.reasoning.default, providerModel) ??
      providerModel.reasoning.levels[0].level
    )
  }
  const mode = resolveReasoningMode(model, providers)
  const raw = (providerModel.defaultReasoning ?? '').toString().trim().toLowerCase()
  if (mode === 'toggle') {
    // Config stores the wire value (`off` / `on`) but the picker exposes
    // `low` / `medium` / `high` / `xhigh`. Map `off` → Low and `on` → Medium
    // (a balanced "thinking on" default) so the desktop mirrors the CLI.
    return raw === 'on' ? 'medium' : 'low'
  }
  // Effort mode: the picker values match the config values directly.
  return raw
}

function resolveProviderModel(
  model: SelectableModel | null,
  providers: ProviderConfig[],
): ProviderModel | null {
  if (!model) {
    return null
  }
  const provider = providers.find((item) => item.id === model.providerId)
  return provider?.models.find((item) => item.id === model.modelId) ?? null
}

function buildReasoningChoices(
  mode: ModelReasoningMode,
  providerModel?: ProviderModel | null,
): ReasoningChoice[] {
  const customChoices = buildCustomReasoningChoices(providerModel)
  if (customChoices.length > 0) {
    return customChoices
  }
  if (mode === 'toggle') {
    // Thinking-only models: the same Low/Medium/High/Extra High ladder as
    // effort-mode is shown so the desktop and CLI share the same labels.
    // Low collapses to `off` on the wire, the rest collapse to `on`.
    return [
      { value: 'low', label: 'Low', description: 'No thinking output.' },
      {
        value: 'medium',
        label: 'Medium',
        description: 'Enable thinking output (balanced).',
      },
      {
        value: 'high',
        label: 'High',
        description: 'Enable thinking output (deeper).',
      },
      {
        value: 'xhigh',
        label: 'Extra High',
        description: 'Enable thinking output (deepest).',
      },
    ]
  }
  if (mode === 'effort') {
    return [
      { value: 'minimal', label: 'Minimal', description: 'Fastest responses.' },
      { value: 'low', label: 'Low', description: 'Light reasoning for simple edits.' },
      { value: 'medium', label: 'Medium', description: 'Balanced reasoning for normal coding work.' },
      { value: 'high', label: 'High', description: 'More reasoning for harder bugs.' },
      { value: 'xhigh', label: 'Extra High', description: 'Maximum reasoning for complex debugging.' },
    ]
  }
  return []
}

function buildCustomReasoningChoices(providerModel?: ProviderModel | null): ReasoningChoice[] {
  return (providerModel?.reasoning?.levels ?? [])
    .map((level) => {
      const value = level.level.trim()
      return {
        value,
        label: level.label?.trim() || reasoningLevelLabel(value),
        description: level.description?.trim() || value,
      }
    })
    .filter((choice) => choice.value.length > 0)
}

function matchingReasoningValue(
  value: string | undefined,
  providerModel: ProviderModel,
): string | undefined {
  const requested = value?.trim()
  if (!requested) {
    return undefined
  }
  return providerModel.reasoning?.levels.find(
    (level) => level.level.toLowerCase() === requested.toLowerCase(),
  )?.level
}

function reasoningLevelLabel(level: string): string {
  switch (level.trim().toLowerCase()) {
    case 'xhigh':
      return 'Extra High'
    case 'minimal':
      return 'Minimal'
    case 'low':
      return 'Low'
    case 'medium':
      return 'Medium'
    case 'high':
      return 'High'
    case 'enabled':
      return 'Enabled'
    case 'disabled':
      return 'Disabled'
    case 'on':
      return 'On'
    case 'off':
      return 'Off'
    default:
      return level.trim()
  }
}

export type { ReasoningChoice }
