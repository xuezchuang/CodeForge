import {
  Check,
  Eye,
  EyeOff,
  Plus,
  Search,
  Server,
  Star,
  Trash2,
} from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import type {
  ModelDefaultReasoning,
  ModelReasoningConfig,
  ModelReasoningMode,
  ProviderConfig,
  ProviderCredential,
  ProviderModel,
} from '../types/provider'

interface ProviderSettingsProps {
  configPath: string
  providers: ProviderConfig[]
  onProvidersChanged: (providers: ProviderConfig[]) => void
}

function ProviderSettings({
  configPath,
  providers,
  onProvidersChanged,
}: ProviderSettingsProps) {
  const [selectedProviderId, setSelectedProviderId] = useState(
    () => defaultProvider(providers)?.id ?? providers[0]?.id ?? '',
  )
  const [visibleKeys, setVisibleKeys] = useState<Record<string, boolean>>({})
  const [modelSearch, setModelSearch] = useState('')
  const [newModelId, setNewModelId] = useState('')

  const selectedProvider =
    providers.find((provider) => provider.id === selectedProviderId) ??
    defaultProvider(providers) ??
    providers[0] ??
    null
  const activeDefaultProvider = defaultProvider(providers)
  const filteredModels = useMemo(
    () => filterModels(selectedProvider?.models ?? [], modelSearch),
    [selectedProvider?.models, modelSearch],
  )

  useEffect(() => {
    if (selectedProviderId && providers.some((provider) => provider.id === selectedProviderId)) {
      return
    }
    setSelectedProviderId(defaultProvider(providers)?.id ?? providers[0]?.id ?? '')
  }, [providers, selectedProviderId])

  const emit = (nextProviders: ProviderConfig[]) => {
    onProvidersChanged(ensureOneDefault(nextProviders.map(finalizeProvider)))
  }

  const addProvider = () => {
    const provider = createProvider(providers)
    setSelectedProviderId(provider.id)
    emit([...providers, provider])
  }

  const removeProvider = (providerId: string) => {
    const nextProviders = providers.filter((provider) => provider.id !== providerId)
    setSelectedProviderId(nextProviders[0]?.id ?? '')
    emit(nextProviders)
  }

  const updateProvider = (
    providerId: string,
    updater: (provider: ProviderConfig) => ProviderConfig,
  ) => {
    emit(providers.map((provider) => (provider.id === providerId ? updater(provider) : provider)))
  }

  const setDefaultProvider = (providerId: string) => {
    emit(
      providers.map((provider) => ({
        ...provider,
        enabled: true,
        isDefault: provider.id === providerId,
      })),
    )
  }

  const setBearerToken = (providerId: string, apiKey: string) => {
    updateProvider(providerId, (provider) => ({
      ...provider,
      credentials: apiKey.trim().length > 0 ? [bearerCredential(apiKey)] : [],
      defaultCredentialId: apiKey.trim().length > 0 ? 'default' : '',
    }))
  }

  const addModel = () => {
    const modelId = newModelId.trim()
    if (!selectedProvider || !modelId) {
      return
    }
    updateProvider(selectedProvider.id, (provider) => ({
      ...provider,
      defaultModel: modelId,
      models: upsertModel(provider.models ?? [], modelId),
    }))
    setNewModelId('')
  }

  return (
    <section className="settings-card model-settings-card config-model-settings">
      <div className="models-admin-header">
        <div>
          <h3>Models</h3>
          <p>{configPath}</p>
        </div>
        <button type="button" className="primary-button" onClick={addProvider}>
          <Plus size={16} aria-hidden="true" />
          Add Provider
        </button>
      </div>

      <div className="config-default-bar">
        <label>
          <span>Default Provider</span>
          <select
            value={activeDefaultProvider?.id ?? ''}
            onChange={(event) => setDefaultProvider(event.target.value)}
          >
            {providers.map((provider) => (
              <option key={provider.id} value={provider.id}>
                {provider.name || provider.id}
              </option>
            ))}
          </select>
        </label>
        <label>
          <span>Default Model</span>
          <input
            value={activeDefaultProvider?.defaultModel ?? ''}
            onChange={(event) => {
              if (activeDefaultProvider) {
                updateProvider(activeDefaultProvider.id, (provider) => ({
                  ...provider,
                  defaultModel: event.target.value,
                  models: upsertModel(provider.models ?? [], event.target.value),
                }))
              }
            }}
            placeholder="model id"
          />
        </label>
      </div>

      {providers.length === 0 ? (
        <div className="empty-state model-empty-state">No model providers in config.toml.</div>
      ) : (
        <div className="config-models-layout">
          <aside className="config-provider-list">
            <div className="config-provider-list-heading">
              <span>Providers</span>
              <span>{providers.length}</span>
            </div>
            {providers.map((provider) => (
              <button
                type="button"
                className={
                  provider.id === selectedProvider?.id ?
                    'config-provider-row active'
                  : 'config-provider-row'
                }
                key={provider.id}
                onClick={() => setSelectedProviderId(provider.id)}
              >
                <span className="config-provider-row-icon" aria-hidden="true">
                  {provider.isDefault ? <Star size={15} /> : <Server size={15} />}
                </span>
                <span className="config-provider-row-main">
                  <strong>{provider.name || provider.id}</strong>
                  <small>{provider.id}</small>
                </span>
                <span className="config-provider-row-status">
                  {providerHasToken(provider) ? 'token' : provider.envKey ? 'env' : 'no key'}
                </span>
              </button>
            ))}
          </aside>

          {selectedProvider ? (
            <div className="config-provider-editor">
              <div className="config-provider-editor-heading">
                <div>
                  <h4>{selectedProvider.name || selectedProvider.id}</h4>
                  <p>[model_providers.{selectedProvider.id}]</p>
                </div>
                <div className="config-provider-editor-actions">
                  <button
                    type="button"
                    className="secondary-button"
                    onClick={() => setDefaultProvider(selectedProvider.id)}
                  >
                    <Star size={15} aria-hidden="true" />
                    {selectedProvider.isDefault ? 'Default' : 'Set Default'}
                  </button>
                  <button
                    type="button"
                    className="icon-button provider-key-delete"
                    onClick={() => removeProvider(selectedProvider.id)}
                    aria-label={`Delete ${selectedProvider.name || selectedProvider.id}`}
                    title={`Delete ${selectedProvider.name || selectedProvider.id}`}
                  >
                    <Trash2 size={16} aria-hidden="true" />
                  </button>
                </div>
              </div>

              <div className="config-provider-fields">
                <label className="provider-key-field">
                  <span>Provider ID</span>
                  <input value={selectedProvider.id} readOnly />
                </label>
                <label className="provider-key-field">
                  <span>Name</span>
                  <input
                    value={selectedProvider.name}
                    onChange={(event) =>
                      updateProvider(selectedProvider.id, (provider) => ({
                        ...provider,
                        name: event.target.value,
                      }))
                    }
                    placeholder="Provider name"
                  />
                </label>
                <label className="provider-key-field">
                  <span>Base URL</span>
                  <input
                    value={selectedProvider.baseUrl}
                    onChange={(event) =>
                      updateProvider(selectedProvider.id, (provider) => ({
                        ...provider,
                        baseUrl: event.target.value,
                      }))
                    }
                    placeholder="https://api.example.com/v1"
                  />
                </label>
                <label className="provider-key-field">
                  <span>Bearer Token</span>
                  <div className="provider-key-secret">
                    <input
                      type={visibleKeys[selectedProvider.id] ? 'text' : 'password'}
                      value={providerBearerToken(selectedProvider)}
                      onChange={(event) => setBearerToken(selectedProvider.id, event.target.value)}
                      placeholder="empty"
                    />
                    <button
                      type="button"
                      className="secondary-button"
                      onClick={() =>
                        setVisibleKeys((current) => ({
                          ...current,
                          [selectedProvider.id]: !current[selectedProvider.id],
                        }))
                      }
                    >
                      {visibleKeys[selectedProvider.id] ?
                        <EyeOff size={15} aria-hidden="true" />
                      : <Eye size={15} aria-hidden="true" />}
                      {visibleKeys[selectedProvider.id] ? 'Hide' : 'Show'}
                    </button>
                  </div>
                </label>
                <label className="provider-key-field">
                  <span>Env Key</span>
                  <input
                    value={selectedProvider.envKey ?? ''}
                    onChange={(event) =>
                      updateProvider(selectedProvider.id, (provider) => ({
                        ...provider,
                        envKey: event.target.value,
                      }))
                    }
                    placeholder="OPENAI_API_KEY"
                  />
                </label>
                <label className="provider-key-field">
                  <span>Wire API</span>
                  <select
                    value={selectedProvider.wireApi ?? 'responses'}
                    onChange={(event) =>
                      updateProvider(selectedProvider.id, (provider) => ({
                        ...provider,
                        wireApi: event.target.value,
                      }))
                    }
                  >
                    <option value="responses">responses</option>
                  </select>
                </label>
                <label className="toggle-row config-auth-toggle">
                  <input
                    type="checkbox"
                    checked={Boolean(selectedProvider.requiresOpenAiAuth)}
                    onChange={(event) =>
                      updateProvider(selectedProvider.id, (provider) => ({
                        ...provider,
                        requiresOpenAiAuth: event.target.checked,
                      }))
                    }
                  />
                  <span>Requires OpenAI Auth</span>
                </label>
              </div>

              <div className="config-models-panel">
                <div className="models-section-heading">
                  <h4>Models</h4>
                  <span>{selectedProvider.models.length}</span>
                </div>
                <div className="config-model-toolbar">
                  <div className="model-picker-search">
                    <Search size={15} aria-hidden="true" />
                    <input
                      value={modelSearch}
                      onChange={(event) => setModelSearch(event.target.value)}
                      placeholder="Search models..."
                    />
                  </div>
                  <div className="config-add-model">
                    <input
                      value={newModelId}
                      onChange={(event) => setNewModelId(event.target.value)}
                      onKeyDown={(event) => {
                        if (event.key === 'Enter') {
                          event.preventDefault()
                          addModel()
                        }
                      }}
                      placeholder="model id"
                    />
                    <button type="button" className="secondary-button" onClick={addModel}>
                      <Plus size={15} aria-hidden="true" />
                      Add
                    </button>
                  </div>
                </div>

                {filteredModels.length === 0 ? (
                  <div className="empty-state model-empty-state">No models found.</div>
                ) : (
                  <div className="config-model-table">
                    {filteredModels.map((model) => {
                      const isDefaultModel = selectedProvider.defaultModel === model.id
                      return (
                        <div className="config-model-row" key={model.id}>
                          <button
                            type="button"
                            className={
                              isDefaultModel ?
                                'config-default-model active'
                              : 'config-default-model'
                            }
                            onClick={() =>
                              updateProvider(selectedProvider.id, (provider) => ({
                                ...provider,
                                defaultModel: model.id,
                                models: upsertModel(provider.models ?? [], model.id),
                              }))
                            }
                            title="Set default model"
                            aria-label={`Set ${model.id} as default model`}
                          >
                            {isDefaultModel ?
                              <Check size={15} aria-hidden="true" />
                            : <Star size={15} aria-hidden="true" />}
                          </button>
                          <span className="config-model-main">
                            <strong>{model.name || model.id}</strong>
                            <small>{model.id}</small>
                          </span>
                          <span className="config-model-reasoning">
                            {model.reasoningMode ?? 'none'}
                          </span>
                        </div>
                      )
                    })}
                  </div>
                )}
              </div>
            </div>
          ) : null}
        </div>
      )}
    </section>
  )
}

function defaultProvider(providers: ProviderConfig[]): ProviderConfig | null {
  return providers.find((provider) => provider.isDefault) ?? null
}

function ensureOneDefault(providers: ProviderConfig[]): ProviderConfig[] {
  if (providers.length === 0) {
    return providers
  }
  let defaultSeen = false
  const hasDefault = providers.some((provider) => provider.isDefault)
  return providers.map((provider, index) => {
    const isDefault = hasDefault ? Boolean(provider.isDefault && !defaultSeen) : index === 0
    if (isDefault) {
      defaultSeen = true
    }
    return {
      ...provider,
      enabled: true,
      isDefault,
    }
  })
}

function createProvider(providers: ProviderConfig[]): ProviderConfig {
  const id = uniqueProviderId(providers)
  return finalizeProvider({
    id,
    type: 'openai-compatible',
    name: 'New Provider',
    enabled: true,
    isDefault: providers.length === 0,
    baseUrl: '',
    baseUrlLocked: false,
    defaultCredentialId: '',
    defaultModel: '',
    temperature: 0.2,
    envKey: '',
    wireApi: 'responses',
    requiresOpenAiAuth: false,
    credentials: [],
    models: [],
  })
}

function finalizeProvider(provider: ProviderConfig): ProviderConfig {
  const token = providerBearerToken(provider)
  const credentials = token.trim().length > 0 ? [bearerCredential(token)] : []
  const models = normalizeModels(provider.models ?? [])
  return {
    ...provider,
    type: provider.type || 'openai-compatible',
    name: provider.name.trim() || provider.id,
    enabled: true,
    baseUrl: provider.baseUrl.trim(),
    baseUrlLocked: false,
    apiKey: undefined,
    defaultCredentialId: credentials[0]?.id ?? '',
    defaultModel:
      provider.defaultModel.trim() ||
      models.find((model) => model.enabled)?.id ||
      models[0]?.id ||
      '',
    temperature: Number.isFinite(provider.temperature) ? provider.temperature : 0.2,
    envKey: provider.envKey?.trim() ?? '',
    wireApi: provider.wireApi || 'responses',
    requiresOpenAiAuth: Boolean(provider.requiresOpenAiAuth),
    credentials,
    models,
  }
}

function normalizeModels(models: ProviderModel[]): ProviderModel[] {
  return models
    .map((model) => {
      const id = model.id.trim()
      const name = (model.name ?? '').trim() || id
      const reasoning = normalizeReasoningConfig(model.reasoning)
      const reasoningMode = normalizeReasoningMode(model.reasoningMode, id, name, reasoning)
      return {
        ...model,
        id,
        name,
        enabled: true,
        reasoning,
        reasoningMode,
        defaultReasoning: normalizeDefaultReasoning(
          reasoningMode,
          model.defaultReasoning,
          reasoning,
        ),
      }
    })
    .filter((model) => model.id.length > 0)
}

function upsertModel(models: ProviderModel[], rawModelId: string): ProviderModel[] {
  const modelId = rawModelId.trim()
  if (!modelId) {
    return models
  }
  if (models.some((model) => model.id === modelId)) {
    return models
  }
  return [
    ...models,
    {
      id: modelId,
      name: modelId,
      enabled: true,
      reasoningMode: inferReasoningMode(modelId, modelId),
      defaultReasoning: 'off',
    },
  ]
}

function filterModels(models: ProviderModel[], search: string): ProviderModel[] {
  const query = search.trim().toLowerCase()
  if (!query) {
    return models
  }
  return models.filter((model) => `${model.name} ${model.id}`.toLowerCase().includes(query))
}

function bearerCredential(apiKey: string): ProviderCredential {
  return {
    id: 'default',
    name: 'Bearer Token',
    enabled: apiKey.trim().length > 0,
    apiKey,
  }
}

function providerBearerToken(provider: ProviderConfig): string {
  return (
    provider.credentials?.find((credential) => credential.apiKey.trim().length > 0)?.apiKey ??
    provider.apiKey ??
    ''
  )
}

function providerHasToken(provider: ProviderConfig): boolean {
  return providerBearerToken(provider).trim().length > 0
}

function uniqueProviderId(providers: ProviderConfig[]): string {
  const used = new Set(providers.map((provider) => provider.id))
  let index = providers.length + 1
  let id = `provider-${index}`
  while (used.has(id)) {
    index += 1
    id = `provider-${index}`
  }
  return id
}

function normalizeReasoningMode(
  value: string | undefined,
  modelId: string,
  modelName: string,
  reasoning?: ModelReasoningConfig | null,
): ModelReasoningMode {
  if (reasoning?.levels.length) {
    return 'custom'
  }
  const inferred = inferReasoningMode(modelId, modelName)
  if (inferred !== 'none' && (!value || value === 'none')) {
    return inferred
  }
  if (value === 'toggle' || value === 'effort' || value === 'none' || value === 'custom') {
    return value
  }
  return inferred
}

function normalizeDefaultReasoning(
  mode: ModelReasoningMode,
  value: string | undefined,
  reasoning?: ModelReasoningConfig | null,
): ModelDefaultReasoning {
  if (reasoning?.levels.length) {
    return matchingReasoningLevel(value, reasoning) ?? reasoning.default ?? reasoning.levels[0].level
  }
  if (mode === 'toggle') {
    return value === 'on' ? 'on' : 'off'
  }
  if (mode === 'effort') {
    return value === 'minimal' ||
      value === 'low' ||
      value === 'medium' ||
      value === 'high' ||
      value === 'xhigh'
      ? value
      : 'medium'
  }
  return 'off'
}

function normalizeReasoningConfig(
  reasoning: ModelReasoningConfig | null | undefined,
): ModelReasoningConfig | undefined {
  const levels = (reasoning?.levels ?? [])
    .map((level) => ({
      ...level,
      level: level.level.trim(),
      label: level.label?.trim(),
      description: level.description?.trim(),
    }))
    .filter((level) => level.level.length > 0)
  if (levels.length === 0) {
    return undefined
  }
  const normalized: ModelReasoningConfig = {
    requestField: reasoning?.requestField?.trim(),
    default: reasoning?.default?.trim(),
    levels,
  }
  normalized.default = matchingReasoningLevel(reasoning?.default, normalized) ?? levels[0].level
  return normalized
}

function matchingReasoningLevel(
  value: string | undefined,
  reasoning: ModelReasoningConfig,
): string | undefined {
  const requested = value?.trim()
  if (!requested) {
    return undefined
  }
  return reasoning.levels.find((level) => level.level.toLowerCase() === requested.toLowerCase())
    ?.level
}

function inferReasoningMode(modelId: string, modelName: string): ModelReasoningMode {
  const normalized = `${modelId} ${modelName}`.toLowerCase()
  return normalized.includes('minimax-m3') ? 'toggle' : 'none'
}

export default ProviderSettings
