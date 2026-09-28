import { useEffect, useId, useState } from 'react'
import type { ChangeEvent, FormEvent } from 'react'
import {
  addMcpServer,
  addModels,
  addProvider,
  deleteMcpServer,
  deleteProvider,
  detectModelCapabilities,
  exportMarkdown,
  failureText,
  fetchMcpTools,
  getSettings,
  listProviderModels,
  openExportFolder,
  removeModel,
  resetModelCapabilities,
  setMcpServerEnabled,
  setMcpToolEnabled,
  setModelCapability,
  setModelContextLength,
  setModelVisible,
  updateGeneralSettings,
  updateLanguage,
  updateToolSettings,
  type NewMcpEndpoint,
} from './api'
import type {
  ApiFormat,
  AvailableModel,
  Capability,
  ExportSummary,
  Language,
  McpServerView,
  ModelView,
  ProviderView,
  SettingsView,
} from './types'
import { matchQuery } from './search'
import { ConfirmButton } from './Dialog'
import Dropdown from './Dropdown'
import { currentLanguage, languageName, LANGUAGES, t, type MessageKey } from './i18n'

interface SettingsProps {
  onClose: () => void
}

const DEFAULT_BASE_URL_BY_FORMAT: Record<ApiFormat, string> = {
  open_ai_compat: 'https://api.openai.com/v1',
}

const API_FORMAT_LABELS: Record<ApiFormat, MessageKey> = {
  open_ai_compat: 'settings.provider.formats.open_ai_compat',
}

type Transport = McpServerView['endpoint']['transport']

const TRANSPORT_LABELS: Record<Transport, MessageKey> = {
  stdio: 'settings.tools.transport_stdio',
  streamable_http: 'settings.tools.transport_http',
}

// httpの許可範囲(crates/scitl-core/src/net.rsのclassify_host)が変わったときに
// 片方だけ直し忘れないよう、URLを入力させる箇所で共通のヒント文を使う。
function httpPlainTextHint(secret: string): string {
  return t('settings.http_warning', { secret })
}

// 検索欄のあるモデルの一覧(登録済みの表・取得したモデルの候補)で、絞り込んだ結果が空のときの一文。
function noModelMatchText(query: string): string {
  return t('settings.model.no_match', { query: query.trim() })
}

// 設定画面(legacy/frontend.md 2〜4節)。
export default function Settings({ onClose }: SettingsProps) {
  const [tab, setTab] = useState<'general' | 'providers' | 'mcp'>('general')
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
    void reload()
  }, [])

  // タブを切り替えたら、前のタブの操作で出たエラーは伏せる(残っていると、
  // 今見ているタブの内容に対する指摘のように見えるため)。
  const selectTab = (next: typeof tab) => {
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
          <button
            type="button"
            className={tab === 'general' ? 'settings-tab selected' : 'settings-tab'}
            onClick={() => selectTab('general')}
          >
            {t('settings.nav.general')}
          </button>
          <button
            type="button"
            className={tab === 'providers' ? 'settings-tab selected' : 'settings-tab'}
            onClick={() => selectTab('providers')}
          >
            {t('settings.nav.provider')}
          </button>
          <button
            type="button"
            className={tab === 'mcp' ? 'settings-tab selected' : 'settings-tab'}
            onClick={() => selectTab('mcp')}
          >
            {t('settings.nav.tools')}
          </button>
        </nav>

        <div className="settings-content">
          <div className="settings-column">
            {settings?.config_error && (
              <p className="error">
                {t('settings.config_unreadable', { error: settings.config_error })}
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
                  // 追加のエラーはフォームの直下に出すため、ここでは握らず呼び出し元へ返す。
                  setSettings(await addProvider(name, format, baseUrl, apiKey))
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
                  // 追加のエラーはフォームの直下に出すため、ここでは握らず呼び出し元へ返す。
                  // 追加したサーバーは、追加前に無かったidで見分ける。
                  const before = new Set(settings.mcp_servers.map((s) => s.id))
                  const next = await addMcpServer(name, endpoint)
                  setSettings(next)
                  return next.mcp_servers.find((s) => !before.has(s.id))?.id ?? null
                }}
                onDeleteServer={(id) => runOrReportError(() => deleteMcpServer(id))}
                onSetServerEnabled={(id, enabled) =>
                  runOrReportError(() => setMcpServerEnabled(id, enabled))
                }
                onSetToolEnabled={(id, toolName, enabled) =>
                  runOrReportError(() => setMcpToolEnabled(id, toolName, enabled))
                }
                onFetchTools={async (id) => {
                  // 取得のエラーはカード内に出すため、ここでは握らず呼び出し元へ返す
                  // (どのサーバーで失敗したかが分かるように。legacy/frontend.md 4節)。
                  setSettings(await fetchMcpTools(id))
                }}
              />
            )}
          </div>
        </div>
      </div>
    </div>
  )
}

interface NumberFieldProps {
  label: string
  // 保存済みの値。nullは未設定(既定値を使う)。
  value: number | null
  // 未設定のときに使われる値。プレースホルダに出す。
  defaultValue: number
  hint?: string
  onSave: (value: number | null) => void
}

// フォーカスを外すと自動保存する数値入力(legacy/frontend.md 2節・4節)。入力中は自身の
// stateだけを更新し、blur時にのみ親へ確定した値を渡す。入力チェック(空欄は未設定、
// それ以外は1以上の整数)をこの1箇所に閉じ、設定欄(NumberField)とモデル表の
// コンテキスト長の両方がこれを使う(ui.md 1節)。
function usePositiveIntegerInput(value: number | null, onSave: (value: number | null) => void) {
  const [text, setText] = useState(value?.toString() ?? '')
  const [invalid, setInvalid] = useState(false)

  useEffect(() => {
    setText(value?.toString() ?? '')
    setInvalid(false)
  }, [value])

  const save = () => {
    // IMEを切り忘れて打った全角の数字も受け付ける。
    const trimmed = text.normalize('NFKC').trim()
    const parsed = trimmed === '' ? null : Number(trimmed)
    if (parsed !== null && (!Number.isInteger(parsed) || parsed <= 0)) {
      setInvalid(true)
      return
    }
    setInvalid(false)
    // フォーカスが通り過ぎただけで保存しない(モデル表では行ごとに欄がある)。
    if (parsed !== value) onSave(parsed)
  }

  return {
    invalid,
    inputProps: {
      type: 'text',
      inputMode: 'numeric' as const,
      value: text,
      onChange: (e: ChangeEvent<HTMLInputElement>) => setText(e.target.value),
      onBlur: save,
      'aria-invalid': invalid,
    },
  }
}

function NumberField({ label, value, defaultValue, hint, onSave }: NumberFieldProps) {
  const { invalid, inputProps } = usePositiveIntegerInput(value, onSave)

  return (
    <label className="settings-field">
      <span>{label}</span>
      <input {...inputProps} placeholder={t('settings.unset_default_hint', { value: defaultValue })} />
      {hint && <p className="settings-hint">{hint}</p>}
      {invalid && <p className="error">{t('errors.positive_integer')}</p>}
    </label>
  )
}

type GeneralUpdate = Parameters<typeof updateGeneralSettings>[0]

interface GeneralTabProps {
  settings: SettingsView
  onSave: (update: GeneralUpdate) => void
  onSaveLanguage: (language: Language) => void
}

interface PromptFieldProps {
  label: string
  value: string
  onChange: (value: string) => void
  onBlur: () => void
  caption?: string
}

function PromptField({ label, value, onChange, onBlur, caption }: PromptFieldProps) {
  return (
    <label className="settings-field">
      <span>{label}</span>
      <textarea value={value} onChange={(e) => onChange(e.target.value)} onBlur={onBlur} />
      {caption && <p className="settings-hint">{caption}</p>}
    </label>
  )
}

// フォーカスを外すと自動保存(legacy/frontend.md 2節)。入力中は自身のstateだけを更新し、
// blur時にのみ親へ確定した値を渡す。
//
// 既定の文面を持つ欄は、未設定の間は既定の文面を表示する(書き換えの起点にできるように)。
// 空欄と既定の文面のままの値は、Rust側が未設定として保存する(`Settings::update_general`)。
function GeneralTab({ settings, onSave, onSaveLanguage }: GeneralTabProps) {
  const { general } = settings
  const languageLabelId = useId()
  const [systemPrompt, setSystemPrompt] = useState(general.system_prompt ?? '')
  const [taskChatSystemPrompt, setTaskChatSystemPrompt] = useState(
    general.task_chat_system_prompt ?? general.default_task_chat_system_prompt,
  )
  const [taskOpeningMessage, setTaskOpeningMessage] = useState(
    general.task_opening_message ?? general.default_task_opening_message,
  )

  useEffect(() => {
    setSystemPrompt(general.system_prompt ?? '')
    setTaskChatSystemPrompt(
      general.task_chat_system_prompt ?? general.default_task_chat_system_prompt,
    )
    setTaskOpeningMessage(general.task_opening_message ?? general.default_task_opening_message)
  }, [general])

  const current = (): GeneralUpdate => ({
    systemPrompt: systemPrompt || null,
    taskChatSystemPrompt: taskChatSystemPrompt || null,
    taskOpeningMessage: taskOpeningMessage || null,
    responseTimeoutSecs: general.response_timeout_secs,
  })

  return (
    <div className="settings-panel">
      <div className="settings-field">
        <span id={languageLabelId}>{t('settings.general.language_label')}</span>
        <Dropdown
          labelledBy={languageLabelId}
          label={languageName(general.language)}
          options={LANGUAGES.map((language) => ({ key: language, label: languageName(language) }))}
          selectedKey={general.language}
          onSelect={(key) => onSaveLanguage(key as Language)}
          direction="down"
          align="start"
        />
        {/* 画面は起動時の言語で描かれているので、保存した言語と違う間だけ出す */}
        {general.language !== currentLanguage() && (
          <p className="settings-hint">{t('settings.general.language_restart_note')}</p>
        )}
      </div>

      <PromptField
        label={t('settings.general.system_prompt_label')}
        value={systemPrompt}
        onChange={setSystemPrompt}
        onBlur={() => onSave(current())}
      />

      <NumberField
        label={t('settings.general.timeout_label')}
        value={general.response_timeout_secs}
        defaultValue={general.default_response_timeout_secs}
        onSave={(secs) => onSave({ ...current(), responseTimeoutSecs: secs })}
      />

      <details className="settings-advanced">
        <summary>{t('settings.general.advanced_settings')}</summary>
        {/* <details>自体はflexにしないので(index.cssの.settings-advanced)、欄の間隔は
            中の入れ物のgapで持つ */}
        <div className="settings-section">
          <PromptField
            label={t('settings.general.task_chat_prompt_label')}
            value={taskChatSystemPrompt}
            onChange={setTaskChatSystemPrompt}
            onBlur={() => onSave(current())}
            caption={t('settings.general.task_chat_prompt_caption')}
          />
          <PromptField
            label={t('settings.general.task_opening_label')}
            value={taskOpeningMessage}
            onChange={setTaskOpeningMessage}
            onBlur={() => onSave(current())}
            caption={t('settings.general.task_opening_caption')}
          />
        </div>
      </details>

      <ExportSection />
    </div>
  )
}

type ExportResult = { ok: true; summary: ExportSummary } | { ok: false; message: string }

// 押すと確認なしで書き出し、成否はこの欄に出す(legacy/frontend.md 2節)。タブ全体のエラー欄を
// 使わないのは、設定の保存とは別の操作の結果だから。
function ExportSection() {
  const [running, setRunning] = useState(false)
  const [result, setResult] = useState<ExportResult | null>(null)

  const runExport = async () => {
    setRunning(true)
    setResult(null)
    try {
      setResult({ ok: true, summary: await exportMarkdown() })
    } catch (e) {
      setResult({
        ok: false,
        message: t('settings.general.export_failed', { error: failureText(e) }),
      })
    } finally {
      setRunning(false)
    }
  }

  const openFolder = async () => {
    try {
      await openExportFolder()
    } catch (e) {
      setResult({
        ok: false,
        message: t('settings.general.open_export_folder_failed', { error: failureText(e) }),
      })
    }
  }

  return (
    <div className="settings-field settings-section-break">
      <span>{t('settings.general.export_label')}</span>
      <p className="settings-hint">{t('settings.general.export_caption')}</p>
      <div className="button-row">
        <button type="button" onClick={runExport} disabled={running}>
          {running ? t('settings.general.exporting') : t('settings.general.export_button')}
        </button>
        <button type="button" onClick={openFolder}>
          {t('settings.general.open_export_folder')}
        </button>
      </div>
      {result?.ok === true && (
        <p>{t('settings.general.export_done', { folder: result.summary.folder })}</p>
      )}
      {result?.ok === true && result.summary.missing_attachments > 0 && (
        <p className="error">
          {t('settings.general.export_missing_attachments', {
            count: result.summary.missing_attachments,
          })}
        </p>
      )}
      {result?.ok === false && <p className="error">{result.message}</p>}
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
  ) => Promise<void>
  onDeleteProvider: (providerId: string) => void
  // モデルの操作(追加・削除・表の各列)は種類が多いため、個別のコールバックを並べずに
  // 呼び出しごと受け取り、結果の反映とエラー表示を親に任せる。
  onUpdateModels: (action: () => Promise<SettingsView>) => Promise<void>
}

function ProvidersTab({
  settings,
  onAddProvider,
  onDeleteProvider,
  onUpdateModels,
}: ProvidersTabProps) {
  const [newModelByProvider, setNewModelByProvider] = useState<Record<string, string>>({})

  return (
    <div className="settings-panel">
      <ul className="provider-list">
        {settings.providers.map((provider) => (
          <ProviderCard
            key={provider.id}
            provider={provider}
            newModel={newModelByProvider[provider.id] ?? ''}
            onSetNewModel={(value) =>
              setNewModelByProvider((prev) => ({ ...prev, [provider.id]: value }))
            }
            onDeleteProvider={() => onDeleteProvider(provider.id)}
            onUpdateModels={onUpdateModels}
          />
        ))}
        {settings.providers.length === 0 && (
          <li className="list-empty">{t('settings.provider.none_registered')}</li>
        )}
      </ul>

      <AddProviderForm onAdd={onAddProvider} />
    </div>
  )
}

interface ProviderCardProps {
  provider: ProviderView
  newModel: string
  onSetNewModel: (value: string) => void
  onDeleteProvider: () => void
  onUpdateModels: (action: () => Promise<SettingsView>) => Promise<void>
}

function ProviderCard({
  provider,
  newModel,
  onSetNewModel,
  onDeleteProvider,
  onUpdateModels,
}: ProviderCardProps) {
  const hasModel = provider.models.length > 0
  const [detecting, setDetecting] = useState(false)
  // 取得したモデル名。設定には書かないので、カードを閉じれば(設定画面を離れれば)捨てる。
  const [available, setAvailable] = useState<AvailableModel[] | null>(null)
  const [listing, setListing] = useState(false)
  // 取得の失敗はカード内に出す(どのプロバイダーで失敗したかが分かるように。MCPの
  // ツール一覧の取得と同じ扱い)。
  const [listError, setListError] = useState<string | null>(null)

  const detect = async () => {
    setDetecting(true)
    await onUpdateModels(() => detectModelCapabilities(provider.id))
    setDetecting(false)
  }

  // 追加したモデルの能力もすぐ表に出す。サーバーに繋がらなくても追加は済んでいるので、
  // 検出の失敗は追加の失敗として出さない(ターンの開始時にもう一度問い合わせる)。
  const add = (models: string[]) =>
    onUpdateModels(async () => {
      const added = await addModels(provider.id, models)
      if (!provider.can_detect_capabilities) return added
      return detectModelCapabilities(provider.id).catch(() => added)
    })

  const listModels = async () => {
    setListing(true)
    setListError(null)
    try {
      setAvailable(await listProviderModels(provider.id))
    } catch (e) {
      setListError(t('common.fetch_failed', { error: failureText(e) }))
    } finally {
      setListing(false)
    }
  }

  return (
    <li className="provider-card">
      <div className="provider-card-header">
        <strong>{provider.name}</strong>
        <ConfirmButton
          label={t('common.delete')}
          confirmTitle={t('settings.provider.delete_provider_dialog_title')}
          confirmMessage={t('settings.provider.delete_provider_dialog_message', {
            name: provider.name,
          })}
          confirmLabel={t('common.delete')}
          onConfirm={onDeleteProvider}
        />
      </div>
      <p className="provider-card-meta">
        {t('settings.provider.meta', {
          url: provider.base_url,
          api_key: provider.has_api_key
            ? t('settings.provider.api_key_set')
            : t('settings.provider.api_key_unset'),
        })}
      </p>
      {provider.error && (
        <p className="error">{t('settings.provider.unusable', { error: provider.error })}</p>
      )}

      {hasModel ? (
        <ModelTable provider={provider} onUpdate={onUpdateModels} />
      ) : (
        <p className="list-empty">{t('settings.model.none_registered')}</p>
      )}
      {hasModel && provider.can_detect_capabilities && (
        <button type="button" onClick={() => void detect()} disabled={detecting}>
          {detecting ? t('settings.model.detecting') : t('settings.model.detect_button')}
        </button>
      )}

      <form
        className="model-add-form"
        onSubmit={(e) => {
          e.preventDefault()
          const model = newModel.trim()
          if (!model) return
          void add([model])
          onSetNewModel('')
        }}
      >
        <input
          value={newModel}
          onChange={(e) => onSetNewModel(e.target.value)}
          placeholder={t('settings.model.add_model_hint')}
        />
        <button type="submit">{t('common.add')}</button>
        <button type="button" onClick={() => void listModels()} disabled={listing}>
          {listing ? t('common.fetching') : t('settings.model.fetch_models_button')}
        </button>
      </form>
      {listError && <p className="error">{listError}</p>}
      {available && (
        <ModelPicker
          available={available}
          registered={provider.models.map((m) => m.name)}
          onAdd={add}
          onClose={() => setAvailable(null)}
        />
      )}
    </li>
  )
}

interface ModelPickerProps {
  // プロバイダーから取得したモデル(登録済みのものを含む)。
  available: AvailableModel[]
  // 登録済みのモデル名。取得したモデルとは名前(label ではなく name)で照らし合わせる。
  registered: string[]
  onAdd: (models: string[]) => Promise<void>
  onClose: () => void
}

// 取得したモデルから、登録するものを選ぶ欄(Issue #33。一括で登録しない理由は
// architecture.md 3節)。登録済みのモデルは候補から外す(追加すると表の側へ移る)。
function ModelPicker({ available, registered, onAdd, onClose }: ModelPickerProps) {
  const [selected, setSelected] = useState<string[]>([])
  const [query, setQuery] = useState('')
  const [adding, setAdding] = useState(false)

  const candidates = available.filter((m) => !registered.includes(m.name))
  const { matched, searching } = matchQuery(candidates, query, (m) => m.label)
  // 追加や別の操作で登録済みになったものは、選択から外れたものとして数える。
  const chosen = selected.filter((name) => candidates.some((m) => m.name === name))

  const toggle = (name: string, checked: boolean) =>
    setSelected((prev) => (checked ? [...prev, name] : prev.filter((n) => n !== name)))

  const submit = async () => {
    setAdding(true)
    // 候補の並び(名前順)で登録する。選んだ順にすると、表の並びが操作の順に左右される。
    await onAdd(candidates.filter((m) => chosen.includes(m.name)).map((m) => m.name))
    setSelected([])
    setAdding(false)
  }

  return (
    <div className="model-picker">
      {candidates.length === 0 ? (
        <p className="list-empty">
          {t('settings.model.picker_all_registered', { count: available.length })}
        </p>
      ) : (
        <>
          <div className="model-table-toolbar">
            <input
              type="search"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder={t('settings.model.picker_search_hint', { count: candidates.length })}
              aria-label={t('settings.model.picker_search_label')}
            />
            <button
              type="button"
              onClick={() =>
                setSelected((prev) => [...new Set([...prev, ...matched.map((m) => m.name)])])
              }
              disabled={matched.every((m) => chosen.includes(m.name))}
            >
              {searching
                ? t('settings.model.picker_select_matched')
                : t('settings.model.picker_select_all')}
            </button>
          </div>
          <ul className="model-list model-picker-options">
            {matched.map((m) => (
              <li key={m.name} className="model-row">
                <label className="choice">
                  <input
                    type="checkbox"
                    checked={chosen.includes(m.name)}
                    onChange={(e) => toggle(m.name, e.target.checked)}
                  />
                  {m.label}
                </label>
              </li>
            ))}
            {matched.length === 0 && <li className="list-empty">{noModelMatchText(query)}</li>}
          </ul>
        </>
      )}
      <div className="button-row model-picker-actions">
        {candidates.length > 0 && (
          <button
            type="button"
            onClick={() => void submit()}
            disabled={chosen.length === 0 || adding}
          >
            {adding
              ? t('common.adding')
              : t('settings.model.picker_add_selected', { count: chosen.length })}
          </button>
        )}
        <button type="button" onClick={onClose}>
          {t('common.close')}
        </button>
      </div>
    </div>
  )
}

// 列見出しは表に収まる短い語にするので、チェックボックスの説明は見出しとは別の1文にする
// (見出しの語を文に差し込むと、言語によっては文にならない)。
interface CapabilityColumn {
  capability: Capability
  label: MessageKey
  checkboxLabel: MessageKey
}

const CAPABILITY_COLUMNS: CapabilityColumn[] = [
  {
    capability: 'image',
    label: 'settings.model.cap_vision_label',
    checkboxLabel: 'settings.model.cap_vision_checkbox_label',
  },
  {
    capability: 'tools',
    label: 'settings.model.cap_tools_label',
    checkboxLabel: 'settings.model.cap_tools_checkbox_label',
  },
  {
    capability: 'thinking',
    label: 'settings.model.cap_reasoning_label',
    checkboxLabel: 'settings.model.cap_reasoning_checkbox_label',
  },
]

interface ModelTableProps {
  provider: ProviderView
  onUpdate: (action: () => Promise<SettingsView>) => void
}

// モデル表(legacy/frontend.md 3節、Issue #65)。能力は解決済みの値を描くだけで、
// 手動設定の正規化(初期値と同じ値なら手動設定を外す)はRust側が持つ。
function ModelTable({ provider, onUpdate }: ModelTableProps) {
  const models = provider.models
  const [expanded, setExpanded] = useState(false)
  const [query, setQuery] = useState('')

  const collapsible = models.length >= LIST_COLLAPSE_THRESHOLD
  // 検索欄は畳める件数のときだけ出す。削除で件数が減って欄が消えたら、打った語は消せない
  // ので、欄が無いあいだは絞り込まない。
  const { matched, searching } = matchQuery(models, collapsible ? query : '', (m) => m.label)
  // 折りたたんでいても、検索したら当たった行は出す(legacy/frontend.md 3節)。
  const shown = collapsible && !expanded && !searching ? [] : matched

  return (
    <>
      {collapsible && (
        <div className="model-table-toolbar">
          <CollapseToggle
            showLabel={t('settings.model.show_all', { count: models.length })}
            expanded={expanded}
            onToggle={() => setExpanded((v) => !v)}
          />
          <input
            type="search"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder={t('settings.model.search_hint')}
            aria-label={t('settings.model.search_hint')}
          />
        </div>
      )}
      {shown.length > 0 && (
        <div className="model-table-scroll">
          <table className="model-table">
            <thead>
              <tr>
                <th className="model-check-col">{t('settings.model.col_visible')}</th>
                <th>{t('settings.model.col_model')}</th>
                {CAPABILITY_COLUMNS.map(({ capability, label }) => (
                  <th key={capability} className="model-check-col">
                    {t(label)}
                  </th>
                ))}
                <th>{t('settings.model.col_context_length')}</th>
                <th aria-label={t('settings.model.col_actions')} />
              </tr>
            </thead>
            <tbody>
              {shown.map((model) => (
                <ModelRow
                  key={model.name}
                  providerId={provider.id}
                  model={model}
                  onUpdate={onUpdate}
                />
              ))}
            </tbody>
          </table>
        </div>
      )}
      {searching && matched.length === 0 && <p className="list-empty">{noModelMatchText(query)}</p>}
    </>
  )
}

interface ModelRowProps {
  providerId: string
  model: ModelView
  onUpdate: (action: () => Promise<SettingsView>) => void
}

function ModelRow({ providerId, model, onUpdate }: ModelRowProps) {
  const { name, label: shown } = model
  // 欄には手動設定だけを出し、既定値はプレースホルダに回す。手動設定が既定値と同じなら
  // Rust側で外されるので、解決済みの値が既定値と違うことが手動設定があることと同じになる。
  const resolvedLength = model.capabilities.context_length
  const contextLength = usePositiveIntegerInput(
    resolvedLength === model.default_context_length ? null : resolvedLength,
    (value) => onUpdate(() => setModelContextLength(providerId, name, value)),
  )

  return (
    <tr className={model.visible ? undefined : 'model-hidden'}>
      <td className="model-check-col">
        <input
          type="checkbox"
          checked={model.visible}
          onChange={(e) => {
            const visible = e.target.checked
            onUpdate(() => setModelVisible(providerId, name, visible))
          }}
          aria-label={t('settings.model.visible_checkbox_label', { model: shown })}
        />
      </td>
      <td className="model-name">{shown}</td>
      {CAPABILITY_COLUMNS.map(({ capability, checkboxLabel }) => (
        <td key={capability} className="model-check-col">
          <span className="model-capability">
            <input
              type="checkbox"
              checked={model.capabilities[capability]}
              onChange={(e) => {
                const supported = e.target.checked
                onUpdate(() => setModelCapability(providerId, name, capability, supported))
              }}
              aria-label={t(checkboxLabel, { model: shown })}
            />
            {capability === 'tools' && !model.capabilities.tools && (
              <span
                className="model-warning"
                role="img"
                aria-label={t('settings.model.tools_warning_tooltip')}
                title={t('settings.model.tools_warning_tooltip')}
              >
                ⚠
              </span>
            )}
          </span>
        </td>
      ))}
      <td>
        <input
          {...contextLength.inputProps}
          className="model-context-length"
          placeholder={t('settings.model.context_length_default_hint', {
            value: model.default_context_length,
          })}
          aria-label={t('settings.model.context_length_label', { model: shown })}
        />
        {contextLength.invalid && (
          <p className="error model-context-length-error">{t('errors.positive_integer')}</p>
        )}
      </td>
      <td>
        <span className="model-actions">
          {model.overridden && (
            <button
              type="button"
              className="icon-button"
              onClick={() => onUpdate(() => resetModelCapabilities(providerId, name))}
              aria-label={t('settings.model.reset_caps_label', { model: shown })}
              title={t('settings.model.reset_caps_tooltip')}
            >
              ↺
            </button>
          )}
          <button
            type="button"
            className="icon-button"
            onClick={() => onUpdate(() => removeModel(providerId, name))}
            aria-label={t('settings.model.delete_model_label', { model: shown })}
            title={t('common.delete')}
          >
            ×
          </button>
        </span>
      </td>
    </tr>
  )
}

interface CollapseToggleProps {
  // 畳んでいるときの文言(何を何件表示するか)。
  showLabel: string
  expanded: boolean
  onToggle: () => void
}

// 件数の多い一覧(モデル表・MCPのツール一覧)を既定で畳むための開閉ボタン。
function CollapseToggle({ showLabel, expanded, onToggle }: CollapseToggleProps) {
  return (
    <button type="button" onClick={onToggle} aria-expanded={expanded}>
      {expanded ? t('common.collapse') : showLabel}
    </button>
  )
}

// この件数以上の一覧は既定で畳む。モデル表とMCPのツール一覧で揃える。
const LIST_COLLAPSE_THRESHOLD = 5

// 追加フォームの送信。結果を待ち、成功したときだけ`onDone`で入力を空にする。失敗は
// フォームの直下に出し、入力は残す(Rust側の検証で弾かれても打ち直さずに済むように)。
function useAddSubmission() {
  const [adding, setAdding] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const run = async (add: () => Promise<unknown>, onDone: () => void) => {
    setAdding(true)
    setError(null)
    try {
      await add()
      onDone()
    } catch (e) {
      setError(failureText(e))
    } finally {
      setAdding(false)
    }
  }

  return { adding, error, run }
}

interface AddProviderFormProps {
  onAdd: (
    name: string,
    apiFormat: ApiFormat,
    baseUrl: string,
    apiKey: string | null,
  ) => Promise<void>
}

function AddProviderForm({ onAdd }: AddProviderFormProps) {
  const apiFormatLabelId = useId()
  const [name, setName] = useState('')
  const [apiFormat, setApiFormat] = useState<ApiFormat>('open_ai_compat')
  const [baseUrl, setBaseUrl] = useState(DEFAULT_BASE_URL_BY_FORMAT.open_ai_compat)
  const [apiKey, setApiKey] = useState('')
  const submission = useAddSubmission()

  return (
    <form
      className="provider-add-form settings-section-break"
      onSubmit={(e) => {
        e.preventDefault()
        if (submission.adding || !name.trim() || !baseUrl.trim()) return
        void submission.run(
          () => onAdd(name.trim(), apiFormat, baseUrl.trim(), apiKey || null),
          () => {
            setName('')
            setApiKey('')
          },
        )
      }}
    >
      <h2>{t('settings.provider.add_provider_heading')}</h2>
      <label className="settings-field">
        <span>{t('settings.provider.display_name_hint')}</span>
        <input value={name} onChange={(e) => setName(e.target.value)} required />
      </label>
      <div className="settings-field">
        <span id={apiFormatLabelId}>{t('settings.provider.api_format_field_label')}</span>
        <Dropdown
          labelledBy={apiFormatLabelId}
          label={t(API_FORMAT_LABELS[apiFormat])}
          options={Object.entries(API_FORMAT_LABELS).map(([key, label]) => ({ key, label: t(label) }))}
          selectedKey={apiFormat}
          onSelect={(key) => {
            const format = key as ApiFormat
            setApiFormat(format)
            setBaseUrl(DEFAULT_BASE_URL_BY_FORMAT[format])
          }}
          direction="down"
          align="start"
        />
      </div>
      <label className="settings-field">
        <span>{t('settings.provider.base_url_label')}</span>
        <input value={baseUrl} onChange={(e) => setBaseUrl(e.target.value)} required />
        <p className="settings-hint">
          {httpPlainTextHint(t('settings.provider.api_key_secret'))}
        </p>
      </label>
      <label className="settings-field">
        <span>{t('settings.provider.api_key_hint')}</span>
        <input
          type="password"
          value={apiKey}
          onChange={(e) => setApiKey(e.target.value)}
          placeholder={t('settings.provider.api_key_placeholder')}
        />
      </label>
      {submission.error && <p className="error">{submission.error}</p>}
      <button type="submit" disabled={submission.adding}>
        {submission.adding ? t('common.adding') : t('common.add')}
      </button>
    </form>
  )
}

// 「1行1件、KEY=VALUE」形式のテキストをパースする(legacy/frontend.md 4節)。
// エラーは行ごとに個別指摘する。行は前後の空白を除いてから見るので、`=`が先頭でなければ
// キーは空にならない。
function parseKeyValueLines(text: string): { pairs: [string, string][]; errors: string[] } {
  const pairs: [string, string][] = []
  const errors: string[] = []
  text.split('\n').forEach((line, i) => {
    const trimmed = line.trim()
    if (trimmed === '') return
    const eq = trimmed.indexOf('=')
    if (eq <= 0) {
      errors.push(
        t('settings.tools.kv_line_invalid', {
          line_no: i + 1,
          line: trimmed,
          sample: t('settings.tools.kv_sample'),
        }),
      )
      return
    }
    pairs.push([trimmed.slice(0, eq).trim(), trimmed.slice(eq + 1).trim()])
  })
  return { pairs, errors }
}

// 使える文字だけを見る(長さの上限はRust側から受け取る)。登録の可否はRust側
// (`config::validate_mcp_server_name`)が決め直す。ここで見るのは、表示言語の文言で理由を出すため。
const MCP_NAME_CHARS = /^[A-Za-z0-9_]+$/

interface McpTabProps {
  settings: SettingsView
  onSaveLimits: (maxRoundsPerTurn: number | null, totalTimeoutSecs: number | null) => void
  // 追加したサーバーのidを返す。
  onAddServer: (name: string, endpoint: NewMcpEndpoint) => Promise<string | null>
  onDeleteServer: (serverId: string) => void
  onSetServerEnabled: (serverId: string, enabled: boolean) => void
  onSetToolEnabled: (serverId: string, toolName: string, enabled: boolean) => void
  onFetchTools: (serverId: string) => Promise<void>
}

function McpTab({
  settings,
  onSaveLimits,
  onAddServer,
  onDeleteServer,
  onSetServerEnabled,
  onSetToolEnabled,
  onFetchTools,
}: McpTabProps) {
  // ツール一覧の取得中のサーバーと、取得のエラー(サーバーごと、カード内に出す)。
  // 追加した直後の自動取得もカードの取得と同じ表示にするため、カードではなくここで持つ。
  const [fetching, setFetching] = useState<string[]>([])
  const [fetchErrors, setFetchErrors] = useState<Record<string, string>>({})

  const fetchTools = async (serverId: string) => {
    setFetching((prev) => [...prev, serverId])
    setFetchErrors((prev) => {
      const next = { ...prev }
      delete next[serverId]
      return next
    })
    try {
      await onFetchTools(serverId)
    } catch (e) {
      setFetchErrors((prev) => ({
        ...prev,
        [serverId]: t('common.fetch_failed', { error: failureText(e) }),
      }))
    } finally {
      setFetching((prev) => prev.filter((id) => id !== serverId))
    }
  }

  // 追加したら続けて1回ツール一覧を取得する(legacy/frontend.md 4節「追加時に自動で1回
  // 接続テスト」)。失敗しても登録は残し、エラーはそのサーバーのカードに出す。
  const addServer = async (name: string, endpoint: NewMcpEndpoint) => {
    const serverId = await onAddServer(name, endpoint)
    if (serverId !== null) void fetchTools(serverId)
  }

  return (
    <div className="settings-panel">
      <p className="settings-hint">{t('settings.tools.intro')}</p>

      <ul className="provider-list">
        {settings.mcp_servers.map((server) => (
          <McpServerCard
            key={server.id}
            server={server}
            onDelete={() => onDeleteServer(server.id)}
            onSetEnabled={(enabled) => onSetServerEnabled(server.id, enabled)}
            onSetToolEnabled={(toolName, enabled) => onSetToolEnabled(server.id, toolName, enabled)}
            fetching={fetching.includes(server.id)}
            fetchError={fetchErrors[server.id] ?? null}
            onFetchTools={() => void fetchTools(server.id)}
          />
        ))}
        {settings.mcp_servers.length === 0 && (
          <li className="list-empty">{t('settings.tools.none_registered')}</li>
        )}
      </ul>

      <AddMcpServerForm
        existingNames={settings.mcp_servers.map((s) => s.name)}
        nameMaxChars={settings.mcp_server_name_max_chars}
        onAdd={addServer}
      />

      {/* ツール呼び出し全体の上限(legacy/frontend.md 4節「共通設定」)。内部ツールにも
          効くので、MCPサーバーの一覧より後ろ、タブの末尾に置く。サーバーの追加とは
          別の話なので、追加フォームと同じ形の仕切り線で切る。 */}
      <section className="settings-section settings-section-break">
        <NumberField
          label={t('settings.tools.max_rounds_label')}
          value={settings.tools.max_rounds_per_turn}
          defaultValue={settings.tools.default_max_rounds_per_turn}
          hint={t('settings.tools.max_rounds_caption')}
          onSave={(rounds) => onSaveLimits(rounds, settings.tools.total_timeout_secs)}
        />

        <NumberField
          label={t('settings.tools.timeout_label')}
          value={settings.tools.total_timeout_secs}
          defaultValue={settings.tools.default_total_timeout_secs}
          hint={t('settings.tools.timeout_caption')}
          onSave={(secs) => onSaveLimits(settings.tools.max_rounds_per_turn, secs)}
        />
      </section>
    </div>
  )
}

interface McpServerCardProps {
  server: McpServerView
  onDelete: () => void
  onSetEnabled: (enabled: boolean) => void
  onSetToolEnabled: (toolName: string, enabled: boolean) => void
  fetching: boolean
  fetchError: string | null
  onFetchTools: () => void
}

// 取得済みのツール一覧は`server.tools`(Rust側のキャッシュ)から来る。カード自身では
// 保持しない——保持すると設定画面を閉じた時点で消え、有効にしたツールを確認することも
// 外すこともできなくなる(Issue #104)。
function McpServerCard({
  server,
  onDelete,
  onSetEnabled,
  onSetToolEnabled,
  fetching,
  fetchError,
  onFetchTools,
}: McpServerCardProps) {
  const tools = server.tools
  const [expanded, setExpanded] = useState(false)

  const endpointSummary =
    server.endpoint.transport === 'stdio'
      ? t('settings.tools.endpoint_stdio', {
          command: [server.endpoint.command, ...server.endpoint.args].join(' '),
        })
      : t('settings.tools.endpoint_http', { url: server.endpoint.url })
  const secretNames =
    server.endpoint.transport === 'stdio' ? server.endpoint.env_names : server.endpoint.header_names
  const secretLabel =
    server.endpoint.transport === 'stdio'
      ? t('settings.tools.env_names_label')
      : t('settings.tools.header_names_label')

  const fetched = server.tools_fetched
  const collapsible = tools.length >= LIST_COLLAPSE_THRESHOLD
  const visibleTools = collapsible && !expanded ? [] : tools

  return (
    <li className="provider-card">
      <div className="provider-card-header">
        <label className="choice">
          <input
            type="checkbox"
            checked={server.enabled}
            onChange={(e) => onSetEnabled(e.target.checked)}
          />
          <strong>{server.name}</strong>
        </label>
        <ConfirmButton
          label={t('settings.tools.unregister_button')}
          confirmTitle={t('settings.tools.delete_server_dialog_title')}
          confirmMessage={t('settings.tools.delete_server_dialog_message', { id: server.name })}
          confirmLabel={t('settings.tools.unregister_button')}
          onConfirm={onDelete}
        />
      </div>

      <p className="provider-card-meta">{endpointSummary}</p>
      {secretNames.length > 0 && (
        <p className="provider-card-meta">
          {t('settings.tools.secret_names_display', {
            label: secretLabel,
            names: secretNames.join(', '),
          })}
        </p>
      )}

      {tools.length === 0 ? (
        <p className="list-empty">
          {fetched ? t('settings.tools.tools_none') : t('settings.tools.tools_none_fetched')}
        </p>
      ) : (
        <>
          {!fetched && (
            <p className="list-empty">{t('settings.tools.tools_enabled_only')}</p>
          )}
          {collapsible && (
            <CollapseToggle
              showLabel={t('settings.tools.show_all', { count: tools.length })}
              expanded={expanded}
              onToggle={() => setExpanded((v) => !v)}
            />
          )}
          <ul className="model-list">
            {visibleTools.map((tool) => {
              const checked = server.enabled_tools.includes(tool.name)
              return (
                <li key={tool.name} className="model-row">
                  <label className="choice" title={tool.description ?? undefined}>
                    <input
                      type="checkbox"
                      checked={checked}
                      // 公開できないツールは有効にさせない。既に有効なら外す操作だけは残す。
                      disabled={!tool.exposable && !checked}
                      onChange={(e) => onSetToolEnabled(tool.name, e.target.checked)}
                    />
                    {tool.label}
                  </label>
                  {!tool.exposable && (
                    <span className="provider-card-meta">{t('settings.tools.not_exposable')}</span>
                  )}
                </li>
              )
            })}
          </ul>
        </>
      )}

      {fetchError && <p className="error">{fetchError}</p>}
      <button type="button" onClick={onFetchTools} disabled={fetching}>
        {fetching ? t('common.fetching') : t('settings.tools.fetch_tools_button')}
      </button>
    </li>
  )
}

interface AddMcpServerFormProps {
  existingNames: string[]
  nameMaxChars: number
  onAdd: (name: string, endpoint: NewMcpEndpoint) => Promise<void>
}

function AddMcpServerForm({ existingNames, nameMaxChars, onAdd }: AddMcpServerFormProps) {
  const transportLabelId = useId()
  const [name, setName] = useState('')
  const [transport, setTransport] = useState<Transport>('stdio')
  const [command, setCommand] = useState('')
  const [argsText, setArgsText] = useState('')
  const [envText, setEnvText] = useState('')
  const [url, setUrl] = useState('')
  const [headersText, setHeadersText] = useState('')
  const [errors, setErrors] = useState<string[]>([])
  const submission = useAddSubmission()

  const reset = () => {
    setName('')
    setCommand('')
    setArgsText('')
    setEnvText('')
    setUrl('')
    setHeadersText('')
  }

  const submit = (e: FormEvent<HTMLFormElement>) => {
    e.preventDefault()
    if (submission.adding) return
    const trimmedName = name.trim()
    const validationErrors: string[] = []
    if (trimmedName === '') {
      validationErrors.push(t('settings.tools.id_required'))
    } else if (trimmedName.length > nameMaxChars || !MCP_NAME_CHARS.test(trimmedName)) {
      validationErrors.push(t('settings.tools.id_invalid', { max: nameMaxChars }))
    } else if (existingNames.includes(trimmedName)) {
      validationErrors.push(t('settings.tools.id_duplicate', { id: trimmedName }))
    }

    if (transport === 'stdio') {
      if (!command.trim()) validationErrors.push(t('settings.tools.command_required'))
      const args = argsText
        .split('\n')
        .map((s) => s.trim())
        .filter((s) => s !== '')
      const { pairs, errors: envErrors } = parseKeyValueLines(envText)
      validationErrors.push(...envErrors)
      if (validationErrors.length > 0) {
        setErrors(validationErrors)
        return
      }
      setErrors([])
      const endpoint: NewMcpEndpoint = {
        transport: 'stdio',
        command: command.trim(),
        args,
        env: pairs,
      }
      void submission.run(() => onAdd(trimmedName, endpoint), reset)
    } else {
      if (!url.trim()) validationErrors.push(t('settings.tools.url_required'))
      const { pairs, errors: headerErrors } = parseKeyValueLines(headersText)
      validationErrors.push(...headerErrors)
      if (validationErrors.length > 0) {
        setErrors(validationErrors)
        return
      }
      setErrors([])
      const endpoint: NewMcpEndpoint = {
        transport: 'streamable_http',
        url: url.trim(),
        headers: pairs,
      }
      void submission.run(() => onAdd(trimmedName, endpoint), reset)
    }
  }

  return (
    <form className="provider-add-form settings-section-break" onSubmit={submit}>
      <h2>{t('settings.tools.add_server_heading')}</h2>
      <label className="settings-field">
        <span>{t('settings.tools.server_id_hint', { max: nameMaxChars })}</span>
        <input
          value={name}
          onChange={(e) => setName(e.target.value)}
          maxLength={nameMaxChars}
          required
        />
      </label>
      <div className="settings-field">
        <span id={transportLabelId}>{t('settings.tools.transport_label')}</span>
        <Dropdown
          labelledBy={transportLabelId}
          label={t(TRANSPORT_LABELS[transport])}
          options={Object.entries(TRANSPORT_LABELS).map(([key, label]) => ({ key, label: t(label) }))}
          selectedKey={transport}
          onSelect={(key) => setTransport(key as Transport)}
          direction="down"
          align="start"
        />
      </div>

      {transport === 'stdio' ? (
        <>
          <label className="settings-field">
            <span>{t('settings.tools.command_hint')}</span>
            <input value={command} onChange={(e) => setCommand(e.target.value)} required />
          </label>
          <label className="settings-field">
            <span>{t('settings.tools.args_hint')}</span>
            <textarea value={argsText} onChange={(e) => setArgsText(e.target.value)} />
          </label>
          <label className="settings-field">
            <span>
              {t('settings.tools.env_hint', { sample: t('settings.tools.kv_sample') })}
            </span>
            <textarea value={envText} onChange={(e) => setEnvText(e.target.value)} />
            <p className="settings-hint">{t('settings.tools.secret_helper')}</p>
          </label>
          <p className="settings-hint">{t('settings.tools.stdio_warning')}</p>
        </>
      ) : (
        <>
          <label className="settings-field">
            <span>{t('settings.tools.url_hint')}</span>
            <input value={url} onChange={(e) => setUrl(e.target.value)} required />
            <p className="settings-hint">
              {httpPlainTextHint(t('settings.tools.header_secret'))}
            </p>
          </label>
          <label className="settings-field">
            <span>
              {t('settings.tools.headers_hint', { sample: t('settings.tools.kv_sample') })}
            </span>
            <textarea value={headersText} onChange={(e) => setHeadersText(e.target.value)} />
            <p className="settings-hint">{t('settings.tools.secret_helper')}</p>
          </label>
        </>
      )}

      {errors.map((e) => (
        <p key={e} className="error">
          {e}
        </p>
      ))}
      {submission.error && <p className="error">{submission.error}</p>}
      <button type="submit" disabled={submission.adding}>
        {submission.adding ? t('common.adding') : t('common.add')}
      </button>
    </form>
  )
}
