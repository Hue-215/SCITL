import { useEffect, useState } from 'react'
import {
  addMcpServer,
  addProvider,
  deleteMcpServer,
  deleteProvider,
  failureText,
  fetchMcpTools,
  getSettings,
  setMcpServerEnabled,
  setMcpToolEnabled,
  updateGeneralSettings,
  updateLanguage,
  updateToolSettings,
} from './api'
import type { SettingsView } from './types'
import { isolated, t } from './i18n'
import { without } from './record'
import { GeneralTab } from './SettingsGeneral'
import { McpTab } from './SettingsMcp'
import { ProvidersTab } from './SettingsProviders'

interface SettingsProps {
  onClose: () => void
}

// 左のレールに並べるタブ(並び順のまま)。
const TABS = [
  { id: 'general', label: 'settings.nav.general' },
  { id: 'providers', label: 'settings.nav.provider' },
  { id: 'mcp', label: 'settings.nav.tools' },
] as const

type TabId = (typeof TABS)[number]['id']

// 設定画面。
export default function Settings({ onClose }: SettingsProps) {
  const [tab, setTab] = useState<TabId>('general')
  const [settings, setSettings] = useState<SettingsView | null>(null)
  const [error, setError] = useState<string | null>(null)

  const reload = async () => {
    try {
      setSettings(await getSettings())
      setError(null)
    } catch (e) {
      setError(failureText(e))
    }
  }

  useEffect(() => {
    // oxlint-disable-next-line react/set-state-in-effect -- IPCで読み込む。stateはawaitの後で変える
    void reload()
  }, [])

  // タブを切り替えたら、前のタブの操作で出たエラーは伏せる(残っていると、
  // 今見ているタブの内容に対する指摘のように見えるため)。
  const selectTab = (next: TabId) => {
    setTab(next)
    setError(null)
  }

  const runOrReportError = async (action: () => Promise<SettingsView>) => {
    try {
      setSettings(await action())
      setError(null)
    } catch (e) {
      setError(failureText(e))
    }
  }

  // 追加の結果は、エラーをフォームの直下に出すため、ここでは握らず呼び出し元へ返す。
  const applyAdded = async (action: () => Promise<SettingsView>) => {
    const next = await action()
    setSettings(next)
    setError(null)
    return next
  }

  // MCPサーバーのツール一覧を取得中のサーバーと、取得のエラー(サーバーごと、カード内に出す)。
  // 取得中にタブを切り替えても結果が失われないよう、タブではなくここで持つ。
  const [fetchingTools, setFetchingTools] = useState<string[]>([])
  const [toolFetchErrors, setToolFetchErrors] = useState<Record<string, string>>({})

  const fetchTools = async (serverId: string) => {
    setFetchingTools((prev) => [...prev, serverId])
    setToolFetchErrors((prev) => without(prev, serverId))
    try {
      setSettings(await fetchMcpTools(serverId))
    } catch (e) {
      setToolFetchErrors((prev) => ({
        ...prev,
        [serverId]: t('common.fetch_failed', { error: isolated(failureText(e)) }),
      }))
    } finally {
      setFetchingTools((prev) => prev.filter((id) => id !== serverId))
    }
  }

  return (
    <div className="settings">
      <header className="settings-header">
        <button
          type="button"
          className="icon-button"
          onClick={onClose}
          aria-label={t('settings.back_tooltip')}
          title={t('settings.back_tooltip')}
        >
          ←
        </button>
        <h1>{t('settings.heading')}</h1>
      </header>

      <div className="settings-body">
        <nav className="settings-rail">
          {TABS.map(({ id, label }) => (
            <button
              key={id}
              type="button"
              className={tab === id ? 'settings-tab selected' : 'settings-tab'}
              onClick={() => selectTab(id)}
            >
              {t(label)}
            </button>
          ))}
        </nav>

        <div className="settings-content">
          <div className="settings-column">
            {settings?.config_error && (
              <p className="error">
                {t('settings.config_unreadable', { error: isolated(settings.config_error) })}
              </p>
            )}
            {error && <p className="error">{error}</p>}

            {settings === null ? (
              <p>{t('common.loading')}</p>
            ) : tab === 'general' ? (
              <GeneralTab
                settings={settings}
                onSave={(update) => runOrReportError(() => updateGeneralSettings(update))}
                onSaveLanguage={(language) => runOrReportError(() => updateLanguage(language))}
              />
            ) : tab === 'providers' ? (
              <ProvidersTab
                settings={settings}
                onAddProvider={async (name, format, baseUrl, apiKey) => {
                  await applyAdded(() => addProvider(name, format, baseUrl, apiKey))
                }}
                onDeleteProvider={(id) => runOrReportError(() => deleteProvider(id))}
                onUpdateModels={runOrReportError}
              />
            ) : (
              <McpTab
                settings={settings}
                onSaveLimits={(maxRoundsPerTurn, totalTimeoutSecs) =>
                  runOrReportError(() =>
                    updateToolSettings({ maxRoundsPerTurn, totalTimeoutSecs }),
                  )
                }
                onAddServer={async (name, endpoint) => {
                  // 追加したら続けて1回ツール一覧を取得する。失敗しても登録は残し、
                  // エラーはそのカードに出す。追加したサーバーは、追加前に無かったidで
                  // 見分ける。
                  const before = new Set(settings.mcp_servers.map((s) => s.id))
                  const next = await applyAdded(() => addMcpServer(name, endpoint))
                  const added = next.mcp_servers.find((s) => !before.has(s.id))
                  if (added) void fetchTools(added.id)
                }}
                onDeleteServer={(id) => runOrReportError(() => deleteMcpServer(id))}
                onSetServerEnabled={(id, enabled) =>
                  runOrReportError(() => setMcpServerEnabled(id, enabled))
                }
                onSetToolEnabled={(id, toolName, enabled) =>
                  runOrReportError(() => setMcpToolEnabled(id, toolName, enabled))
                }
                fetchingTools={fetchingTools}
                toolFetchErrors={toolFetchErrors}
                onFetchTools={(id) => void fetchTools(id)}
              />
            )}
          </div>
        </div>
      </div>
    </div>
  )
}
