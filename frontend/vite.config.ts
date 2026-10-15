import { defineConfig } from 'vitest/config'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'

export default defineConfig({
  base: './',
  plugins: [react(), tailwindcss()],
  server: { host: '127.0.0.1', strictPort: true },
  build: { target: 'es2022', chunkSizeWarningLimit: 1800 },
  test: { environment: 'jsdom', setupFiles: ['./src/test/setup.ts'], exclude: ['e2e/**', 'node_modules/**'] },
})
