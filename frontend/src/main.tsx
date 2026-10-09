import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import './tokens.css'
import './index.css'
import App from './App.tsx'
import { ComposeProvider } from './ChatCompose'
import { getDisplayLanguage, getStartupFailure } from './api'
import { DEFAULT_LANGUAGE, initI18n } from './i18n'
import StartupFailure from './StartupFailure'
import { applyTheme } from './theme.ts'
import { followVisualViewport } from './viewport'

// シード色はユーザー設定を持たないため固定値。
applyTheme('#0c6cf2')
followVisualViewport()

// データフォルダを開けなかったら、アプリの代わりに理由だけを描く。
const failure = await getStartupFailure().catch(() => null)

// 文言は表示言語が決まってから引く(i18n.ts)。読めなければ(起動に失敗した場合と、Viteだけを
// ブラウザで開いた場合を含む)既定の言語で描く。
initI18n(await getDisplayLanguage().catch(() => DEFAULT_LANGUAGE))

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    {failure ? (
      <StartupFailure failure={failure} />
    ) : (
      <ComposeProvider>
        <App />
      </ComposeProvider>
    )}
  </StrictMode>,
)
