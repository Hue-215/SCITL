// 設定画面の複数のタブが共有する入力欄と部品。
import { t } from './i18n'
import { usePositiveIntegerInput } from './settingsInput'

interface NumberFieldProps {
  label: string
  // 保存済みの値。nullは未設定(既定値を使う)。
  value: number | null
  // 未設定のときに使われる値。プレースホルダに出す。
  defaultValue: number
  hint?: string
  onSave: (value: number | null) => void
}

export function NumberField({ label, value, defaultValue, hint, onSave }: NumberFieldProps) {
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
