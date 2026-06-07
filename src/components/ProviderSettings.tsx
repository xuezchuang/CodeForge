import {
  AudioWaveform,
  Bot,
  Check,
  ChevronDown,
  Eye,
  EyeOff,
  ListChecks,
  Pencil,
  Plus,
  RefreshCw,
  Search,
  Trash2,
} from 'lucide-react'
import { useMemo, useState } from 'react'
import { fetchMiniMaxModels } from '../api/tauriApi'
import { defaultProviders } from '../state/appState'
import type { ProviderConfig, ProviderCredential, ProviderModel } from '../types/provider'
import { minimaxOpenAiBaseUrl, providerTypeLabels } from '../types/provider'

interface ProviderSettingsProps {
  providers: ProviderConfig[]
  onProvidersChanged: (providers: ProviderConfig[]) => void
}

type FetchStatus = 'not-fetched' | 'fetched' | 'error'

interface FetchState {
  status: FetchStatus
  message?: string
}

interface PickerState {
  providerId: string
  credentialId: string
  draftModelIds: string[]
  search: string
}

interface ProviderKeyCard {
  id: string
  provider: ProviderConfig
  credential: ProviderCredential
  selectedCount: number
  fetchState: FetchState
}

interface SelectedModelRow {
  key: string
  provider: ProviderConfig
  credential: ProviderCredential | null
  credentialId: string
  model: ProviderModel
}

function ProviderSettings({ providers, onProvidersChanged }: ProviderSettingsProps) {
  const [fetchStateByCardId, setFetchStateByCardId] = useState<Record<string, FetchState>>({})
  const [modelCatalogs, setModelCatalogs] = useState<Record<string, ProviderModel[]>>({})
  const [keyVisibleByCardId, setKeyVisibleByCardId] = useState<Record<string, boolean>>({})
  const [keyEditingByCardId, setKeyEditingByCardId] = useState<Record<string, boolean>>({})
  const [picker, setPicker] = useState<PickerState | null>(null)
  const [selectedModelSearch, setSelectedModelSearch] = useState('')
  const [editingModelKey, setEditingModelKey] = useState<string | null>(null)
  const [notice, setNotice] = useState<string | null>(null)

  const providerKeyCards = useMemo(
    () => buildProviderKeyCards(providers, fetchStateByCardId),
    [providers, fetchStateByCardId],
  )
  const selectedRows = useMemo(() => buildSelectedModelRows(providers), [providers])
  const filteredSelectedRows = useMemo(
    () => filterSelectedRows(selectedRows, selectedModelSearch),
    [selectedRows, selectedModelSearch],
  )

  const addProviderKey = () => {
    const provider = providers.find((item) => item.id === 'minimax') ?? providers[0]
    if (!provider) {
      return
    }
    const credential = createCredential(provider)
    setNotice(null)
    setKeyEditingByCardId((current) => ({
      ...current,
      [cardId(provider.id, credential.id)]: true,
    }))
    updateProvider(provider.id, (current) =>
      finalizeProvider({
        ...current,
        credentials: [...(current.credentials ?? []), credential],
        defaultCredentialId: current.defaultCredentialId || credential.id,
      }),
    )
  }

  const updateProvider = (
    providerId: string,
    updater: (provider: ProviderConfig) => ProviderConfig,
  ) => {
    onProvidersChanged(
      providers.map((provider) =>
        provider.id === providerId ? finalizeProvider(updater(provider)) : provider,
      ),
    )
  }

  const updateCredential = (
    providerId: string,
    credentialId: string,
    patch: Partial<ProviderCredential>,
  ) => {
    setNotice(null)
    updateProvider(providerId, (provider) => ({
      ...provider,
      credentials: (provider.credentials ?? []).map((credential) =>
        credential.id === credentialId ? { ...credential, ...patch } : credential,
      ),
    }))
  }

  const changeCredentialProvider = (
    provider: ProviderConfig,
    credential: ProviderCredential,
    nextProviderId: string,
  ) => {
    if (provider.id === nextProviderId) {
      return
    }
    const nextProvider = providers.find((item) => item.id === nextProviderId)
    if (!nextProvider) {
      return
    }

    const movedCredential = {
      ...credential,
      id: uniqueCredentialId(nextProvider.credentials ?? []),
    }
    const movedModels = (provider.models ?? [])
      .filter((model) => modelBelongsToCredential(provider, model, credential.id))
      .map((model) => ({ ...model, credentialId: movedCredential.id }))

    setPicker(null)
    setNotice(null)
    onProvidersChanged(
      providers.map((item) => {
        if (item.id === provider.id) {
          return finalizeProvider({
            ...item,
            credentials: (item.credentials ?? []).filter(
              (current) => current.id !== credential.id,
            ),
            models: (item.models ?? []).filter(
              (model) => !modelBelongsToCredential(item, model, credential.id),
            ),
          })
        }
        if (item.id === nextProvider.id) {
          return finalizeProvider({
            ...item,
            credentials: [...(item.credentials ?? []), movedCredential],
            defaultCredentialId: item.defaultCredentialId || movedCredential.id,
            models: [...(item.models ?? []), ...movedModels],
          })
        }
        return item
      }),
    )
  }

  const removeProviderKey = (provider: ProviderConfig, credential: ProviderCredential) => {
    setPicker(null)
    setNotice(null)
    updateProvider(provider.id, (current) => ({
      ...current,
      credentials: (current.credentials ?? []).filter((item) => item.id !== credential.id),
      models: (current.models ?? []).filter(
        (model) => !modelBelongsToCredential(current, model, credential.id),
      ),
      defaultCredentialId:
        current.defaultCredentialId === credential.id ? '' : current.defaultCredentialId,
    }))
  }

  const fetchModels = async (card: ProviderKeyCard) => {
    if (!isMiniMax(card.provider)) {
      setFetchStateByCardId((current) => ({
        ...current,
        [card.id]: {
          status: 'error',
          message: 'Fetch models is currently implemented for MiniMax only.',
        },
      }))
      return
    }
    if (!card.credential.apiKey.trim()) {
      setFetchStateByCardId((current) => ({
        ...current,
        [card.id]: { status: 'error', message: 'API key is required.' },
      }))
      return
    }

    try {
      setFetchStateByCardId((current) => ({
        ...current,
        [card.id]: { status: 'not-fetched', message: 'Fetching...' },
      }))
      const fetchedModels = await fetchMiniMaxModels(card.credential.apiKey)
      setModelCatalogs((current) => ({ ...current, [card.id]: fetchedModels }))
      setFetchStateByCardId((current) => ({
        ...current,
        [card.id]: { status: 'fetched' },
      }))
    } catch (caught) {
      setFetchStateByCardId((current) => ({
        ...current,
        [card.id]: {
          status: 'error',
          message: caught instanceof Error ? caught.message : String(caught),
        },
      }))
    }
  }

  const openPicker = (card: ProviderKeyCard) => {
    setNotice(null)
    setPicker({
      providerId: card.provider.id,
      credentialId: card.credential.id,
      draftModelIds: selectedModelIdsForCredential(card.provider, card.credential.id),
      search: '',
    })
  }

  const togglePickerModel = (modelId: string) => {
    setPicker((current) => {
      if (!current) {
        return current
      }
      const selected = current.draftModelIds.includes(modelId)
      return {
        ...current,
        draftModelIds: selected
          ? current.draftModelIds.filter((id) => id !== modelId)
          : [...current.draftModelIds, modelId],
      }
    })
  }

  const applyPicker = (card: ProviderKeyCard) => {
    if (!picker) {
      return
    }
    const options = modelOptionsForCard(card.provider, card.credential, modelCatalogs[card.id])
    updateProvider(card.provider.id, (provider) =>
      applySelectedModels(provider, card.credential.id, options, picker.draftModelIds),
    )
    setPicker(null)
  }

  const removeSelectedModel = (row: SelectedModelRow) => {
    updateProvider(row.provider.id, (provider) => ({
      ...provider,
      models: (provider.models ?? []).filter(
        (model) =>
          !(
            model.id === row.model.id &&
            modelCredentialId(provider, model) === row.credentialId
          ),
      ),
    }))
  }

  const updateSelectedModel = (
    row: SelectedModelRow,
    patch: Partial<ProviderModel>,
  ) => {
    updateProvider(row.provider.id, (provider) => ({
      ...provider,
      models: (provider.models ?? []).map((model) =>
        model.id === row.model.id && modelCredentialId(provider, model) === row.credentialId
          ? { ...model, ...patch, credentialId: row.credentialId }
          : model,
      ),
    }))
  }

  return (
    <section className="settings-card model-settings-card">
      <div className="models-admin-header">
        <div>
          <h3>Models</h3>
          <p>Manage providers, API keys and selectable models for agents.</p>
        </div>
        <button type="button" className="primary-button" onClick={addProviderKey}>
          <Plus size={16} aria-hidden="true" />
          Add Provider Key
        </button>
      </div>

      <div className="models-section">
        <div className="models-section-heading">
          <h4>Provider Keys</h4>
        </div>
        {providerKeyCards.length === 0 ? (
          <div className="empty-state model-empty-state">No provider keys configured.</div>
        ) : (
          <div className="provider-key-grid">
            {providerKeyCards.map((card) => {
              const keyVisible = keyVisibleByCardId[card.id] === true
              const keyEditing = keyEditingByCardId[card.id] === true
              const cardPicker =
                picker?.providerId === card.provider.id &&
                picker.credentialId === card.credential.id
                  ? picker
                  : null
              const modelOptions = modelOptionsForCard(
                card.provider,
                card.credential,
                modelCatalogs[card.id],
              )
              const pickerOptions = filterModelOptions(modelOptions, cardPicker?.search ?? '')

              return (
                <div className="provider-key-card" key={card.id}>
                  <div className="provider-key-card-header">
                    <div className="provider-key-identity">
                      <span className={providerBrandClass(card.provider)} aria-hidden="true">
                        {isMiniMax(card.provider) ? (
                          <AudioWaveform size={22} />
                        ) : (
                          <Bot size={22} />
                        )}
                      </span>
                      <div className="provider-key-title">
                        <strong>{card.provider.name}</strong>
                        <div className="provider-key-badges">
                          <span className={statusBadgeClass(card.fetchState.status)}>
                            {statusLabel(card.fetchState.status)}
                          </span>
                          <span className="model-count-badge">
                            {card.selectedCount} models selected
                          </span>
                        </div>
                      </div>
                    </div>
                    <div className="provider-key-actions">
                      <button
                        type="button"
                        className="secondary-button"
                        onClick={() => void fetchModels(card)}
                      >
                        <RefreshCw size={15} aria-hidden="true" />
                        Fetch Models
                      </button>
                      <button
                        type="button"
                        className="secondary-button"
                        onClick={() =>
                          setKeyEditingByCardId((current) => ({
                            ...current,
                            [card.id]: !current[card.id],
                          }))
                        }
                      >
                        {keyEditing ? (
                          <Check size={15} aria-hidden="true" />
                        ) : (
                          <Pencil size={15} aria-hidden="true" />
                        )}
                        {keyEditing ? 'Done' : 'Edit Key'}
                      </button>
                      <button
                        type="button"
                        className="icon-button provider-key-delete"
                        onClick={() => removeProviderKey(card.provider, card.credential)}
                        aria-label={`Delete ${credentialAlias(card.credential)}`}
                        title={`Delete ${credentialAlias(card.credential)}`}
                      >
                        <Trash2 size={16} aria-hidden="true" />
                      </button>
                    </div>
                  </div>

                  <div className="provider-key-fields">
                    <label className="provider-key-field">
                      <span>Key Alias</span>
                      <input
                        value={card.credential.name}
                        onChange={(event) =>
                          updateCredential(card.provider.id, card.credential.id, {
                            name: event.target.value,
                          })
                        }
                        placeholder="minimax-main"
                      />
                    </label>
                    <label className="provider-key-field">
                      <span>API Key</span>
                      <div className="provider-key-secret">
                        <input
                          type={keyVisible || keyEditing ? 'text' : 'password'}
                          value={
                            keyVisible || keyEditing
                              ? card.credential.apiKey
                              : maskApiKey(card.credential.apiKey)
                          }
                          readOnly={!keyEditing}
                          onChange={(event) =>
                            updateCredential(card.provider.id, card.credential.id, {
                              apiKey: event.target.value,
                            })
                          }
                          placeholder="empty"
                        />
                        <button
                          type="button"
                          className="secondary-button"
                          onClick={() =>
                            setKeyVisibleByCardId((current) => ({
                              ...current,
                              [card.id]: !current[card.id],
                            }))
                          }
                        >
                          {keyVisible ? (
                            <EyeOff size={15} aria-hidden="true" />
                          ) : (
                            <Eye size={15} aria-hidden="true" />
                          )}
                          {keyVisible ? 'Hide' : 'Show'}
                        </button>
                      </div>
                    </label>
                    <label className="provider-key-field">
                      <span>Provider</span>
                      <select
                        value={card.provider.id}
                        onChange={(event) =>
                          changeCredentialProvider(
                            card.provider,
                            card.credential,
                            event.target.value,
                          )
                        }
                      >
                        {providers.filter(providerUsesCredentials).map((provider) => (
                          <option key={provider.id} value={provider.id}>
                            {providerTypeLabels[provider.type] ?? provider.name}
                          </option>
                        ))}
                      </select>
                    </label>
                  </div>

                  {card.fetchState.message ? (
                    <div className="provider-key-message">{card.fetchState.message}</div>
                  ) : null}

                  <div className="provider-key-footer">
                    <button
                      type="button"
                      className="secondary-button model-selector-button"
                      onClick={() => openPicker(card)}
                      aria-expanded={cardPicker ? 'true' : 'false'}
                    >
                      <ListChecks size={16} aria-hidden="true" />
                      Select Models
                      <ChevronDown size={14} aria-hidden="true" />
                    </button>
                  </div>

                  {cardPicker ? (
                    <div className="model-picker-popover">
                      <div className="model-picker-panel">
                        <div className="model-picker-toolbar">
                          <div className="model-picker-search">
                            <Search size={15} aria-hidden="true" />
                            <input
                              value={cardPicker.search}
                              onChange={(event) =>
                                setPicker({ ...cardPicker, search: event.target.value })
                              }
                              placeholder="Search models..."
                            />
                          </div>
                          <div className="model-picker-bulk">
                            <button
                              type="button"
                              className="ghost-button"
                              onClick={() =>
                                setPicker({
                                  ...cardPicker,
                                  draftModelIds: modelOptions.map((model) => model.id),
                                })
                              }
                            >
                              Select all
                            </button>
                            <button
                              type="button"
                              className="ghost-button"
                              onClick={() => setPicker({ ...cardPicker, draftModelIds: [] })}
                            >
                              Clear
                            </button>
                          </div>
                        </div>

                        <div className="model-picker-list">
                          {pickerOptions.length === 0 ? (
                            <div className="empty-state model-choice-empty">No models found.</div>
                          ) : (
                            pickerOptions.map((model) => {
                              const selected = cardPicker.draftModelIds.includes(model.id)
                              return (
                                <label className="model-picker-option" key={model.id}>
                                  <input
                                    type="checkbox"
                                    checked={selected}
                                    onChange={() => togglePickerModel(model.id)}
                                  />
                                  <span className="model-picker-option-main">
                                    <strong>{model.id}</strong>
                                  </span>
                                  <span className="model-picker-tags">
                                    {selected ? <span className="selected">selected</span> : null}
                                    {modelTags(model.id).map((tag) => (
                                      <span className={tag} key={tag}>
                                        {tag}
                                      </span>
                                    ))}
                                  </span>
                                </label>
                              )
                            })
                          )}
                        </div>

                        <div className="model-picker-footer">
                          <button
                            type="button"
                            className="secondary-button"
                            onClick={() => setPicker(null)}
                          >
                            Cancel
                          </button>
                          <button
                            type="button"
                            className="primary-button"
                            onClick={() => applyPicker(card)}
                          >
                            Apply selected models
                          </button>
                        </div>
                      </div>
                    </div>
                  ) : null}
                </div>
              )
            })}
          </div>
        )}
      </div>

      <div className="models-section selected-models-section">
        <div className="models-section-heading">
          <h4>Selected Models</h4>
          <span>{selectedRows.length}</span>
        </div>
        <div className="selected-models-toolbar">
          <div className="model-picker-search">
            <Search size={15} aria-hidden="true" />
            <input
              value={selectedModelSearch}
              onChange={(event) => setSelectedModelSearch(event.target.value)}
              placeholder="Search selected models..."
            />
          </div>
        </div>

        {filteredSelectedRows.length === 0 ? (
          <div className="empty-state model-empty-state">No selected models.</div>
        ) : (
          <div className="selected-models-table">
            <div className="selected-models-row selected-models-head">
              <span>Model</span>
              <span>Status</span>
              <span>Actions</span>
            </div>
            {filteredSelectedRows.map((row) => {
              const editing = editingModelKey === row.key
              return (
                <div className="selected-models-row" key={row.key}>
                  <span className="selected-model-display">
                    {editing ? (
                      <input
                        value={row.model.name || row.model.id}
                        onChange={(event) =>
                          updateSelectedModel(row, { name: event.target.value })
                        }
                      />
                    ) : (
                      row.model.name || row.model.id
                    )}
                  </span>
                  <label className="selected-model-switch">
                    <input
                      type="checkbox"
                      checked={row.model.enabled}
                      onChange={(event) =>
                        updateSelectedModel(row, { enabled: event.target.checked })
                      }
                      aria-label={
                        row.model.enabled ?
                          `${row.model.name || row.model.id} enabled`
                        : `${row.model.name || row.model.id} disabled`
                      }
                    />
                  </label>
                  <span className="selected-model-actions">
                    <button
                      type="button"
                      className="ghost-button"
                      onClick={() => setEditingModelKey(editing ? null : row.key)}
                    >
                      {editing ? (
                        <Check size={15} aria-hidden="true" />
                      ) : (
                        <Pencil size={15} aria-hidden="true" />
                      )}
                      {editing ? 'Done' : 'Edit'}
                    </button>
                    <button
                      type="button"
                      className="ghost-button danger-text-button"
                      onClick={() => removeSelectedModel(row)}
                    >
                      <Trash2 size={15} aria-hidden="true" />
                      Remove
                    </button>
                  </span>
                </div>
              )
            })}
          </div>
        )}
      </div>

      {notice ? <div className="provider-test-result">{notice}</div> : null}
    </section>
  )
}

function buildProviderKeyCards(
  providers: ProviderConfig[],
  fetchStateByCardId: Record<string, FetchState>,
): ProviderKeyCard[] {
  return providers.flatMap((provider) =>
    providerUsesCredentials(provider) ?
    (provider.credentials ?? []).map((credential) => {
      const id = cardId(provider.id, credential.id)
      return {
        id,
        provider,
        credential,
        selectedCount: selectedModelIdsForCredential(provider, credential.id).length,
        fetchState: fetchStateByCardId[id] ?? { status: 'not-fetched' },
      }
    })
    : [],
  )
}

function buildSelectedModelRows(providers: ProviderConfig[]): SelectedModelRow[] {
  return providers.flatMap((provider) =>
    (provider.models ?? []).map((model) => {
      const credentialId = modelCredentialId(provider, model)
      const credential =
        (provider.credentials ?? []).find((item) => item.id === credentialId) ??
        (provider.credentials ?? [])[0] ??
        null
      return {
        key: selectedModelKey(provider.id, credentialId, model.id),
        provider,
        credential,
        credentialId,
        model,
      }
    }),
  )
}

function filterSelectedRows(
  rows: SelectedModelRow[],
  search: string,
): SelectedModelRow[] {
  const query = search.trim().toLowerCase()
  if (!query) {
    return rows
  }
  return rows.filter((row) =>
    [
      row.model.name,
      row.model.id,
      row.provider.name,
      row.credential ? credentialAlias(row.credential) : '',
    ]
      .join(' ')
      .toLowerCase()
      .includes(query),
  )
}

function filterModelOptions(models: ProviderModel[], search: string): ProviderModel[] {
  const query = search.trim().toLowerCase()
  if (!query) {
    return models
  }
  return models.filter((model) =>
    `${model.name} ${model.id}`.toLowerCase().includes(query),
  )
}

function modelOptionsForCard(
  provider: ProviderConfig,
  credential: ProviderCredential,
  fetchedModels: ProviderModel[] | undefined,
): ProviderModel[] {
  const options = new Map<string, ProviderModel>()
  const defaultProvider = defaultProviders.find((item) => item.id === provider.id)
  addModelOptions(options, defaultProvider?.models ?? [])
  addModelOptions(options, fetchedModels ?? [])
  addModelOptions(
    options,
    (provider.models ?? []).filter((model) =>
      modelBelongsToCredential(provider, model, credential.id),
    ),
  )

  const defaultModel = defaultProvider?.defaultModel ?? provider.defaultModel
  if (options.size === 0 && defaultModel.trim().length > 0) {
    options.set(defaultModel, {
      id: defaultModel,
      name: defaultModel,
      enabled: false,
      credentialId: credential.id,
    })
  }
  return [...options.values()]
}

function addModelOptions(target: Map<string, ProviderModel>, models: ProviderModel[]): void {
  for (const model of models) {
    const id = model.id.trim()
    if (!id || target.has(id)) {
      continue
    }
    target.set(id, {
      ...model,
      id,
      name: (model.name ?? '').trim() || id,
    })
  }
}

function selectedModelIdsForCredential(
  provider: ProviderConfig,
  credentialId: string,
): string[] {
  return (provider.models ?? [])
    .filter((model) => modelBelongsToCredential(provider, model, credentialId))
    .map((model) => model.id)
}

function applySelectedModels(
  provider: ProviderConfig,
  credentialId: string,
  options: ProviderModel[],
  selectedModelIds: string[],
): ProviderConfig {
  const selectedIds = new Set(selectedModelIds)
  const optionById = new Map(options.map((model) => [model.id, model]))
  const otherModels = (provider.models ?? []).filter(
    (model) => !modelBelongsToCredential(provider, model, credentialId),
  )
  const existingById = new Map(
    (provider.models ?? [])
      .filter((model) => modelBelongsToCredential(provider, model, credentialId))
      .map((model) => [model.id, model]),
  )
  const selectedModels: ProviderModel[] = []
  for (const modelId of selectedIds) {
    const existing = existingById.get(modelId)
    const option = optionById.get(modelId)
    const source = existing ?? option
    if (!source) {
      continue
    }
    selectedModels.push({
      ...source,
      ...(existing ?? {}),
      id: modelId,
      name: (existing?.name ?? option?.name ?? modelId).trim() || modelId,
      enabled: existing?.enabled ?? true,
      credentialId,
    })
  }

  return {
    ...provider,
    models: [...otherModels, ...selectedModels],
  }
}

function modelBelongsToCredential(
  provider: ProviderConfig,
  model: ProviderModel,
  credentialId: string,
): boolean {
  return modelCredentialId(provider, model) === credentialId
}

function modelCredentialId(provider: ProviderConfig, model: ProviderModel): string {
  return (
    model.credentialId?.trim() ||
    provider.defaultCredentialId ||
    (provider.credentials ?? [])[0]?.id ||
    ''
  )
}

function finalizeProvider(provider: ProviderConfig): ProviderConfig {
  const credentials = normalizeCredentials(provider.credentials ?? [])
  const models = normalizeModels(provider, provider.models ?? [], credentials)
  const enabledCredential = credentials.find((credential) => credential.enabled)
  const enabledModel = models.find((model) => model.enabled)
  const usesCredentials = providerUsesCredentials(provider)
  const enabledWithoutCredentials =
    provider.type === 'codex-cli'
      ? provider.enabled && provider.defaultModel.trim().length > 0
      : Boolean(enabledModel)

  return {
    ...provider,
    apiKey: undefined,
    enabled:
      usesCredentials ? Boolean(enabledCredential && enabledModel) : enabledWithoutCredentials,
    baseUrl: isMiniMax(provider) ? minimaxOpenAiBaseUrl : provider.baseUrl,
    baseUrlLocked: isMiniMax(provider) ? true : provider.baseUrlLocked,
    credentials,
    defaultCredentialId: usesCredentials ? enabledCredential?.id ?? credentials[0]?.id ?? '' : '',
    defaultModel: enabledModel?.id ?? models[0]?.id ?? provider.defaultModel,
    models,
  }
}

function normalizeCredentials(credentials: ProviderCredential[]): ProviderCredential[] {
  return credentials
    .map((credential, index) => {
      const apiKey = (credential.apiKey ?? '').trim()
      return {
        ...credential,
        id: credential.id?.trim() || `key-${index + 1}`,
        name: credential.name?.trim() || `key-${index + 1}`,
        enabled: apiKey.length > 0,
        apiKey,
      }
    })
    .filter((credential) => credential.id.length > 0)
}

function normalizeModels(
  provider: ProviderConfig,
  models: ProviderModel[],
  credentials: ProviderCredential[],
): ProviderModel[] {
  return models
    .map((model) => {
      const id = model.id.trim()
      const credentialId =
        providerUsesCredentials(provider) ?
          model.credentialId?.trim() ||
          provider.defaultCredentialId ||
          credentials[0]?.id ||
          ''
        : ''
      return {
        ...model,
        id,
        name: (model.name ?? '').trim() || id,
        enabled: Boolean(model.enabled),
        credentialId,
      }
    })
    .filter((model) => model.id.length > 0)
}

function createCredential(provider: ProviderConfig): ProviderCredential {
  const id = uniqueCredentialId(provider.credentials ?? [])
  return {
    id,
    name: '',
    enabled: false,
    apiKey: '',
  }
}

function uniqueCredentialId(credentials: ProviderCredential[]): string {
  let index = credentials.length + 1
  let id = `key-${index}`
  const used = new Set(credentials.map((credential) => credential.id))
  while (used.has(id)) {
    index += 1
    id = `key-${index}`
  }
  return id
}

function cardId(providerId: string, credentialId: string): string {
  return `${providerId}:${credentialId}`
}

function selectedModelKey(
  providerId: string,
  credentialId: string,
  modelId: string,
): string {
  return `${providerId}:${credentialId || 'default'}:${modelId}`
}

function credentialAlias(credential: ProviderCredential): string {
  return credential.name.trim() || credential.id || 'key'
}

function statusBadgeClass(status: FetchStatus): string {
  return `status-badge ${status}`
}

function statusLabel(status: FetchStatus): string {
  if (status === 'fetched') {
    return 'Fetched'
  }
  if (status === 'error') {
    return 'Error'
  }
  return 'Not fetched'
}

function providerBrandClass(provider: ProviderConfig): string {
  if (provider.id === 'codex-cli' || provider.type === 'codex-cli') {
    return 'provider-key-brand codex-cli'
  }
  if (isMiniMax(provider)) {
    return 'provider-key-brand minimax'
  }
  if (provider.id === 'codebuddy' || provider.type === 'codebuddy') {
    return 'provider-key-brand codebuddy'
  }
  return 'provider-key-brand'
}

function maskApiKey(apiKey: string | undefined): string {
  if (!apiKey) {
    return ''
  }
  return '************'
}

function isMiniMax(provider: ProviderConfig): boolean {
  return provider.id === 'minimax' || provider.type === 'minimax'
}

function providerUsesCredentials(provider: ProviderConfig): boolean {
  return provider.type !== 'ollama' && provider.type !== 'codex-cli'
}

function modelTags(modelId: string): string[] {
  const normalized = modelId.toLowerCase()
  return [
    normalized.includes('highspeed') ? 'highspeed' : null,
    normalized.includes('reason') ? 'reasoning' : null,
    normalized.includes('vision') || normalized.includes('vl') ? 'vision' : null,
  ].filter((tag): tag is string => tag !== null)
}

export default ProviderSettings
