import path from 'path'
import react from '@vitejs/plugin-react'
import { defineConfig } from 'vitest/config'

export default defineConfig({
  plugins: [react()],
  oxc: {
    jsx: {
      runtime: 'automatic',
      importSource: 'react',
    },
  },
  test: {
    globals: true,
    environment: 'jsdom',
    setupFiles: ['./src/test/setup.ts'],
    include: ['src/**/*.{test,spec}.{js,mjs,cjs,ts,mts,cts,jsx,tsx}'],
    exclude: ['node_modules', 'dist', '.idea', '.git', '.cache'],
    coverage: {
      provider: 'v8',
      reporter: ['text', 'json', 'json-summary', 'html'],
      // Desktop: every source file counts, also the ones no test imports.
      // Without this the denominator is only the files the tests happen to
      // load, which reads about 18 points higher than the truth.
      include: ['src/**/*.{ts,tsx}'],
      exclude: [
        'node_modules/',
        'src/test/',
        '**/*.d.ts',
        '**/*.config.*',
        '**/types/*',
      ],
      // Desktop: a ratchet, set half a point under the coverage measured on
      // 2026-10-10 (rounded down), so CI fails when it drops. Raise these as
      // coverage rises; never lower one, and never exclude a file to meet it.
      thresholds: {
        lines: 53,
        statements: 52,
        functions: 47,
        branches: 47,
        'src/lib/**': { lines: 82, statements: 79, functions: 83, branches: 72 },
        'src/stores/**': { lines: 32, statements: 32, functions: 40, branches: 22 },
        'src/hooks/**': { lines: 63, statements: 62, functions: 65, branches: 56 },
      },
    },
    css: true,
  },
  resolve: {
    alias: {
      '@': path.resolve(__dirname, './src'),
    },
  },
})
