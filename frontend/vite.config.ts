import react from '@vitejs/plugin-react'
import { fileURLToPath } from 'node:url'
import { defineConfig, searchForWorkspaceRoot } from 'vite'

// Tauri固有の設定(devUrl: http://localhost:1420, tauri.conf.jsonと合わせる)。
// https://v2.tauri.app/start/frontend/vite/
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  // バンドルに入った依存のライセンスの一覧を`dist/.vite/license.json`に出す。許容の確認と、配布物に
  // 同梱する一覧の組み立ては`scripts/assemble-dist.mjs`が行う。
  build: { license: { fileName: '.vite/license.json' } },
  server: {
    port: 1420,
    strictPort: true,
    // 言語ファイルはcoreと共有するため、frontendの外(リポジトリ直下のlang/)にある。
    // 指定すると既定の許可範囲が置き換わるので、既定の範囲も並べる。
    fs: {
      allow: [searchForWorkspaceRoot(process.cwd()), fileURLToPath(new URL('../lang', import.meta.url))],
    },
  },
})
