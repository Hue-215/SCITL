import { useEffect, useState } from 'react'
import { failureText, inspectLink, openConfirmedLink } from './api'
import Dialog from './Dialog'
import { t } from './i18n'
import type { LinkInspection } from './types'

interface LinkDialogProps {
  url: string
  onClose: () => void
}

// 本文中のリンクを開く前の確認(legacy/frontend.md 1節)。何を警告するかの判定は
// Rust側(scitl-core/src/link.rs)に閉じ、ここは結果を表示するだけにする。
export default function LinkDialog({ url, onClose }: LinkDialogProps) {
  const [inspection, setInspection] = useState<LinkInspection | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    let cancelled = false
    inspectLink(url)
      .then((result) => {
        if (!cancelled) setInspection(result)
      })
      .catch((e) => {
        if (!cancelled) setError(failureText(e))
      })
    return () => {
      cancelled = true
    }
  }, [url])

  const open = async () => {
    try {
      await openConfirmedLink(url)
      onClose()
    } catch (e) {
      setError(failureText(e))
    }
  }

  const verdict = inspection?.verdict
  const hasWarning = !!inspection?.real_url || !!inspection?.userinfo_host

  return (
    <Dialog title={t('link.dialog_title')} onClose={onClose}>
      <p className="link-dialog-url">{inspection?.url ?? url}</p>
      {inspection?.real_url && (
        <div className="link-dialog-warning">
          <h3>{t('link.special_char_title')}</h3>
          <p>{t('link.special_char_body')}</p>
          <p className="link-dialog-url">
            {t('link.real_url_label', { url: inspection.real_url })}
          </p>
        </div>
      )}
      {inspection?.userinfo_host && (
        <div className="link-dialog-warning">
          <h3>{t('link.userinfo_title')}</h3>
          <p>{t('link.userinfo_body')}</p>
          <p className="link-dialog-url">
            {t('link.userinfo_domain_label', { domain: inspection.userinfo_host })}
          </p>
        </div>
      )}
      {verdict?.kind === 'unreadable' && (
        <p className="link-dialog-warning">{t('link.unreadable')}</p>
      )}
      {verdict?.kind === 'scheme_blocked' && (
        <p className="link-dialog-warning">
          {t('link.scheme_blocked', { scheme: verdict.scheme })}
        </p>
      )}
      {verdict?.kind === 'mail' && <p>{t('link.mailto_note')}</p>}
      {verdict?.kind === 'web' && !hasWarning && <p>{t('link.generic_warning')}</p>}
      {error && <p className="link-dialog-warning">{error}</p>}
      <div className="dialog-actions">
        <button type="button" onClick={onClose}>
          {t('common.cancel')}
        </button>
        {inspection?.can_open && (
          <button type="button" onClick={() => void open()}>
            {t('link.open')}
          </button>
        )}
      </div>
    </Dialog>
  )
}
