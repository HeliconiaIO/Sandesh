import { resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { defineConfig } from 'vite'

const webRoot = fileURLToPath(new URL('.', import.meta.url))

export default defineConfig({
  root: resolve(webRoot, 'site'),
  base: process.env.GITHUB_ACTIONS === 'true' ? '/Sandesh/' : '/',
  publicDir: false,
  build: {
    outDir: resolve(webRoot, 'site-dist'),
    emptyOutDir: true,
  },
})