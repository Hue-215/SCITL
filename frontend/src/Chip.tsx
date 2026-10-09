import type { ReactNode } from 'react'
import Icon from './Icon'

interface ChipProps {
  // 利用者が付けた名前(ファイル名等)でありうるので、要素の境界で閉じ込める。
  label: string
  // 名前の後ろに添える補足(大きさ等)。
  detail?: string
  // 名前の前に置くもの(サムネイル・警告の印等)。
  leading?: ReactNode
  // 名前の下に出す説明(警告・失敗の理由)。指で操作する端末ではツールチップが出ないので、
  // 知らないと困ることはここに書く。
  note?: string
  // 補助の説明のツールチップ(押したときに何が起きるか等)。
  title?: string
  tone?: 'normal' | 'warning' | 'error'
  // 押したときの操作。無ければ押せない。
  onOpen?: () => void
  // 取り消しの×ボタン。`removeLabel`は読み上げ用の名前。
  onRemove?: () => void
  removeLabel?: string
  disabled?: boolean
}

// 添付ファイル名などを枠で表示する共通部品。
export default function Chip({
  label,
  detail,
  leading,
  note,
  title,
  tone = 'normal',
  onOpen,
  onRemove,
  removeLabel,
  disabled = false,
}: ChipProps) {
  const content = (
    <>
      <span className="chip-line">
        {leading}
        <bdi className="chip-label">{label}</bdi>
        {detail && <span className="chip-detail">{detail}</span>}
      </span>
      {note && <span className="chip-note">{note}</span>}
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
          <Icon name="close" />
        </button>
      )}
    </span>
  )
}
