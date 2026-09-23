import { useEffect, useState } from 'react'
import { inspectLink, openConfirmedLink } from './api'
import Dialog from './Dialog'
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
        if (!cancelled) setError(String(e))
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
      setError(String(e))
    }
  }

  const verdict = inspection?.verdict
  const canOpen = verdict?.kind === 'web' || verdict?.kind === 'mail'
  const hasWarning = !!inspection?.real_url || !!inspection?.userinfo_host

  return (
    <Dialog title="外部サイトへ移動" onClose={onClose}>
      <p className="link-dialog-url">{inspection?.url ?? url}</p>
      {inspection?.real_url && (
        <div className="link-dialog-warning">
          <h3>警告: 特殊文字を含むドメイン</h3>
          <p>特殊文字を含むドメインにアクセスします。URLを再度確認してください。</p>
          <p className="link-dialog-url">移動先URL: {inspection.real_url}</p>
        </div>
      )}
      {inspection?.userinfo_host && (
        <div className="link-dialog-warning">
          <h3>警告: @を含むURL</h3>
          <p>URLに@が含まれています。意図しないドメインにアクセスする可能性があります。</p>
          <p className="link-dialog-url">移動先ドメイン: {inspection.userinfo_host}</p>
        </div>
      )}
      {verdict?.kind === 'unreadable' && (
        <p className="link-dialog-warning">URLを解釈できませんでした。</p>
      )}
      {verdict?.kind === 'scheme_blocked' && (
        <p className="link-dialog-warning">
          SCITLは"{verdict.scheme}:" のリンクに対応しません。
        </p>
      )}
      {verdict?.kind === 'mail' && <p>メールを開きます</p>}
      {verdict?.kind === 'web' && !hasWarning && (
        <p>SCITL外部のサイトに移動します。URLが意図したものか確認してください。</p>
      )}
      {error && <p className="link-dialog-warning">{error}</p>}
      <div className="dialog-actions">
        <button type="button" onClick={onClose}>
          キャンセル
        </button>
        {canOpen && (
          <button type="button" onClick={() => void open()}>
            開く
          </button>
        )}
      </div>
    </Dialog>
  )
}
