import { useEffect, useRef, useState, type ReactNode } from 'react'
import { createPortal } from 'react-dom'
import { t } from './i18n'
import { FOCUSABLE_SELECTOR } from './keyboard'
import { useCloseOnBack } from './useCloseOnBack'

interface DialogProps {
  title: string
  onClose: () => void
  children: ReactNode
  // 呼び出し側ごとに必要な幅が異なる(確認ダイアログと画像プレビュー・全文表示等)ため、
  // 枠自体は既定幅だけを持ち、外から上書きできるようにする。
  width?: string
}

// ダイアログの共通枠。余白・角丸・ボタン配置を統一する。本文は children に委ね、枠自体は
// 内容を知らない。
export default function Dialog({ title, onClose, children, width }: DialogProps) {
  const boxRef = useRef<HTMLDivElement>(null)
  const triggerRef = useRef<Element | null>(null)
  // Androidの「戻る」はEscと同じく閉じる(確認のダイアログでは取り消しになる)。
  useCloseOnBack(true, onClose)

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
    <div className="dialog-overlay scrim" onClick={onClose}>
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

// 破壊的操作の確認ダイアログ。「キャンセル/実行」の2択で、実行ボタンを警告色にする。
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
      <div className="button-row dialog-actions">
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

interface NoticeDialogProps {
  title: string
  message: string
  onClose: () => void
}

// 知らせるだけのダイアログ。「閉じる」の1つだけを置く。
export function NoticeDialog({ title, message, onClose }: NoticeDialogProps) {
  return (
    <Dialog title={title} onClose={onClose}>
      <p>{message}</p>
      <div className="button-row dialog-actions">
        <button type="button" onClick={onClose}>
          {t('common.close')}
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
  disabled?: boolean
}

// 破壊的操作のトリガーボタン+確認ダイアログの組。確定したら閉じてから本処理を呼ぶ。呼び出し側は
// 文言と実処理だけを渡す。トリガーはホバーしたときだけ警告色にし、確定のボタンは常に警告色。
export function ConfirmButton({
  label,
  confirmTitle,
  confirmMessage,
  confirmLabel,
  onConfirm,
  disabled = false,
}: ConfirmButtonProps) {
  const [open, setOpen] = useState(false)
  return (
    <>
      <button
        type="button"
        className="danger-hover"
        disabled={disabled}
        onClick={() => setOpen(true)}
      >
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
