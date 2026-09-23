import { useEffect, useState } from 'react'
import type { FormEvent } from 'react'
import {
  addMcpServer,
  addModel,
  addProvider,
  deleteMcpServer,
  deleteProvider,
  fetchMcpTools,
  getSettings,
  removeModel,
  setActiveModel,
  setActiveProvider,
  setMcpServerEnabled,
  setMcpToolEnabled,
  updateGeneralSettings,
  updateToolSettings,
  type NewMcpEndpoint,
} from './api'
import type {
  ApiFormat,
  McpServerView,
  McpToolInfo,
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

// フォーカスを外すと自動保存する数値入力(legacy/frontend.md 2節・4節)。入力中は自身の
// stateだけを更新し、blur時にのみ親へ確定した値を渡す。入力チェック(空欄は未設定、
// それ以外は1以上の整数)をこの1箇所に閉じる。3箇所目が出た時点ではなく2箇所目で
// 部品にしたのは、同じ検証を書き写すと片方だけ直す事故が起きるため(ui.md 1節)。
function NumberField({ label, value, defaultValue, hint, onSave }: NumberFieldProps) {
  const [text, setText] = useState(value?.toString() ?? '')
  const [invalid, setInvalid] = useState(false)

  useEffect(() => {
    setText(value?.toString() ?? '')
    setInvalid(false)
  }, [value])

  const save = () => {
    const trimmed = text.trim()
    if (trimmed === '') {
      setInvalid(false)
      onSave(null)
      return
    }
    const parsed = Number(trimmed)
    if (!Number.isInteger(parsed) || parsed <= 0) {
      setInvalid(true)
      return
    }
    setInvalid(false)
    onSave(parsed)
  }

  return (
    <label className="settings-field">
      <span>{label}</span>
      <input
        type="text"
        inputMode="numeric"
        value={text}
        onChange={(e) => setText(e.target.value)}
        onBlur={save}
        placeholder={`未設定(既定値 ${defaultValue})`}
      />
      {hint && <p className="settings-hint">{hint}</p>}
      {invalid && <p className="error">1以上の整数を入力してください</p>}
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
          <ProviderCard
            key={provider.id}
            provider={provider}
            active={settings.active_provider_id === provider.id}
            newModel={newModelByProvider[provider.id] ?? ''}
            onSetNewModel={(value) =>
              setNewModelByProvider((prev) => ({ ...prev, [provider.id]: value }))
            }
            onDeleteProvider={() => onDeleteProvider(provider.id)}
            onSetActiveProvider={() => onSetActiveProvider(provider.id)}
            onAddModel={(model) => onAddModel(provider.id, model)}
            onRemoveModel={(model) => onRemoveModel(provider.id, model)}
            onSetActiveModel={(model) => onSetActiveModel(provider.id, model)}
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
  active: boolean
  newModel: string
  onSetNewModel: (value: string) => void
  onDeleteProvider: () => void
  onSetActiveProvider: () => void
  onAddModel: (model: string) => void
  onRemoveModel: (model: string) => void
  onSetActiveModel: (model: string) => void
}

function ProviderCard({
  provider,
  active,
  newModel,
  onSetNewModel,
  onDeleteProvider,
  onSetActiveProvider,
  onAddModel,
  onRemoveModel,
  onSetActiveModel,
}: ProviderCardProps) {
  const hasModel = provider.models.length > 0

  return (
    <li className="provider-card">
      <div className="provider-card-header">
        <label className="choice" title={hasModel ? undefined : 'モデルを1件以上登録すると選択できます'}>
          <input
            type="radio"
            name="active-provider"
            checked={active}
            onChange={onSetActiveProvider}
            disabled={!hasModel}
          />
          <strong>{provider.name}</strong>
        </label>
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
          {'このプロバイダーは使えません。削除するか、別のプロバイダーに切り替えてください。'}
          {`(${provider.error})`}
        </p>
      )}

      <ul className="model-list">
        {provider.models.map((model) => (
          <li key={model} className="model-row">
            <label className="choice">
              <input
                type="radio"
                name={`active-model-${provider.id}`}
                checked={provider.active_model === model}
                onChange={() => onSetActiveModel(model)}
              />
              {model}
            </label>
            <button type="button" onClick={() => onRemoveModel(model)}>
              削除
            </button>
          </li>
        ))}
        {provider.models.length === 0 && (
          <li className="list-empty">モデル未登録(登録するとアクティブに選択できます)</li>
        )}
      </ul>

      <form
        className="model-add-form"
        onSubmit={(e) => {
          e.preventDefault()
          const model = newModel.trim()
          if (!model) return
          onAddModel(model)
          onSetNewModel('')
        }}
      >
        <input
          value={newModel}
          onChange={(e) => onSetNewModel(e.target.value)}
          placeholder="モデル名を入力して追加"
        />
        <button type="submit">追加</button>
      </form>
    </li>
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

const TOOL_COLLAPSE_THRESHOLD = 5

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

  // 一覧が未取得でも、ユーザーが有効にしたツールの名前はconfig.tomlから分かる。
  // それを描かないと、アプリを再起動した直後は有効化済みのツールを確認することも
  // 外すこともできない(キャッシュはアプリ起動中のみ。Issue #104)。説明文はサーバーに
  // 聞かないと分からないので、取得するまで出ない。
  const fetched = tools !== null
  const displayTools: McpToolInfo[] =
    tools ?? server.enabled_tools.map((name) => ({ name, description: null }))
  const collapsible = displayTools.length >= TOOL_COLLAPSE_THRESHOLD
  const visibleTools = collapsible && !expanded ? [] : displayTools

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

      {displayTools.length === 0 ? (
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
            <button type="button" onClick={() => setExpanded((v) => !v)}>
              {expanded ? '折りたたむ' : `${displayTools.length}件のツールを表示`}
            </button>
          )}
          <ul className="model-list">
            {visibleTools.map((tool) => (
              <li key={tool.name} className="model-row">
                <label className="choice" title={tool.description ?? undefined}>
                  <input
                    type="checkbox"
                    checked={server.enabled_tools.includes(tool.name)}
                    onChange={(e) => onSetToolEnabled(tool.name, e.target.checked)}
                  />
                  {tool.name}
                </label>
              </li>
            ))}
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
