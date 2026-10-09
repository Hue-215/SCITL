/**
 * 画面の入れ物の高さ(`--viewport-height`)を、見えている範囲の高さに合わせ続ける。
 *
 * AndroidのWebView(M139以降)は、ソフトキーボードが出るとvisual viewportだけを縮め、窓の
 * 高さ(`100vh`・`innerHeight`)は変えない。窓の高さのままだと、入力欄がキーボードの下に隠れるか、
 * 入力欄を見せるために画面ごと上へずらされて見出しが隠れる。`interactive-widget`の指定と
 * VirtualKeyboard APIは、WebViewでは効かない(System WebView 145で確認)。
 *
 * 指で拡大している間はvisual viewportの高さが拡大率の分だけ縮むので、掛け戻して窓の高さを保つ。
 */
export function followVisualViewport(): void {
  const viewport = window.visualViewport
  // 無ければtokens.cssの既定(窓の高さ)のまま。
  if (!viewport) return
  const update = () =>
    document.documentElement.style.setProperty(
      '--viewport-height',
      `${viewport.height * viewport.scale}px`,
    )
  update()
  viewport.addEventListener('resize', update)
}
