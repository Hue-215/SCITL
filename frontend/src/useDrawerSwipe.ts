import { useRef, type TouchEvent } from 'react'
import type { DrawerState } from './useDrawer'

// 畳んだカラムの隣の内容に付けるもの。畳んである間、内容のどこからでも右へスワイプすれば引き出す。
// 画面の端から始めたスワイプは、Androidのジェスチャーナビゲーションでは「戻る」に取られる。
export function useSwipeToOpen(drawer: DrawerState) {
  const swipe = useHorizontalSwipe('right', drawer.open)
  return drawer.narrow && !drawer.shown ? swipe : undefined
}

// 引き出したカラムと暗幕に付けるもの。左へスワイプすれば閉じる。
export function useSwipeToClose(drawer: DrawerState) {
  const swipe = useHorizontalSwipe('left', drawer.cancel)
  return drawer.shown ? swipe : undefined
}

// 横へのスワイプとみなす指の移動の量(px)。縦の移動の2倍より大きいことも求める(会話欄の縦の
// スクロールと取り違えないため)。
const SWIPE_DISTANCE = 48

// 指が`toward`の向きへ横に動いたら`onSwipe`を呼ぶ、タッチの受け手。1回の接触で1回だけ呼ぶ。
function useHorizontalSwipe(toward: 'left' | 'right', onSwipe: () => void) {
  const start = useRef<{ x: number; y: number } | null>(null)
  const sign = toward === 'right' ? 1 : -1
  return {
    onTouchStart: (e: TouchEvent<HTMLElement>) => {
      const target = e.target as Element
      const touch = e.touches[0]
      start.current =
        e.touches.length === 1 &&
        // ダイアログ等は画面上は外にあっても、Reactのイベントは中の部品から届く。
        e.currentTarget.contains(target) &&
        // 横の動きを自分で使う部品(入力欄の中の文字の選択・カーソルの移動、開いた選択一覧)。
        !target.closest(
          'input, textarea, select, [contenteditable]:not([contenteditable="false"]), .dropdown-popup',
        ) &&
        !scrollsSideways(target, e.currentTarget, sign)
          ? { x: touch.clientX, y: touch.clientY }
          : null
    },
    onTouchMove: (e: TouchEvent<HTMLElement>) => {
      const from = start.current
      if (from === null || e.touches.length !== 1) return
      const dx = (e.touches[0].clientX - from.x) * sign
      const dy = Math.abs(e.touches[0].clientY - from.y)
      if (dx > SWIPE_DISTANCE && dx > dy * 2) {
        start.current = null
        // 文字を選んでいる指の動きでは開け閉めしない(前に選んだ文字が残っている間も同じ)。
        if (window.getSelection()?.isCollapsed !== false) onSwipe()
      } else if (dy > SWIPE_DISTANCE) {
        start.current = null
      }
    },
  }
}

// 触れた所から`root`までに、指の向きへまだ横にスクロールできる入れ物(コードの塊・表)があるか。
// あれば、そのスワイプは入れ物のスクロールに使う。指が右へ動くと、中身は左の端へ向かって戻る。
function scrollsSideways(target: Element, root: Element, sign: 1 | -1): boolean {
  for (let el: Element | null = target; el && el !== root; el = el.parentElement) {
    if (el.scrollWidth <= el.clientWidth) continue
    const overflow = getComputedStyle(el).overflowX
    if (overflow !== 'auto' && overflow !== 'scroll') continue
    const canMove =
      sign > 0 ? el.scrollLeft > 0 : el.scrollLeft + el.clientWidth < el.scrollWidth - 1
    if (canMove) return true
  }
  return false
}
