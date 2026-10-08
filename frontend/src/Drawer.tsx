import type { ReactNode } from 'react'
import type { DrawerState } from './useDrawer'

// 狭い窓で畳む左のカラムの入れ物。引き出している間は、内容の上に重ね、後ろに暗幕を敷く。
export function Drawer({ drawer, children }: { drawer: DrawerState; children: ReactNode }) {
  return (
    <>
      {drawer.shown && <div className="drawer-backdrop scrim" onClick={drawer.cancel} />}
      <div id={drawer.id} className={drawer.shown ? 'drawer open' : 'drawer'}>
        {children}
      </div>
    </>
  )
}

// 畳んだカラムを引き出すボタン。広い窓では出さない。
export function DrawerToggle({ drawer, label }: { drawer: DrawerState; label: string }) {
  if (!drawer.narrow) return null
  return (
    <button
      id={drawer.toggleId}
      type="button"
      className="icon-button"
      onClick={drawer.open}
      aria-expanded={drawer.shown}
      aria-controls={drawer.id}
      aria-label={label}
      title={label}
    >
      ☰
    </button>
  )
}
