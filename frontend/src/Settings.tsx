import { useEffect, useState } from 'react'
import type { ChangeEvent, FormEvent } from 'react'
import {
  addMcpServer,
  addModels,
  addProvider,
  deleteMcpServer,
  deleteProvider,
  detectModelCapabilities,
  fetchMcpTools,
  getSettings,
  listProviderModels,
  removeModel,
  resetModelCapabilities,
  setMcpServerEnabled,
  setMcpToolEnabled,
  setModelCapability,
  setModelContextLength,
  setModelVisible,
  updateGeneralSettings,
  updateToolSettings,
  type NewMcpEndpoint,
} from './api'
import type {
  ApiFormat,
  AvailableModel,
  Capability,
  McpServerView,
  ModelView,
  ProviderView,
  SettingsView,
} from './types'
import { ConfirmButton } from './Dialog'

interface SettingsProps {
  onClose: () => void
}

const DEFAULT_BASE_URL_BY_FORMAT: Record<ApiFormat, string> = {
  open_ai_compat: 'https://api.openai.com/v1',
}

// httpの許可範囲(crates/scitl-core/src/net.rsのclassify_host)が変わったときに
// 片方だけ直し忘れないよう、URLを入力させる箇所で共通のヒント文を使う。
function httpPlainTextHint(secretLabel: string): string {
  return `httpsを推奨します。httpはループバックまたはプライベートIPアドレス(LAN内等)への接続のみ許可され、通信は暗号化されません。${secretLabel}も平文で流れます。`
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
      setError(String(e))
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
      setError(String(e))
    }
  }

  return (
    <div className="settings">
      <header className="settings-header">
        <button
          type="button"
          className="icon-button settings-back"
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
            onClick={() => selectTab('general')}
          >
            一般
          </button>
          <button
            type="button"
            className={tab === 'providers' ? 'settings-tab selected' : 'settings-tab'}
            onClick={() => selectTab('providers')}
          >
            APIプロバイダー
          </button>
          <button
            type="button"
            className={tab === 'mcp' ? 'settings-tab selected' : 'settings-tab'}
            onClick={() => selectTab('mcp')}
          >
            ツール/MCP
          </button>
        </nav>

        <div className="settings-content">
          <div className="settings-column">
            {settings?.config_error && (
              <p className="error">
                {'設定ファイルを読み込めなかったため、空の設定で起動しています。'}
                {'ファイルを直してアプリを再起動するまで、設定は保存されません。'}
                {`(${settings.config_error})`}
              </p>
            )}
            {error && <p className="error">{error}</p>}

            {settings === null ? (
              <p>読み込み中…</p>
            ) : tab === 'general' ? (
              <GeneralTab
                settings={settings}
                onSave={(systemPrompt, taskChatSystemPrompt, timeout) =>
                  runOrReportError(() =>
                    updateGeneralSettings({
                      systemPrompt,
                      taskChatSystemPrompt,
                      responseTimeoutSecs: timeout,
                    }),
                  )
                }
              />
            ) : tab === 'providers' ? (
              <ProvidersTab
                settings={settings}
                onAddProvider={(name, format, baseUrl, apiKey) =>
                  runOrReportError(() => addProvider(name, format, baseUrl, apiKey))
                }
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
                onAddServer={(name, endpoint) =>
                  runOrReportError(() => addMcpServer(name, endpoint))
                }
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

const POSITIVE_INTEGER_ERROR = '1以上の整数を入力してください'

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
      <input {...inputProps} placeholder={`未設定(既定値 ${defaultValue})`} />
      {hint && <p className="settings-hint">{hint}</p>}
      {invalid && <p className="error">{POSITIVE_INTEGER_ERROR}</p>}
    </label>
  )
}

interface GeneralTabProps {
  settings: SettingsView
  onSave: (
    systemPrompt: string | null,
    taskChatSystemPrompt: string | null,
    responseTimeoutSecs: number | null,
  ) => void
}

// フォーカスを外すと自動保存(legacy/frontend.md 2節)。入力中は自身のstateだけを更新し、
// blur時にのみ親へ確定した値を渡す。
function GeneralTab({ settings, onSave }: GeneralTabProps) {
  const [systemPrompt, setSystemPrompt] = useState(settings.general.system_prompt ?? '')
  const [taskChatSystemPrompt, setTaskChatSystemPrompt] = useState(
    settings.general.task_chat_system_prompt ?? '',
  )

  useEffect(() => {
    setSystemPrompt(settings.general.system_prompt ?? '')
    setTaskChatSystemPrompt(settings.general.task_chat_system_prompt ?? '')
  }, [settings])

  return (
    <div className="settings-panel">
      <label className="settings-field">
        <span>システムプロンプト</span>
        <textarea
          value={systemPrompt}
          onChange={(e) => setSystemPrompt(e.target.value)}
          onBlur={() =>
            onSave(
              systemPrompt || null,
              taskChatSystemPrompt || null,
              settings.general.response_timeout_secs,
            )
          }
        />
      </label>

      <NumberField
        label="応答タイムアウト(秒)"
        value={settings.general.response_timeout_secs}
        defaultValue={settings.general.default_response_timeout_secs}
        onSave={(secs) => onSave(systemPrompt || null, taskChatSystemPrompt || null, secs)}
      />

      <details className="settings-advanced">
        <summary>高度な設定</summary>
        <label className="settings-field">
          <span>タスクチャット用のシステムプロンプト</span>
          <textarea
            value={taskChatSystemPrompt}
            onChange={(e) => setTaskChatSystemPrompt(e.target.value)}
            onBlur={() =>
              onSave(
                systemPrompt || null,
                taskChatSystemPrompt || null,
                settings.general.response_timeout_secs,
              )
            }
          />
          <p className="settings-hint">
            総合チャットには無い、工程の追加・更新・削除など個別タスクの操作に関する指示は
            こちらに書く(上のシステムプロンプトの後ろに追加される)。
          </p>
        </label>
      </details>
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
        {settings.providers.length === 0 && <li className="list-empty">プロバイダーが未登録です。</li>}
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
      setListError(String(e))
    } finally {
      setListing(false)
    }
  }

  return (
    <li className="provider-card">
      <div className="provider-card-header">
        <strong>{provider.name}</strong>
        <ConfirmButton
          label="削除"
          confirmTitle="プロバイダーを削除"
          confirmMessage={`プロバイダー「${provider.name}」を削除しますか?保存済みのAPIキーも同時に削除されます。`}
          onConfirm={onDeleteProvider}
        />
      </div>
      <p className="provider-card-meta">
        {provider.base_url} · {provider.has_api_key ? 'APIキー設定済み' : 'APIキー未設定'}
      </p>
      {provider.error && (
        <p className="error">
          {'このプロバイダーは使えません。削除するか、チャットで別のプロバイダーのモデルに切り替えてください。'}
          {`(${provider.error})`}
        </p>
      )}

      {hasModel ? (
        <ModelTable provider={provider} onUpdate={onUpdateModels} />
      ) : (
        <p className="list-empty">モデル未登録(登録するとチャットで選べます)</p>
      )}
      {hasModel && provider.can_detect_capabilities && (
        <button type="button" onClick={() => void detect()} disabled={detecting}>
          {detecting ? '検出中…' : '能力をサーバーから検出'}
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
          placeholder="モデル名を入力して追加"
        />
        <button type="submit">追加</button>
        <button type="button" onClick={() => void listModels()} disabled={listing}>
          {listing ? '取得中…' : 'モデル一覧を取得'}
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
  const needle = query.trim().toLowerCase()
  const matched = needle
    ? candidates.filter((m) => m.label.toLowerCase().includes(needle))
    : candidates
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
        <p className="list-empty">取得した{available.length}件のモデルはすべて登録済みです。</p>
      ) : (
        <>
          <div className="model-table-toolbar">
            <input
              type="search"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder={`${candidates.length}件の未登録モデルから検索`}
              aria-label="取得したモデルを検索"
            />
            <button
              type="button"
              onClick={() =>
                setSelected((prev) => [...new Set([...prev, ...matched.map((m) => m.name)])])
              }
              disabled={matched.every((m) => chosen.includes(m.name))}
            >
              {needle ? '一致したものをすべて選択' : 'すべて選択'}
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
            {matched.length === 0 && (
              <li className="list-empty">「{query.trim()}」に一致するモデルはありません。</li>
            )}
          </ul>
        </>
      )}
      <div className="model-picker-actions">
        {candidates.length > 0 && (
          <button
            type="button"
            onClick={() => void submit()}
            disabled={chosen.length === 0 || adding}
          >
            {adding ? '追加中…' : `選択した${chosen.length}件を追加`}
          </button>
        )}
        <button type="button" onClick={onClose}>
          閉じる
        </button>
      </div>
    </div>
  )
}

const CAPABILITY_COLUMNS: { capability: Capability; label: string }[] = [
  { capability: 'image', label: '画像' },
  { capability: 'tools', label: 'ツール' },
  { capability: 'thinking', label: '思考' },
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
  const needle = collapsible ? query.trim().toLowerCase() : ''
  const matched = needle ? models.filter((m) => m.label.toLowerCase().includes(needle)) : models
  // 折りたたんでいても、検索したら当たった行は出す(legacy/frontend.md 3節)。
  const shown = collapsible && !expanded && !needle ? [] : matched

  return (
    <>
      {collapsible && (
        <div className="model-table-toolbar">
          <CollapseToggle
            count={models.length}
            noun="モデル"
            expanded={expanded}
            onToggle={() => setExpanded((v) => !v)}
          />
          <input
            type="search"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="モデル名で検索"
            aria-label="モデル名で検索"
          />
        </div>
      )}
      {shown.length > 0 && (
        <div className="model-table-scroll">
          <table className="model-table">
            <thead>
              <tr>
                <th>表示</th>
                <th>モデル</th>
                {CAPABILITY_COLUMNS.map(({ capability, label }) => (
                  <th key={capability}>{label}</th>
                ))}
                <th>コンテキスト長</th>
                <th aria-label="操作" />
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
      {needle && matched.length === 0 && (
        <p className="list-empty">「{query.trim()}」に一致するモデルはありません。</p>
      )}
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
      <td>
        <input
          type="checkbox"
          checked={model.visible}
          onChange={(e) => {
            const visible = e.target.checked
            onUpdate(() => setModelVisible(providerId, name, visible))
          }}
          aria-label={`${shown}をチャットのモデル一覧に出す`}
        />
      </td>
      <td className="model-name">{shown}</td>
      {CAPABILITY_COLUMNS.map(({ capability, label }) => (
        <td key={capability}>
          <span className="model-capability">
            <input
              type="checkbox"
              checked={model.capabilities[capability]}
              onChange={(e) => {
                const supported = e.target.checked
                onUpdate(() => setModelCapability(providerId, name, capability, supported))
              }}
              aria-label={`${shown}の${label}対応`}
            />
            {capability === 'tools' && !model.capabilities.tools && (
              <span
                className="model-warning"
                role="img"
                aria-label={TOOLS_OFF_WARNING}
                title={TOOLS_OFF_WARNING}
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
          placeholder={`既定 ${model.default_context_length}`}
          aria-label={`${shown}のコンテキスト長`}
        />
        {contextLength.invalid && (
          <p className="error model-context-length-error">{POSITIVE_INTEGER_ERROR}</p>
        )}
      </td>
      <td>
        <span className="model-actions">
          {model.overridden && (
            <button
              type="button"
              className="icon-button"
              onClick={() => onUpdate(() => resetModelCapabilities(providerId, name))}
              aria-label={`${shown}の能力を初期値に戻す`}
              title="能力を初期値に戻す"
            >
              ↺
            </button>
          )}
          <button
            type="button"
            className="icon-button"
            onClick={() => onUpdate(() => removeModel(providerId, name))}
            aria-label={`${shown}を削除`}
            title="削除"
          >
            ×
          </button>
        </span>
      </td>
    </tr>
  )
}

const TOOLS_OFF_WARNING =
  'ツール呼び出しに対応しないモデルでは、タスクや工程の更新ができなくなります'

interface CollapseToggleProps {
  count: number
  noun: string
  expanded: boolean
  onToggle: () => void
}

// 件数の多い一覧(モデル表・MCPのツール一覧)を既定で畳むための開閉ボタン。
function CollapseToggle({ count, noun, expanded, onToggle }: CollapseToggleProps) {
  return (
    <button type="button" onClick={onToggle} aria-expanded={expanded}>
      {expanded ? '折りたたむ' : `${count}件の${noun}を表示`}
    </button>
  )
}

// この件数以上の一覧は既定で畳む。モデル表とMCPのツール一覧で揃える。
const LIST_COLLAPSE_THRESHOLD = 5

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
      className="provider-add-form settings-section-break"
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
        <p className="settings-hint">{httpPlainTextHint('APIキー')}</p>
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

// 「1行1件、KEY=VALUE」形式のテキストをパースする(legacy/frontend.md 4節)。
// エラーは行ごとに個別指摘する。
function parseKeyValueLines(text: string): { pairs: [string, string][]; errors: string[] } {
  const pairs: [string, string][] = []
  const errors: string[] = []
  text.split('\n').forEach((line, i) => {
    const trimmed = line.trim()
    if (trimmed === '') return
    const eq = trimmed.indexOf('=')
    if (eq <= 0) {
      errors.push(`${i + 1}行目: "キー=値"の形式で入力してください`)
      return
    }
    const key = trimmed.slice(0, eq).trim()
    const value = trimmed.slice(eq + 1).trim()
    if (key === '') {
      errors.push(`${i + 1}行目: キーが空です`)
      return
    }
    pairs.push([key, value])
  })
  return { pairs, errors }
}

const MCP_NAME_PATTERN = /^[A-Za-z0-9_]{1,16}$/

interface McpTabProps {
  settings: SettingsView
  onSaveLimits: (maxRoundsPerTurn: number | null, totalTimeoutSecs: number | null) => void
  onAddServer: (name: string, endpoint: NewMcpEndpoint) => void
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
  return (
    <div className="settings-panel">
      <p className="settings-hint">
        登録したサーバーのツール説明はそのままモデルに渡ります。信頼できるサーバーだけを
        登録してください。
      </p>

      <ul className="provider-list">
        {settings.mcp_servers.map((server) => (
          <McpServerCard
            key={server.id}
            server={server}
            onDelete={() => onDeleteServer(server.id)}
            onSetEnabled={(enabled) => onSetServerEnabled(server.id, enabled)}
            onSetToolEnabled={(toolName, enabled) => onSetToolEnabled(server.id, toolName, enabled)}
            onFetchTools={() => onFetchTools(server.id)}
          />
        ))}
        {settings.mcp_servers.length === 0 && <li className="list-empty">サーバーが未登録です。</li>}
      </ul>

      <AddMcpServerForm existingNames={settings.mcp_servers.map((s) => s.name)} onAdd={onAddServer} />

      {/* ツール呼び出し全体の上限(legacy/frontend.md 4節「共通設定」)。内部ツールにも
          効くので、MCPサーバーの一覧より後ろ、タブの末尾に置く。サーバーの追加とは
          別の話なので、追加フォームと同じ形の仕切り線で切る。 */}
      <section className="settings-section settings-section-break">
        <NumberField
          label="1ターンあたりの最大ツール呼び出し回数"
          value={settings.tools.max_rounds_per_turn}
          defaultValue={settings.tools.default_max_rounds_per_turn}
          hint="ツールを実行するモデルとの往復の回数。1回の往復でツールを複数呼ぶこともある。使い切ったら、ツールを使わずに返信させるため、もう一度だけモデルを呼ぶ。"
          onSave={(rounds) => onSaveLimits(rounds, settings.tools.total_timeout_secs)}
        />

        <NumberField
          label="ツール呼び出し全体のタイムアウト(秒)"
          value={settings.tools.total_timeout_secs}
          defaultValue={settings.tools.default_total_timeout_secs}
          hint="1ターン内のツール実行に使える時間の合計。モデルの応答待ちは含まない。"
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
  onFetchTools: () => Promise<void>
}

// 取得済みのツール一覧は`server.tools`(Rust側のキャッシュ)から来る。カード自身では
// 保持しない——保持すると設定画面を閉じた時点で消え、有効にしたツールを確認することも
// 外すこともできなくなる(Issue #104)。
function McpServerCard({
  server,
  onDelete,
  onSetEnabled,
  onSetToolEnabled,
  onFetchTools,
}: McpServerCardProps) {
  const tools = server.tools
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [expanded, setExpanded] = useState(false)

  const handleFetchTools = async () => {
    setLoading(true)
    setError(null)
    try {
      await onFetchTools()
    } catch (e) {
      setError(String(e))
    } finally {
      setLoading(false)
    }
  }

  const endpointSummary =
    server.endpoint.transport === 'stdio'
      ? `標準入出力: ${[server.endpoint.command, ...server.endpoint.args].join(' ')}`
      : `streamable HTTP: ${server.endpoint.url}`
  const secretNames =
    server.endpoint.transport === 'stdio' ? server.endpoint.env_names : server.endpoint.header_names
  const secretLabel = server.endpoint.transport === 'stdio' ? '環境変数' : 'ヘッダー'

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
          label="削除"
          confirmTitle="サーバーを削除"
          confirmMessage={`サーバー「${server.name}」を削除しますか?保存済みの秘密情報も同時に削除されます。`}
          onConfirm={onDelete}
        />
      </div>

      <p className="provider-card-meta">{endpointSummary}</p>
      {secretNames.length > 0 && (
        <p className="provider-card-meta">
          {secretLabel}: {secretNames.join(', ')}(値は安全な場所に保存されています)
        </p>
      )}

      {tools.length === 0 ? (
        <p className="list-empty">
          {fetched ? 'ツールがありません。' : 'ツール一覧は未取得です。'}
        </p>
      ) : (
        <>
          {!fetched && (
            <p className="list-empty">
              ツール一覧は未取得です。有効化済みのツールのみ表示しています。
            </p>
          )}
          {collapsible && (
            <CollapseToggle
              count={tools.length}
              noun="ツール"
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
                    <span className="provider-card-meta">
                      モデルに渡せない名前のため、有効にできません
                    </span>
                  )}
                </li>
              )
            })}
          </ul>
        </>
      )}

      {error && <p className="error">{error}</p>}
      <button type="button" onClick={handleFetchTools} disabled={loading}>
        {loading ? '取得中…' : 'ツール一覧を取得'}
      </button>
    </li>
  )
}

interface AddMcpServerFormProps {
  existingNames: string[]
  onAdd: (name: string, endpoint: NewMcpEndpoint) => void
}

function AddMcpServerForm({ existingNames, onAdd }: AddMcpServerFormProps) {
  const [name, setName] = useState('')
  const [transport, setTransport] = useState<'stdio' | 'streamable_http'>('stdio')
  const [command, setCommand] = useState('')
  const [argsText, setArgsText] = useState('')
  const [envText, setEnvText] = useState('')
  const [url, setUrl] = useState('')
  const [headersText, setHeadersText] = useState('')
  const [errors, setErrors] = useState<string[]>([])

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
    const trimmedName = name.trim()
    const validationErrors: string[] = []
    if (!MCP_NAME_PATTERN.test(trimmedName)) {
      validationErrors.push('識別子は16字以内の英数字とアンダースコアのみで入力してください')
    } else if (existingNames.includes(trimmedName)) {
      validationErrors.push(`識別子「${trimmedName}」は既に使われています`)
    }

    if (transport === 'stdio') {
      if (!command.trim()) validationErrors.push('コマンドを入力してください')
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
      onAdd(trimmedName, { transport: 'stdio', command: command.trim(), args, env: pairs })
    } else {
      if (!url.trim()) validationErrors.push('URLを入力してください')
      const { pairs, errors: headerErrors } = parseKeyValueLines(headersText)
      validationErrors.push(...headerErrors)
      if (validationErrors.length > 0) {
        setErrors(validationErrors)
        return
      }
      setErrors([])
      onAdd(trimmedName, { transport: 'streamable_http', url: url.trim(), headers: pairs })
    }
    reset()
  }

  return (
    <form className="provider-add-form settings-section-break" onSubmit={submit}>
      <h2>サーバーを追加</h2>
      <label className="settings-field">
        <span>識別子(16字以内、英数字とアンダースコアのみ)</span>
        <input value={name} onChange={(e) => setName(e.target.value)} maxLength={16} required />
      </label>
      <label className="settings-field">
        <span>接続方式</span>
        <select
          value={transport}
          onChange={(e) => setTransport(e.target.value as 'stdio' | 'streamable_http')}
        >
          <option value="stdio">標準入出力(コマンド実行)</option>
          <option value="streamable_http">streamable HTTP</option>
        </select>
      </label>

      {transport === 'stdio' ? (
        <>
          <label className="settings-field">
            <span>コマンド</span>
            <input value={command} onChange={(e) => setCommand(e.target.value)} required />
          </label>
          <label className="settings-field">
            <span>引数(1行に1つ)</span>
            <textarea value={argsText} onChange={(e) => setArgsText(e.target.value)} />
          </label>
          <label className="settings-field">
            <span>環境変数(1行1件、キー=値)</span>
            <textarea value={envText} onChange={(e) => setEnvText(e.target.value)} />
            <p className="settings-hint">値は安全な場所(秘密情報ストア)に保存されます。</p>
          </label>
          <p className="settings-hint">
            この方式はアプリと同じ権限でコマンドを実行します。信頼できるコマンドだけを登録してください。
          </p>
        </>
      ) : (
        <>
          <label className="settings-field">
            <span>URL</span>
            <input value={url} onChange={(e) => setUrl(e.target.value)} required />
            <p className="settings-hint">{httpPlainTextHint('ヘッダーの値(認証情報を含む)')}</p>
          </label>
          <label className="settings-field">
            <span>ヘッダー(1行1件、キー=値)</span>
            <textarea value={headersText} onChange={(e) => setHeadersText(e.target.value)} />
            <p className="settings-hint">値は安全な場所(秘密情報ストア)に保存されます。</p>
          </label>
        </>
      )}

      {errors.map((e) => (
        <p key={e} className="error">
          {e}
        </p>
      ))}
      <button type="submit">追加</button>
    </form>
  )
}
