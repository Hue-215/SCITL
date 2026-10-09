import { useLayoutEffect, useRef } from 'react'
import Icon from './Icon'
import { onPopupListKeyDown, usePopup } from './usePopup'

export interface MenuItem {
  key: string
  label: string
  // 取り消せない操作(削除)。警告の色で出す。
  danger?: boolean
  onSelect: () => void
}

// 押すと操作の一覧を開くボタン(︙)。一覧はボタンの下に、右端を揃えて開く。位置と幅の基準は
// 呼び出し側が用意する位置決めされた入れ物(Dropdown.tsxと同じ)。項目を選ぶと閉じてから
// 操作を呼ぶ。
export default function MenuButton({
  label,
  items,
  disabled,
}: {
  // ボタンの名前(読み上げ・ツールチップ)。
  label: string
  items: MenuItem[]
  disabled?: boolean
}) {
  const { open, toggle, close, rootRef, toggleRef, onBlur } = usePopup()
  return (
    <div className="menu" ref={rootRef} onBlur={onBlur}>
      <button
        type="button"
        ref={toggleRef}
        className="icon-button"
        disabled={disabled}
        aria-label={label}
        title={label}
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={toggle}
      >
        <Icon name="more_vert" />
      </button>
      {open && <MenuList items={items} onClose={close} />}
    </div>
  )
}

// 開いている間だけ存在する。開いたら先頭の項目へフォーカスを移す。
function MenuList({ items, onClose }: { items: MenuItem[]; onClose: () => void }) {
  const listRef = useRef<HTMLUListElement>(null)
  useLayoutEffect(() => {
    listRef.current?.querySelector<HTMLElement>('[role="menuitem"]')?.focus()
  }, [])
  return (
    <div
      className="dropdown-popup dropdown-popup-down dropdown-popup-end"
      onKeyDown={(e) => onPopupListKeyDown(e, listRef.current, 'menuitem', onClose)}
    >
      <ul className="dropdown-options" role="menu" ref={listRef}>
        {items.map((item) => (
          <li key={item.key} role="none">
            <button
              type="button"
              role="menuitem"
              className={item.danger ? 'list-row menu-danger' : 'list-row'}
              onClick={() => {
                onClose()
                item.onSelect()
              }}
            >
              {item.label}
            </button>
          </li>
        ))}
      </ul>
    </div>
  )
}
