import { useCallback, useEffect, useId, useRef, useState, useSyncExternalStore } from 'react'
import { FOCUSABLE_SELECTOR } from './keyboard'
import { useCloseOnBack } from './useCloseOnBack'

// 左のカラム(サイドバー・設定のメニュー)を畳む窓の幅。値はtokens.css末尾の「畳む境目」の式で
// 求めたもの(メディアクエリの条件にはvar()を書けないため、求めた値をここに持つ)。
const NARROW_QUERY = '(width < 38.5rem)'

const narrowMedia = window.matchMedia(NARROW_QUERY)

function subscribeNarrow(onChange: () => void) {
  narrowMedia.addEventListener('change', onChange)
  return () => narrowMedia.removeEventListener('change', onChange)
}

export interface DrawerState {
  // 左のカラムを畳む幅か。
  narrow: boolean
  // 畳んだカラムを引き出しているか。広い窓では常にfalse。
  shown: boolean
  open: () => void
  // カラムの中で行き先を選んだときに閉じる。行き先の画面に移るので、フォーカスは開くボタンへ
  // 戻さない(ヘッダーごと作り直されることもある)。閉じていれば何もしない。
  close: () => void
  // 選ばずに閉じる(Esc・暗幕)。開くボタンへフォーカスを戻す。
  cancel: () => void
  // カラムの入れ物と開くボタンのid(Drawer.tsx)。
  id: string
  toggleId: string
}

// 狭い窓で畳む左のカラムの開け閉め。引き出している間、呼び出し側はカラムの隣の内容を
// `inert`にして操作できなくする。開いたらカラムの先頭へフォーカスを移す。
export function useDrawer(): DrawerState {
  const narrow = useSyncExternalStore(subscribeNarrow, () => narrowMedia.matches)
  const [opened, setOpened] = useState(false)
  // 広げたら閉じる。開いたまま残すと、次に狭めたときに引き出された状態で出る。
  if (!narrow && opened) setOpened(false)
  const shown = narrow && opened

  const id = useId()
  const toggleId = `${id}-toggle`
  // 閉じたあとで開くボタンへフォーカスを戻すか。
  const returnFocus = useRef(false)

  const cancel = useCallback(() => {
    returnFocus.current = true
    setOpened(false)
  }, [])
  // Androidの「戻る」はEscと同じく閉じる。
  useCloseOnBack(shown, cancel)

  useEffect(() => {
    if (!shown) {
      if (returnFocus.current) {
        returnFocus.current = false
        document.getElementById(toggleId)?.focus()
      }
      return
    }
    // 今いる場所(設定の選んでいるタブ)があればそこへ、無ければ先頭へ移す。設定のカラムの先頭は
    // 戻るボタンなので、先頭へ移すとそのままEnterで設定を閉じてしまう。
    const column = document.getElementById(id)
    ;(
      column?.querySelector<HTMLElement>('[aria-current="page"]') ??
      column?.querySelector<HTMLElement>(FOCUSABLE_SELECTOR)
    )?.focus()
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') cancel()
    }
    window.addEventListener('keydown', onKeyDown)
    return () => window.removeEventListener('keydown', onKeyDown)
  }, [shown, cancel, id, toggleId])

  return {
    narrow,
    shown,
    open: () => setOpened(true),
    close: () => setOpened(false),
    cancel,
    id,
    toggleId,
  }
}
