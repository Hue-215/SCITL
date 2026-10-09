// 設定画面の複数のタブが共有する入力欄と部品。
import { t } from './i18n'
import { useNumberInput } from './settingsInput'

interface NumberFieldProps {
  label: string
  // 保存済みの値。nullは未設定(既定値を使う)。
  value: number | null
  // 未設定のときに使われる値。プレースホルダに出す。
  defaultValue: number
  // 欄の文字列のまま保存する。断られたら理由の文言を返す(`useNumberInput`)。
  onSave: (text: string) => Promise<string[]>
}

export function NumberField({ label, value, defaultValue, onSave }: NumberFieldProps) {
  const { errors, inputProps } = useNumberInput(value, onSave)

  return (
    <label className="settings-field">
      <span>{label}</span>
      <input {...inputProps} placeholder={t('settings.unset_default_hint', { value: defaultValue })} />
      {errors.map((e) => (
        <p key={e} className="error">
          {e}
        </p>
      ))}
    </label>
  )
}

interface CollapseToggleProps {
  // 畳んでいるときの文言(何を何件表示するか)。
  showLabel: string
  expanded: boolean
  onToggle: () => void
}

// 件数の多い一覧(モデル表・MCPのツール一覧)を既定で畳むための開閉ボタン。
export function CollapseToggle({ showLabel, expanded, onToggle }: CollapseToggleProps) {
  return (
    <button type="button" onClick={onToggle} aria-expanded={expanded}>
      {expanded ? t('common.collapse') : showLabel}
    </button>
  )
}

// 通信先を登録するタブ(APIプロバイダー・外部ツール)の先頭に置く、通信先と信頼についての知らせ。
// 1文ずつ行を分けて出す。
export function ServerNotice() {
  return (
    <div>
      <p>{t('settings.server_notice.destination')}</p>
      <p>{t('settings.server_notice.trust')}</p>
    </div>
  )
}
