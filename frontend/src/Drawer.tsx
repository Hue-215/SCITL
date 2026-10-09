import type { ReactNode } from 'react'
import type { DrawerState } from './useDrawer'
import { useSwipeToClose } from './useDrawerSwipe'

// 狭い窓で畳む左のカラムの入れ物。引き出している間は、内容の上に重ね、後ろに暗幕を敷く。
// 引き出したカラムと暗幕は、左へのスワイプでも閉じる。
export function Drawer({ drawer, children }: { drawer: DrawerState; children: ReactNode }) {
  const swipeToClose = useSwipeToClose(drawer)
  return (
    <>
      {drawer.shown && (
        <div className="drawer-backdrop scrim" onClick={drawer.cancel} {...swipeToClose} />
      )}
      <div id={drawer.id} className={drawer.shown ? 'drawer open' : 'drawer'} {...swipeToClose}>
        {children}
      </div>
    </>
  )
}

// 畳んだカラムを開け閉めするボタン。広い窓では出さない。
export function DrawerToggle({ drawer, label }: { drawer: DrawerState; label: string }) {
  if (!drawer.narrow) return null
  return (
    <button
      id={drawer.toggleId}
      type="button"
      className="icon-button"
      onClick={drawer.shown ? drawer.cancel : drawer.open}
      aria-expanded={drawer.shown}
      aria-controls={drawer.id}
      aria-label={label}
      title={label}
    >
      ☰
    </button>
  )
}
