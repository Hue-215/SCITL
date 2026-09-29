import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import './tokens.css'
import './index.css'
import App from './App.tsx'
import { getDisplayLanguage } from './api'
import { DEFAULT_LANGUAGE, initI18n } from './i18n'
import { applyTheme } from './theme.ts'

// シード色はユーザー設定を持たないため固定値。
applyTheme('#2563eb')

// 文言は表示言語が決まってから引く(i18n.ts)。読めなければ(Viteだけをブラウザで開いた
// 場合を含む)既定の言語で描く。
initI18n(await getDisplayLanguage().catch(() => DEFAULT_LANGUAGE))

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <App />
  </StrictMode>,
)
