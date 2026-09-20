import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'

// Tauri固有の設定(devUrl: http://localhost:1420, tauri.conf.jsonと合わせる)。
// https://v2.tauri.app/start/frontend/vite/
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
  },
})
