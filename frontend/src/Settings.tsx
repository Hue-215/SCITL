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
import { rejectionText } from './rejection'
import type { FormOutcome, SettingsView } from './types'
import { Drawer, DrawerToggle } from './Drawer'
import { isolated, t } from './i18n'
import { without } from './record'
import { GeneralTab } from './SettingsGeneral'
import { McpTab } from './SettingsMcp'
import { MemoryTab } from './SettingsMemory'
import { ProvidersTab } from './SettingsProviders'
import { useDrawer } from './useDrawer'

interface SettingsProps {
  onClose: () => void
}

// 左のレールに並べるタブ(並び順のまま)。
const TABS = [
  { id: 'general', label: 'settings.nav.general' },
  { id: 'memory', label: 'settings.nav.memory' },
  { id: 'providers', label: 'settings.nav.provider' },
  { id: 'mcp', label: 'settings.nav.tools' },
] as const

type TabId = (typeof TABS)[number]['id']

// 設定画面。
export default function Settings({ onClose }: SettingsProps) {
  const [tab, setTab] = useState<TabId>('general')
  const [settings, setSettings] = useState<SettingsView | null>(null)
  const [error, setError] = useState<string | null>(null)
  // 狭い窓で畳む左のメニュー。タブを選んだら閉じる。
  const drawer = useDrawer()

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
    drawer.close()
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

  // 欄の誤り(Rust側が断った理由)は文言にして呼び出し元へ返し、欄の近くに出させる。
  // 受け付けたら空を返す。コマンド自体の失敗は上のエラー欄に出す。
  const saveField = async (action: () => Promise<FormOutcome<SettingsView>>) => {
    try {
      const outcome = await action()
      if (outcome.status === 'rejected') return outcome.reasons.map(rejectionText)
      setSettings(outcome.value)
      setError(null)
    } catch (e) {
      setError(failureText(e))
    }
    return []
  }

  // 追加のフォームは、欄の誤りもコマンド自体の失敗もフォームの直下に出すため、失敗は握らず
  // 呼び出し元へ投げる。欄の誤りは文言にして返し、受け付けたら空を返す。
  const applyAdded = async <T,>(
    action: () => Promise<FormOutcome<T>>,
    accepted: (value: T) => SettingsView,
  ) => {
    const outcome = await action()
    if (outcome.status === 'rejected') return outcome.reasons.map(rejectionText)
    setSettings(accepted(outcome.value))
    setError(null)
    return []
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
    <div className={drawer.narrow ? 'settings narrow' : 'settings'}>
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
        <DrawerToggle drawer={drawer} label={t('settings.nav_open_tooltip')} />
        <h1>{t('settings.heading')}</h1>
      </header>

      <div className="settings-body">
        <Drawer drawer={drawer}>
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
        </Drawer>

        <div className="settings-content" inert={drawer.shown}>
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
                onSave={(update) => saveField(() => updateGeneralSettings(update))}
                onSaveLanguage={(language) => runOrReportError(() => updateLanguage(language))}
              />
            ) : tab === 'memory' ? (
              <MemoryTab />
            ) : tab === 'providers' ? (
              <ProvidersTab
                settings={settings}
                onAddProvider={(name, format, baseUrl, apiKey, headers) =>
                  applyAdded(
                    () => addProvider(name, format, baseUrl, apiKey, headers),
                    (next) => next,
                  )
                }
                onDeleteProvider={(id) => runOrReportError(() => deleteProvider(id))}
                onUpdateModels={runOrReportError}
                onSaveModelField={saveField}
              />
            ) : (
              <McpTab
                settings={settings}
                onSaveLimits={(maxRoundsPerTurn, totalTimeoutSecs) =>
                  saveField(() => updateToolSettings({ maxRoundsPerTurn, totalTimeoutSecs }))
                }
                onAddServer={(name, endpoint) =>
                  // 追加に続くツール一覧の取得はRust側が行う。取得の失敗はそのカードに出す。
                  applyAdded(
                    () => addMcpServer(name, endpoint),
                    ({ settings: next, server_id, tools_error }) => {
                      if (tools_error !== null) {
                        setToolFetchErrors((prev) => ({
                          ...prev,
                          [server_id]: t('common.fetch_failed', { error: isolated(tools_error) }),
                        }))
                      }
                      return next
                    },
                  )
                }
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
