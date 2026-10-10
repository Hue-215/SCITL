import { useId, useLayoutEffect, useRef, type ReactNode } from 'react'
import Icon from './Icon'
import { onPopupListKeyDown, usePopup } from './usePopup'

export interface DropdownOption {
  key: string
  label: string
  // 名前の横に弱めて添える補足(モデルの属するプロバイダー名等)。
  detail?: string
}

interface DropdownProps {
  // 項目名の要素のid。渡すと、読み上げの名前を「項目名 選択中の値」にする(ボタンの名前は
  // 中身の文字、つまり選択中の値だけになり、何を選ぶ欄かが伝わらないため)。
  labelledBy?: string
  // 閉じたボタンに出す文言。
  label: ReactNode
  title?: string
  disabled?: boolean
  // 閉じたボタンの見た目を呼び出し側で変えるときのクラス。無ければ基盤層のボタンのまま。
  toggleClassName?: string
  options: DropdownOption[]
  selectedKey: string | null
  onSelect: (key: string) => void
  // 一覧が空のときの一文。
  emptyText?: string
  // 一覧をボタンの上下どちらに開くか。下に場所の無い所(チャット入力欄の下)では上に開く。
  direction: 'up' | 'down'
  // 一覧をボタンのどちら側の端に揃えるか。入れ物の端に置いたボタンで、一覧が入れ物の外へ
  // はみ出さないようにする。
  align: 'start' | 'end'
}

// 選択一覧。アプリ内の選択はすべてこれで描き、開いた一覧の見た目を揃える(OSが描く<select>の
// 一覧は形も開く向きも変えられないため)。
//
// 一覧の位置と幅の基準は、呼び出し側が用意する位置決めされた入れ物(`position: relative`)。
// この部品自身は基準にならない。ボタンの幅ではなく並んだ入れ物全体の幅まで広げられるように
// するため。
//
// 開け閉めは`usePopup`(外を押す・フォーカスが外へ移る・「戻る」で閉じる)。
export default function Dropdown({
  labelledBy,
  label,
  title,
  disabled,
  toggleClassName,
  direction,
  align,
  ...listProps
}: DropdownProps) {
  const { open, toggle, close, rootRef, toggleRef, onBlur } = usePopup()
  const valueId = useId()

  // 矢印は一覧が開く向きを指し、開いている間は閉じる向きを指す。
  const arrow = (direction === 'up') === open ? 'keyboard_arrow_down' : 'keyboard_arrow_up'

  return (
    <div className="dropdown" ref={rootRef} onBlur={onBlur}>
      <button
        type="button"
        ref={toggleRef}
        className={toggleClassName ? `dropdown-toggle ${toggleClassName}` : 'dropdown-toggle'}
        title={title}
        disabled={disabled}
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-labelledby={labelledBy && `${labelledBy} ${valueId}`}
        onClick={toggle}
      >
        <span id={valueId} className="dropdown-label">
          {label}
        </span>
        <Icon name={arrow} />
      </button>
      {open && (
        <DropdownList {...listProps} direction={direction} align={align} onClose={close} />
      )}
    </div>
  )
}

type DropdownListProps = Pick<
  DropdownProps,
  'options' | 'selectedKey' | 'onSelect' | 'emptyText' | 'direction' | 'align'
> & { onClose: () => void }

// 開いている間だけ存在する。
function DropdownList({
  options,
  selectedKey,
  onSelect,
  emptyText,
  direction,
  align,
  onClose,
}: DropdownListProps) {
  const listRef = useRef<HTMLUListElement>(null)

  // 選択中の項目が一覧の中ほどに見えるようにする。scrollIntoViewは祖先の
  // (overflow: hiddenの)入れ物までスクロールさせうるため、一覧の中だけを動かす。
  useLayoutEffect(() => {
    const list = listRef.current
    const selected = list?.querySelector<HTMLElement>('[aria-selected="true"]')
    if (list && selected) {
      const listRect = list.getBoundingClientRect()
      const rowRect = selected.getBoundingClientRect()
      list.scrollTop += rowRect.top - listRect.top - (listRect.height - rowRect.height) / 2
    }
    ;(selected ?? list?.querySelector<HTMLElement>('[role="option"]'))?.focus()
  }, [])

  const pick = (key: string) => {
    onClose()
    onSelect(key)
  }

  return (
    <div
      className={`dropdown-popup dropdown-popup-${direction} dropdown-popup-${align}`}
      onKeyDown={(e) => onPopupListKeyDown(e, listRef.current, 'option', onClose)}
    >
      {options.length === 0 ? (
        emptyText && <p className="list-empty dropdown-empty">{emptyText}</p>
      ) : (
        <ul className="dropdown-options" role="listbox" ref={listRef}>
          {options.map((option) => {
            const selected = option.key === selectedKey
            return (
              <li key={option.key}>
                <button
                  type="button"
                  role="option"
                  aria-selected={selected}
                  className={selected ? 'list-row selected' : 'list-row'}
                  onClick={() => pick(option.key)}
                >
                  {option.label}
                  {option.detail && <span className="list-row-detail">{option.detail}</span>}
                </button>
              </li>
            )
          })}
        </ul>
      )}
    </div>
  )
}
