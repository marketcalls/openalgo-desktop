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
      // Desktop: only what never runs in the app is left out: test helpers,
      // declaration files and config. Files under types/ stay in: some hold
      // executable helpers (strategy_module.ts, option-chain.ts), and the
      // rest are type-only, which adds nothing to either side of the ratio.
      exclude: ['node_modules/', 'src/test/', '**/*.d.ts', '**/*.config.*'],
      // Desktop: a ratchet, set half a point under the coverage measured on
      // 2026-10-12 (rounded down), so CI fails when it drops. Raise these as
      // coverage rises; never lower one, and never exclude a file to meet it.
      // The trading areas have floors of their own, so a drop there cannot
      // hide behind a rise elsewhere.
      thresholds: {
        lines: 54,
        statements: 53,
        functions: 49,
        branches: 48,
        'src/lib/**': { lines: 82, statements: 79, functions: 83, branches: 72 },
        'src/stores/**': { lines: 32, statements: 32, functions: 40, branches: 22 },
        'src/hooks/**': { lines: 72, statements: 71, functions: 70, branches: 59 },
        'src/types/**': { lines: 92, statements: 90, functions: 93, branches: 84 },
        // Orders: the book pages, order entry and the terminal's order dock.
        'src/pages/{OrderBook,TradeBook,Positions}.tsx': {
          lines: 52,
          statements: 50,
          functions: 48,
          branches: 42,
        },
        'src/components/trading/{PlaceOrderDialog,ExecuteBasketDialog,GttTab,dock/*}.{ts,tsx}': {
          lines: 57,
          statements: 55,
          functions: 47,
          branches: 51,
        },
        // Strategies: the strategy module and the strategy builder.
        'src/{pages/strategy/*,api/strategy_module,types/strategy_module,lib/trading/strategy*}.{ts,tsx}':
          { lines: 57, statements: 56, functions: 46, branches: 44 },
        'src/{pages/StrategyBuilder,components/strategy-builder/*,lib/strategyMath,lib/strategyTemplates}.{ts,tsx}':
          { lines: 88, statements: 86, functions: 82, branches: 78 },
        // Scalping and its protection (stop-loss, target, trailing).
        'src/{pages/Scalping,components/scalping/*,hooks/useTrailingSL,lib/scalping*,api/scalping}.{ts,tsx}':
          { lines: 9, statements: 8, functions: 3, branches: 11 },
        // Broker connection pages.
        'src/{pages/BrokerSelect,pages/BrokerTOTP,components/auth/Broker*,stores/brokerStore}.{ts,tsx}':
          { lines: 59, statements: 59, functions: 70, branches: 57 },
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
