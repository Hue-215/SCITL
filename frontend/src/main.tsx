import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import './tokens.css'
import './index.css'
import App from './App.tsx'
import { applyTheme } from './theme.ts'

// シード色は現時点でユーザー設定を持たないため固定値(旧配色のプライマリブルーを踏襲)。
applyTheme('#2563eb')

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <App />
  </StrictMode>,
)
