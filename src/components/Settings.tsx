import { useEffect, useRef, useState } from 'react'
import { Save } from 'lucide-react'
import { updateSettings } from '../api/tauriApi'
import type { ProviderConfig } from '../types/provider'
import type { AppSettings, UiPreferences as UiPreferencesModel } from '../types/settings'
import ProviderSettings from './ProviderSettings'
import UiPreferences from './UiPreferences'
import VisualStudioSettings from './VisualStudioSettings'

interface SettingsProps {
  settings: AppSettings | null
  providers: ProviderConfig[]
  onSettingsChanged: (settings: AppSettings) => void
  onProvidersChanged: (providers: ProviderConfig[]) => void
  onError: (message: string) => void
  onNotice: (message: string) => void
}

function Settings({
  settings,
  providers,
  onSettingsChanged,
  onProvidersChanged,
  onError,
  onNotice,
}: SettingsProps) {
  if (!settings) {
    return (
      <section className="page-section">
        <div className="empty-state">Loading settings...</div>
      </section>
    )
  }

  return (
    <SettingsForm
      key={`${settings.dataDir}:${settings.configPath}`}
      settings={settings}
      providers={providers}
      onSettingsChanged={onSettingsChanged}
      onProvidersChanged={onProvidersChanged}
      onError={onError}
      onNotice={onNotice}
    />
  )
}

interface SettingsFormProps {
  settings: AppSettings
  providers: ProviderConfig[]
  onSettingsChanged: (settings: AppSettings) => void
  onProvidersChanged: (providers: ProviderConfig[]) => void
  onError: (message: string) => void
  onNotice: (message: string) => void
}

function SettingsForm({
  settings,
  providers,
  onSettingsChanged,
  onProvidersChanged,
  onError,
  onNotice,
}: SettingsFormProps) {
  const [devenvPath, setDevenvPath] = useState(settings.devenvPath ?? '')
  const [uiPreferences, setUiPreferences] = useState(settings.uiPreferences)
  const [busy, setBusy] = useState(false)
  const hasMountedProviders = useRef(false)
  const lastSavedProvidersJson = useRef(JSON.stringify(providers))

  const saveSettings = async (
    nextProviders = providers,
    nextUiPreferences: UiPreferencesModel = uiPreferences,
    notice = 'Settings saved',
  ) => {
    try {
      setBusy(true)
      const saved = await updateSettings({
        devenvPath: devenvPath.trim().length > 0 ? devenvPath.trim() : null,
        providerNotes: settings.providerNotes ?? null,
        uiPreferences: nextUiPreferences,
        providers: nextProviders,
      })
      lastSavedProvidersJson.current = JSON.stringify(saved.providers)
      onSettingsChanged(saved)
      onProvidersChanged(saved.providers)
      onNotice(notice)
    } catch (caught) {
      onError(caught instanceof Error ? caught.message : String(caught))
    } finally {
      setBusy(false)
    }
  }

  useEffect(() => {
    const providersJson = JSON.stringify(providers)
    if (!hasMountedProviders.current) {
      hasMountedProviders.current = true
      lastSavedProvidersJson.current = providersJson
      return
    }
    if (providersJson === lastSavedProvidersJson.current) {
      return
    }

    const timerId = window.setTimeout(() => {
      void saveSettings(providers, uiPreferences, 'Provider settings saved')
    }, 300)

    return () => {
      window.clearTimeout(timerId)
    }
  }, [providers])

  return (
    <section className="page-section settings-page">
      <div className="section-header">
        <div>
          <h2>Settings</h2>
          <p>Configure global desktop preferences and local provider profiles.</p>
        </div>
        <button
          type="button"
          className="primary-button"
          onClick={() => void saveSettings()}
          disabled={busy}
        >
          <Save size={16} aria-hidden="true" />
          Save Settings
        </button>
      </div>

      <div className="settings-grid">
        <div className="settings-column">
          <VisualStudioSettings
            devenvPath={devenvPath}
            dataDir={settings.dataDir}
            configPath={settings.configPath}
            onChange={setDevenvPath}
            onError={onError}
          />
          <UiPreferences
            preferences={uiPreferences}
            onChange={setUiPreferences}
          />
        </div>
        <ProviderSettings
          configPath={settings.configPath}
          providers={providers}
          onProvidersChanged={onProvidersChanged}
        />
      </div>
    </section>
  )
}

export default Settings
