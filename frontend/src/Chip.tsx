import type { ReactNode } from 'react'

interface ChipProps {
  // 利用者が付けた名前(ファイル名等)でありうるので、要素の境界で閉じ込める
  // (architecture.md 10節「画面(モデル・ユーザーが書いたもの)」)。
  label: string
  // 名前の後ろに添える補足(大きさ等)。
  detail?: string
  // 名前の前に置くもの(サムネイル・警告の印等)。
  leading?: ReactNode
  // 説明のツールチップ。
  title?: string
  tone?: 'normal' | 'warning' | 'error'
  // 押したときの操作。無ければ押せない。
  onOpen?: () => void
  // 取り消しの×ボタン。`removeLabel`は読み上げ用の名前。
  onRemove?: () => void
  removeLabel?: string
  disabled?: boolean
}

// 添付ファイル名などを枠で表示する共通部品(legacy/frontend.md 5節「チップ」、
// principles.md 6節「共通の操作は共通部品を経由させる」)。
export default function Chip({
  label,
  detail,
  leading,
  title,
  tone = 'normal',
  onOpen,
  onRemove,
  removeLabel,
  disabled = false,
}: ChipProps) {
  const content = (
    <>
      {leading}
      <bdi className="chip-label">{label}</bdi>
      {detail && <span className="chip-detail">{detail}</span>}
    </>
  )
  return (
    <span className={`chip chip-${tone}`} title={title}>
      {onOpen ? (
        <button type="button" className="chip-main" onClick={onOpen} disabled={disabled}>
          {content}
        </button>
      ) : (
        <span className="chip-main">{content}</span>
      )}
      {onRemove && (
        <button
          type="button"
          className="chip-remove"
          onClick={onRemove}
          aria-label={removeLabel}
          disabled={disabled}
        >
          {/* グリフをアイコン代わりに使う(tokens.cssの--icon-size-*の注記) */}×
        </button>
      )}
    </span>
  )
}
