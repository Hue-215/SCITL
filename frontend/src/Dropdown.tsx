import { useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent, type ReactNode } from 'react'
import { isCommitEnter } from './keyboard'

export interface DropdownOption {
  key: string
  label: string
  // 名前の横に弱めて添える補足(モデルの属するプロバイダー名等)。検索の対象に含める。
  detail?: string
}

interface DropdownProps {
  // 開閉は呼び出し側が持つ。隣り合う一覧のうち1つだけを開いておくため
  // (legacy/frontend.md 1節「片方を開くともう片方は自動で閉じる」)。
  open: boolean
  onOpenChange: (open: boolean) => void
  // 閉じたボタンに出す文言。
  label: ReactNode
  title?: string
  disabled?: boolean
  options: DropdownOption[]
  selectedKey: string | null
  onSelect: (key: string) => void
  // 渡したときだけ検索欄を出す。
  searchPlaceholder?: string
  // 一覧が空のときの一文。
  emptyText: string
  // 一覧をボタンのどちら側の端に揃えるか。入れ物の端に置いたボタンで、一覧が入れ物の外へ
  // はみ出さないようにする。
  align: 'start' | 'end'
}

// 検索欄付きの選択一覧(チャット入力欄の下のモデル・思考の強さ。legacy/frontend.md 1節)。
// 一覧はボタンの上に開く(入力欄の下に置くため、下には場所が無い)。
//
// 一覧の位置と幅の基準は、呼び出し側が用意する位置決めされた入れ物(`position: relative`)。
// この部品自身は基準にならない。ボタンの幅ではなく並んだ入れ物全体の幅まで広げるため。
export default function Dropdown({
  open,
  onOpenChange,
  label,
  title,
  disabled,
  align,
  ...listProps
}: DropdownProps) {
  const rootRef = useRef<HTMLDivElement>(null)
  const toggleRef = useRef<HTMLButtonElement>(null)

  // 一覧の外を押したら閉じる。
  useEffect(() => {
    if (!open) return
    const onPointerDown = (e: PointerEvent) => {
      if (!rootRef.current?.contains(e.target as Node)) onOpenChange(false)
    }
    document.addEventListener('pointerdown', onPointerDown)
    return () => document.removeEventListener('pointerdown', onPointerDown)
  }, [open, onOpenChange])

  const close = () => {
    onOpenChange(false)
    toggleRef.current?.focus()
  }

  return (
    <div className="dropdown" ref={rootRef}>
      <button
        type="button"
        ref={toggleRef}
        className="dropdown-toggle"
        title={title}
        disabled={disabled}
        aria-haspopup="listbox"
        aria-expanded={open}
        onClick={() => onOpenChange(!open)}
      >
        {label}
        {/* 一覧は上に開くので、閉じているときは上向き */}
        {open ? ' ▼' : ' ▲'}
      </button>
      {open && <DropdownList {...listProps} align={align} onClose={close} />}
    </div>
  )
}

type DropdownListProps = Pick<
  DropdownProps,
  'options' | 'selectedKey' | 'onSelect' | 'searchPlaceholder' | 'emptyText' | 'align'
> & { onClose: () => void }

function matches(option: DropdownOption, query: string): boolean {
  const q = query.trim().toLowerCase()
  if (!q) return true
  return `${option.label} ${option.detail ?? ''}`.toLowerCase().includes(q)
}

// 開いている間だけ存在する。検索語は開くたびに空から始まる。
function DropdownList({
  options,
  selectedKey,
  onSelect,
  searchPlaceholder,
  emptyText,
  align,
  onClose,
}: DropdownListProps) {
  const [query, setQuery] = useState('')
  const listRef = useRef<HTMLUListElement>(null)
  const searchRef = useRef<HTMLInputElement>(null)

  const visible = options.filter((o) => matches(o, query))

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
    ;(searchRef.current ?? selected ?? list?.querySelector<HTMLElement>('[role="option"]'))?.focus()
  }, [])

  const pick = (key: string) => {
    onClose()
    onSelect(key)
  }

  const onKeyDown = (e: KeyboardEvent) => {
    if (e.key === 'Escape') {
      e.preventDefault()
      onClose()
      return
    }
    if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return
    e.preventDefault()
    const rows = Array.from(listRef.current?.querySelectorAll<HTMLElement>('[role="option"]') ?? [])
    if (rows.length === 0) return
    const at = rows.indexOf(document.activeElement as HTMLElement)
    const next = e.key === 'ArrowDown' ? Math.min(at + 1, rows.length - 1) : at - 1
    // 先頭より上へは検索欄に戻る(検索欄が無ければ先頭に留まる)。
    if (next < 0) (searchRef.current ?? rows[0]).focus()
    else rows[next].focus()
  }

  return (
    <div className={`dropdown-popup dropdown-popup-${align}`} onKeyDown={onKeyDown}>
      {searchPlaceholder !== undefined && (
        <input
          ref={searchRef}
          type="search"
          className="dropdown-search"
          value={query}
          placeholder={searchPlaceholder}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={(e) => {
            // 絞り込んだ先頭を選ぶ。
            if (isCommitEnter(e) && visible.length > 0) {
              e.preventDefault()
              pick(visible[0].key)
            }
          }}
        />
      )}
      {visible.length === 0 ? (
        <p className="list-empty dropdown-empty">{emptyText}</p>
      ) : (
        <ul className="dropdown-options" role="listbox" ref={listRef}>
          {visible.map((option) => {
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
