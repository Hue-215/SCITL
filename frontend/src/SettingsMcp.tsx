// 設定画面の「ツール」タブ。
import { useId, useState } from 'react'
import type { FormEvent } from 'react'
import type { NewMcpEndpoint } from './api'
import type { McpServerView, SettingsView } from './types'
import { ConfirmButton } from './Dialog'
import Dropdown from './Dropdown'
import { isolated, type MessageKey, t } from './i18n'
import { CollapseToggle, LIST_COLLAPSE_THRESHOLD, NumberField } from './settingsFields'
import { httpPlainTextHint } from './settingsInput'
import { useAsyncAction } from './useAsyncAction'

type Transport = McpServerView['endpoint']['transport']

const TRANSPORT_LABELS: Record<Transport, MessageKey> = {
  stdio: 'settings.tools.transport_stdio',
  streamable_http: 'settings.tools.transport_http',
}

// 「1行1件、KEY=VALUE」形式のテキストをパースする。エラーは行ごとに個別指摘する。行は前後の
// 空白を除いてから見るので、`=`が先頭でなければキーは空にならない。
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
          line: isolated(trimmed),
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
  onAddServer: (name: string, endpoint: NewMcpEndpoint) => Promise<void>
  onDeleteServer: (serverId: string) => void
  onSetServerEnabled: (serverId: string, enabled: boolean) => void
  onSetToolEnabled: (serverId: string, toolName: string, enabled: boolean) => void
  fetchingTools: string[]
  toolFetchErrors: Record<string, string>
  onFetchTools: (serverId: string) => void
}

export function McpTab({
  settings,
  onSaveLimits,
  onAddServer,
  onDeleteServer,
  onSetServerEnabled,
  onSetToolEnabled,
  fetchingTools,
  toolFetchErrors,
  onFetchTools,
}: McpTabProps) {
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
            fetching={fetchingTools.includes(server.id)}
            fetchError={toolFetchErrors[server.id] ?? null}
            onFetchTools={() => onFetchTools(server.id)}
          />
        ))}
        {settings.mcp_servers.length === 0 && (
          <li className="list-empty">{t('settings.tools.none_registered')}</li>
        )}
      </ul>

      <AddMcpServerForm
        existingNames={settings.mcp_servers.map((s) => s.name)}
        nameMaxChars={settings.mcp_server_name_max_chars}
        onAdd={onAddServer}
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
// 外すこともできなくなる。
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
          command: isolated([server.endpoint.command, ...server.endpoint.args].join(' ')),
        })
      : t('settings.tools.endpoint_http', { url: isolated(server.endpoint.url) })
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
          confirmMessage={t('settings.tools.delete_server_dialog_message', {
            id: isolated(server.name),
          })}
          confirmLabel={t('settings.tools.unregister_button')}
          onConfirm={onDelete}
        />
      </div>

      <p className="provider-card-meta">{endpointSummary}</p>
      {secretNames.length > 0 && (
        <p className="provider-card-meta">
          {t('settings.tools.secret_names_display', {
            label: secretLabel,
            names: secretNames.map(isolated).join(', '),
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
  // 失敗はフォームの直下に出し、入力は残す(Rust側の検証で弾かれても打ち直さずに済むように)。
  // 入力を空にするのは成功したときだけ。
  const submission = useAsyncAction()

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
    if (submission.running) return
    const trimmedName = name.trim()
    const validationErrors: string[] = []
    if (trimmedName === '') {
      validationErrors.push(t('settings.tools.id_required'))
    } else if (trimmedName.length > nameMaxChars || !MCP_NAME_CHARS.test(trimmedName)) {
      validationErrors.push(t('settings.tools.id_invalid', { max: nameMaxChars }))
    } else if (existingNames.includes(trimmedName)) {
      validationErrors.push(t('settings.tools.id_duplicate', { id: isolated(trimmedName) }))
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
      <button type="submit" disabled={submission.running}>
        {submission.running ? t('common.adding') : t('common.add')}
      </button>
    </form>
  )
}
