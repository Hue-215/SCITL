import { useEffect, type ReactNode } from 'react'
import { createPortal } from 'react-dom'

interface DialogProps {
  title: string
  onClose: () => void
  children: ReactNode
}

// ダイアログの共通枠(legacy/frontend.md 5節)。余白・角丸(未導入方針なので実質0)・
// ボタン配置を統一する。破壊的操作の確認以外(リンク確認・画像プレビュー・テキスト添付の
// 全文表示)もこの枠を経由させる想定のため、本文は children に委ね、枠自体は内容を知らない。
export default function Dialog({ title, onClose, children }: DialogProps) {
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose()
    }
    window.addEventListener('keydown', onKeyDown)
    return () => window.removeEventListener('keydown', onKeyDown)
  }, [onClose])

  return createPortal(
    <div className="dialog-overlay" onClick={onClose}>
      <div
        className="dialog"
        role="dialog"
        aria-modal="true"
        aria-label={title}
        onClick={(e) => e.stopPropagation()}
      >
        <h2>{title}</h2>
        {children}
      </div>
    </div>,
    document.body,
  )
}

interface ConfirmDialogProps {
  title: string
  message: string
  confirmLabel?: string
  onConfirm: () => void
  onCancel: () => void
}

// 破壊的操作の確認ダイアログ。「キャンセル/実行」の2択で、実行ボタンを警告色にする
// (legacy/frontend.md 5節)。
export function ConfirmDialog({
  title,
  message,
  confirmLabel = '実行',
  onConfirm,
  onCancel,
}: ConfirmDialogProps) {
  return (
    <Dialog title={title} onClose={onCancel}>
      <p>{message}</p>
      <div className="dialog-actions">
        <button type="button" onClick={onCancel}>
          キャンセル
        </button>
        <button type="button" className="danger" onClick={onConfirm}>
          {confirmLabel}
        </button>
      </div>
    </Dialog>
  )
}
