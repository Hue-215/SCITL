import { useEffect, useRef, useState, type ReactNode } from 'react'
import { createPortal } from 'react-dom'
import { t } from './i18n'

interface DialogProps {
  title: string
  onClose: () => void
  children: ReactNode
  // 呼び出し側ごとに必要な幅が異なる(確認ダイアログと画像プレビュー・全文表示等)ため、
  // 枠自体は既定幅だけを持ち、外から上書きできるようにする(principles.md 6節
  // 「共通の操作は共通部品を経由させ、個別に組み立てない」: 個別CSSの上書きを避ける)。
  width?: string
}

const FOCUSABLE_SELECTOR =
  'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])'

// ダイアログの共通枠(legacy/frontend.md 5節)。余白・角丸(未導入方針なので実質0)・
// ボタン配置を統一する。破壊的操作の確認以外(リンク確認・画像プレビュー・テキスト添付の
// 全文表示)もこの枠を経由させる想定のため、本文は children に委ね、枠自体は内容を知らない。
export default function Dialog({ title, onClose, children, width }: DialogProps) {
  const boxRef = useRef<HTMLDivElement>(null)
  const triggerRef = useRef<Element | null>(null)

  useEffect(() => {
    // 開いた時のフォーカス移動と、閉じた時に呼び出し元(削除ボタン等)へ戻す
    // (role="dialog" aria-modal="true" を名乗る以上、モーダルとして最低限必要な挙動)。
    triggerRef.current = document.activeElement
    boxRef.current?.focus()
    return () => {
      if (triggerRef.current instanceof HTMLElement) triggerRef.current.focus()
    }
  }, [])

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        onClose()
        return
      }
      if (e.key !== 'Tab' || !boxRef.current) return
      // フォーカストラップ: Tabで背後の画面へ抜けないよう、ダイアログ内で循環させる。
      const focusable = boxRef.current.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR)
      if (focusable.length === 0) return
      const first = focusable[0]
      const last = focusable[focusable.length - 1]
      if (e.shiftKey && document.activeElement === first) {
        e.preventDefault()
        last.focus()
      } else if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault()
        first.focus()
      }
    }
    window.addEventListener('keydown', onKeyDown)
    return () => window.removeEventListener('keydown', onKeyDown)
  }, [onClose])

  return createPortal(
    <div className="dialog-overlay" onClick={onClose}>
      <div
        ref={boxRef}
        className="dialog"
        role="dialog"
        aria-modal="true"
        aria-label={title}
        tabIndex={-1}
        style={width ? { width } : undefined}
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
  // 何をするボタンかを操作ごとに書く(「削除」「登録を解除」)。
  confirmLabel: string
  onConfirm: () => void
  onCancel: () => void
}

// 破壊的操作の確認ダイアログ。「キャンセル/実行」の2択で、実行ボタンを警告色にする
// (legacy/frontend.md 5節)。
export function ConfirmDialog({
  title,
  message,
  confirmLabel,
  onConfirm,
  onCancel,
}: ConfirmDialogProps) {
  return (
    <Dialog title={title} onClose={onCancel}>
      <p>{message}</p>
      <div className="dialog-actions">
        <button type="button" onClick={onCancel}>
          {t('common.cancel')}
        </button>
        <button type="button" className="danger" onClick={onConfirm}>
          {confirmLabel}
        </button>
      </div>
    </Dialog>
  )
}

interface ConfirmButtonProps {
  label: string
  confirmTitle: string
  confirmMessage: string
  confirmLabel: string
  onConfirm: () => void
}

// 破壊的操作のトリガーボタン+確認ダイアログの組。「押すと開閉状態を持ち、確定したら
// 閉じてから本処理を呼ぶ」という判断はここ1箇所に閉じ、呼び出し側(各カード)は
// 文言と実処理だけを渡す(principles.md 5節「1つの機能に関わる判断を1箇所に閉じる」)。
export function ConfirmButton({
  label,
  confirmTitle,
  confirmMessage,
  confirmLabel,
  onConfirm,
}: ConfirmButtonProps) {
  const [open, setOpen] = useState(false)
  return (
    <>
      <button type="button" className="danger" onClick={() => setOpen(true)}>
        {label}
      </button>
      {open && (
        <ConfirmDialog
          title={confirmTitle}
          message={confirmMessage}
          confirmLabel={confirmLabel}
          onCancel={() => setOpen(false)}
          onConfirm={() => {
            setOpen(false)
            onConfirm()
          }}
        />
      )}
    </>
  )
}
