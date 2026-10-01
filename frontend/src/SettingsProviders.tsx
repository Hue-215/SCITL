// 設定画面の「プロバイダー」タブ。
import { useId, useState } from 'react'
import {
  addModels,
  detectModelCapabilities,
  listProviderModels,
  removeModel,
  resetModelCapabilities,
  setModelCapability,
  setModelContextLength,
  setModelVisible,
} from './api'
import type {
  ApiFormat,
  AvailableModel,
  Capability,
  ModelView,
  ProviderView,
  SettingsView,
} from './types'
import { matchQuery } from './search'
import { ConfirmButton } from './Dialog'
import Dropdown from './Dropdown'
import { isolated, type MessageKey, t } from './i18n'
import { CollapseToggle } from './settingsFields'
import { httpPlainTextHint, usePositiveIntegerInput } from './settingsInput'
import { useAsyncAction } from './useAsyncAction'
import { useCollapse } from './useCollapse'

const DEFAULT_BASE_URL_BY_FORMAT: Record<ApiFormat, string> = {
  open_ai_compat: 'https://api.openai.com/v1',
  anthropic: 'https://api.anthropic.com',
  gemini: 'https://generativelanguage.googleapis.com',
}

const API_FORMAT_LABELS: Record<ApiFormat, MessageKey> = {
  open_ai_compat: 'settings.provider.formats.open_ai_compat',
  anthropic: 'settings.provider.formats.anthropic',
  gemini: 'settings.provider.formats.gemini',
}

// Anthropic・Gemini形式のベースURLはAPIの版のパスを含まない(アダプタが`v1/messages`・
// `v1beta/interactions`を足す)。OpenAI互換の癖で版まで書くと、存在しないパスに送ることになる。
// 登録は止めず、ヒントで知らせる。
const EXTRA_PATH_HINTS: Partial<Record<ApiFormat, { pattern: RegExp; hint: MessageKey }>> = {
  anthropic: {
    pattern: /\/v1(\/messages)?\/?$/,
    hint: 'settings.provider.base_url_extra_path_anthropic',
  },
  gemini: {
    pattern: /\/v1(beta)?(\/interactions|\/openai)?\/?$/,
    hint: 'settings.provider.base_url_extra_path_gemini',
  },
}

function extraPathHint(format: ApiFormat, baseUrl: string): MessageKey | null {
  const rule = EXTRA_PATH_HINTS[format]
  if (!rule) return null
  try {
    return rule.pattern.test(new URL(baseUrl.trim()).pathname) ? rule.hint : null
  } catch {
    return null
  }
}

// 検索欄のあるモデルの一覧(登録済みの表・取得したモデルの候補)で、絞り込んだ結果が空のときの一文。
function noModelMatchText(query: string): string {
  return t('settings.model.no_match', { query: isolated(query.trim()) })
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

export function ProvidersTab({
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
  // 検出の失敗は、モデルの操作と同じく親のエラー欄に出る(`onUpdateModels`)。
  const detection = useAsyncAction()
  // 取得したモデル名。設定には書かないので、カードを閉じれば(設定画面を離れれば)捨てる。
  const [available, setAvailable] = useState<AvailableModel[] | null>(null)
  // 取得の失敗はカード内に出す(どのプロバイダーで失敗したかが分かるように)。
  const listing = useAsyncAction((error) => t('common.fetch_failed', { error: isolated(error) }))

  // 追加したモデルの能力もすぐ表に出す。サーバーに繋がらなくても追加は済んでいるので、
  // 検出の失敗は追加の失敗として出さない(ターンの開始時にもう一度問い合わせる)。
  const add = (models: string[]) =>
    onUpdateModels(async () => {
      const added = await addModels(provider.id, models)
      if (!provider.can_detect_capabilities) return added
      return detectModelCapabilities(provider.id).catch(() => added)
    })

  return (
    <li className="provider-card">
      <div className="provider-card-header">
        <strong>{provider.name}</strong>
        <ConfirmButton
          label={t('common.delete')}
          confirmTitle={t('settings.provider.delete_provider_dialog_title')}
          confirmMessage={t('settings.provider.delete_provider_dialog_message', {
            name: isolated(provider.name),
          })}
          confirmLabel={t('common.delete')}
          onConfirm={onDeleteProvider}
        />
      </div>
      <p className="provider-card-meta">
        {t('settings.provider.meta', {
          url: isolated(provider.base_url),
          api_key: provider.has_api_key
            ? t('settings.provider.api_key_set')
            : t('settings.provider.api_key_unset'),
        })}
      </p>
      {provider.error && (
        <p className="error">
          {t('settings.provider.unusable', { error: isolated(provider.error) })}
        </p>
      )}
      {provider.key_error && (
        <p className="error">
          {t('settings.provider.key_unavailable', { error: isolated(provider.key_error) })}
        </p>
      )}

      {hasModel ? (
        <ModelTable provider={provider} onUpdate={onUpdateModels} />
      ) : (
        <p className="list-empty">{t('settings.model.none_registered')}</p>
      )}
      {hasModel && provider.can_detect_capabilities && (
        <button
          type="button"
          onClick={() =>
            void detection.run(() => onUpdateModels(() => detectModelCapabilities(provider.id)))
          }
          disabled={detection.running}
        >
          {detection.running ? t('settings.model.detecting') : t('settings.model.detect_button')}
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
        <button
          type="button"
          onClick={() => void listing.run(() => listProviderModels(provider.id), setAvailable)}
          disabled={listing.running}
        >
          {listing.running ? t('common.fetching') : t('settings.model.fetch_models_button')}
        </button>
      </form>
      {listing.error && <p className="error">{listing.error}</p>}
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

// 取得したモデルから、登録するものを選ぶ欄。登録済みのモデルは候補から外す(追加すると表の側へ
// 移る)。
function ModelPicker({ available, registered, onAdd, onClose }: ModelPickerProps) {
  const [selected, setSelected] = useState<string[]>([])
  const [query, setQuery] = useState('')
  // 追加の失敗は、モデルの操作と同じく親のエラー欄に出る(`onAdd`)。
  const adding = useAsyncAction()

  const candidates = available.filter((m) => !registered.includes(m.name))
  const { matched, searching } = matchQuery(candidates, query, (m) => m.label)
  // 追加や別の操作で登録済みになったものは、選択から外れたものとして数える。
  const chosen = selected.filter((name) => candidates.some((m) => m.name === name))

  const toggle = (name: string, checked: boolean) =>
    setSelected((prev) => (checked ? [...prev, name] : prev.filter((n) => n !== name)))

  const submit = () =>
    adding.run(
      // 候補の並び(名前順)で登録する。選んだ順にすると、表の並びが操作の順に左右される。
      () => onAdd(candidates.filter((m) => chosen.includes(m.name)).map((m) => m.name)),
      () => setSelected([]),
    )

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
            disabled={chosen.length === 0 || adding.running}
          >
            {adding.running
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
    capability: 'thinking',
    label: 'settings.model.cap_reasoning_label',
    checkboxLabel: 'settings.model.cap_reasoning_checkbox_label',
  },
]

interface ModelTableProps {
  provider: ProviderView
  onUpdate: (action: () => Promise<SettingsView>) => void
}

// モデル表。能力は解決済みの値を描くだけで、手動設定の正規化(初期値と同じ値なら手動設定を
// 外す)はRust側が持つ。
function ModelTable({ provider, onUpdate }: ModelTableProps) {
  const models = provider.models
  const { collapsible, collapsed, toggle } = useCollapse(models.length)
  const [query, setQuery] = useState('')

  // 検索欄は畳める件数のときだけ出す。削除で件数が減って欄が消えたら、打った語は消せない
  // ので、欄が無いあいだは絞り込まない。
  const { matched, searching } = matchQuery(models, collapsible ? query : '', (m) => m.label)
  // 折りたたんでいても、検索したら当たった行は出す。
  const shown = collapsed && !searching ? [] : matched

  return (
    <>
      {collapsible && (
        <div className="model-table-toolbar">
          <CollapseToggle
            showLabel={t('settings.model.show_all', { count: models.length })}
            {...toggle}
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
          aria-label={t('settings.model.visible_checkbox_label', { model: isolated(shown) })}
        />
      </td>
      <td className="model-name">
        {shown}
        {model.lacks_tools && (
          <span
            className="model-warning"
            role="img"
            aria-label={t('settings.model.lacks_tools_warning')}
            title={t('settings.model.lacks_tools_warning')}
          >
            ⚠
          </span>
        )}
      </td>
      {CAPABILITY_COLUMNS.map(({ capability, checkboxLabel }) => (
        <td key={capability} className="model-check-col">
          <input
            type="checkbox"
            checked={model.capabilities[capability]}
            onChange={(e) => {
              const supported = e.target.checked
              onUpdate(() => setModelCapability(providerId, name, capability, supported))
            }}
            aria-label={t(checkboxLabel, { model: isolated(shown) })}
          />
        </td>
      ))}
      <td>
        <input
          {...contextLength.inputProps}
          className="model-context-length"
          placeholder={t('settings.model.context_length_default_hint', {
            value: model.default_context_length,
          })}
          aria-label={t('settings.model.context_length_label', { model: isolated(shown) })}
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
              aria-label={t('settings.model.reset_caps_label', { model: isolated(shown) })}
              title={t('settings.model.reset_caps_tooltip')}
            >
              ↺
            </button>
          )}
          <button
            type="button"
            className="icon-button"
            onClick={() => onUpdate(() => removeModel(providerId, name))}
            aria-label={t('settings.model.delete_model_label', { model: isolated(shown) })}
            title={t('common.delete')}
          >
            ×
          </button>
        </span>
      </td>
    </tr>
  )
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
  const pathHint = extraPathHint(apiFormat, baseUrl)
  // 失敗はフォームの直下に出し、入力は残す(Rust側の検証で弾かれても打ち直さずに済むように)。
  // 入力を空にするのは成功したときだけ。
  const submission = useAsyncAction()

  return (
    <form
      className="provider-add-form settings-section-break"
      onSubmit={(e) => {
        e.preventDefault()
        if (submission.running || !name.trim() || !baseUrl.trim()) return
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
        {pathHint && <p className="settings-hint">{t(pathHint)}</p>}
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
      <button type="submit" disabled={submission.running}>
        {submission.running ? t('common.adding') : t('common.add')}
      </button>
    </form>
  )
}

