import react from '@vitejs/plugin-react'
import { existsSync, mkdirSync, readdirSync, renameSync, rmdirSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { defineConfig, type Plugin, searchForWorkspaceRoot } from 'vite'

// Viteが`dist/.vite/`に出すライセンスの一覧(下の`build.license`)を、`dist-meta/`へ移す。`dist`に
// 残すと、`tauri build`が画面の資産として実行ファイルに埋め込む。
function moveLicenseList(): Plugin {
  const from = fileURLToPath(new URL('./dist/.vite/license.json', import.meta.url))
  const to = fileURLToPath(new URL('./dist-meta/license.json', import.meta.url))
  return {
    name: 'scitl-move-license-list',
    apply: 'build',
    closeBundle() {
      if (!existsSync(from)) return
      mkdirSync(fileURLToPath(new URL('./dist-meta', import.meta.url)), { recursive: true })
      renameSync(from, to)
      const emptied = fileURLToPath(new URL('./dist/.vite', import.meta.url))
      if (readdirSync(emptied).length === 0) rmdirSync(emptied)
    },
  }
}

// Tauri固有の設定(devUrl: http://localhost:1420, tauri.conf.jsonと合わせる)。
// https://v2.tauri.app/start/frontend/vite/
export default defineConfig({
  plugins: [react(), moveLicenseList()],
  clearScreen: false,
  // バンドルに入った依存のライセンスの一覧を出す(`moveLicenseList`が`dist-meta/license.json`へ移す)。
  // 許容の確認と、配布物に同梱する一覧の組み立ては`scripts/assemble-dist.mjs`が行う。
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
