import { isolated, t } from './i18n'
import type { ExportOutcome } from './types'

// 書き出しの結果の文言。取りやめ(`cancelled`)は何も出さない。
export default function ExportOutcomeText({ outcome }: { outcome: ExportOutcome }) {
  const summary =
    outcome.status === 'written' || outcome.status === 'saved' ? outcome.summary : null
  return (
    <>
      {outcome.status === 'written' && (
        <p>{t('export.done', { folder: isolated(outcome.summary.folder) })}</p>
      )}
      {outcome.status === 'saved' && <p>{t('export.saved')}</p>}
      {summary && summary.missing_attachments > 0 && (
        <p className="error">
          {t('export.missing_attachments', { count: summary.missing_attachments })}
        </p>
      )}
      {outcome.status === 'left_incomplete' && (
        <p className="error">{t('export.left_incomplete', { error: isolated(outcome.reason) })}</p>
      )}
    </>
  )
}
