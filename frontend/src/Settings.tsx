import { useCallback, useEffect, useRef, useState } from 'react'
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
  updateSyncSettings,
  updateToolSettings,
} from './api'
import { type AddResult, rejectionText } from './rejection'
import type { FormOutcome, SettingsView } from './types'
import { Drawer, DrawerToggle } from './Drawer'
import { isolated, t } from './i18n'
import Icon from './Icon'
import { without } from './record'
import { GeneralTab } from './SettingsGeneral'
import { McpTab } from './SettingsMcp'
import { MemoryTab } from './SettingsMemory'
import { ProvidersTab } from './SettingsProviders'
import { SyncScreen, SyncTab } from './SettingsSync'
import { type Drafts, DraftsContext } from './settingsInput'
import { useCloseOnBack } from './useCloseOnBack'
import { useDrawer } from './useDrawer'
import { useSwipeToOpen } from './useDrawerSwipe'

interface SettingsProps {
  onClose: () => void
}

// 左のカラムに並べるタブ(並び順のまま)。
const TABS = [
  { id: 'general', label: 'settings.nav.general' },
  { id: 'memory', label: 'settings.nav.memory' },
  { id: 'providers', label: 'settings.nav.provider' },
  { id: 'mcp', label: 'settings.nav.tools' },
  { id: 'sync', label: 'settings.nav.sync' },
] as const

type TabId = (typeof TABS)[number]['id']

export default function Settings({ onClose }: SettingsProps) {
  const [tab, setTab] = useState<TabId>('general')
  const [settings, setSettings] = useState<SettingsView | null>(null)
  const [error, setError] = useState<string | null>(null)
  // 同期の画面を開いている間は、本文の代わりに窓全体へ出す(出るのは同期の画面の「戻る」だけ)。
  const [syncOpen, setSyncOpen] = useState(false)
  // 自動保存の欄の書きかけ(`useDraft`)と、その保存でコマンド自体が失敗した回数。
  const [drafts] = useState<Drafts>(() => ({ entries: new Set(), closed: false }))
  const saveFailures = useRef(0)
  // 狭い窓で畳む左のメニュー。タブを選んだら閉じる。
  const drawer = useDrawer()
  const swipeToOpen = useSwipeToOpen(drawer)

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
      if (outcome.status === 'accepted') {
        setSettings(outcome.value)
        setError(null)
      }
    } catch (e) {
      saveFailures.current++
      setError(failureText(e))
    }
    return []
  }

  // 自動保存の欄の書きかけをすべて保存する。`rejected`は欄の誤りで断られたもの、`failed`は
  // コマンド自体が失敗したものがある。
  const flushDrafts = useCallback(async () => {
    const failuresBefore = saveFailures.current
    let rejected = false
    for (const flush of [...drafts.entries]) {
      try {
        if ((await flush()).length > 0) rejected = true
      } catch {
        saveFailures.current++
      }
    }
    if (rejected) return 'rejected'
    return saveFailures.current === failuresBefore ? 'saved' : 'failed'
  }, [drafts])

  // 設定画面を閉じる(「←」とAndroidの「戻る」)。書きかけを保存してから閉じ、保存できなければ
  // 留まる(断られた理由は欄の下に、コマンドの失敗は上のエラー欄に出ている)。閉じたあと会話側が
  // 設定を読み直すので、保存を待ってから閉じる。コマンドの失敗は欄を直しても解けないので、
  // 続けてもう一度閉じようとしたら、書きかけを捨てて閉じる(閉じられなくならないように)。
  const leaving = useRef(false)
  const failedToLeave = useRef(false)
  const leave = async () => {
    if (leaving.current) return
    leaving.current = true
    try {
      const outcome = await flushDrafts()
      if (outcome === 'saved' || (outcome === 'failed' && failedToLeave.current)) {
        drafts.closed = true
        onClose()
        return
      }
      failedToLeave.current = outcome === 'failed'
      // 狭い窓では「←」が引き出した左のカラムにあり、開いたままだと理由が隠れる。
      drawer.close()
    } finally {
      leaving.current = false
    }
  }
  // 設定画面の中で開いたもの(ダイアログ・左のカラム)が先に閉じる。
  useCloseOnBack(true, () => void leave())

  // 裏へ回ったときにも保存する(Androidで、裏へ回したまま終了されても残るように)。
  useEffect(() => {
    const onVisibilityChange = () => {
      if (document.visibilityState === 'hidden') void flushDrafts()
    }
    document.addEventListener('visibilitychange', onVisibilityChange)
    return () => document.removeEventListener('visibilitychange', onVisibilityChange)
  }, [flushDrafts])

  // 追加のフォームは、欄の誤りもコマンド自体の失敗もフォームの直下に出すため、失敗は握らず
  // 呼び出し元へ投げる。欄の誤りは文言にして返す。
  const applyAdded = async <T,>(
    action: () => Promise<FormOutcome<T>>,
    accepted: (value: T) => SettingsView,
  ): Promise<AddResult> => {
    const outcome = await action()
    switch (outcome.status) {
      case 'rejected':
        return { added: false, errors: outcome.reasons.map(rejectionText) }
      // 確認のダイアログで取りやめた。入力を残し、何も出さない。
      case 'cancelled':
        return { added: false, errors: [] }
      case 'accepted':
        setSettings(accepted(outcome.value))
        setError(null)
        return { added: true, errors: [] }
    }
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

  const tabLabel = TABS.find(({ id }) => id === tab)?.label ?? TABS[0].label

  return (
    // 左のカラムはチャットのサイドバーと同じ形の入れ物で、設定を開いている間はサイドバーが設定の
    // メニューに切り替わったように見せる(畳み方・引き出し方もサイドバーと同じ)。
    // 同期の画面を開いている間は、広い窓でも狭い窓と同じく左のカラムを畳む(窓を狭めたときと同じく
    // 左の外へ滑る)。引き出すボタンは同期の画面に無いので、閉じるまで出てこない。
    <div className={drawer.narrow || syncOpen ? 'settings narrow' : 'settings'}>
      <Drawer drawer={drawer}>
        <nav className="sidebar">
          <div className="sidebar-top-row">
            <button
              type="button"
              className="icon-button"
              onClick={() => void leave()}
              aria-label={t('settings.back_tooltip')}
              title={t('settings.back_tooltip')}
            >
              <Icon name="arrow_back" />
            </button>
            <span className="settings-nav-heading">{t('settings.heading')}</span>
          </div>
          <div className="settings-tabs">
            {TABS.map(({ id, label }) => (
              <button
                key={id}
                type="button"
                className={tab === id ? 'settings-tab selected' : 'settings-tab'}
                aria-current={tab === id ? 'page' : undefined}
                onClick={() => selectTab(id)}
              >
                {t(label)}
              </button>
            ))}
          </div>
        </nav>
      </Drawer>

      {syncOpen ? (
        <SyncScreen onClose={() => setSyncOpen(false)} />
      ) : (
        <div className="settings-main" inert={drawer.shown} {...swipeToOpen}>
          <header className="pane-top">
            <div className="settings-header">
              <DrawerToggle drawer={drawer} label={t('settings.nav_open_tooltip')} />
              <h1>{t(tabLabel)}</h1>
            </div>
          </header>

          <div className="settings-content">
            <div className="settings-column">
              {settings?.config_error && (
                <p className="error">
                  {t('settings.config_unreadable', { error: isolated(settings.config_error) })}
                </p>
              )}
              {error && <p className="error">{error}</p>}

              <DraftsContext value={drafts}>
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
                ) : tab === 'sync' ? (
                  <SyncTab
                    settings={settings}
                    onStart={() => setSyncOpen(true)}
                    onSaveListenPort={(text) => saveField(() => updateSyncSettings(text))}
                  />
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
                              [server_id]: t('common.fetch_failed', {
                                error: isolated(tools_error),
                              }),
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
              </DraftsContext>
            </div>
          </div>
        </div>
      )}
    </div>
  )
}
