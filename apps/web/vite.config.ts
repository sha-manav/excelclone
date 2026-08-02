/// <reference types="vitest/config" />
import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

export default defineConfig({
  plugins: [react()],
  // The engine is a local wasm-pack package linked from crates/wasm/pkg.
  // Pre-bundling it breaks the `?url` asset import the loader relies on, and
  // the dev server needs permission to read outside apps/web to serve the
  // .wasm itself.
  optimizeDeps: { exclude: ['gridline-wasm'] },
  server: {
    fs: { allow: ['..', '../../crates/wasm/pkg'] },
  },
  test: {
    // `e2e/` belongs to Playwright, which has its own runner; without this
    // vitest tries to execute the specs and fails on its missing globals.
    include: ['src/**/*.test.{ts,tsx}'],
    environment: 'node',
  },
})
