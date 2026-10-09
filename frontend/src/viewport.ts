/**
 * 見えている範囲が窓より低い間(ソフトキーボードが出ている間)だけ、その高さを`--viewport-height`に
 * 写す。それ以外は上書きを外し、tokens.cssの既定(窓の高さ)に任せる。指で拡大している間に縮む分は
 * 拡大率を掛け戻す。なぜ要るかはui.md「指で操作する端末」。
 */
export function followVisualViewport(): void {
  const viewport = window.visualViewport
  if (!viewport) return
  const root = document.documentElement.style
  let written = ''
  const update = () => {
    const height = Math.round(viewport.height * viewport.scale)
    const value = height < window.innerHeight ? `${height}px` : ''
    if (value === written) return
    written = value
    if (value) root.setProperty('--viewport-height', value)
    else root.removeProperty('--viewport-height')
  }
  update()
  viewport.addEventListener('resize', update)
  window.addEventListener('resize', update)
}
