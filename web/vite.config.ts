/// <reference types="vitest/config" />
import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

// The dev server proxies the API to the Rust backend so that application code
// never needs to know whether it is running against Vite or against the
// embedded assets: in both cases the API is same-origin at `/api`. That is what
// keeps `http://localhost:8080` out of the components (§43).
const BACKEND = process.env.SANDESH_API ?? 'http://127.0.0.1:8080'

export default defineConfig({
  plugins: [react()],
  build: {
    // Served by the Rust binary from `web/dist`; see `sandesh-api::assets`.
    outDir: 'dist',
    sourcemap: true,
  },
  server: {
    proxy: {
      '/api': {
        target: BACKEND,
        changeOrigin: true,
        // The event stream shares the `/api` prefix, so the proxy must speak
        // WebSocket too or live updates silently fail in development only.
        ws: true,
      },
    },
  },
  test: {
    environment: 'jsdom',
    setupFiles: ['./src/test/setup.ts'],
    globals: true,
    testTimeout: 15000,
    // Playwright drives the browser suite; vitest must not try to run it.
    exclude: ['e2e/**', 'node_modules/**'],
  },
})
