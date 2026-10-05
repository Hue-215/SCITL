// 設定画面の「ツール」タブ。
import { useState } from 'react'
import type { FormEvent } from 'react'
import type { McpServerView, NewMcpEndpoint, SettingsView } from './types'
import { ConfirmButton } from './Dialog'
import { isolated, t } from './i18n'
import { CollapseToggle, NumberField, ServerNotice } from './settingsFields'
import { useAsyncAction } from './useAsyncAction'
import { useCollapse } from './useCollapse'

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

// 使える文字と並びだけを見る(長さの上限はRust側から受け取る)。アンダーバーは英数字の間に
// 1つずつだけ置ける。登録の可否はRust側(`config::validate_mcp_server_name`)が決め直す。
// ここで見るのは、表示言語の文言で理由を出すため。
const MCP_NAME_CHARS = /^[A-Za-z0-9]+(_[A-Za-z0-9]+)*$/

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
      <ServerNotice />
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

      {/* ツール呼び出し全体の上限。内部ツールにも
          効くので、MCPサーバーの一覧より後ろ、タブの末尾に置く。サーバーの追加とは
          別の話なので、追加フォームと同じ形の仕切り線で切る。 */}
      <section className="settings-section settings-section-break">
        <NumberField
          label={t('settings.tools.max_rounds_label')}
          value={settings.tools.max_rounds_per_turn}
          defaultValue={settings.tools.default_max_rounds_per_turn}
          onSave={(rounds) => onSaveLimits(rounds, settings.tools.total_timeout_secs)}
        />

        <NumberField
          label={t('settings.tools.timeout_label')}
          value={settings.tools.total_timeout_secs}
          defaultValue={settings.tools.default_total_timeout_secs}
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

// 取得済みのツール一覧は`server.tools`(Rust側のキャッシュ)から来る。カード自身で保持すると、
// 設定画面を閉じた時点で消える。
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
  const { collapsible, collapsed, toggle } = useCollapse(tools.length)

  const endpointSummary = t('settings.tools.endpoint_http', { url: isolated(server.endpoint.url) })
  const secretNames = server.endpoint.header_names
  const secretLabel = t('settings.tools.header_names_label')

  const fetched = server.tools_fetched
  const visibleTools = collapsed ? [] : tools

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
              {...toggle}
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
  const [name, setName] = useState('')
  const [url, setUrl] = useState('')
  const [headersText, setHeadersText] = useState('')
  const [errors, setErrors] = useState<string[]>([])
  // 失敗はフォームの直下に出し、入力は残す(Rust側の検証で弾かれても打ち直さずに済むように)。
  // 入力を空にするのは成功したときだけ。
  const submission = useAsyncAction()

  const reset = () => {
    setName('')
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

  return (
    <form className="settings-section settings-section-break" onSubmit={submit}>
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
      <label className="settings-field">
        <span>{t('settings.tools.url_hint')}</span>
        <input value={url} onChange={(e) => setUrl(e.target.value)} required />
      </label>
      <label className="settings-field">
        <span>{t('settings.tools.headers_hint', { sample: t('settings.tools.kv_sample') })}</span>
        <textarea value={headersText} onChange={(e) => setHeadersText(e.target.value)} />
      </label>

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
