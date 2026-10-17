import { t } from './i18n'
import { DisclosureMark } from './Icon'

/** 畳んだ説明を開け閉めする「説明」のボタン。開いた説明は呼び出し側が描く。 */
export function DescriptionToggle({ open, onToggle }: { open: boolean; onToggle: () => void }) {
  return (
    <button type="button" className="description-toggle" aria-expanded={open} onClick={onToggle}>
      <DisclosureMark />
      {t('common.description_toggle')}
    </button>
  )
}
