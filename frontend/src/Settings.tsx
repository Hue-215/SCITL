import { useEffect, useState } from 'react'
import {
  addModel,
  addProvider,
  deleteProvider,
  getSettings,
  removeModel,
  setActiveModel,
  setActiveProvider,
  updateGeneralSettings,
} from './api'
import type { ApiFormat, SettingsView } from './types'

interface SettingsProps {
  onClose: () => void
}

const DEFAULT_BASE_URL_BY_FORMAT: Record<ApiFormat, string> = {
  open_ai_compat: 'https://api.openai.com/v1',
}

// 設定画面(legacy/frontend.md 2〜3節)。「ツール」タブはMCPクライアントという新しい
// 外部通信手段の導入を伴い、Opusレビュー対象のため別Issueで実装する(Issue #22コメント参照)。
export default function Settings({ onClose }: SettingsProps) {
  const [tab, setTab] = useState<'general' | 'providers'>('general')
  const [settings, setSettings] = useState<SettingsView | null>(null)
  const [error, setError] = useState<string | null>(null)

  const reload = async () => {
    try {
      setSettings(await getSettings())
      setError(null)
    } catch (e) {
      setError(String(e))
    }
  }

  useEffect(() => {
    void reload()
  }, [])

  const runOrReportError = async (action: () => Promise<SettingsView>) => {
    try {
      setSettings(await action())
      setError(null)
    } catch (e) {
      setError(String(e))
    }
  }

  return (
    <div className="settings">
      <header className="settings-header">
        <button
          type="button"
          className="settings-back"
          onClick={onClose}
          aria-label="戻る"
          title="戻る"
        >
          ←
        </button>
        <h1>設定</h1>
      </header>

      <div className="settings-body">
        <nav className="settings-rail">
          <button
            type="button"
            className={tab === 'general' ? 'settings-tab selected' : 'settings-tab'}
            onClick={() => setTab('general')}
          >
            一般
          </button>
          <button
            type="button"
            className={tab === 'providers' ? 'settings-tab selected' : 'settings-tab'}
            onClick={() => setTab('providers')}
          >
            APIプロバイダー
          </button>
        </nav>

        <div className="settings-content">
          {error && <p className="error">{error}</p>}

          {settings === null ? (
            <p>読み込み中…</p>
          ) : tab === 'general' ? (
            <GeneralTab
              settings={settings}
              onSave={(systemPrompt, timeout) =>
                runOrReportError(() => updateGeneralSettings(systemPrompt, timeout))
              }
            />
          ) : (
            <ProvidersTab
              settings={settings}
              onAddProvider={(name, format, baseUrl, apiKey) =>
                runOrReportError(() => addProvider(name, format, baseUrl, apiKey))
              }
              onDeleteProvider={(id) => runOrReportError(() => deleteProvider(id))}
              onSetActiveProvider={(id) => runOrReportError(() => setActiveProvider(id))}
              onAddModel={(providerId, model) =>
                runOrReportError(() => addModel(providerId, model))
              }
              onRemoveModel={(providerId, model) =>
                runOrReportError(() => removeModel(providerId, model))
              }
              onSetActiveModel={(providerId, model) =>
                runOrReportError(() => setActiveModel(providerId, model))
              }
            />
          )}
        </div>
      </div>
    </div>
  )
}

interface GeneralTabProps {
  settings: SettingsView
  onSave: (systemPrompt: string | null, responseTimeoutSecs: number | null) => void
}

// フォーカスを外すと自動保存(legacy/frontend.md 2節)。入力中は自身のstateだけを更新し、
// blur時にのみ親へ確定した値を渡す。
function GeneralTab({ settings, onSave }: GeneralTabProps) {
  const [systemPrompt, setSystemPrompt] = useState(settings.general.system_prompt ?? '')
  const [timeoutText, setTimeoutText] = useState(
    settings.general.response_timeout_secs?.toString() ?? '',
  )
  const [timeoutError, setTimeoutError] = useState(false)

  useEffect(() => {
    setSystemPrompt(settings.general.system_prompt ?? '')
    setTimeoutText(settings.general.response_timeout_secs?.toString() ?? '')
  }, [settings])

  const saveTimeout = () => {
    const trimmed = timeoutText.trim()
    if (trimmed === '') {
      setTimeoutError(false)
      onSave(systemPrompt || null, null)
      return
    }
    const parsed = Number(trimmed)
    if (!Number.isInteger(parsed) || parsed <= 0) {
      setTimeoutError(true)
      return
    }
    setTimeoutError(false)
    onSave(systemPrompt || null, parsed)
  }

  return (
    <div className="settings-panel">
      <label className="settings-field">
        <span>システムプロンプト</span>
        <textarea
          rows={6}
          value={systemPrompt}
          onChange={(e) => setSystemPrompt(e.target.value)}
          onBlur={() => onSave(systemPrompt || null, settings.general.response_timeout_secs)}
        />
      </label>

      <label className="settings-field">
        <span>応答タイムアウト(秒)</span>
        <input
          type="text"
          inputMode="numeric"
          value={timeoutText}
          onChange={(e) => setTimeoutText(e.target.value)}
          onBlur={saveTimeout}
          placeholder="未設定(既定値を使用)"
        />
        {timeoutError && <p className="error">1以上の整数を入力してください</p>}
      </label>
    </div>
  )
}

interface ProvidersTabProps {
  settings: SettingsView
  onAddProvider: (
    name: string,
    apiFormat: ApiFormat,
    baseUrl: string,
    apiKey: string | null,
  ) => void
  onDeleteProvider: (providerId: string) => void
  onSetActiveProvider: (providerId: string) => void
  onAddModel: (providerId: string, model: string) => void
  onRemoveModel: (providerId: string, model: string) => void
  onSetActiveModel: (providerId: string, model: string) => void
}

function ProvidersTab({
  settings,
  onAddProvider,
  onDeleteProvider,
  onSetActiveProvider,
  onAddModel,
  onRemoveModel,
  onSetActiveModel,
}: ProvidersTabProps) {
  const [newModelByProvider, setNewModelByProvider] = useState<Record<string, string>>({})

  return (
    <div className="settings-panel">
      <ul className="provider-list">
        {settings.providers.map((provider) => (
          <li key={provider.id} className="provider-card">
            <div className="provider-card-header">
              <label>
                <input
                  type="radio"
                  name="active-provider"
                  checked={settings.active_provider_id === provider.id}
                  onChange={() => onSetActiveProvider(provider.id)}
                />
                <strong>{provider.name}</strong>
              </label>
              <button
                type="button"
                className="danger"
                onClick={() => {
                  if (
                    window.confirm(
                      `プロバイダー「${provider.name}」を削除しますか?保存済みのAPIキーも同時に削除されます。`,
                    )
                  ) {
                    onDeleteProvider(provider.id)
                  }
                }}
              >
                削除
              </button>
            </div>
            <p className="provider-card-meta">
              {provider.base_url} · {provider.has_api_key ? 'APIキー設定済み' : 'APIキー未設定'}
            </p>

            <ul className="model-list">
              {provider.models.map((model) => (
                <li key={model} className="model-row">
                  <label>
                    <input
                      type="radio"
                      name={`active-model-${provider.id}`}
                      checked={provider.active_model === model}
                      onChange={() => onSetActiveModel(provider.id, model)}
                    />
                    {model}
                  </label>
                  <button type="button" onClick={() => onRemoveModel(provider.id, model)}>
                    削除
                  </button>
                </li>
              ))}
              {provider.models.length === 0 && <li className="model-row-empty">モデル未登録</li>}
            </ul>

            <form
              className="model-add-form"
              onSubmit={(e) => {
                e.preventDefault()
                const model = (newModelByProvider[provider.id] ?? '').trim()
                if (!model) return
                onAddModel(provider.id, model)
                setNewModelByProvider((prev) => ({ ...prev, [provider.id]: '' }))
              }}
            >
              <input
                value={newModelByProvider[provider.id] ?? ''}
                onChange={(e) =>
                  setNewModelByProvider((prev) => ({ ...prev, [provider.id]: e.target.value }))
                }
                placeholder="モデル名を入力して追加"
              />
              <button type="submit">追加</button>
            </form>
          </li>
        ))}
        {settings.providers.length === 0 && <p>プロバイダーが未登録です。</p>}
      </ul>

      <AddProviderForm onAdd={onAddProvider} />
    </div>
  )
}

interface AddProviderFormProps {
  onAdd: (name: string, apiFormat: ApiFormat, baseUrl: string, apiKey: string | null) => void
}

function AddProviderForm({ onAdd }: AddProviderFormProps) {
  const [name, setName] = useState('')
  const [apiFormat, setApiFormat] = useState<ApiFormat>('open_ai_compat')
  const [baseUrl, setBaseUrl] = useState(DEFAULT_BASE_URL_BY_FORMAT.open_ai_compat)
  const [apiKey, setApiKey] = useState('')

  return (
    <form
      className="provider-add-form"
      onSubmit={(e) => {
        e.preventDefault()
        if (!name.trim() || !baseUrl.trim()) return
        onAdd(name.trim(), apiFormat, baseUrl.trim(), apiKey || null)
        setName('')
        setApiKey('')
      }}
    >
      <h2>プロバイダーを追加</h2>
      <label className="settings-field">
        <span>表示名</span>
        <input value={name} onChange={(e) => setName(e.target.value)} required />
      </label>
      <label className="settings-field">
        <span>API形式</span>
        <select
          value={apiFormat}
          onChange={(e) => {
            const format = e.target.value as ApiFormat
            setApiFormat(format)
            setBaseUrl(DEFAULT_BASE_URL_BY_FORMAT[format])
          }}
        >
          <option value="open_ai_compat">OpenAI互換</option>
        </select>
      </label>
      <label className="settings-field">
        <span>ベースURL</span>
        <input value={baseUrl} onChange={(e) => setBaseUrl(e.target.value)} required />
      </label>
      <label className="settings-field">
        <span>APIキー</span>
        <input
          type="password"
          value={apiKey}
          onChange={(e) => setApiKey(e.target.value)}
          placeholder="ローカル推論サーバー等では省略可"
        />
      </label>
      <button type="submit">追加</button>
    </form>
  )
}
