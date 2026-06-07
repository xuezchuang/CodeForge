import type { ProviderConfig } from '../types/provider'

export interface SelectableModel {
  id: string
  providerId: string
  credentialId: string | null
  credentialName: string | null
  modelId: string
  modelName: string
  label: string
}

export function getSelectableModels(providers: ProviderConfig[]): SelectableModel[] {
  const enabledProviders = providers.filter(
    (provider) =>
      provider.enabled ||
      provider.models.some((model) => model.enabled) ||
      (provider.credentials ?? []).some((credential) => credential.enabled),
  )

  return enabledProviders.flatMap((provider) => {
    const enabledCredentials = providerUsesCredentials(provider)
      ? (provider.credentials ?? []).filter((credential) => credential.enabled)
      : [{ id: null, name: null }]
    const enabledModels = (provider.models ?? []).filter((model) => model.enabled)
    if (enabledModels.length > 0) {
      return enabledModels.flatMap((model) => {
        const modelCredentials =
          providerUsesCredentials(provider) && model.credentialId
            ? enabledCredentials.filter((credential) => credential.id === model.credentialId)
            : enabledCredentials
        return modelCredentials.map((credential) =>
          selectableModel(provider, credential, model.id, model.name || model.id),
        )
      })
    }

    if (provider.defaultModel.trim().length === 0) {
      return []
    }

    return enabledCredentials.map((credential) =>
      selectableModel(provider, credential, provider.defaultModel, provider.defaultModel),
    )
  })
}

function providerUsesCredentials(provider: ProviderConfig): boolean {
  return provider.type !== 'ollama' && provider.type !== 'codex-cli'
}

function selectableModel(
  provider: ProviderConfig,
  credential: { id: string | null; name: string | null },
  modelId: string,
  modelName: string,
): SelectableModel {
  return {
    id: `${provider.id}:${credential.id ?? 'default'}:${modelId}`,
    providerId: provider.id,
    credentialId: credential.id,
    credentialName: credential.name,
    modelId,
    modelName,
    label: modelName,
  }
}
